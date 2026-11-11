//! The background channel on each sidebar ssh connection: starting it,
//! reading it, and stopping it when the connection's last pane goes.
//!
//! One exec channel per connection runs `monitor_proto::MONITOR_SCRIPT`.
//! A thread reads its output and hands each frame to the main thread, where
//! the latest server status and each pane's directory are kept for the status
//! bar and the file page to draw from.

use crate::sessions::monitor_proto::{
    FrameParser, ParseEvent, ServerStatus, StatusComputer, MONITOR_SCRIPT,
};
use crate::sessions::ssh::remote_ssh_domain;
use ::window::WindowOps;
use mux::domain::DomainId;
use mux::pane::PaneId;
use mux::ssh::RemoteSshDomain;
use mux::Mux;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};
use wezterm_ssh::{ChildKiller, FileDescriptor, SshChildProcess};

/// How long a failed channel waits before it is opened again.
const RESTART_AFTER: Duration = Duration::from_secs(30);

/// The script sends a frame every second. Numbers older than this are shown
/// as out of date: the connection has stalled, or the server has.
pub const STALE_AFTER: Duration = Duration::from_secs(5);

/// A channel that has sent nothing for this long is given up on, and opened
/// again after `RESTART_AFTER`.
const DATA_TIMEOUT: Duration = Duration::from_secs(15);

/// What the status bar says for a pane on a sidebar ssh connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarText {
    /// Not logged in yet.
    Connecting,
    /// The pane gave up before the connection came up.
    ConnectFailed,
    /// The connection came up, and has since gone.
    Disconnected,
    /// Logged in, waiting for the first numbers.
    Fetching,
    /// The server has no /proc.
    Unsupported,
    /// The channel failed before it sent any numbers.
    Unavailable,
    /// The numbers, and how many seconds old they are if that is too old.
    Values { stale_for: Option<u64> },
}

/// `pane_dead`: the active pane has ended. `authenticated`: the domain's
/// current connection has logged in.
pub fn bar_text(
    pane_dead: bool,
    authenticated: bool,
    view: Option<&MonitorView>,
    now: Instant,
) -> BarText {
    if !authenticated {
        return if pane_dead {
            BarText::ConnectFailed
        } else {
            BarText::Connecting
        };
    }
    let Some(view) = view else {
        return BarText::Fetching;
    };
    if pane_dead && view.state == MonitorState::Failed {
        return BarText::Disconnected;
    }
    match (view.state, view.status.is_some()) {
        (MonitorState::Unsupported, _) => BarText::Unsupported,
        (state, true) => {
            let age = now.saturating_duration_since(view.last_frame);
            let stale = state == MonitorState::Failed || age >= STALE_AFTER;
            BarText::Values {
                stale_for: if stale { Some(age.as_secs()) } else { None },
            }
        }
        (MonitorState::Failed, false) => BarText::Unavailable,
        _ => BarText::Fetching,
    }
}

/// "5 秒前", "3 分钟前", "2 小时前".
pub fn age_text(secs: u64) -> String {
    if secs < 60 {
        format!("{secs} 秒前")
    } else if secs < 3600 {
        format!("{} 分钟前", secs / 60)
    } else {
        format!("{} 小时前", secs / 3600)
    }
}

/// What the once-a-second check does for a monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tick {
    Quiet,
    /// The bar shows how old the numbers are, and that has changed.
    Repaint,
    /// Nothing has come for too long: close the channel.
    Fail,
}

fn tick_action(state: MonitorState, has_status: bool, last_frame: Instant, now: Instant) -> Tick {
    let age = now.saturating_duration_since(last_frame);
    match state {
        MonitorState::Starting | MonitorState::Running if age >= DATA_TIMEOUT => Tick::Fail,
        MonitorState::Running if age >= STALE_AFTER => Tick::Repaint,
        MonitorState::Failed if has_status => Tick::Repaint,
        _ => Tick::Quiet,
    }
}

/// Whether a monitor in `state` should be replaced by a fresh channel: one
/// that failed, once it has waited. Not on every paint, and not for a server
/// that has no /proc to read.
fn may_restart(state: MonitorState, failed_at: Option<Instant>, now: Instant) -> bool {
    match (state, failed_at) {
        (MonitorState::Failed, Some(at)) => now.saturating_duration_since(at) >= RESTART_AFTER,
        _ => false,
    }
}

