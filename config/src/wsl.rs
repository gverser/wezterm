use crate::config::validate_domain_name;
use crate::*;
use luahelper::impl_lua_conversion_dynamic;
use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use wezterm_dynamic::{FromDynamic, ToDynamic};

#[derive(Default, Debug, Clone, FromDynamic, ToDynamic)]
pub struct WslDomain {
    #[dynamic(validate = "validate_domain_name")]
    pub name: String,
    pub distribution: Option<String>,
    pub username: Option<String>,
    pub default_cwd: Option<PathBuf>,
    pub default_prog: Option<Vec<String>>,
}
impl_lua_conversion_dynamic!(WslDomain);

/// How long `wsl -l -v` may take. A healthy WSL service answers well within
/// this; a stuck one never answers at all, and wezterm-gui used to wait for it
/// before it would open its first window.
const WSL_LIST_TIMEOUT: Duration = Duration::from_secs(3);

/// How old the distro list may get before a caller starts fetching a new one.
#[cfg(windows)]
const WSL_LIST_REFRESH_AFTER: Duration = Duration::from_secs(30);

#[cfg(windows)]
lazy_static::lazy_static! {
    static ref DISTROS: DistroCache =
        DistroCache::new(WSL_LIST_REFRESH_AFTER, Arc::new(WslDistro::load_distro_list));
}

