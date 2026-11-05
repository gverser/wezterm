//! Wiring: subscribe to the mux, accept a client, pump both directions.
//!
//! Exactly one thread writes to the socket. The mux notifications, the flush
//! timer and the command loop all hand it messages through a channel instead
//! of writing directly, so two replies can never interleave halfway through a
//! line -- which would break NDJSON framing in a way that only shows up under
//! load.

use crate::commands::{describe, dispatch};
use crate::mux_access::MuxPane;
use crate::proto::{Inbound, Outbound};
use crate::state::BridgeState;
use anyhow::Context;
use mux::{Mux, MuxNotification};
use parking_lot::Mutex;
use portable_pty::CommandBuilder;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use wezterm_sessions::{NodeId, NodeKind, SessionStore};
use wezterm_term::Alert;

/// How often pending output is reported to the client.
///
/// A pane that is printing fires PaneOutput hundreds of times a second, and
/// the client only needs to know that there is something new to read.
const FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// The sender for whichever client is connected, if any.
type Outbox = Arc<Mutex<Option<Sender<Outbound>>>>;

pub fn start(
    port: u16,
    token_path: PathBuf,
    feishu_command: Vec<String>,
    data_dir: PathBuf,
    legacy_hook_token: PathBuf,
) -> anyhow::Result<()> {
    let token = crate::token::load_or_create(&token_path)
        .with_context(|| format!("loading the bridge token from {}", token_path.display()))?;

    let state = Arc::new(BridgeState::new());
    let outbox: Outbox = Arc::new(Mutex::new(None));
    let feishu = crate::feishu::FeishuService::new(
        feishu_command,
        crate::feishu::ServicePaths {
            port,
            token_path,
            data_dir,
            legacy_hook_token,
        },
    );

    subscribe(state.clone(), outbox.clone(), Arc::clone(&feishu));

    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("binding the bridge to 127.0.0.1:{port}"))?;
    log::info!("bridge listening on 127.0.0.1:{port}");

    std::thread::spawn(move || {
        // One client at a time. incoming() is sequential, so a second client
        // waits until the first has gone.
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    // The service being reachable is what "connected" means to
                    // the user, and only this loop knows it.
                    feishu.on_client(true);
                    if let Err(err) = serve_client(stream, &token, &state, &outbox) {
                        log::warn!("bridge client ended: {err:#}");
                    }
                    outbox.lock().take();
                    feishu.on_client(false);
                }
                Err(err) => log::warn!("bridge accept failed: {err:#}"),
            }
        }
    });

    Ok(())
}

