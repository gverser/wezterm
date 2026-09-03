//! Overlays for editing and deleting session sidebar nodes.
//!
//! These run as termwiz overlays on a worker thread, so everything that touches
//! the session store is hopped back onto the main thread, where it lives.

use crate::sessions::store::with_store;
use crate::sessions::{NodeId, NodeKind, SessionSpawn};
use config::keyassignment::SpawnTabDomain;
use mux::termwiztermtab::TermWizTerminal;
use std::path::PathBuf;
use termwiz::input::{InputEvent, KeyCode, KeyEvent};
use termwiz::lineedit::*;
use termwiz::surface::Change;
use termwiz::terminal::Terminal;

struct EditHost {
    history: BasicHistory,
}

impl EditHost {
    fn new() -> Self {
        Self {
            history: BasicHistory::default(),
        }
    }
}

impl LineEditorHost for EditHost {
    fn history(&mut self) -> &mut dyn History {
        &mut self.history
    }

    fn resolve_action(
        &mut self,
        event: &InputEvent,
        _editor: &mut LineEditor<'_>,
    ) -> Option<Action> {
        // Escape abandons the whole edit, even mid-word: there is nothing
        // destructive about backing out of this dialog.
        match event {
            InputEvent::Key(KeyEvent {
                key: KeyCode::Escape,
                ..
            }) => Some(Action::Cancel),
            _ => None,
        }
    }
}

fn ask(
    term: &mut TermWizTerminal,
    host: &mut EditHost,
    prompt: &str,
    initial: &str,
) -> anyhow::Result<Option<String>> {
    let mut editor = LineEditor::new(term);
    editor.set_prompt(prompt);
    let initial = if initial.is_empty() {
        None
    } else {
        Some(initial)
    };
    Ok(editor.read_line_with_optional_initial_value(host, initial)?)
}

/// Render a `SpawnTabDomain` the way we accept it back.
///
/// An empty string means the default domain, which is what a plain local shell
/// wants; anything else is a domain name such as `SSHMUX:host` or `WSL:Ubuntu`.
fn domain_to_string(domain: &SpawnTabDomain) -> String {
    match domain {
        SpawnTabDomain::DefaultDomain | SpawnTabDomain::CurrentPaneDomain => String::new(),
        SpawnTabDomain::DomainName(name) => name.clone(),
        SpawnTabDomain::DomainId(id) => id.to_string(),
    }
}

fn domain_from_string(text: &str) -> SpawnTabDomain {
    let text = text.trim();
    if text.is_empty() {
        SpawnTabDomain::DefaultDomain
    } else {
        SpawnTabDomain::DomainName(text.to_string())
    }
}

/// Prompt for each field of a session node in turn, then apply the result.
///
/// This is deliberately a sequence of line prompts rather than a form: there is
/// no text input widget in the box model yet, and a real form is worth more
/// than it costs only once there is one.
pub fn edit_session_node(
    mut term: TermWizTerminal,
    id: NodeId,
    name: String,
    spawn: SessionSpawn,
) -> anyhow::Result<()> {
    term.no_grab_mouse_in_raw_mode();

    term.render(&[Change::Text(
        "Edit session\r\n\
         \r\n\
         Enter accepts each field, Escape abandons the edit.\r\n\
         \r\n"
            .to_string(),
    )])?;

    let mut host = EditHost::new();

    let name = match ask(&mut term, &mut host, "Name: ", &name)? {
        Some(value) => value,
        None => return Ok(()),
    };

    let domain = match ask(
        &mut term,
        &mut host,
        "Domain (blank for local): ",
        &domain_to_string(&spawn.domain),
    )? {
        Some(value) => domain_from_string(&value),
        None => return Ok(()),
    };

    let cwd = match ask(
        &mut term,
        &mut host,
        "Directory (blank for the default): ",
        &spawn
            .cwd
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
    )? {
        Some(value) => {
            let value = value.trim();
            if value.is_empty() {
                None
            } else {
                Some(PathBuf::from(value))
            }
        }
        None => return Ok(()),
    };

    let existing_args = spawn
        .args
        .as_ref()
        .and_then(|args| shlex::try_join(args.iter().map(|s| s.as_str())).ok())
        .unwrap_or_default();

    let args = match ask(
        &mut term,
        &mut host,
        "Command (blank for the default shell): ",
        &existing_args,
    )? {
        Some(value) => {
            let value = value.trim();
            if value.is_empty() {
                None
            } else {
                match shlex::split(value) {
                    Some(args) if !args.is_empty() => Some(args),
                    _ => {
                        term.render(&[Change::Text(format!(
                            "\r\nCould not parse {value:?} as a command line; \
                             leaving the command unchanged.\r\n"
                        ))])?;
                        spawn.args.clone()
                    }
                }
            }
        }
        None => return Ok(()),
    };

    let updated = SessionSpawn {
        domain,
        cwd,
        args,
        env: spawn.env.clone(),
    };

    promise::spawn::spawn_into_main_thread(async move {
        with_store(|store| {
            store.mutate(|tree| match tree.find_mut(id) {
                Some(node) => {
                    node.name = name;
                    match &mut node.kind {
                        NodeKind::Session { spawn } => *spawn = updated,
                        NodeKind::Folder { .. } => {}
                    }
                }
                None => log::warn!("session node {id} vanished while it was being edited"),
            })
        });
        anyhow::Result::<()>::Ok(())
    })
    .detach();

    Ok(())
}

/// Confirm closing a window that would take background sessions with it.
///
/// The plain "kill this window" prompt doesn't mention them, and killing only
/// the current workspace leaves the process running with no window at all.
pub fn confirm_close_sessions(
    mut term: TermWizTerminal,
    message: String,
    mux_window_id: mux::window::WindowId,
    window: ::window::Window,
    tab_id: mux::tab::TabId,
) -> anyhow::Result<()> {
    if crate::overlay::confirm::run_confirmation(&message, &mut term)? {
        promise::spawn::spawn_into_main_thread(async move {
            let mux = mux::Mux::get();

            // Same rule as `TermWindow::kill_unattended_windows`: anything no
            // GUI window is showing only exists because the sidebar opened it.
            let front_end = crate::frontend::front_end();
            for window_id in mux.iter_windows() {
                if window_id != mux_window_id
                    && front_end.gui_window_for_mux_window(window_id).is_none()
                {
                    mux.kill_window(window_id);
                }
            }
            mux.kill_window(mux_window_id);
        })
        .detach();
    }

    crate::TermWindow::schedule_cancel_overlay(window, tab_id, None);
    Ok(())
}

/// Confirm, then drop a node from the tree.
///
/// Removing the entry does not kill anything running under it: the workspace
/// survives and is still reachable from the launcher.
pub fn confirm_delete_session_node(
    mut term: TermWizTerminal,
    id: NodeId,
    name: String,
) -> anyhow::Result<()> {
    let message = format!(
        "Delete \"{name}\" from the sidebar?\n\n\
         Terminals running under it will keep running; only the saved entry \
         goes away."
    );

    if crate::overlay::confirm::run_confirmation(&message, &mut term)? {
        promise::spawn::spawn_into_main_thread(async move {
            with_store(|store| store.mutate(|tree| tree.remove(id)));
            anyhow::Result::<()>::Ok(())
        })
        .detach();
    }

    Ok(())
}