/// The lines of `reader`, with any bytes that aren't UTF-8 replaced. A
/// directory name in another encoding must not end the stream.
fn lossy_lines<R: BufRead>(mut reader: R) -> impl Iterator<Item = String> {
    std::iter::from_fn(move || {
        let mut buf = vec![];
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
                    buf.pop();
                }
                Some(String::from_utf8_lossy(&buf).into_owned())
            }
        }
    })
}

/// The status bar shows for panes opened from a sidebar ssh node, and only
/// when it is enabled.
pub fn wants_status_bar(enabled: bool, pane_is_sidebar_ssh: bool) -> bool {
    enabled && pane_is_sidebar_ssh
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorState {
    /// The channel is being opened, or no frame with numbers has come yet.
    Starting,
    Running,
    /// The server has no /proc.
    Unsupported,
    /// The channel could not be opened or has closed.
    Failed,
}

pub struct MonitorView {
    pub state: MonitorState,
    pub status: Option<ServerStatus>,
    /// When the last frame came, or the channel was started if none has.
    pub last_frame: Instant,
}

struct Monitor {
    /// `RemoteSshDomain::authenticated_session`'s counter: a reconnect makes
    /// this monitor stale.
    generation: u64,
    /// Which channel this is, so that a restarted one ignores whatever the
    /// one before it still had in flight.
    instance: u64,
    state: MonitorState,
    failed_at: Option<Instant>,
    status: Option<ServerStatus>,
    last_frame: Instant,
    cwds: HashMap<PaneId, String>,
    computer: StatusComputer,
    /// Held for as long as the channel should live: dropping it sends EOF,
    /// which closes the channel.
    stdin: Option<FileDescriptor>,
    child: Option<SshChildProcess>,
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
        }
    }
}

thread_local! {
    static MONITORS: RefCell<HashMap<DomainId, Monitor>> = RefCell::new(HashMap::new());
    static CWD_GENERATION: Cell<u64> = Cell::new(0);
    static NEXT_INSTANCE: Cell<u64> = Cell::new(0);
}

/// Repaint the windows whose active pane is on this connection: the status
/// bar and the file page there draw from its monitor. Other windows have
/// nothing of it on screen.
fn invalidate_windows_showing(domain_id: DomainId) {
    let mux = Mux::get();
    for win in crate::frontend::front_end().gui_windows() {
        let shows = mux
            .get_active_tab_for_window(win.mux_window_id)
            .and_then(|tab| tab.get_active_pane())
            .map_or(false, |pane| pane.domain_id() == domain_id);
        if shows {
            win.window.invalidate();
        }
    }
}

/// Once a second while the monitor `instance` lives: mark its numbers out
/// of date as they age, and give up on a channel that has gone quiet.
fn spawn_ticker(domain_id: DomainId, instance: u64) {
    promise::spawn::spawn(async move {
        loop {
            smol::Timer::after(Duration::from_secs(1)).await;
            let action = MONITORS.with(|m| {
                let monitors = m.borrow();
                let mon = monitors
                    .get(&domain_id)
                    .filter(|m| m.instance == instance)?;
                Some(tick_action(
                    mon.state,
                    mon.status.is_some(),
                    mon.last_frame,
                    Instant::now(),
                ))
            });
            match action {
                None => break,
                Some(Tick::Quiet) => {}
                Some(Tick::Repaint) => invalidate_windows_showing(domain_id),
                Some(Tick::Fail) => {
                    log::warn!("ssh monitor for domain {domain_id}: nothing for {DATA_TIMEOUT:?}");
                    let channel = MONITORS.with(|m| {
                        m.borrow_mut()
                            .get_mut(&domain_id)
                            .map(|mon| (mon.stdin.take(), mon.child.take()))
                    });
                    // Closed outside the borrow, as in `Monitor::drop`.
                    if let Some((stdin, child)) = channel {
                        drop(stdin);
                        if let Some(mut child) = child {
                            let _ = child.kill();
                        }
                    }
                    set_state(domain_id, instance, MonitorState::Failed);
                }
            }
        }
    })
    .detach();
}

