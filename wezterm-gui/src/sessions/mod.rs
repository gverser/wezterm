//! The session tree that backs the left sidebar.
//!
//! `tree` is the data model, `store` is its persistence. Both are free of
//! rendering concerns; the drawing and event handling live in
//! `termwindow::sidebar` and `termwindow::render::sidebar`.

pub mod store;
pub mod tree;

pub use tree::{NodeId, NodeKind, RowKind, SessionNode, SessionSpawn, VisibleRow};

use std::ops::Range;

/// Clamp a scroll position so the list never scrolls past its end.
pub fn clamp_scroll(total_rows: usize, scroll_offset: usize, capacity: usize) -> usize {
    scroll_offset.min(total_rows.saturating_sub(capacity))
}

/// The half-open range of rows to draw.
///
/// box_model has no scrolling container, so the sidebar only builds elements
/// for the rows inside this window, the same way charselect handles a long
/// match list.
pub fn visible_range(total_rows: usize, scroll_offset: usize, capacity: usize) -> Range<usize> {
    let start = clamp_scroll(total_rows, scroll_offset, capacity);
    let end = start.saturating_add(capacity).min(total_rows);
    start..end
}

/// Constrain the sidebar width to something the window can actually afford.
///
/// Without an upper bound, dragging the splitter past the window edge leaves
/// zero pixels for the terminal, and the division in `resize.rs` then produces
/// a `TerminalSize` of 0 rows by 0 cols.
pub fn clamp_width(desired: f32, window_width: f32, min_width: f32) -> f32 {
    let max_width = window_width * 0.5;
    if max_width <= min_width {
        // A window too narrow to honor the minimum: half the window is still
        // better than letting the terminal collapse.
        return max_width.max(0.);
    }
    desired.max(min_width).min(max_width)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn shows_everything_when_it_fits() {
        assert_eq!(visible_range(3, 0, 10), 0..3);
    }

    #[test]
    fn windows_a_long_list() {
        assert_eq!(visible_range(100, 10, 5), 10..15);
    }

    #[test]
    fn never_scrolls_past_the_end() {
        assert_eq!(
            visible_range(100, 999, 5),
            95..100,
            "an over-large offset pins to the last full page, not to blank space"
        );
    }

    #[test]
    fn an_empty_list_yields_an_empty_range() {
        assert_eq!(visible_range(0, 0, 10), 0..0);
        assert_eq!(visible_range(0, 5, 10), 0..0);
    }

    #[test]
    fn zero_capacity_yields_an_empty_range() {
        assert_eq!(visible_range(10, 0, 0), 0..0);
    }

    #[test]
    fn clamp_scroll_pins_to_the_last_page() {
        assert_eq!(clamp_scroll(100, 999, 5), 95);
        assert_eq!(clamp_scroll(3, 999, 10), 0);
    }

    #[test]
    fn width_is_clamped_to_half_the_window() {
        assert_eq!(clamp_width(900., 1000., 100.), 500.);
    }

    #[test]
    fn width_respects_the_minimum() {
        assert_eq!(clamp_width(10., 1000., 100.), 100.);
    }

    #[test]
    fn width_in_range_is_untouched() {
        assert_eq!(clamp_width(250., 1000., 100.), 250.);
    }

    #[test]
    fn a_window_narrower_than_the_minimum_still_leaves_room_for_the_terminal() {
        // 120px window: the 100px minimum can't be honored without starving the
        // terminal, so half the window wins.
        assert_eq!(clamp_width(100., 120., 100.), 60.);
    }

    #[test]
    fn a_zero_width_window_yields_zero() {
        assert_eq!(clamp_width(100., 0., 100.), 0.);
    }
}