fn subscribe(state: Arc<BridgeState>, outbox: Outbox, feishu: Arc<crate::feishu::FeishuService>) {
    Mux::get().subscribe(move |notif| {
        match notif {
            MuxNotification::PaneAdded(pane_id) => {
                state.add_pane(pane_id);
                if let Some(added) = describe_pane(&state, pane_id) {
                    post(&outbox, added);
                }
            }
            MuxNotification::PaneRemoved(pane_id) => {
                state.remove_pane(pane_id);
                post(&outbox, Outbound::PaneRemoved { pane: pane_id });
            }
            MuxNotification::TabAddedToWindow { tab_id, .. } => {
                // Describe the panes again, now that the description can be
                // finished. A pane is announced by add_pane, which runs before
                // the tab it is in reaches a window, so at that point
                // resolve_pane_id finds nothing and the workspace -- and with
                // it the session name -- comes out empty. Everything else about
                // the pane is already right, which is what made this look like
                // a missing session name rather than a missing window.
                //
                // The client merges a second description over the first, so
                // repeating one costs nothing. It is not an announcement: the
                // client says a session is up on pane_mirror, never on this.
                let Some(mux) = Mux::try_get() else {
                    return true;
                };
                let Some(tab) = mux.get_tab(tab_id) else {
                    return true;
                };
                for p in tab.iter_all_panes() {
                    if let Some(added) = describe_pane(&state, p.pane_id()) {
                        post(&outbox, added);
                    }
                }
            }
            MuxNotification::PaneOutput(pane_id) => {
                // Just a bit. What was printed is read later, off the flush
                // timer, straight out of the scrollback.
                state.mark_dirty(pane_id);
            }
            MuxNotification::PaneFeishuChanged { pane_id, enabled } => {
                // The mux is the authority on this; the bridge just mirrors it
                // so that the flush timer can skip panes nobody is watching.
                state.set_subscribed(pane_id, enabled);
                // The client announces a session going up off this, not off
                // pane_added: opening a shell to run one command in is not an
                // event anyone wants on their phone. The first subscription of
                // all is what starts the service, so there is nobody connected
                // to hear it -- that one the client picks up from `list`.
                post(
                    &outbox,
                    Outbound::PaneMirror {
                        pane: pane_id,
                        enabled,
                    },
                );
                // Read the count back from the mux rather than tracking it here:
                // the mux already de-duplicates, and a pane disappearing also
                // changes this without any notification the bridge would see.
                let count = Mux::try_get().map_or(0, |mux| mux.feishu_pane_count());
                feishu.on_subscription_count(count);
            }
            MuxNotification::Alert { pane_id, alert } => match alert {
                Alert::WindowTitleChanged(_) | Alert::TabTitleChanged(_) => {
                    // Re-ask the pane rather than forwarding the title on the
                    // alert. get_title() is assembled from more than the escape
                    // sequence that just arrived, and it is the answer the pane
                    // was first described with -- two spellings of the same
                    // title would look to the client like two different panes.
                    if let Some(title) = Mux::try_get()
                        .and_then(|mux| mux.get_pane(pane_id))
                        .map(|pane| pane.get_title())
                    {
                        post(
                            &outbox,
                            Outbound::PaneTitle {
                                pane: pane_id,
                                title,
                            },
                        );
                    }
                }
                other => {
                    post(
                        &outbox,
                        Outbound::Alert {
                            pane: pane_id,
                            kind: format!("{other:?}"),
                        },
                    );
                }
            },
            _ => {}
        }
        true
    });
}

/// The `pane_added` for a pane, or None if the mux has already lost it.
///
/// Goes through `commands::describe` so that a pane arriving and a pane being
/// listed are described by the same code rather than by two that agree today.
fn describe_pane(state: &BridgeState, pane_id: mux::pane::PaneId) -> Option<Outbound> {
    let pane = Mux::try_get()?.get_pane(pane_id)?;
    Some(Outbound::PaneAdded(describe(&MuxPane(pane), state)))
}

fn post(outbox: &Outbox, msg: Outbound) {
    if let Some(tx) = outbox.lock().as_ref() {
        let _ = tx.send(msg);
    }
}

fn serve_client(
    stream: TcpStream,
    token: &str,
    state: &Arc<BridgeState>,
    outbox: &Outbox,
) -> anyhow::Result<()> {
    let mut auth_writer = stream.try_clone().context("cloning the stream for auth")?;
    let mut reader = BufReader::new(stream.try_clone().context("cloning the stream to read")?);

    let mut first = String::new();
    reader
        .read_line(&mut first)
        .context("reading the first line")?;
    if let Err(err) = crate::server::authenticate(first.trim(), token) {
        let _ = writeln!(
            auth_writer,
            "{}",
            json(&Outbound::Error {
                message: err.clone()
            })
        );
        anyhow::bail!("{err}");
    }

    let (tx, rx) = channel::<Outbound>();

    // The one writer. When the socket goes it drops rx, and every sender then
    // fails in turn, which is how the other threads learn to stop.
    let mut writer = stream;
    std::thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            if writeln!(writer, "{}", json(&msg)).is_err() {
                break;
            }
        }
    });

    tx.send(Outbound::AuthOk).ok();
    // Register the panes that already exist, but do not announce them. A
    // pane_added for a pane that has been open for hours is a lie the client
    // cannot see through -- it reads as a session starting, with whatever
    // title that pane happened to have when the socket came up. The standing
    // list is what `list` is for, and the client asks for it on connect.
    for pane_id in Mux::get().iter_panes().iter().map(|p| p.pane_id()) {
        state.add_pane(pane_id);
    }
    *outbox.lock() = Some(tx.clone());

    let flush_state = state.clone();
    let flush_tx = tx.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(FLUSH_INTERVAL);
        for pane_id in flush_state.take_dirty() {
            let seqno = Mux::try_get()
                .and_then(|mux| mux.get_pane(pane_id))
                .map(|pane| pane.get_current_seqno())
                .unwrap_or(0);
            if flush_tx
                .send(Outbound::Dirty {
                    pane: pane_id,
                    seqno,
                })
                .is_err()
            {
                return;
            }
        }
    });

    for line in reader.lines() {
        let line = line.context("reading a client line")?;
        let msg: Inbound = match serde_json::from_str(&line) {
            Ok(msg) => msg,
            Err(err) => {
                tx.send(Outbound::Error {
                    message: format!("bad message: {err}"),
                })
                .ok();
                continue;
            }
        };

        let replies = match msg {
            Inbound::Spawn { node_id } => handle_spawn(node_id),
            Inbound::Kill { pane } => handle_kill(pane),
            other => dispatch(other, state, |id| {
                Mux::try_get().and_then(|mux| mux.get_pane(id)).map(MuxPane)
            }),
        };
        for reply in replies {
            tx.send(reply).ok();
        }
    }

    Ok(())
}

