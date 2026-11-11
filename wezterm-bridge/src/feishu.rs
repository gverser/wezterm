//! The Feishu service process.
//!
//! Started when the first pane asks to be mirrored, stopped when the last one
//! stops. Nobody has to remember to launch it, and nothing is left running
//! when nobody is watching.

use mux::{FeishuState, Mux};
use parking_lot::Mutex;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

/// The service's app id and secret, when they live in the data dir.
pub const CREDENTIALS_FILE: &str = "feishu-credentials.txt";

/// The token Claude Code hooks present to the service, in the data dir.
pub const HOOK_TOKEN_FILE: &str = "bridge-hook-token";

/// Where the files the Feishu service shares with the bridge are.
pub struct ServicePaths {
    pub port: u16,
    pub token_path: PathBuf,
    pub data_dir: PathBuf,
    /// Where the service kept its hook token before it lived in the data dir.
    pub legacy_hook_token: PathBuf,
}

/// The environment the Feishu service is started with.
///
/// The service has defaults of its own for every one of these, and they
/// point at wherever things happened to be when it was written. Telling it
/// is what lets the files move with the install. A variable already set in
/// our own environment is left out, so an explicit choice is never
/// overridden.
fn service_env(
    paths: &ServicePaths,
    already_set: impl Fn(&str) -> bool,
) -> Vec<(&'static str, OsString)> {
    let mut env: Vec<(&'static str, OsString)> = vec![
        (
            "BRIDGE_TOKEN_PATH",
            paths.token_path.clone().into_os_string(),
        ),
        ("BRIDGE_PORT", paths.port.to_string().into()),
    ];

    // A hook token that exists has already been pasted into someone's Claude
    // Code settings. Pointing the service at a path with nothing in it would
    // have it mint a different token, and every hook would start failing. So
    // the data dir is used once it has a token, or when there is none
    // anywhere to keep.
    let hook_token = paths.data_dir.join(HOOK_TOKEN_FILE);
    if hook_token.exists() || !paths.legacy_hook_token.exists() {
        env.push(("HOOK_TOKEN_PATH", hook_token.into_os_string()));
    }

    // Only when it is there: otherwise the service's own default still
    // applies, rather than a path that is certain to fail.
    let credentials = paths.data_dir.join(CREDENTIALS_FILE);
    if credentials.is_file() {
        env.push(("FEISHU_CREDENTIALS", credentials.into_os_string()));
    }

    env.retain(|(name, _)| !already_set(name));
    env
}

/// Work out what to report, given what we can observe.
///
/// Split out from the process handling because it is the part worth testing:
/// "running" and "usable" are different questions, and only combining both
/// answers gives something a user can act on.
fn derive_state(running: bool, client_attached: bool, exited_on_its_own: bool) -> FeishuState {
    if exited_on_its_own {
        FeishuState::Failed
    } else if !running {
        FeishuState::Stopped
    } else if client_attached {
        FeishuState::Connected
    } else {
        FeishuState::Starting
    }
}

