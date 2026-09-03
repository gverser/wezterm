//! Production `PaneAccess`: a thin wrapper over a real mux pane.
//!
//! Deliberately nothing but delegation. Everything worth testing lives in
//! `commands` and `state`, which work against the trait rather than this.

use crate::state::PaneAccess;
use mux::pane::{Pane, PaneId};
use std::sync::Arc;
use termwiz::surface::SequenceNo;
use wezterm_term::{KeyCode, KeyModifiers};

pub struct MuxPane(pub Arc<dyn Pane>);

impl PaneAccess for MuxPane {
    fn pane_id(&self) -> PaneId {
        self.0.pane_id()
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