impl WslDomain {
    pub fn default_domains() -> Vec<WslDomain> {
        #[allow(unused_mut)]
        let mut domains = vec![];

        #[cfg(windows)]
        for distro in DISTROS.get() {
            domains.push(WslDomain {
                name: format!("WSL:{}", distro.name),
                distribution: Some(distro.name.clone()),
                username: None,
                default_cwd: Some("~".into()),
                default_prog: None,
            });
        }

        domains
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslDistro {
    pub name: String,
    pub state: String,
    pub version: String,
    pub is_default: bool,
}

impl WslDistro {
    pub fn load_distro_list() -> anyhow::Result<Vec<Self>> {
        #[cfg(windows)]
        use std::os::windows::process::CommandExt;
        #[cfg(windows)]
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

        let mut cmd = std::process::Command::new("wsl.exe");
        cmd.arg("-l");
        cmd.arg("-v");
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let output = output_with_timeout(&mut cmd, WSL_LIST_TIMEOUT)?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::ensure!(
            output.status.success(),
            "wsl -l command invocation failed: {}",
            stderr
        );

        /// Ungh: https://github.com/microsoft/WSL/issues/4456
        fn utf16_to_utf8(bytes: &[u8]) -> anyhow::Result<String> {
            if bytes.len() % 2 != 0 {
                anyhow::bail!("input data has odd length, cannot be utf16");
            }

            // This is "safe" because we checked that the length seems reasonable,
            // and our new slice is within those same bounds.
            let wide: &[u16] = unsafe {
                std::slice::from_raw_parts(bytes.as_ptr() as *const u16, bytes.len() / 2)
            };

            String::from_utf16(wide).map_err(|_| anyhow!("wsl -l -v output is not valid utf16"))
        }

        let wsl_list = utf16_to_utf8(&output.stdout)?.replace("\r\n", "\n");

        Ok(parse_wsl_distro_list(&wsl_list))
    }
}

type DistroLoader = Arc<dyn Fn() -> anyhow::Result<Vec<WslDistro>> + Send + Sync>;

/// The distro list, remembered between callers.
///
/// It is asked for far more often than it changes: at startup, on every config
/// reload, and on every spawn in a local domain, which looks itself up here to
/// learn whether it is a WSL domain. Running `wsl -l -v` for each of those made
/// every new tab pay for it, and made all of them hang along with a stuck WSL
/// service. Only the first caller waits for wsl.exe now; after that a stale
/// list is handed out at once while a single background thread fetches another.
#[cfg_attr(not(windows), allow(dead_code))]
struct DistroCache {
    load: DistroLoader,
    refresh_after: Duration,
    state: Arc<Mutex<CacheState>>,
}

#[derive(Default)]
#[cfg_attr(not(windows), allow(dead_code))]
struct CacheState {
    distros: Option<Vec<WslDistro>>,
    loaded_at: Option<Instant>,
    refreshing: bool,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl DistroCache {
    fn new(refresh_after: Duration, load: DistroLoader) -> Self {
        Self {
            load,
            refresh_after,
            state: Arc::new(Mutex::new(CacheState::default())),
        }
    }

    fn get(&self) -> Vec<WslDistro> {
        let mut state = lock(&self.state);

        let distros = match &state.distros {
            Some(distros) => distros.clone(),
            None => {
                // The lock stays held across the load on purpose: whoever
                // arrives meanwhile wants the same answer, and one wsl.exe
                // is enough.
                let distros = (self.load)().unwrap_or_else(|err| {
                    log::warn!("Unable to list WSL distributions: {:#}", err);
                    vec![]
                });
                state.distros = Some(distros.clone());
                state.loaded_at = Some(Instant::now());
                return distros;
            }
        };

        let stale = state
            .loaded_at
            .map_or(true, |at| at.elapsed() >= self.refresh_after);
        if stale && !state.refreshing {
            state.refreshing = true;
            let load = Arc::clone(&self.load);
            let shared = Arc::clone(&self.state);
            std::thread::spawn(move || {
                let result = load();
                let mut state = lock(&shared);
                match result {
                    Ok(distros) => state.distros = Some(distros),
                    // Keep the last good list. Dropping it would quietly turn
                    // a WSL tab into a local shell, since LocalDomain only
                    // wraps its command in wsl.exe when it finds itself here.
                    Err(err) => log::warn!("Unable to refresh WSL distributions: {:#}", err),
                }
                state.loaded_at = Some(Instant::now());
                state.refreshing = false;
            });
        }

        distros
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn lock(state: &Mutex<CacheState>) -> std::sync::MutexGuard<'_, CacheState> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `Command::output`, except that a command still running after `timeout` is
/// killed and reported as an error rather than waited on for ever.
fn output_with_timeout(cmd: &mut Command, timeout: Duration) -> anyhow::Result<Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut tree = ProcessTree::spawn(cmd)?;
    let deadline = Instant::now() + timeout;

    // Read on their own threads, so that a command which fills a pipe does not
    // stall while we are polling for it to exit.
    let stdout = drain(tree.child.stdout.take());
    let stderr = drain(tree.child.stderr.take());

    let status = loop {
        if let Some(status) = tree.child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            tree.kill();
            anyhow::bail!("{:?} did not finish within {:?}", cmd, timeout);
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    // Whatever the command started may still hold the pipes open after the
    // command itself has gone. The floor lets a reader that is just finishing
    // get its bytes across when the exit landed right at the deadline.
    let remaining = || {
        deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(100))
    };
    match (
        stdout.recv_timeout(remaining()),
        stderr.recv_timeout(remaining()),
    ) {
        (Ok(stdout), Ok(stderr)) => Ok(Output {
            status,
            stdout,
            stderr,
        }),
        _ => {
            tree.kill();
            anyhow::bail!(
                "{:?} exited but its output was still open after {:?}",
                cmd,
                timeout
            )
        }
    }
}

/// A spawned command together with everything it goes on to start.
///
/// Killing only the process we spawned is not enough: wsl.exe is three
/// processes deep (System32 -> WindowsApps -> Program Files\WSL), and the two
/// we did not start would go on waiting on a stuck WslService for good. Before
/// this, every launch of wezterm-gui against a stuck service left three more of
/// them behind.
struct ProcessTree {
    child: std::process::Child,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl ProcessTree {
    fn spawn(cmd: &mut Command) -> std::io::Result<Self> {
        let child = cmd.spawn()?;
        Ok(Self {
            #[cfg(windows)]
            job: job::Job::adopt(&child),
            child,
        })
    }

    fn kill(&mut self) {
        // Closing the job is what ends the processes the child started.
        #[cfg(windows)]
        let _ = self.job.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// A job that takes its members with it when it is closed. Whatever a member
    /// starts joins the job too.
    pub struct Job(HANDLE);

    // The handle is only ever used to add a process and to close.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        /// Put `child` in a job of its own.
        ///
        /// Anything the child starts before this call stays outside the job.
        /// wsl.exe spends far longer loading than that gap lasts.
        pub fn adopt(child: &Child) -> Option<Self> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    log::warn!("could not create a job object; a command that is given up on may leave its children behind");
                    return None;
                }
                // From here on, returning closes the handle.
                let job = Self(handle);

                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    log::warn!("could not set kill-on-close on the job object");
                    return None;
                }

                if AssignProcessToJobObject(handle, child.as_raw_handle() as HANDLE) == 0 {
                    log::warn!("could not put the child process in its job object");
                    return None;
                }

                Some(job)
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = vec![];
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = tx.send(bytes);
    });
    rx
}

/// This function parses the `wsl -l -v` output.
/// It tries to be robust in the face of future changes
/// by looking at the tabulated output headers, determining
/// where the columns are and then collecting the information
/// into a hashmap and then grokking from there.
#[allow(dead_code)]
fn parse_wsl_distro_list(output: &str) -> Vec<WslDistro> {
    let lines = output.lines().collect::<Vec<_>>();

    // Determine where the field columns start
    let mut field_starts = vec![];
    {
        let mut last_char = ' ';
        for (idx, c) in lines[0].char_indices() {
            if last_char == ' ' && c != ' ' {
                field_starts.push(idx);
            }
            last_char = c;
        }
    }

    fn field_slice(s: &str, start: usize, end: Option<usize>) -> &str {
        if let Some(end) = end {
            &s[start..end]
        } else {
            &s[start..]
        }
    }

    fn opt_field_slice(s: &str, start: usize, end: Option<usize>) -> Option<&str> {
        if let Some(end) = end {
            s.get(start..end)
        } else {
            s.get(start..)
        }
    }

    // Now build up a name -> column position map
    let mut field_map = HashMap::new();
    {
        let mut iter = field_starts.into_iter().peekable();

        while let Some(start_idx) = iter.next() {
            let end_idx = iter.peek().copied();
            let label = field_slice(&lines[0], start_idx, end_idx).trim();
            field_map.insert(label, (start_idx, end_idx));
        }
    }

    let mut result = vec![];

    // and now process the output rows
    for line in lines.iter().skip(1) {
        if line.is_empty() {
            continue;
        }

        let is_default = line.starts_with("*");

        let mut fields = HashMap::new();
        for (label, (start_idx, end_idx)) in field_map.iter() {
            if let Some(value) = opt_field_slice(line, *start_idx, *end_idx) {
                fields.insert(*label, value.trim().to_string());
            } else {
                return result;
            }
        }

        result.push(WslDistro {
            name: fields.get("NAME").cloned().unwrap_or_default(),
            state: fields.get("STATE").cloned().unwrap_or_default(),
            version: fields.get("VERSION").cloned().unwrap_or_default(),
            is_default,
        });
    }

    result
}

#[cfg(test)]
#[test]
fn test_parse_wsl_distro_list() {
    let data = "  NAME                   STATE           VERSION
* Arch                   Running         2
  docker-desktop-data    Stopped         2
  docker-desktop         Stopped         2
  Ubuntu                 Stopped         2
  nvim                   Stopped         2";

    assert_eq!(
        parse_wsl_distro_list(data),
        vec![
            WslDistro {
                name: "Arch".to_string(),
                state: "Running".to_string(),
                version: "2".to_string(),
                is_default: true
            },
            WslDistro {
                name: "docker-desktop-data".to_string(),
                state: "Stopped".to_string(),
                version: "2".to_string(),
                is_default: false
            },
            WslDistro {
                name: "docker-desktop".to_string(),
                state: "Stopped".to_string(),
                version: "2".to_string(),
                is_default: false
            },
            WslDistro {
                name: "Ubuntu".to_string(),
                state: "Stopped".to_string(),
                version: "2".to_string(),
                is_default: false
            },
            WslDistro {
                name: "nvim".to_string(),
                state: "Stopped".to_string(),
                version: "2".to_string(),
                is_default: false
            },
        ]
    );
}

#[cfg(test)]
mod test {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    fn distro(name: &str) -> WslDistro {
        WslDistro {
            name: name.to_string(),
            state: "Stopped".to_string(),
            version: "2".to_string(),
            is_default: false,
        }
    }

    /// A loader that is handed the 0-based number of the call it is answering,
    /// and whose calls are counted.
    fn counting<F>(answer: F) -> (DistroLoader, Arc<AtomicUsize>)
    where
        F: Fn(usize) -> anyhow::Result<Vec<WslDistro>> + Send + Sync + 'static,
    {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let load: DistroLoader = Arc::new(move || answer(counter.fetch_add(1, Ordering::SeqCst)));
        (load, calls)
    }

    fn eventually(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_fresh_list_is_served_without_asking_wsl_again() {
        let (load, calls) = counting(|_| Ok(vec![distro("Ubuntu")]));
        let cache = DistroCache::new(Duration::from_secs(3600), load);

        assert_eq!(cache.get(), vec![distro("Ubuntu")]);
        assert_eq!(cache.get(), vec![distro("Ubuntu")]);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "every spawn used to run wsl -l -v again"
        );
    }

    #[test]
    fn a_first_load_that_fails_means_no_distros_and_is_not_retried_at_once() {
        let (load, calls) = counting(|_| Err(anyhow::anyhow!("wsl -l -v timed out")));
        let cache = DistroCache::new(Duration::from_secs(3600), load);

        assert_eq!(cache.get(), vec![]);
        assert_eq!(cache.get(), vec![]);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a broken WSL must not cost every spawn another timeout"
        );
    }

    #[test]
    fn a_stale_list_is_answered_at_once_and_refreshed_in_the_background() {
        let (release, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let (load, _calls) = counting(move |n| {
            if n == 0 {
                return Ok(vec![distro("Ubuntu")]);
            }
            // WSL hangs until the test lets it answer.
            let _ = gate.lock().unwrap().recv_timeout(Duration::from_secs(10));
            Ok(vec![distro("Arch")])
        });
        let cache = DistroCache::new(Duration::ZERO, load);
        assert_eq!(cache.get(), vec![distro("Ubuntu")]);

        let started = Instant::now();
        assert_eq!(cache.get(), vec![distro("Ubuntu")]);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the caller waited on the refresh: {:?}",
            started.elapsed()
        );

        release.send(()).unwrap();
        drop(release);
        assert!(
            eventually(|| cache.get() == vec![distro("Arch")]),
            "the refreshed list never replaced the stale one"
        );
    }

    #[test]
    fn callers_arriving_during_a_refresh_do_not_start_another() {
        let (release, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let (load, calls) = counting(move |n| {
            if n > 0 {
                let _ = gate.lock().unwrap().recv_timeout(Duration::from_secs(10));
            }
            Ok(vec![distro("Ubuntu")])
        });
        let cache = DistroCache::new(Duration::ZERO, load);

        for _ in 0..5 {
            cache.get();
        }
        assert!(eventually(|| calls.load(Ordering::SeqCst) >= 2));
        // Room for any refresh that should not have been started to reach the loader.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "one initial load plus a single refresh; a hung WSL must not pile up wsl.exe"
        );
        drop(release);
    }

    #[test]
    fn a_refresh_that_fails_keeps_the_last_good_list() {
        // Forgetting the list would quietly turn a WSL:Ubuntu tab into a local
        // shell: LocalDomain only wraps the command in wsl.exe when it finds its
        // own name among these.
        let (load, calls) = counting(|n| {
            if n == 0 {
                Ok(vec![distro("Ubuntu")])
            } else {
                Err(anyhow::anyhow!("wsl -l -v timed out"))
            }
        });
        let cache = DistroCache::new(Duration::ZERO, load);
        assert_eq!(cache.get(), vec![distro("Ubuntu")]);

        // A third load can only start once the failed second one has been fully
        // recorded, so every answer seen on the way there has passed through it.
        assert!(eventually(|| {
            assert_eq!(cache.get(), vec![distro("Ubuntu")]);
            calls.load(Ordering::SeqCst) >= 3
        }));
    }

    #[cfg(windows)]
    #[test]
    fn a_command_that_finishes_in_time_hands_back_its_output() {
        let output = output_with_timeout(
            Command::new("cmd.exe").args(["/c", "echo", "Ubuntu"]),
            Duration::from_secs(10),
        )
        .unwrap();

        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "Ubuntu");
    }