/// Start the channel for this domain's connection if it is authenticated and
/// none is running for it yet. Cheap to call on every paint.
pub fn ensure_monitor(domain_id: DomainId) {
    let Some(domain) = remote_ssh_domain(domain_id) else {
        return;
    };
    let Some(ssh) = domain.downcast_ref::<RemoteSshDomain>() else {
        return;
    };
    let existing = MONITORS.with(|m| {
        m.borrow()
            .get(&domain_id)
            .map(|m| (m.generation, m.state, m.failed_at))
    });
    if let Some((generation, state, failed_at)) = existing {
        if Some(generation) == ssh.authenticated_generation()
            && !may_restart(state, failed_at, Instant::now())
        {
            // Running or starting for this connection, or failed too
            // recently to try again; a reconnect brings a new generation.
            return;
        }
    }
    let Some((generation, session)) = ssh.authenticated_session() else {
        return;
    };
    let instance = NEXT_INSTANCE.with(|n| {
        n.set(n.get() + 1);
        n.get()
    });

    MONITORS.with(|m| {
        m.borrow_mut().insert(
            domain_id,
            Monitor {
                generation,
                instance,
                state: MonitorState::Starting,
                failed_at: None,
                status: None,
                last_frame: Instant::now(),
                cwds: HashMap::new(),
                computer: StatusComputer::default(),
                stdin: None,
                child: None,
            },
        )
    });

    spawn_ticker(domain_id, instance);

    promise::spawn::spawn(async move {
        let exec = match session.exec("sh -s", None).await {
            Ok(exec) => exec,
            Err(err) => {
                log::error!("ssh monitor for domain {domain_id}: exec failed: {err:#}");
                set_state(domain_id, instance, MonitorState::Failed);
                return;
            }
        };
        let wezterm_ssh::ExecResult {
            mut stdin,
            stdout,
            mut stderr,
            child,
        } = exec;
        if let Err(err) = stdin.write_all(MONITOR_SCRIPT.as_bytes()) {
            log::error!("ssh monitor for domain {domain_id}: sending script: {err:#}");
            set_state(domain_id, instance, MonitorState::Failed);
            return;
        }

        let stored = MONITORS.with(|m| match m.borrow_mut().get_mut(&domain_id) {
            Some(mon) if mon.instance == instance => {
                mon.stdin = Some(stdin);
                mon.child = Some(child);
                true
            }
            _ => false,
        });
        if !stored {
            // Pruned while we were opening it; dropping stdin closes it.
            return;
        }

        // Keep stderr drained so that a chatty error can't stall the channel.
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while matches!(stderr.read(&mut buf), Ok(n) if n > 0) {}
        });

        std::thread::spawn(move || {
            let mut parser = FrameParser::default();
            for line in lossy_lines(BufReader::new(stdout)) {
                if let Some(event) = parser.feed_line(&line) {
                    promise::spawn::spawn_into_main_thread(async move {
                        apply_event(domain_id, instance, event);
                    })
                    .detach();
                }
            }
            promise::spawn::spawn_into_main_thread(async move {
                let unsupported = MONITORS.with(|m| {
                    m.borrow()
                        .get(&domain_id)
                        .map(|m| m.state == MonitorState::Unsupported)
                        .unwrap_or(false)
                });
                if !unsupported {
                    set_state(domain_id, instance, MonitorState::Failed);
                }
            })
            .detach();
        });
    })
    .detach();
}

fn set_state(domain_id: DomainId, instance: u64, state: MonitorState) {
    let changed = MONITORS.with(|m| match m.borrow_mut().get_mut(&domain_id) {
        Some(mon) if mon.instance == instance && mon.state != state => {
            mon.state = state;
            if state == MonitorState::Failed {
                mon.failed_at = Some(Instant::now());
            }
            true
        }
        _ => false,
    });
    if changed {
        invalidate_windows_showing(domain_id);
    }
}

fn apply_event(domain_id: DomainId, instance: u64, event: ParseEvent) {
    let mut cwd_changed = false;
    // Whether anything the status bar or file page shows has changed: a
    // frame with the same numbers as the last one repaints nothing.
    let mut shown_changed = false;
    MONITORS.with(|m| {
        let mut monitors = m.borrow_mut();
        let Some(mon) = monitors.get_mut(&domain_id) else {
            return;
        };
        if mon.instance != instance {
            return;
        }
        let state_before = mon.state;
        match event {
            ParseEvent::Unsupported => mon.state = MonitorState::Unsupported,
            ParseEvent::Frame(frame) => {
                let now = Instant::now();
                // The bar was saying how old the numbers were.
                if now.saturating_duration_since(mon.last_frame) >= STALE_AFTER {
                    shown_changed = true;
                }
                mon.last_frame = now;
                for (pane, dir) in frame.cwds {
                    let Some(dir) = dir else { continue };
                    let pane = pane as PaneId;
                    if mon.cwds.get(&pane) != Some(&dir) {
                        mon.cwds.insert(pane, dir);
                        cwd_changed = true;
                    }
                }
                if let Some(metrics) = frame.metrics {
                    let status = Some(mon.computer.update(metrics));
                    if status != mon.status {
                        mon.status = status;
                        shown_changed = true;
                    }
                    mon.state = MonitorState::Running;
                }
            }
        }
        if mon.state != state_before {
            shown_changed = true;
        }
    });
    if cwd_changed {
        CWD_GENERATION.with(|g| g.set(g.get() + 1));
    }
    if cwd_changed || shown_changed {
        invalidate_windows_showing(domain_id);
    }
}

