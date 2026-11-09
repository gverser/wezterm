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
use wezterm_ssh::{ChildKiller, FileDescriptor, SshChildProcess};

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
}

struct Monitor {
    /// `RemoteSshDomain::authenticated_session`'s counter: a reconnect makes
    /// this monitor stale.
    generation: u64,
    state: MonitorState,
    status: Option<ServerStatus>,
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
}

fn invalidate_all_windows() {
    for win in crate::frontend::front_end().gui_windows() {
        win.window.invalidate();
    }
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
    let Some((generation, session)) = ssh.authenticated_session() else {
        return;
    };
    let current = MONITORS.with(|m| m.borrow().get(&domain_id).map(|m| m.generation));
    if current == Some(generation) {
        // Running, starting, or given up on for this connection. A failed
        // channel is not retried on every paint; a reconnect brings a new
        // generation and a fresh try.
        return;
    }

    MONITORS.with(|m| {
        m.borrow_mut().insert(
            domain_id,
            Monitor {
                generation,
                state: MonitorState::Starting,
                status: None,
                cwds: HashMap::new(),
                computer: StatusComputer::default(),
                stdin: None,
                child: None,
            },
        )
    });

    promise::spawn::spawn(async move {
        let exec = match session.exec("sh -s", None).await {
            Ok(exec) => exec,
            Err(err) => {
                log::error!("ssh monitor for domain {domain_id}: exec failed: {err:#}");
                set_state(domain_id, generation, MonitorState::Failed);
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
            set_state(domain_id, generation, MonitorState::Failed);
            return;
        }

        let stored = MONITORS.with(|m| match m.borrow_mut().get_mut(&domain_id) {
            Some(mon) if mon.generation == generation => {
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
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(event) = parser.feed_line(&line) {
                    promise::spawn::spawn_into_main_thread(async move {
                        apply_event(domain_id, generation, event);
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
                    set_state(domain_id, generation, MonitorState::Failed);
                }
            })
            .detach();
        });
    })
    .detach();
}

fn set_state(domain_id: DomainId, generation: u64, state: MonitorState) {
    MONITORS.with(|m| {
        if let Some(mon) = m.borrow_mut().get_mut(&domain_id) {
            if mon.generation == generation {
                mon.state = state;
            }
        }
    });
    invalidate_all_windows();
}

fn apply_event(domain_id: DomainId, generation: u64, event: ParseEvent) {
    let mut cwd_changed = false;
    MONITORS.with(|m| {
        let mut monitors = m.borrow_mut();
        let Some(mon) = monitors.get_mut(&domain_id) else {
            return;
        };
        if mon.generation != generation {
            return;
        }
        match event {
            ParseEvent::Unsupported => mon.state = MonitorState::Unsupported,
            ParseEvent::Frame(frame) => {
                for (pane, dir) in frame.cwds {
                    let Some(dir) = dir else { continue };
                    let pane = pane as PaneId;
                    if mon.cwds.get(&pane) != Some(&dir) {
                        mon.cwds.insert(pane, dir);
                        cwd_changed = true;
                    }
                }
                if let Some(metrics) = frame.metrics {
                    mon.status = Some(mon.computer.update(metrics));
                    mon.state = MonitorState::Running;
                }
            }
        }
    });
    if cwd_changed {
        CWD_GENERATION.with(|g| g.set(g.get() + 1));
    }
    invalidate_all_windows();
}

pub fn monitor_view(domain_id: DomainId) -> Option<MonitorView> {
    MONITORS.with(|m| {
        m.borrow().get(&domain_id).map(|mon| MonitorView {
            state: mon.state,
            status: mon.status.clone(),
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
    fn status_bar_only_for_sidebar_ssh_panes() {
        assert!(wants_status_bar(true, true));
        assert!(!wants_status_bar(true, false));
        assert!(!wants_status_bar(false, true));
        assert!(!wants_status_bar(false, false));
    }
}
