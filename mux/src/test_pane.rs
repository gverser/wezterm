//! A minimal `Pane` implementation shared by the unit tests in `tab` and
//! `pane_stack`, so each of them doesn't need its own copy of the ~30
//! `unimplemented!()` trait methods.
use crate::domain::DomainId;
use crate::pane::{CachePolicy, ForEachPaneLogicalLine, LogicalLine, Pane, PaneId, WithPaneLines};
use crate::renderable::{RenderableDimensions, StableCursorPosition};
use parking_lot::{MappedMutexGuard, Mutex};
use rangeset::RangeSet;
use std::ops::Range;
use std::sync::Arc;
use termwiz::surface::SequenceNo;
use url::Url;
use wezterm_term::color::ColorPalette;
use wezterm_term::{KeyCode, KeyModifiers, Line, MouseEvent, StableRowIndex, TerminalSize};

pub(crate) struct FakePane {
    id: PaneId,
    size: Mutex<TerminalSize>,
    focus: Mutex<Option<bool>>,
    fail_resize: bool,
    resizes: Mutex<usize>,
}

impl FakePane {
    pub(crate) fn new(id: PaneId, size: TerminalSize) -> Arc<dyn Pane> {
        Arc::new(Self {
            id,
            size: Mutex::new(size),
            focus: Mutex::new(None),
            fail_resize: false,
            resizes: Mutex::new(0),
        })
    }

    /// A pane whose `resize` always fails, for exercising error paths.
    pub(crate) fn with_failing_resize(id: PaneId, size: TerminalSize) -> Arc<dyn Pane> {
        Arc::new(Self {
            id,
            size: Mutex::new(size),
            focus: Mutex::new(None),
            fail_resize: true,
            resizes: Mutex::new(0),
        })
    }
}

/// A terminal size with 10x25 pixel cells at 96 dpi, matching `tab_splitting`.
pub(crate) fn size(rows: usize, cols: usize) -> TerminalSize {
    TerminalSize {
        rows,
        cols,
        pixel_width: cols * 10,
        pixel_height: rows * 25,
        dpi: 96,
    }
}

pub(crate) fn ids(panes: &[Arc<dyn Pane>]) -> Vec<PaneId> {
    panes.iter().map(|p| p.pane_id()).collect()
}

/// The value of the most recent `focus_changed` call, if any.
pub(crate) fn last_focus(pane: &Arc<dyn Pane>) -> Option<bool> {
    let fake = pane
        .downcast_ref::<FakePane>()
        .expect("last_focus: not a FakePane");
    let focus = *fake.focus.lock();
    focus
}

pub(crate) fn current_size(pane: &Arc<dyn Pane>) -> TerminalSize {
    let fake = pane
        .downcast_ref::<FakePane>()
        .expect("current_size: not a FakePane");
    let size = *fake.size.lock();
    size
}

/// How many times `resize` has been called on the pane, failed calls included.
pub(crate) fn resize_count(pane: &Arc<dyn Pane>) -> usize {
    let fake = pane
        .downcast_ref::<FakePane>()
        .expect("resize_count: not a FakePane");
    let count = *fake.resizes.lock();
    count
}

impl Pane for FakePane {
    fn pane_id(&self) -> PaneId {
        self.id
    }

    fn get_cursor_position(&self) -> StableCursorPosition {
        Default::default()
    }

    fn get_current_seqno(&self) -> SequenceNo {
        unimplemented!();
    }

    fn get_changed_since(
        &self,
        _lines: Range<StableRowIndex>,
        _: SequenceNo,
    ) -> RangeSet<StableRowIndex> {
        unimplemented!();
    }

    fn with_lines_mut(
        &self,
        _stable_range: Range<StableRowIndex>,
        _with_lines: &mut dyn WithPaneLines,
    ) {
        unimplemented!();
    }

    fn for_each_logical_line_in_stable_range_mut(
        &self,
        _lines: Range<StableRowIndex>,
        _for_line: &mut dyn ForEachPaneLogicalLine,
    ) {
        unimplemented!();
    }

    fn get_lines(&self, _lines: Range<StableRowIndex>) -> (StableRowIndex, Vec<Line>) {
        unimplemented!();
    }

    fn get_logical_lines(&self, _lines: Range<StableRowIndex>) -> Vec<LogicalLine> {
        unimplemented!();
    }

    fn get_dimensions(&self) -> RenderableDimensions {
        let size = *self.size.lock();
        RenderableDimensions {
            cols: size.cols,
            viewport_rows: size.rows,
            scrollback_rows: size.rows,
            physical_top: 0,
            scrollback_top: 0,
            dpi: size.dpi,
            pixel_width: size.pixel_width,
            pixel_height: size.pixel_height,
            reverse_video: false,
        }
    }

    fn get_title(&self) -> String {
        format!("pane{}", self.id)
    }
    fn send_paste(&self, _text: &str) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn reader(&self) -> anyhow::Result<Option<Box<dyn std::io::Read + Send>>> {
        Ok(None)
    }
    fn writer(&self) -> MappedMutexGuard<'_, dyn std::io::Write> {
        unimplemented!()
    }
    fn resize(&self, size: TerminalSize) -> anyhow::Result<()> {
        *self.resizes.lock() += 1;
        if self.fail_resize {
            anyhow::bail!("FakePane {} refuses to resize", self.id);
        }
        *self.size.lock() = size;
        Ok(())
    }
    fn focus_changed(&self, focused: bool) {
        *self.focus.lock() = Some(focused);
    }

    fn key_down(&self, _key: KeyCode, _mods: KeyModifiers) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn key_up(&self, _: KeyCode, _: KeyModifiers) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn mouse_event(&self, _event: MouseEvent) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn is_dead(&self) -> bool {
        false
    }
    fn palette(&self) -> ColorPalette {
        unimplemented!()
    }
    fn domain_id(&self) -> DomainId {
        1
    }
    fn is_mouse_grabbed(&self) -> bool {
        false
    }
    fn is_alt_screen_active(&self) -> bool {
        false
    }
    fn get_current_working_dir(&self, _policy: CachePolicy) -> Option<Url> {
        None
    }
}