/// Ties spawned processes to this one's lifetime.
///
/// `Drop` handles the ordinary exit, but a mux server that is force-killed
/// never runs it, and the Feishu service then survives as an orphan holding a
/// connection nobody can see or stop. A job object closes with the process
/// whatever way it dies, and takes its members with it.
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

    pub struct Job(HANDLE);

    // The handle is only ever used to add processes and to close, both of
    // which the OS serialises for us.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new() -> Option<Self> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    log::warn!("could not create a job object; a force-killed mux server will leave the Feishu service behind");
                    return None;
                }
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
                    CloseHandle(handle);
                    return None;
                }
                Some(Self(handle))
            }
        }

        pub fn adopt(&self, child: &Child) {
            unsafe {
                if AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) == 0 {
                    log::warn!("could not put the Feishu service in the job object");
                }
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

pub struct FeishuService {
    command: Vec<String>,
    paths: ServicePaths,
    child: Mutex<Option<Child>>,
    /// Whether a client is attached to the bridge. The service *running* and
    /// the service being *usable* are different things, and only the bridge
    /// knows the second one.
    client_attached: Mutex<bool>,
    /// Set when the process went away without being asked to.
    crashed: Mutex<Option<String>>,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl FeishuService {
    pub fn new(command: Vec<String>, paths: ServicePaths) -> Arc<Self> {
        Arc::new(Self {
            command,
            paths,
            child: Mutex::new(None),
            client_attached: Mutex::new(false),
            crashed: Mutex::new(None),
            #[cfg(windows)]
            job: job::Job::new(),
        })
    }

    /// Whether this service is configured at all. With no command there is
    /// nothing to start, and the GUI hides the whole feature.
    pub fn is_configured(&self) -> bool {
        !self.command.is_empty()
    }

    /// React to the number of mirrored panes changing.
    pub fn on_subscription_count(self: &Arc<Self>, count: usize) {
        if count > 0 {
            self.ensure_running();
        } else {
            self.stop();
        }
    }

    /// The bridge calls this when a client attaches or detaches.
    pub fn on_client(&self, attached: bool) {
        *self.client_attached.lock() = attached;
        self.publish();
    }

    fn ensure_running(self: &Arc<Self>) {
        if !self.is_configured() {
            log::warn!("a pane asked for Feishu but feishu_command is not configured");
            return;
        }
        if self.child.lock().is_some() {
            return;
        }

        let mut cmd = Command::new(&self.command[0]);
        cmd.args(&self.command[1..]);
        // The service's own output is its business; we only care that it runs.
        // Its log tells the story when something goes wrong there.
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());
        cmd.stdin(Stdio::null());

        // Worked out at every start rather than once: dropping the
        // credentials file into place should not need a mux server restart.
        for (name, value) in service_env(&self.paths, |name| std::env::var_os(name).is_some()) {
            cmd.env(name, value);
        }

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Without this every start flashes a console window across whatever
            // the user is doing.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        match cmd.spawn() {
            Ok(child) => {
                log::info!("started the Feishu service: {:?}", self.command);
                #[cfg(windows)]
                if let Some(job) = &self.job {
                    job.adopt(&child);
                }
                *self.crashed.lock() = None;
                *self.child.lock() = Some(child);
                self.watch();
            }
            Err(err) => {
                let detail = format!("{err:#}");
                log::error!("could not start the Feishu service: {detail}");
                *self.crashed.lock() = Some(detail);
            }
        }
        self.publish();
    }

    fn stop(&self) {
        let mut child = self.child.lock();
        if let Some(mut c) = child.take() {
            log::info!("stopping the Feishu service");
            let _ = c.kill();
            let _ = c.wait();
        }
        // Asked to stop, so an exit is not news.
        *self.crashed.lock() = None;
        drop(child);
        self.publish();
    }

    /// Notice when the process goes away without being asked to.
    ///
    /// Polled rather than waited on: `wait()` needs to own the child, and kill
    /// needs it too. A couple of seconds late is fine for something whose only
    /// consumer is a status line.
    fn watch(self: &Arc<Self>) {
        let me = Arc::clone(self);
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let status = {
                let mut child = me.child.lock();
                match child.as_mut() {
                    // Someone called stop(); nothing to watch any more.
                    None => return,
                    Some(c) => c.try_wait(),
                }
            };
            match status {
                Ok(Some(status)) => {
                    let detail = format!("服务退出了（{status}）");
                    log::warn!("{detail}");
                    me.child.lock().take();
                    *me.crashed.lock() = Some(detail);
                    // Deliberately not restarting: a service that crashes on
                    // startup would spin here forever.
                    me.publish();
                    return;
                }
                Ok(None) => {}
                Err(err) => {
                    log::warn!("could not check on the Feishu service: {err:#}");
                    return;
                }
            }
        });
    }

    fn publish(&self) {
        let crashed = self.crashed.lock().clone();
        let state = derive_state(
            self.child.lock().is_some(),
            *self.client_attached.lock(),
            crashed.is_some(),
        );
        // Deferred on purpose. publish() is reached from inside a mux
        // notification callback, and Mux::notify holds the subscriber lock
        // while it runs them -- a lock that is not reentrant. Notifying inline
        // deadlocks the mux thread, which stops it answering anything at all:
        // the pane subscribes, the service starts, and then the whole server
        // goes quiet.
        promise::spawn::spawn_into_main_thread(async move {
            if let Some(mux) = Mux::try_get() {
                mux.set_feishu_service_state(state, crashed);
            }
        })
        .detach();
    }
}

impl Drop for FeishuService {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.lock().take() {
            // The mux server is going away; leaving an orphan behind would mean
            // a Feishu connection nobody can see or stop.
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn nothing_running_is_stopped() {
        assert_eq!(derive_state(false, false, false), FeishuState::Stopped);
    }

