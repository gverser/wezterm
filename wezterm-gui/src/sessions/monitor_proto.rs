//! What the background channel on an ssh connection says, and how it is
//! turned into the status bar and the file page's idea of each pane's
//! directory.
//!
//! The remote side is one POSIX sh script fed to `sh -s` over an exec channel
//! on the same connection as the panes. It prints a frame every second: the
//! directory of every wezterm pane's shell on this connection, and every
//! third frame the raw counters from /proc. Rates and percentages are worked
//! out here, from the difference between two frames, so the script stays dumb
//! and writes nothing on the server.

use termwiz::cell::unicode_column_width;

/// Fed to `sh -s` on the server. Reads /proc and a few standard tools,
/// writes nothing. See the module docs for the frame format.
///
/// Finding a pane's shell: each one carries `WEZTERM_REMOTE_PANE=<pane id>`
/// in its environment (`RemoteSshDomain` puts it on the command line). The
/// candidate must descend from the sshd process that owns this channel -- one
/// sshd process carries all the channels of a connection -- so that another
/// connection's pane with the same id is not mistaken for ours. The directory
/// reported is that of the terminal's foreground process group; when it can't
/// be read (after `su`, say) "?" is printed and the last answer stands.
pub const MONITOR_SCRIPT: &str = r##"conn=$$
while :; do
  c=$(cat /proc/$conn/comm 2>/dev/null) || { conn=; break; }
  case "$c" in sshd*) break;; esac
  s=$(cat /proc/$conn/stat 2>/dev/null) || { conn=; break; }
  s=${s##*) }; set -- $s; conn=$2
  [ "$conn" -gt 1 ] 2>/dev/null || { conn=; break; }
done
if [ ! -r /proc/stat ]; then echo @unsupported; exit 0; fi
t=0
while :; do
  echo @frame
  grep -z -H '^WEZTERM_REMOTE_PANE=' /proc/[0-9]*/environ 2>/dev/null | tr '\0' '\n' | {
    seen=' '
    while IFS= read -r l; do
      pid=${l#/proc/}; pid=${pid%%/*}; pane=${l#*=}
      case "$seen" in *" $pane "*) continue;; esac
      if [ -n "$conn" ]; then
        p=$pid; ok=; n=0
        while [ "$p" -gt 1 ] 2>/dev/null && [ $n -lt 16 ]; do
          s=$(cat /proc/$p/stat 2>/dev/null) || break
          s=${s##*) }; set -- $s; p=$2; n=$((n+1))
          [ "$p" = "$conn" ] && { ok=1; break; }
        done
        [ -n "$ok" ] || continue
      fi
      s=$(cat /proc/$pid/stat 2>/dev/null) || continue
      s=${s##*) }; set -- $s
      [ "$5" = 0 ] && continue
      seen="$seen$pane "
      d=$(readlink /proc/$6/cwd 2>/dev/null) || d='?'
      printf 'cwd\t%s\t%s\n' "$pane" "$d"
    done
  }
  if [ $((t % 3)) = 0 ]; then
    head -n 1 /proc/stat
    grep -E '^(MemTotal|MemAvailable):' /proc/meminfo
    tail -n +3 /proc/net/dev | sed 's/^/net /'
    echo "up $(cut -d' ' -f1 /proc/uptime)"
    who 2>/dev/null | sed 's/^/user /'
    df -P -T -k 2>/dev/null | tail -n +2 | sed 's/^/disk /'
    echo "host $(hostname 2>/dev/null || cat /proc/sys/kernel/hostname)"
  fi
  echo @end
  t=$((t+1))
  sleep 1
done
"##;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Frame {
    /// (pane id, directory); None when the directory couldn't be read.
    pub cwds: Vec<(u64, Option<String>)>,
    /// Present every third frame.
    pub metrics: Option<RawMetrics>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct RawMetrics {
    pub cpu_total: u64,
    /// idle + iowait
    pub cpu_idle: u64,
    pub mem_total_kb: u64,
    pub mem_avail_kb: u64,
    /// (interface, rx bytes, tx bytes), loopback excluded.
    pub net: Vec<(String, u64, u64)>,
    pub uptime_secs: f64,
    /// Logged in users, each once, in the order `who` listed them.
    pub users: Vec<String>,
    pub disks: Vec<DiskUsage>,
    pub host: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiskUsage {
    pub mount: String,
    pub fstype: String,
    pub size_kb: u64,
    pub used_kb: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParseEvent {
    Frame(Frame),
    /// The server has no /proc: not Linux.
    Unsupported,
}

#[derive(Default)]
pub struct FrameParser {
    frame: Frame,
}

impl FrameParser {
    /// Feed one line of the script's output; a completed frame comes back at
    /// its `@end`. Lines that don't parse are dropped.
    pub fn feed_line(&mut self, line: &str) -> Option<ParseEvent> {
        let line = line.trim_end_matches(['\r', '\n']);
        match line {
            "@unsupported" => return Some(ParseEvent::Unsupported),
            "@frame" => {
                self.frame = Frame::default();
                return None;
            }
            "@end" => return Some(ParseEvent::Frame(std::mem::take(&mut self.frame))),
            _ => {}
        }

        if let Some(rest) = line.strip_prefix("cwd\t") {
            let mut parts = rest.splitn(2, '\t');
            if let (Some(pane), Some(dir)) = (parts.next(), parts.next()) {
                if let Ok(pane) = pane.parse::<u64>() {
                    let dir = if dir == "?" || dir.is_empty() {
                        None
                    } else {
                        Some(dir.to_string())
                    };
                    self.frame.cwds.push((pane, dir));
                }
            }
            return None;
        }

        if let Some(rest) = line.strip_prefix("cpu ") {
            let fields: Vec<u64> = rest
                .split_whitespace()
                .filter_map(|f| f.parse().ok())
                .collect();
            if fields.len() >= 4 {
                // user nice system idle iowait irq softirq steal; guest time
                // is already counted in user.
                let m = metrics(&mut self.frame);
                m.cpu_total = fields.iter().take(8).sum();
                m.cpu_idle = fields[3] + fields.get(4).copied().unwrap_or(0);
            }
        } else if let Some(rest) = line.strip_prefix("MemTotal:") {
            if let Some(kb) = first_number(rest) {
                metrics(&mut self.frame).mem_total_kb = kb;
            }
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            if let Some(kb) = first_number(rest) {
                metrics(&mut self.frame).mem_avail_kb = kb;
            }
        } else if let Some(rest) = line.strip_prefix("net ") {
            if let Some((iface, counters)) = rest.split_once(':') {
                let iface = iface.trim();
                let fields: Vec<u64> = counters
                    .split_whitespace()
                    .filter_map(|f| f.parse().ok())
                    .collect();
                if iface != "lo" && fields.len() >= 9 {
                    metrics(&mut self.frame)
                        .net
                        .push((iface.to_string(), fields[0], fields[8]));
                }
            }
        } else if let Some(rest) = line.strip_prefix("up ") {
            if let Ok(secs) = rest.trim().parse::<f64>() {
                metrics(&mut self.frame).uptime_secs = secs;
            }
        } else if let Some(rest) = line.strip_prefix("user ") {
            if let Some(user) = rest.split_whitespace().next() {
                let m = metrics(&mut self.frame);
                if !m.users.iter().any(|u| u == user) {
                    m.users.push(user.to_string());
                }
            }
        } else if let Some(rest) = line.strip_prefix("disk ") {
            // Filesystem Type 1024-blocks Used Available Capacity Mounted-on,
            // where the mount point may itself contain spaces.
            let fields: Vec<&str> = rest.split_whitespace().collect();
            if fields.len() >= 7 {
                if let (Ok(size_kb), Ok(used_kb)) = (fields[2].parse(), fields[3].parse()) {
                    metrics(&mut self.frame).disks.push(DiskUsage {
                        mount: fields[6..].join(" "),
                        fstype: fields[1].to_string(),
                        size_kb,
                        used_kb,
                    });
                }
            }
        } else if let Some(rest) = line.strip_prefix("host ") {
            metrics(&mut self.frame).host = rest.trim().to_string();
        }
        None
    }
}

/// The frame's metrics, started on the first metrics line.
fn metrics(frame: &mut Frame) -> &mut RawMetrics {
    frame.metrics.get_or_insert_with(RawMetrics::default)
}

fn first_number(text: &str) -> Option<u64> {
    text.split_whitespace().next()?.parse().ok()
}

/// Whether a mount is a real filesystem worth a place on the status bar.
pub fn keep_disk(d: &DiskUsage) -> bool {
    const PSEUDO: &[&str] = &[
        "tmpfs",
        "devtmpfs",
        "overlay",
        "squashfs",
        "proc",
        "sysfs",
        "devpts",
        "cgroup",
        "cgroup2",
        "mqueue",
        "ramfs",
        "efivarfs",
        "tracefs",
        "debugfs",
        "securityfs",
        "pstore",
        "bpf",
        "configfs",
        "fusectl",
        "hugetlbfs",
        "autofs",
        "nsfs",
        "binfmt_misc",
        "fuse.lxcfs",
        "rpc_pipefs",
    ];
    d.size_kb > 0 && !PSEUDO.contains(&d.fstype.as_str())
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerStatus {
    pub host: String,
    pub cpu_pct: Option<f32>,
    pub mem_used_kb: u64,
    pub mem_total_kb: u64,
    pub rx_bps: Option<f64>,
    pub tx_bps: Option<f64>,
    pub uptime_secs: u64,
    pub users: Vec<String>,
    pub disks: Vec<DiskUsage>,
}

/// Turns successive raw samples into rates.
#[derive(Default)]
pub struct StatusComputer {
    prev: Option<RawMetrics>,
}

impl StatusComputer {
    /// The interval between two samples is taken from the server's own uptime,
    /// so a frame delayed in transit doesn't skew the rates. The first sample,
    /// and any where a counter went backwards (reboot, wrap), has no rates.
    pub fn update(&mut self, m: RawMetrics) -> ServerStatus {
        let (cpu_pct, rx_bps, tx_bps) = match &self.prev {
            Some(prev) => {
                let cpu = if m.cpu_total > prev.cpu_total && m.cpu_idle >= prev.cpu_idle {
                    let total = (m.cpu_total - prev.cpu_total) as f64;
                    let idle = (m.cpu_idle - prev.cpu_idle) as f64;
                    Some((((total - idle) / total) * 100.).clamp(0., 100.) as f32)
                } else {
                    None
                };
                let elapsed = m.uptime_secs - prev.uptime_secs;
                let sum = |v: &[(String, u64, u64)], rx: bool| -> u64 {
                    v.iter().map(|(_, r, t)| if rx { *r } else { *t }).sum()
                };
                let rate = |rx: bool| {
                    let now = sum(&m.net, rx);
                    let before = sum(&prev.net, rx);
                    if elapsed > 0. && now >= before {
                        Some((now - before) as f64 / elapsed)
                    } else {
                        None
                    }
                };
                (cpu, rate(true), rate(false))
            }
            None => (None, None, None),
        };
        let status = ServerStatus {
            host: m.host.clone(),
            cpu_pct,
            mem_used_kb: m.mem_total_kb.saturating_sub(m.mem_avail_kb),
            mem_total_kb: m.mem_total_kb,
            rx_bps,
            tx_bps,
            uptime_secs: m.uptime_secs.max(0.) as u64,
            users: m.users.clone(),
            disks: m.disks.iter().filter(|d| keep_disk(d)).cloned().collect(),
        };
        self.prev = Some(m);
        status
    }
}

const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];

/// The unit index and divisor that suit `n` bytes.
fn unit_for(n: f64) -> (usize, f64) {
    let mut unit = 0;
    let mut div = 1.;
    while unit + 1 < UNITS.len() && n / div >= 1024. {
        unit += 1;
        div *= 1024.;
    }
    (unit, div)
}

fn number(value: f64) -> String {
    if value < 10. {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    }
}

/// 1024-based, one decimal below 10: "512B", "80K", "1.2M", "7.8G".
pub fn human_bytes(n: u64) -> String {
    let (unit, div) = unit_for(n as f64);
    if unit == 0 {
        return format!("{n}B");
    }
    format!("{}{}", number(n as f64 / div), UNITS[unit])
}

pub fn human_uptime(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    if days > 0 {
        format!("{days}天{hours}时")
    } else if hours > 0 {
        format!("{hours}时{minutes}分")
    } else {
        format!("{minutes}分")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Normal,
    Warn,
    Crit,
}

pub fn level_for(pct: f32) -> Level {
    if pct > 90. {
        Level::Crit
    } else if pct > 80. {
        Level::Warn
    } else {
        Level::Normal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    Host,
    Cpu,
    Mem,
    Net,
    Uptime,
    Users,
    Disks,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub kind: SegmentKind,
    pub text: String,
    pub level: Level,
}

const SEPARATOR_CELLS: usize = 3; // " │ "

/// Cells taken by the segments with separators between them.
pub fn segments_width(segments: &[Segment]) -> usize {
    let text: usize = segments
        .iter()
        .map(|s| unicode_column_width(&s.text, None))
        .sum();
    text + SEPARATOR_CELLS * segments.len().saturating_sub(1)
}

fn pct(used: u64, total: u64) -> f32 {
    if total == 0 {
        0.
    } else {
        (used as f64 / total as f64 * 100.) as f32
    }
}

/// The status bar's segments, left to right, fitted into `max_cells`.
///
/// When they don't fit, the disks fold into the first one plus a count, then
/// the users, uptime, network and host go in that order. CPU and memory always
/// stay, even if that overflows.
pub fn status_segments(s: &ServerStatus, max_cells: usize) -> Vec<Segment> {
    use SegmentKind as Kind;
    let normal = |kind: Kind, text: String| Segment {
        kind,
        text,
        level: Level::Normal,
    };

    let mut segments: Vec<Segment> = vec![];
    if !s.host.is_empty() {
        segments.push(normal(Kind::Host, s.host.clone()));
    }
    segments.push(match s.cpu_pct {
        Some(p) => Segment {
            kind: Kind::Cpu,
            text: format!("CPU {p:.0}%"),
            level: level_for(p),
        },
        None => normal(Kind::Cpu, "CPU --".to_string()),
    });
    let (unit, div) = unit_for(s.mem_total_kb as f64 * 1024.);
    segments.push(Segment {
        kind: Kind::Mem,
        text: format!(
            "内存 {}/{}{}",
            number(s.mem_used_kb as f64 * 1024. / div),
            number(s.mem_total_kb as f64 * 1024. / div),
            UNITS[unit]
        ),
        level: level_for(pct(s.mem_used_kb, s.mem_total_kb)),
    });
    let rate = |r: Option<f64>| match r {
        Some(r) => format!("{}/s", human_bytes(r.max(0.) as u64)),
        None => "--".to_string(),
    };
    segments.push(normal(
        Kind::Net,
        format!("↓{} ↑{}", rate(s.rx_bps), rate(s.tx_bps)),
    ));
    segments.push(normal(
        Kind::Uptime,
        format!("运行 {}", human_uptime(s.uptime_secs)),
    ));
    if !s.users.is_empty() {
        segments.push(normal(Kind::Users, format!("用户 {}", s.users.join(","))));
    }
    let disk_level = s
        .disks
        .iter()
        .map(|d| level_for(pct(d.used_kb, d.size_kb)))
        .max()
        .unwrap_or(Level::Normal);
    let disk_text = |d: &DiskUsage| format!("{} {:.0}%", d.mount, pct(d.used_kb, d.size_kb));
    if !s.disks.is_empty() {
        segments.push(Segment {
            kind: Kind::Disks,
            text: s.disks.iter().map(disk_text).collect::<Vec<_>>().join("  "),
            level: disk_level,
        });
    }

    let fits = |segments: &[Segment]| segments_width(segments) <= max_cells;

    if !fits(&segments) && s.disks.len() > 1 {
        if let Some(seg) = segments.iter_mut().find(|seg| seg.kind == Kind::Disks) {
            seg.text = format!("{} +{}", disk_text(&s.disks[0]), s.disks.len() - 1);
        }
    }
    // The host name tells you which server this is, so it outlasts the rest.
    for drop in [
        Kind::Users,
        Kind::Uptime,
        Kind::Net,
        Kind::Disks,
        Kind::Host,
    ] {
        if fits(&segments) {
            break;
        }
        segments.retain(|seg| seg.kind != drop);
    }

    segments
}

#[cfg(test)]
mod test {
    use super::*;

    const SAMPLE: &str = "@frame
cwd\t3\t/var/log
cwd\t5\t?
cpu  7825 1230 5743 1633743 1813 0 1909 0 0 0
MemTotal:        4013328 kB
MemAvailable:    3452200 kB
net     lo:   37908     242    0    0    0     0          0         0    37908     242    0    0    0     0       0          0
net   eth0:   53285     356    0    0    0     0          0         0    56946     324    0    0    0     0       0          0
up 4134.12
user tester   pts/0        2026-11-09 09:00 (172.17.0.1)
user root     pts/1        2026-11-09 09:05 (172.17.0.1)
user tester   pts/2        2026-11-09 09:06 (172.17.0.1)
disk overlay        overlay  1055762868 13885876 988173520       2% /
disk tmpfs          tmpfs         65536        0     65536       0% /dev
disk /dev/sda1      ext4      102400000 51200000  51200000      50% /
disk /dev/sdb1      xfs       204800000 184320000 20480000      90% /data dir
host web01
@end
";

    fn parse_all(text: &str) -> Vec<ParseEvent> {
        let mut parser = FrameParser::default();
        text.lines().filter_map(|l| parser.feed_line(l)).collect()
    }

    fn metrics(cpu_total: u64, cpu_idle: u64, rx: u64, tx: u64, up: f64) -> RawMetrics {
        RawMetrics {
            cpu_total,
            cpu_idle,
            mem_total_kb: 8 * 1024 * 1024,
            mem_avail_kb: 5 * 1024 * 1024,
            net: vec![("eth0".to_string(), rx, tx)],
            uptime_secs: up,
            users: vec!["root".to_string()],
            disks: vec![],
            host: "web01".to_string(),
        }
    }

    #[test]
    fn script_is_plain_lf_text() {
        assert!(MONITOR_SCRIPT.ends_with('\n'));
        assert!(!MONITOR_SCRIPT.contains('\r'));
        assert!(MONITOR_SCRIPT.contains("WEZTERM_REMOTE_PANE="));
    }

    #[test]
    fn parses_full_frame() {
        let events = parse_all(SAMPLE);
        assert_eq!(events.len(), 1);
        let ParseEvent::Frame(frame) = &events[0] else {
            panic!("expected a frame, got {:?}", events[0]);
        };
        assert_eq!(
            frame.cwds,
            vec![(3, Some("/var/log".to_string())), (5, None)]
        );
        let m = frame.metrics.as_ref().unwrap();
        assert_eq!(m.cpu_total, 7825 + 1230 + 5743 + 1633743 + 1813 + 1909);
        assert_eq!(m.cpu_idle, 1633743 + 1813);
        assert_eq!(m.mem_total_kb, 4013328);
        assert_eq!(m.mem_avail_kb, 3452200);
        assert_eq!(m.net, vec![("eth0".to_string(), 53285, 56946)]);
        assert_eq!(m.uptime_secs, 4134.12);
        assert_eq!(m.users, vec!["tester".to_string(), "root".to_string()]);
        assert_eq!(m.host, "web01");
        assert_eq!(m.disks.len(), 4, "{:#?}", m.disks);
        assert_eq!(
            m.disks[3],
            DiskUsage {
                mount: "/data dir".to_string(),
                fstype: "xfs".to_string(),
                size_kb: 204800000,
                used_kb: 184320000,
            }
        );
    }

    #[test]
    fn cwd_lines_without_metrics() {
        let events = parse_all("@frame\ncwd\t7\t/etc\n@end\n");
        assert_eq!(
            events,
            vec![ParseEvent::Frame(Frame {
                cwds: vec![(7, Some("/etc".to_string()))],
                metrics: None,
            })]
        );
    }

    #[test]
    fn unsupported_marker() {
        assert_eq!(parse_all("@unsupported\n"), vec![ParseEvent::Unsupported]);
    }

    #[test]
    fn garbage_lines_are_ignored() {
        let events = parse_all("hello\n@frame\ncwd\tx\t/etc\nnonsense here\nup abc\n@end\n");
        assert_eq!(
            events,
            vec![ParseEvent::Frame(Frame {
                cwds: vec![],
                metrics: None,
            })]
        );
    }

    #[test]
    fn keeps_only_real_filesystems() {
        let disk = |fstype: &str, size_kb| DiskUsage {
            mount: "/x".to_string(),
            fstype: fstype.to_string(),
            size_kb,
            used_kb: 1,
        };
        assert!(keep_disk(&disk("ext4", 10)));
        assert!(keep_disk(&disk("xfs", 10)));
        assert!(!keep_disk(&disk("tmpfs", 10)));
        assert!(!keep_disk(&disk("overlay", 10)));
        assert!(!keep_disk(&disk("squashfs", 10)));
        assert!(!keep_disk(&disk("ext4", 0)));
    }

    #[test]
    fn first_frame_has_no_rates() {
        let mut computer = StatusComputer::default();
        let status = computer.update(metrics(1000, 500, 100, 100, 10.0));
        assert_eq!(status.cpu_pct, None);
        assert_eq!(status.rx_bps, None);
        assert_eq!(status.tx_bps, None);
        assert_eq!(status.uptime_secs, 10);
        assert_eq!(status.mem_total_kb, 8 * 1024 * 1024);
        assert_eq!(status.mem_used_kb, 3 * 1024 * 1024);
    }

    #[test]
    fn cpu_and_net_rates_from_two_frames() {
        let mut computer = StatusComputer::default();
        computer.update(metrics(1000, 500, 0, 0, 10.0));
        let status = computer.update(metrics(1100, 575, 3 * 1048576, 1048576, 13.0));
        assert_eq!(status.cpu_pct, Some(25.0));
        assert_eq!(status.rx_bps, Some(1048576.0));
        assert_eq!(status.tx_bps, Some(1048576.0 / 3.0));
    }

    #[test]
    fn counter_going_backwards_gives_no_rate() {
        let mut computer = StatusComputer::default();
        computer.update(metrics(1000, 500, 5000, 5000, 10.0));
        let status = computer.update(metrics(1100, 550, 10, 10, 13.0));
        assert_eq!(status.rx_bps, None);
        assert_eq!(status.tx_bps, None);
        assert_eq!(status.cpu_pct, Some(50.0));
    }

    #[test]
    fn status_disks_are_filtered() {
        let ParseEvent::Frame(frame) = parse_all(SAMPLE).remove(0) else {
            unreachable!()
        };
        let status = StatusComputer::default().update(frame.metrics.unwrap());
        let mounts: Vec<&str> = status.disks.iter().map(|d| d.mount.as_str()).collect();
        assert_eq!(mounts, vec!["/", "/data dir"]);
    }

    #[test]
    fn human_bytes_examples() {
        assert_eq!(human_bytes(512), "512B");
        assert_eq!(human_bytes(80 * 1024), "80K");
        assert_eq!(human_bytes(1258291), "1.2M");
        assert_eq!(human_bytes(8375186227), "7.8G");
    }

    #[test]
    fn human_uptime_examples() {
        assert_eq!(human_uptime(12 * 86400 + 3 * 3600 + 59), "12天3时");
        assert_eq!(human_uptime(3 * 3600 + 25 * 60), "3时25分");
        assert_eq!(human_uptime(25 * 60 + 10), "25分");
    }

    #[test]
    fn levels_at_thresholds() {
        assert_eq!(level_for(80.0), Level::Normal);
        assert_eq!(level_for(80.5), Level::Warn);
        assert_eq!(level_for(90.0), Level::Warn);
        assert_eq!(level_for(91.0), Level::Crit);
    }

    fn sample_status() -> ServerStatus {
        ServerStatus {
            host: "web01".to_string(),
            cpu_pct: Some(12.4),
            mem_used_kb: 3250586,
            mem_total_kb: 8178893,
            rx_bps: Some(1258291.0),
            tx_bps: Some(81920.0),
            uptime_secs: 12 * 86400 + 3 * 3600,
            users: vec!["root".to_string(), "ty".to_string()],
            disks: vec![
                DiskUsage {
                    mount: "/".to_string(),
                    fstype: "ext4".to_string(),
                    size_kb: 100,
                    used_kb: 45,
                },
                DiskUsage {
                    mount: "/data".to_string(),
                    fstype: "xfs".to_string(),
                    size_kb: 100,
                    used_kb: 81,
                },
            ],
        }
    }

    fn texts(segments: &[Segment]) -> Vec<&str> {
        segments.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn wide_bar_shows_everything() {
        let segments = status_segments(&sample_status(), 200);
        assert_eq!(
            texts(&segments),
            vec![
                "web01",
                "CPU 12%",
                "内存 3.1/7.8G",
                "↓1.2M/s ↑80K/s",
                "运行 12天3时",
                "用户 root,ty",
                "/ 45%  /data 81%",
            ]
        );
        assert_eq!(segments[1].level, Level::Normal);
        assert_eq!(segments[6].level, Level::Warn);
    }

    #[test]
    fn unknown_rates_show_dashes() {
        let mut s = sample_status();
        s.cpu_pct = None;
        s.rx_bps = None;
        s.tx_bps = None;
        let segments = status_segments(&s, 200);
        assert_eq!(segments[1].text, "CPU --");
        assert_eq!(segments[3].text, "↓-- ↑--");
    }

    #[test]
    fn disks_collapse_before_users_drop() {
        let full = status_segments(&sample_status(), 200);
        let width = segments_width(&full);
        let segments = status_segments(&sample_status(), width - 1);
        assert_eq!(segments.last().unwrap().text, "/ 45% +1");
        assert!(texts(&segments).contains(&"用户 root,ty"));
        assert_eq!(segments.last().unwrap().level, Level::Warn);
    }

    #[test]
    fn narrow_width_keeps_cpu_and_mem() {
        let segments = status_segments(&sample_status(), 20);
        assert_eq!(texts(&segments), vec!["CPU 12%", "内存 3.1/7.8G"]);
    }

    #[test]
    fn segments_never_exceed_width_when_avoidable() {
        for width in 25..120 {
            let segments = status_segments(&sample_status(), width);
            assert!(
                segments_width(&segments) <= width,
                "width {width}: {:?}",
                texts(&segments)
            );
        }
    }
}
