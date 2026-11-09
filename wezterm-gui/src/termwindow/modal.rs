use crate::termwindow::box_model::ComputedElement;
use crate::TermWindow;
use config::keyassignment::KeyAssignment;
use downcast_rs::{impl_downcast, Downcast};
use std::cell::Ref;
use wezterm_term::{KeyCode, KeyModifiers, MouseEvent};

pub trait Modal: Downcast {
    fn perform_assignment(
        &self,
        _assignment: &KeyAssignment,
        _term_window: &mut TermWindow,
    ) -> bool {
        false
    }
    fn mouse_event(&self, event: MouseEvent, term_window: &mut TermWindow) -> anyhow::Result<()>;
    fn key_down(
        &self,
        key: KeyCode,
        mods: KeyModifiers,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<bool>;
    fn computed_element(
        &self,
        term_window: &mut TermWindow,
    ) -> anyhow::Result<Ref<'_, [ComputedElement]>>;
    fn reconfigure(&self, term_window: &mut TermWindow);

    /// Committed text from the IME or the clipboard. Return true if the
    /// modal took it; otherwise it goes on to the pane as usual.
    fn insert_text(&self, _text: &str, _term_window: &mut TermWindow) -> bool {
        false
    }

    /// The IME's uncommitted text. Return true if the modal will draw it
    /// itself, so that the terminal doesn't draw it at its cursor too.
    fn set_composing(&self, _text: Option<String>, _term_window: &mut TermWindow) -> bool {
        false
    }

    /// Where the IME candidate window should appear, in window pixels.
    fn text_cursor_rect(&self, _term_window: &TermWindow) -> Option<::window::Rect> {
        None
    }

    /// A mouse event on an element the modal tagged `UIItemType::ModalItem(index)`.
    fn ui_item_event(
        &self,
        _index: usize,
        _event: &::window::MouseEvent,
        _term_window: &mut TermWindow,
    ) -> bool {
        false
    }

    /// Whether clicks outside the modal should be swallowed rather than
    /// reaching the terminal or sidebar underneath.
    fn captures_mouse(&self) -> bool {
        false
    }
}
impl_downcast!(Modal);