    #[cfg(windows)]
    #[test]
    fn a_command_that_hangs_is_given_up_on_at_the_timeout() {
        let started = Instant::now();
        let result = output_with_timeout(
            Command::new("ping.exe").args(["-n", "30", "127.0.0.1"]),
            Duration::from_millis(500),
        );

        assert!(result.is_err(), "ping -n 30 cannot finish in half a second");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    #[cfg(windows)]
    #[test]
    fn giving_up_also_ends_the_processes_the_command_started() {
        // wsl.exe is three processes deep (System32 -> WindowsApps -> Program
        // Files\WSL). Killing only the one we started leaves the other two
        // waiting on a stuck WslService for good.
        // The payload size is only there to tell this test's ping from any other.
        let result = output_with_timeout(
            Command::new("cmd.exe").args(["/c", "ping", "-n", "15", "-l", "1473", "127.0.0.1"]),
            Duration::from_millis(1500),
        );

        assert!(result.is_err());
        assert!(
            eventually(|| pings_with_payload_1473() == 0),
            "the ping cmd.exe started outlived the timeout"
        );
    }

    #[cfg(windows)]
    fn pings_with_payload_1473() -> usize {
        let output = Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "@(Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'PING.EXE' -and $_.CommandLine -like '*-l 1473 *' }).Count",
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap()
    }
}
