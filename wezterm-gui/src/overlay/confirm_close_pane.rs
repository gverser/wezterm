use super::confirm;
use crate::frontend::front_end;
use crate::TermWindow;
use mux::domain::DomainId;
use mux::pane::PaneId;
use mux::tab::TabId;
use mux::termwiztermtab::TermWizTerminal;
use mux::window::WindowId;
use mux::Mux;
use smol::prelude::*;
use window::WindowOps;

pub fn confirm_close_pane(
    pane_id: PaneId,
    mut term: TermWizTerminal,
    mux_window_id: WindowId,
    window: ::window::Window,
) -> anyhow::Result<()> {
    if confirm::run_confirmation("🛑 Really kill this pane?", &mut term)? {
        promise::spawn::spawn_into_main_thread(async move {
            let mux = Mux::get();
            let tab = match mux.get_active_tab_for_window(mux_window_id) {
                Some(tab) => tab,
                None => return,
            };
            tab.kill_pane(pane_id);
        })
        .detach();
    }
    TermWindow::schedule_cancel_overlay_for_pane(window, pane_id);

    Ok(())
}

pub fn confirm_close_tab(
    tab_id: TabId,
    mut term: TermWizTerminal,
    _mux_window_id: WindowId,
    window: ::window::Window,
) -> anyhow::Result<()> {
    if confirm::run_confirmation(
        "🛑 Really kill this tab and all contained panes?",
        &mut term,
    )? {
        promise::spawn::spawn_into_main_thread(async move {
            let mux = Mux::get();
            mux.remove_tab(tab_id);
        })
        .detach();
    }
    TermWindow::schedule_cancel_overlay(window, tab_id, None);

    Ok(())
}

pub fn confirm_close_window(
    mut term: TermWizTerminal,
    mux_window_id: WindowId,
    window: ::window::Window,
    tab_id: TabId,
) -> anyhow::Result<()> {
    if confirm::run_confirmation(
        "🛑 Really kill this window and all contained tabs and panes?",
        &mut term,
    )? {
        promise::spawn::spawn_into_main_thread(async move {
            let mux = Mux::get();
            mux.kill_window(mux_window_id);
        })
        .detach();
    }
    TermWindow::schedule_cancel_overlay(window, tab_id, None);

    Ok(())
}

/// Closing the last window of a mux session: offer to stop the server too.
///
/// Everything happens here rather than in close_requested, because the prompt
/// runs on another thread and the window must still exist to show it. `n` is
/// what wezterm has always done: detach and leave the work running.
pub fn confirm_stop_mux_server(
    mut term: TermWizTerminal,
    mux_window_id: WindowId,
    window: ::window::Window,
    tab_id: TabId,
    domain_id: DomainId,
    other_sessions: usize,
) -> anyhow::Result<()> {
    let first_line = if other_sessions == 0 {
        "🛑 This session will keep running on the mux server.".to_string()
    } else {
        format!(
            "🛑 This session and {other_sessions} other(s) will keep running on the mux server."
        )
    };
    let prompt = format!(
        "{first_line}\n\
         y = stop the mux server and close this window\n\
         n = close this window and leave the server running"
    );
    let stop_server = confirm::run_confirmation(&prompt, &mut term)?;

    let close_window = window.clone();
    promise::spawn::spawn_into_main_thread(async move {
        if stop_server {
            // The server exits as it answers, so a connection that ends is
            // the success case, not a failure.
            match wezterm_client::domain::ClientDomain::get_client_inner_for_domain(domain_id) {
                Ok(inner) => {
                    // A server that is alive but wedged must not trap this
                    // window forever: cap the wait and close regardless of
                    // whether we got a reply, an error, or nothing at all.
                    let res = inner
                        .client
                        .kill_server()
                        .or(async {
                            smol::Timer::after(std::time::Duration::from_secs(5)).await;
                            anyhow::bail!("timed out waiting for the server to acknowledge")
                        })
                        .await;
                    if let Err(err) = res {
                        log::info!("kill_server: {err:#}");
                    }
                }
                Err(err) => log::warn!("no client for domain {domain_id}: {err:#}"),
            }
        }
        let mux = Mux::get();
        mux.kill_window(mux_window_id);
        close_window.close();
        front_end().forget_known_window(&close_window);
    })
    .detach();

    TermWindow::schedule_cancel_overlay(window, tab_id, None);
    Ok(())
}

pub fn confirm_quit_program(
    mut term: TermWizTerminal,
    window: ::window::Window,
    tab_id: TabId,
) -> anyhow::Result<()> {
    if confirm::run_confirmation("🛑 Really Quit WezTerm?", &mut term)? {
        promise::spawn::spawn_into_main_thread(async move {
            use ::window::{Connection, ConnectionOps};
            let con = Connection::get().expect("call on gui thread");
            con.terminate_message_loop();
        })
        .detach();
    }
    TermWindow::schedule_cancel_overlay(window, tab_id, None);

    Ok(())
}
