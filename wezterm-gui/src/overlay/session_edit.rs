//! Overlays for the session sidebar that run in a terminal tab: confirming
//! a window close and confirming a delete. Editing a node is a form dialog
//! instead (`sessions::forms`).
//!
//! These run as termwiz overlays on a worker thread, so everything that touches
//! the session store is hopped back onto the main thread, where it lives.

use crate::sessions::store::with_store;
use crate::sessions::NodeId;
use mux::termwiztermtab::TermWizTerminal;

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
            crate::sessions::ssh::forget_deleted_nodes();
            anyhow::Result::<()>::Ok(())
        })
        .detach();
    }

    Ok(())
}
