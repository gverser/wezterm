//! Wiring: subscribe to the mux, accept a client, pump both directions.
//!
//! Exactly one thread writes to the socket. The mux notifications, the flush
//! timer and the command loop all hand it messages through a channel instead
//! of writing directly, so two replies can never interleave halfway through a
//! line -- which would break NDJSON framing in a way that only shows up under
//! load.

use crate::commands::dispatch;
use crate::mux_access::MuxPane;
use crate::proto::{Inbound, Outbound};
use crate::state::BridgeState;
use anyhow::Context;
use mux::{Mux, MuxNotification};
use parking_lot::Mutex;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

/// How often pending output is reported to the client.
///
/// A pane that is printing fires PaneOutput hundreds of times a second, and
/// the client only needs to know that there is something new to read.
const FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// The sender for whichever client is connected, if any.
type Outbox = Arc<Mutex<Option<Sender<Outbound>>>>;

pub fn start(port: u16, token_path: PathBuf) -> anyhow::Result<()> {
    let token = crate::token::load_or_create(&token_path)
        .with_context(|| format!("loading the bridge token from {}", token_path.display()))?;

    let state = Arc::new(BridgeState::new());
    let outbox: Outbox = Arc::new(Mutex::new(None));

    subscribe(state.clone(), outbox.clone());

    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("binding the bridge to 127.0.0.1:{port}"))?;
    log::info!("bridge listening on 127.0.0.1:{port}");

    std::thread::spawn(move || {
        // One client at a time. incoming() is sequential, so a second client
        // waits until the first has gone.
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    if let Err(err) = serve_client(stream, &token, &state, &outbox) {
                        log::warn!("bridge client ended: {err:#}");
                    }
                    outbox.lock().take();
                }
                Err(err) => log::warn!("bridge accept failed: {err:#}"),
            }
        }
    });

    Ok(())
}

fn subscribe(state: Arc<BridgeState>, outbox: Outbox) {
    Mux::get().subscribe(move |notif| {
        match notif {
            MuxNotification::PaneAdded(pane_id) => {
                state.add_pane(pane_id);
                post(&outbox, describe_pane(pane_id));
            }
            MuxNotification::PaneRemoved(pane_id) => {
                state.remove_pane(pane_id);
                post(&outbox, Outbound::PaneRemoved { pane: pane_id });
            }
            MuxNotification::PaneOutput(pane_id) => {
                // Just a bit. What was printed is read later, off the flush
                // timer, straight out of the scrollback.
                state.mark_dirty(pane_id);
            }
            MuxNotification::Alert { pane_id, alert } => {
                post(
                    &outbox,
                    Outbound::Alert {
                        pane: pane_id,
                        kind: format!("{alert:?}"),
                    },
                );
            }
            _ => {}
        }
        true
    });
}

fn describe_pane(pane_id: mux::pane::PaneId) -> Outbound {
    let title = Mux::try_get()
        .and_then(|mux| mux.get_pane(pane_id))
        .map(|pane| pane.get_title())
        .unwrap_or_default();
    Outbound::PaneAdded {
        pane: pane_id,
        workspace: String::new(),
        title,
        cwd: None,
    }
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
    for pane_id in Mux::get().iter_panes().iter().map(|p| p.pane_id()) {
        state.add_pane(pane_id);
        tx.send(describe_pane(pane_id)).ok();
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

        for reply in dispatch(msg, state, |id| {
            Mux::try_get().and_then(|mux| mux.get_pane(id)).map(MuxPane)
        }) {
            tx.send(reply).ok();
        }
    }

    Ok(())
}

fn json(msg: &Outbound) -> String {
    serde_json::to_string(msg).unwrap_or_else(|err| {
        format!(r#"{{"t":"error","message":"failed to serialize a reply: {err}"}}"#)
    })
}
