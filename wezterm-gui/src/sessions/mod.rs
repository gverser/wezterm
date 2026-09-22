//! Gui-side helpers for the session sidebar.
//!
//! The data model and its persistence live in the `wezterm-sessions` crate so
//! that the mux-server bridge can share them. What stays here is the sidebar's
//! own layout arithmetic and prompt copy, which is a rendering concern; the
//! drawing and event handling live in `termwindow::sidebar` and
//! `termwindow::render::sidebar`.

pub use wezterm_sessions::{
    store, NodeId, NodeKind, RowKind, SessionNode, SessionSpawn, VisibleRow,
};

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

/// The prompt shown when closing a window would also take workspaces the user
/// cannot see.
///
/// `hidden` is (label, terminal count). Returns None when there is nothing to
/// warn about, which is the signal to close the ordinary way.
pub fn close_prompt(hidden: &[(String, usize)]) -> Option<String> {
    if hidden.is_empty() {
        return None;
    }

    let terminals: usize = hidden.iter().map(|(_, n)| n).sum();
    let mut text = format!(
        "{} terminal{} still running out of sight:
",
        terminals,
        if terminals == 1 { "" } else { "s" },
    );

    // A long list would push the buttons off the bottom of a short window.
    const MAX_LISTED: usize = 8;
    for (label, count) in hidden.iter().take(MAX_LISTED) {
        text.push_str(&format!(
            "
    {label}  ({count})"
        ));
    }
    if hidden.len() > MAX_LISTED {
        text.push_str(&format!(
            "
    ... and {} more",
            hidden.len() - MAX_LISTED
        ));
    }

    text.push_str(
        "

Close them all and quit?",
    );
    Some(text)
}

/// Whether closing this window should offer to stop the mux server too.
///
/// Deliberately not a function of how many panes the client mirrors: that
/// mirror includes the panes of the very window being closed, so it is never
/// zero and would decide nothing.
pub fn should_confirm_stop_server(
    is_last_window: bool,
    is_client_domain: bool,
    cfg: config::MuxServerCloseConfirmation,
) -> bool {
    matches!(cfg, config::MuxServerCloseConfirmation::AlwaysPrompt)
        && is_last_window
        && is_client_domain
}

#[cfg(test)]
mod test {
    use super::*;

    fn hidden(items: &[(&str, usize)]) -> Vec<(String, usize)> {
        items.iter().map(|(n, c)| (n.to_string(), *c)).collect()
    }

    #[test]
    fn the_last_window_of_a_mux_session_asks() {
        assert!(should_confirm_stop_server(
            true,
            true,
            config::MuxServerCloseConfirmation::AlwaysPrompt
        ));
    }

    #[test]
    fn another_window_is_still_open_so_the_server_is_still_wanted() {
        assert!(!should_confirm_stop_server(
            false,
            true,
            config::MuxServerCloseConfirmation::AlwaysPrompt
        ));
    }

    #[test]
    fn a_local_window_has_no_server_to_stop() {
        assert!(!should_confirm_stop_server(
            true,
            false,
            config::MuxServerCloseConfirmation::AlwaysPrompt
        ));
    }

    #[test]
    fn never_prompt_keeps_the_old_silent_detach() {
        assert!(!should_confirm_stop_server(
            true,
            true,
            config::MuxServerCloseConfirmation::NeverPrompt
        ));
    }

    #[test]
    fn nothing_hidden_means_no_prompt() {
        assert_eq!(close_prompt(&[]), None);
    }

    #[test]
    fn a_single_terminal_reads_singular() {
        let text = close_prompt(&hidden(&[("casb", 1)])).unwrap();
        assert!(
            text.contains("1 terminal still running out of sight"),
            "{text}"
        );
        assert!(text.contains("casb  (1)"), "{text}");
    }

    #[test]
    fn totals_are_summed_across_workspaces() {
        let text = close_prompt(&hidden(&[("casb", 2), ("wezterm", 3)])).unwrap();
        assert!(
            text.contains("5 terminals still running out of sight"),
            "{text}"
        );
        assert!(text.contains("casb  (2)"), "{text}");
        assert!(text.contains("wezterm  (3)"), "{text}");
    }

    #[test]
    fn a_long_list_is_truncated() {
        let many: Vec<(String, usize)> = (0..12).map(|i| (format!("s{i}"), 1)).collect();
        let text = close_prompt(&many).unwrap();
        assert!(
            text.contains("12 terminals still running out of sight"),
            "{text}"
        );
        assert!(text.contains("... and 4 more"), "{text}");
        assert!(
            !text.contains("s8"),
            "entries past the cap are not listed: {text}"
        );
    }

    #[test]
    fn always_asks_the_question() {
        let text = close_prompt(&hidden(&[("casb", 1)])).unwrap();
        assert!(text.ends_with("Close them all and quit?"), "{text}");
    }

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