/// Start the session named by a node in sessions.json.
///
/// Only entries already in the tree can be started, which is the line between
/// this and a remote shell: the client picks a node, not a command.
pub fn handle_spawn(node_id: u64) -> Vec<Outbound> {
    let store = SessionStore::load();
    let spawn = match store.tree().find(NodeId(node_id)) {
        None => {
            return vec![Outbound::Error {
                message: format!("no such session node: {node_id}"),
            }]
        }
        Some(node) => match &node.kind {
            NodeKind::Session { spawn } => spawn.clone(),
            NodeKind::Folder { .. } => {
                return vec![Outbound::Error {
                    message: format!("session node {node_id} is a folder, not a session"),
                }]
            }
        },
    };

    // NodeId::workspace_name is what the gui sidebar uses too, so a session
    // started from here and one started by double-clicking the sidebar are
    // the same session rather than two that happen to share a name.
    let workspace = NodeId(node_id).workspace_name();

    // Two hops. Domain is #[async_trait(?Send)], so its spawn future cannot
    // cross a thread boundary; what crosses is this closure, which is plain
    // data. Once on the main thread the !Send future can be run there.
    promise::spawn::spawn_into_main_thread(async move {
        promise::spawn::spawn(async move {
            let mux = Mux::get();
            let size = config::configuration().initial_size(0, None);

            let mut cmd = match &spawn.args {
                Some(args) if !args.is_empty() => CommandBuilder::from_argv(
                    args.iter().map(|a| OsString::from(a.as_str())).collect(),
                ),
                _ => CommandBuilder::new_default_prog(),
            };
            for (name, value) in &spawn.env {
                cmd.env(name, value);
            }
            let cwd = spawn.cwd.as_ref().map(|p| p.to_string_lossy().to_string());

            let window_id = mux.new_empty_window(Some(workspace), None);
            let domain = mux.default_domain();
            if let Err(err) = domain.spawn(size, Some(cmd), cwd, *window_id).await {
                log::error!("bridge could not spawn session node {node_id}: {err:#}");
            }
        })
        .detach();
    })
    .detach();

    // Spawning is async, so there is nothing to reply with: the client learns
    // the new pane id from pane_added.
    vec![]
}

/// End one pane.
pub fn handle_kill(pane: mux::pane::PaneId) -> Vec<Outbound> {
    promise::spawn::spawn_into_main_thread(async move {
        Mux::get().remove_pane(pane);
    })
    .detach();
    vec![]
}

fn json(msg: &Outbound) -> String {
    serde_json::to_string(msg).unwrap_or_else(|err| {
        format!(r#"{{"t":"error","message":"failed to serialize a reply: {err}"}}"#)
    })
}