    #[test]
    fn running_without_a_client_is_still_starting() {
        assert_eq!(
            derive_state(true, false, false),
            FeishuState::Starting,
            "the process being up says nothing about it being ready to carry messages"
        );
    }

    #[test]
    fn running_with_a_client_is_connected() {
        assert_eq!(derive_state(true, true, false), FeishuState::Connected);
    }

    #[test]
    fn an_unasked_for_exit_is_a_failure() {
        assert_eq!(
            derive_state(false, false, true),
            FeishuState::Failed,
            "a service that left on its own is different from one we shut down"
        );
    }

    #[test]
    fn a_crash_outranks_a_stale_client_flag() {
        assert_eq!(
            derive_state(false, true, true),
            FeishuState::Failed,
            "the client flag can lag behind the process dying; the death wins"
        );
    }

    #[test]
    fn an_unconfigured_service_knows_it() {
        let dir = tempfile::tempdir().unwrap();
        let svc = FeishuService::new(vec![], paths_in(dir.path()));
        assert!(!svc.is_configured());
    }

    fn paths_in(dir: &std::path::Path) -> ServicePaths {
        let data_dir = dir.join("data");
        let home = dir.join("home");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        ServicePaths {
            port: 8788,
            token_path: data_dir.join("bridge-token"),
            data_dir,
            legacy_hook_token: home.join(".wezterm-bridge-hook-token"),
        }
    }

    fn get(env: &[(&'static str, OsString)], name: &str) -> Option<OsString> {
        env.iter().find(|(n, _)| *n == name).map(|(_, v)| v.clone())
    }

    fn nothing_set(_: &str) -> bool {
        false
    }

    #[test]
    fn the_bridge_token_and_port_are_always_passed() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let env = service_env(&paths, nothing_set);

        assert_eq!(
            get(&env, "BRIDGE_TOKEN_PATH"),
            Some(paths.token_path.clone().into_os_string())
        );
        assert_eq!(get(&env, "BRIDGE_PORT"), Some(OsString::from("8788")));
    }

    #[test]
    fn a_fresh_install_keeps_its_hook_token_in_the_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        let env = service_env(&paths, nothing_set);

        assert_eq!(
            get(&env, "HOOK_TOKEN_PATH"),
            Some(paths.data_dir.join(HOOK_TOKEN_FILE).into_os_string())
        );
    }

    #[test]
    fn a_hook_token_already_in_the_data_dir_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        std::fs::write(paths.data_dir.join(HOOK_TOKEN_FILE), "new").unwrap();
        std::fs::write(&paths.legacy_hook_token, "old").unwrap();
        let env = service_env(&paths, nothing_set);

        assert_eq!(
            get(&env, "HOOK_TOKEN_PATH"),
            Some(paths.data_dir.join(HOOK_TOKEN_FILE).into_os_string())
        );
    }

    #[test]
    fn a_hook_token_still_in_home_is_left_where_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        std::fs::write(&paths.legacy_hook_token, "old").unwrap();
        let env = service_env(&paths, nothing_set);

        assert_eq!(
            get(&env, "HOOK_TOKEN_PATH"),
            None,
            "pointing the service at an empty path would mint a new token and break every configured hook"
        );
    }

    #[test]
    fn credentials_are_passed_only_when_the_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        assert_eq!(
            get(&service_env(&paths, nothing_set), "FEISHU_CREDENTIALS"),
            None
        );

        let credentials = paths.data_dir.join(CREDENTIALS_FILE);
        std::fs::write(&credentials, "APP_ID:x").unwrap();
        assert_eq!(
            get(&service_env(&paths, nothing_set), "FEISHU_CREDENTIALS"),
            Some(credentials.into_os_string())
        );
    }

    #[test]
    fn an_explicit_environment_variable_wins() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        std::fs::write(paths.data_dir.join(CREDENTIALS_FILE), "APP_ID:x").unwrap();
        let env = service_env(&paths, |name| {
            name == "BRIDGE_PORT" || name == "FEISHU_CREDENTIALS"
        });

        assert_eq!(get(&env, "BRIDGE_PORT"), None);
        assert_eq!(get(&env, "FEISHU_CREDENTIALS"), None);
        assert!(get(&env, "BRIDGE_TOKEN_PATH").is_some());
    }
}
