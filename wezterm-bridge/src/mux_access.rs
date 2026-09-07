//! Production `PaneAccess`: a thin wrapper over a real mux pane.
//!
//! Deliberately nothing but delegation. Everything worth testing lives in
//! `commands` and `state`, which work against the trait rather than this.

use crate::state::PaneAccess;
use mux::pane::{CachePolicy, Pane, PaneId};
use mux::Mux;
use std::sync::Arc;
use termwiz::surface::SequenceNo;
use wezterm_term::{KeyCode, KeyModifiers};

pub struct MuxPane(pub Arc<dyn Pane>);

impl PaneAccess for MuxPane {
    fn pane_id(&self) -> PaneId {
        self.0.pane_id()
    }

    fn title(&self) -> String {
        self.0.get_title()
    }

    fn workspace(&self) -> String {
        Mux::try_get()
            .and_then(|mux| {
                let (_domain, window_id, _tab) = mux.resolve_pane_id(self.0.pane_id())?;
                Some(mux.get_window(window_id)?.get_workspace().to_string())
            })
            .unwrap_or_default()
    }

    fn cwd(&self) -> Option<String> {
        // AllowStale rather than FetchImmediate: this is called from the mux
        // notification thread, and blocking it to ask a remote server where a
        // pane is would stall every other pane's events behind it.
        let url = self.0.get_current_working_dir(CachePolicy::AllowStale)?;
        // to_file_path gives the native spelling -- D:\dev\x rather than
        // /D:/dev/x -- which is what the user recognises. It fails for a
        // non-local url, and then the url itself is the honest answer.
        Some(match url.to_file_path() {
            Ok(path) => path.to_string_lossy().to_string(),
            Err(()) => url.to_string(),
        })
    }

    fn session(&self) -> Option<String> {
        crate::labels::session_for_workspace(&self.workspace())
    }

    fn current_seqno(&self) -> SequenceNo {
        self.0.get_current_seqno()
    }

    fn changed_since(&self, seqno: SequenceNo) -> bool {
        let dims = self.0.get_dimensions();
        let range = dims.physical_top..dims.physical_top + dims.viewport_rows as isize;
        !self.0.get_changed_since(range, seqno).is_empty()
    }

    fn read_lines(&self, max_lines: usize) -> (isize, Vec<String>) {
        let dims = self.0.get_dimensions();
        let end = dims.physical_top + dims.viewport_rows as isize;
        let start = end.saturating_sub(max_lines as isize);
        let (first_row, lines) = self.0.get_lines(start..end);
        (
            first_row,
            lines
                .iter()
                .map(|line| line.as_str().trim_end().to_string())
                .collect(),
        )
    }

    fn send_key(&self, key: &str, modifiers: &[String]) -> anyhow::Result<()> {
        let code =
            crate::keys::parse_key(key).ok_or_else(|| anyhow::anyhow!("unknown key: {key}"))?;
        let mods = crate::keys::parse_modifiers(modifiers);
        self.0.key_down(code, mods)?;
        self.0.key_up(code, mods)?;
        Ok(())
    }

    fn send_text(&self, text: &str, submit: bool) -> anyhow::Result<()> {
        // send_paste rather than writing to the pty directly: it honours
        // bracketed paste, which is what a TUI prompt expects from text that
        // did not come from a keyboard.
        self.0.send_paste(text)?;
        if submit {
            self.0.key_down(KeyCode::Enter, KeyModifiers::NONE)?;
        }
        Ok(())
    }
}