/// Whether the domain's current connection has logged in.
pub fn is_authenticated(domain_id: DomainId) -> bool {
    remote_ssh_domain(domain_id)
        .and_then(|domain| {
            domain
                .downcast_ref::<RemoteSshDomain>()
                .map(|ssh| ssh.authenticated_generation().is_some())
        })
        .unwrap_or(false)
}

pub fn monitor_view(domain_id: DomainId) -> Option<MonitorView> {
    MONITORS.with(|m| {
        m.borrow().get(&domain_id).map(|mon| MonitorView {
            state: mon.state,
            status: mon.status.clone(),
            last_frame: mon.last_frame,
        })
    })
}

/// The last directory read for this pane's shell, if any.
pub fn pane_cwd(domain_id: DomainId, pane_id: PaneId) -> Option<String> {
    MONITORS.with(|m| {
        m.borrow()
            .get(&domain_id)
            .and_then(|mon| mon.cwds.get(&pane_id).cloned())
    })
}

/// Bumped whenever any pane's directory changes.
pub fn cwd_generation() -> u64 {
    CWD_GENERATION.with(|g| g.get())
}

/// Stop the channels of connections that have no panes left. The channel is
/// what keeps an ssh connection open once its panes are gone, so this is also
/// what lets the connection close.
pub fn prune_monitors() {
    let live: std::collections::HashSet<DomainId> = Mux::get()
        .iter_panes()
        .into_iter()
        .map(|p| p.domain_id())
        .collect();
    let gone: Vec<Monitor> = MONITORS.with(|m| {
        let mut monitors = m.borrow_mut();
        let dead: Vec<DomainId> = monitors
            .keys()
            .copied()
            .filter(|id| !live.contains(id))
            .collect();
        dead.into_iter()
            .filter_map(|id| monitors.remove(&id))
            .collect()
    });
    // Dropped outside the borrow: `Monitor::drop` closes the channel.
    drop(gone);
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn lossy_lines_survive_bytes_that_are_not_utf8() {
        // A directory named in GBK ("测试").
        let input: &[u8] = b"@frame\ncwd\t1\t/tmp/\xb2\xe2\xca\xd4\r\n@end\n";
        let lines: Vec<String> = lossy_lines(input).collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "@frame");
        assert!(lines[1].starts_with("cwd\t1\t/tmp/"));
        assert!(lines[1].contains('\u{fffd}'));
        assert!(!lines[1].ends_with('\r'));
        assert_eq!(lines[2], "@end");
    }

    #[test]
    fn failed_monitor_restarts_after_backoff() {
        let failed_at = Instant::now();
        let soon = failed_at + Duration::from_secs(5);
        let later = failed_at + RESTART_AFTER;
        assert!(!may_restart(MonitorState::Failed, Some(failed_at), soon));
        assert!(may_restart(MonitorState::Failed, Some(failed_at), later));
        assert!(!may_restart(MonitorState::Running, None, later));
        assert!(!may_restart(MonitorState::Starting, None, later));
        // A server without /proc won't grow one.
        assert!(!may_restart(
            MonitorState::Unsupported,
            Some(failed_at),
            later
        ));
    }

    fn status() -> ServerStatus {
        ServerStatus {
            host: "h".to_string(),
            cpu_pct: None,
            mem_used_kb: 0,
            mem_total_kb: 0,
            rx_bps: None,
            tx_bps: None,
            uptime_secs: 0,
            users: vec![],
            disks: vec![],
        }
    }

    fn view(state: MonitorState, with_status: bool, age: u64, now: Instant) -> MonitorView {
        MonitorView {
            state,
            status: if with_status { Some(status()) } else { None },
            last_frame: now - Duration::from_secs(age),
        }
    }

    #[test]
    fn the_bar_says_connecting_until_the_login_is_done() {
        let now = Instant::now();
        assert_eq!(bar_text(false, false, None, now), BarText::Connecting);
    }

    #[test]
    fn a_pane_whose_connection_failed_says_so() {
        let now = Instant::now();
        assert_eq!(bar_text(true, false, None, now), BarText::ConnectFailed);
        // Numbers from the connection before this one don't count.
        let old = view(MonitorState::Running, true, 1, now);
        assert_eq!(
            bar_text(true, false, Some(&old), now),
            BarText::ConnectFailed
        );
    }

    #[test]
    fn a_lost_connection_says_disconnected() {
        let now = Instant::now();
        let ended = view(MonitorState::Failed, true, 3, now);
        assert_eq!(
            bar_text(true, true, Some(&ended), now),
            BarText::Disconnected
        );
        // A shell that exited badly on a connection that is fine.
        let fine = view(MonitorState::Running, true, 1, now);
        assert_eq!(
            bar_text(true, true, Some(&fine), now),
            BarText::Values { stale_for: None }
        );
    }

    #[test]
    fn numbers_older_than_five_seconds_are_marked_stale() {
        let now = Instant::now();
        let fresh = view(MonitorState::Running, true, 4, now);
        assert_eq!(
            bar_text(false, true, Some(&fresh), now),
            BarText::Values { stale_for: None }
        );
        let late = view(MonitorState::Running, true, 5, now);
        assert_eq!(
            bar_text(false, true, Some(&late), now),
            BarText::Values { stale_for: Some(5) }
        );
        // A channel that ended leaves its last numbers, marked as old.
        let ended = view(MonitorState::Failed, true, 40, now);
        assert_eq!(
            bar_text(false, true, Some(&ended), now),
            BarText::Values {
                stale_for: Some(40)
            }
        );
    }

    #[test]
    fn before_there_are_any_numbers() {
        let now = Instant::now();
        assert_eq!(bar_text(false, true, None, now), BarText::Fetching);
        let starting = view(MonitorState::Starting, false, 2, now);
        assert_eq!(
            bar_text(false, true, Some(&starting), now),
            BarText::Fetching
        );
        let failed = view(MonitorState::Failed, false, 2, now);
        assert_eq!(
            bar_text(false, true, Some(&failed), now),
            BarText::Unavailable
        );
        let no_proc = view(MonitorState::Unsupported, false, 2, now);
        assert_eq!(
            bar_text(false, true, Some(&no_proc), now),
            BarText::Unsupported
        );
    }

    #[test]
    fn a_channel_that_goes_quiet_is_given_up_after_fifteen_seconds() {
        let now = Instant::now();
        let at = |age: u64| now - Duration::from_secs(age);
        use MonitorState::*;
        assert_eq!(tick_action(Running, true, at(3), now), Tick::Quiet);
        // Once stale, the age on show changes every second.
        assert_eq!(tick_action(Running, true, at(6), now), Tick::Repaint);
        assert_eq!(tick_action(Running, true, at(15), now), Tick::Fail);
        // A channel that never sent anything gets as long.
        assert_eq!(tick_action(Starting, false, at(6), now), Tick::Quiet);
        assert_eq!(tick_action(Starting, false, at(15), now), Tick::Fail);
        assert_eq!(tick_action(Failed, true, at(40), now), Tick::Repaint);
        assert_eq!(tick_action(Failed, false, at(40), now), Tick::Quiet);
        assert_eq!(tick_action(Unsupported, false, at(40), now), Tick::Quiet);
    }

    #[test]
    fn the_age_of_old_numbers_reads_in_the_largest_whole_unit() {
        assert_eq!(age_text(5), "5 秒前");
        assert_eq!(age_text(59), "59 秒前");
        assert_eq!(age_text(60), "1 分钟前");
        assert_eq!(age_text(3599), "59 分钟前");
        assert_eq!(age_text(7200), "2 小时前");
    }

    #[test]
    fn status_bar_only_for_sidebar_ssh_panes() {
        assert!(wants_status_bar(true, true));
        assert!(!wants_status_bar(true, false));
        assert!(!wants_status_bar(false, true));
        assert!(!wants_status_bar(false, false));
    }
}
