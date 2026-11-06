//! Pure layout of one pane stack's tab strip: titles and states in,
//! one `Line` of cells plus hit regions out. Rendering and mouse
//! registration live elsewhere; keeping this free of GUI state makes the
//! width arithmetic unit-testable.
use finl_unicode::grapheme_clusters::Graphemes;
use mux::pane::PaneId;
use termwiz::cell::{Cell, CellAttributes};
use termwiz::surface::SEQ_ZERO;
use wezterm_term::Line;

pub struct StripTab {
    pub pane_id: PaneId,
    pub title: String,
    pub unseen: bool,
    pub is_visible: bool,
}

pub struct StripStyle {
    pub background: CellAttributes,
    pub active: CellAttributes,
    pub visible: CellAttributes,
    pub hidden: CellAttributes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripHitKind {
    Tab(PaneId),
    Close(PaneId),
}

/// `x` and `width` are in cells, measured from the left edge of the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StripHit {
    pub kind: StripHitKind,
    pub x: usize,
    pub width: usize,
}

pub struct StripLayout {
    pub line: Line,
    pub hits: Vec<StripHit>,
}

pub fn layout_strip(
    tabs: &[StripTab],
    leaf_is_active: bool,
    width: usize,
    style: &StripStyle,
) -> StripLayout {
    let mut cells: Vec<Cell> = Vec::with_capacity(width);
    let mut hits = vec![];
    let n = tabs.len();

    if width > 0 && n > 0 {
        let naturals: Vec<usize> = tabs.iter().map(natural_width).collect();
        let (first, widths): (usize, Vec<usize>) = if naturals.iter().sum::<usize>() <= width {
            (0, naturals)
        } else if n * MIN_TAB <= width {
            let cap = MIN_TAB.max(width / n);
            (0, naturals.iter().map(|&w| w.min(cap)).collect())
        } else {
            // Not even MIN_TAB per tab fits: show a window of equal-width
            // tabs that contains the visible one, centred where possible.
            let tab_w = MIN_TAB.min(width);
            let k = (width / MIN_TAB).max(1).min(n);
            let visible = tabs.iter().position(|t| t.is_visible).unwrap_or(0);
            let first = visible.saturating_sub((k - 1) / 2).min(n - k);
            (first, vec![tab_w; k])
        };

        let mut x = 0;
        for (tab, &w) in tabs[first..].iter().zip(widths.iter()) {
            let attrs = if !tab.is_visible {
                &style.hidden
            } else if leaf_is_active {
                &style.active
            } else {
                &style.visible
            };
            cells.extend(tab_cells(tab, w, attrs));
            hits.push(StripHit {
                kind: StripHitKind::Tab(tab.pane_id),
                x,
                width: w,
            });
            if w >= 4 + marker_width(tab) {
                hits.push(StripHit {
                    kind: StripHitKind::Close(tab.pane_id),
                    x: x + w - 2,
                    width: 1,
                });
            }
            x += w;
        }

        let hidden_left = first > 0;
        let hidden_right = first + widths.len() < n;
        cells.resize(width, Cell::blank_with_attrs(style.background.clone()));
        if hidden_left {
            cells[0] = Cell::new('‹', style.hidden.clone());
        }
        if hidden_right {
            cells[width - 1] = Cell::new('›', style.hidden.clone());
        }
    }

    cells.resize(width, Cell::blank_with_attrs(style.background.clone()));
    StripLayout {
        line: Line::from_cells(cells, SEQ_ZERO),
        hits,
    }
}

/// Smallest width a tab is squeezed to before tabs start scrolling off.
const MIN_TAB: usize = 8;

/// Only hidden tabs with unseen output get the marker; a visible tab is
/// being looked at, so flagging it would be noise.
fn marker_width(tab: &StripTab) -> usize {
    if tab.unseen && !tab.is_visible {
        2
    } else {
        0
    }
}

/// " " + marker + title + " × " — the fixed parts are 4 cells.
fn natural_width(tab: &StripTab) -> usize {
    Graphemes::new(&tab.title)
        .map(grapheme_width)
        .sum::<usize>()
        + 4
        + marker_width(tab)
}

/// Columns that `push_text` writes for a grapheme: 2 for a wide one and 1
/// for anything else. Measuring with anything else (e.g.
/// `unicode_column_width`) disagrees for zero-width graphemes and control
/// chars, and that would push the trailing " × " out of its hit region.
/// Even `Cell::width` reports 0 for a zero-width grapheme of 8 bytes or
/// more, which it keeps on the heap with its raw width, so it is only
/// asked whether the grapheme is wide; that width is capped at 2.
fn grapheme_width(g: &str) -> usize {
    let w = Cell::new_grapheme(g, CellAttributes::default(), None).width();
    if w == 2 {
        2
    } else {
        1
    }
}

/// Append `s` grapheme by grapheme. A wide grapheme is followed by a blank
/// cell so that `Line` sees the same layout the terminal model produces.
fn push_text(cells: &mut Vec<Cell>, s: &str, attrs: &CellAttributes) {
    for g in Graphemes::new(s) {
        let cell = Cell::new_grapheme(g, attrs.clone(), None);
        let w = cell.width();
        cells.push(cell);
        if w == 2 {
            cells.push(Cell::blank_with_attrs(attrs.clone()));
        }
    }
}

/// Render one tab into exactly `w` cells.
fn tab_cells(tab: &StripTab, w: usize, attrs: &CellAttributes) -> Vec<Cell> {
    let marker = if marker_width(tab) > 0 { "• " } else { "" };
    let fixed = 4 + marker_width(tab);
    let mut cells = Vec::with_capacity(w);
    push_text(&mut cells, " ", attrs);
    push_text(&mut cells, marker, attrs);

    // Titles are cut on grapheme boundaries by cell width, so a wide char
    // that does not fit is dropped whole and replaced by padding.
    let budget = w.saturating_sub(fixed);
    let mut used = 0;
    for g in Graphemes::new(&tab.title) {
        let gw = grapheme_width(g);
        if used + gw > budget {
            break;
        }
        push_text(&mut cells, g, attrs);
        used += gw;
    }
    for _ in used..budget {
        push_text(&mut cells, " ", attrs);
    }
    push_text(&mut cells, " × ", attrs);

    // Only reachable when w < fixed; the fixed parts are all single-cell.
    cells.truncate(w);
    cells
}

#[cfg(test)]
mod test {
    use super::*;
    use termwiz::cell::unicode_column_width;
    use termwiz::color::AnsiColor;

    fn attrs(color: AnsiColor) -> CellAttributes {
        let mut a = CellAttributes::default();
        a.set_foreground(color);
        a
    }

    fn style() -> StripStyle {
        StripStyle {
            background: attrs(AnsiColor::Black),
            active: attrs(AnsiColor::Red),
            visible: attrs(AnsiColor::Green),
            hidden: attrs(AnsiColor::Blue),
        }
    }

    fn tab(id: PaneId, title: &str, visible: bool, unseen: bool) -> StripTab {
        StripTab {
            pane_id: id,
            title: title.to_string(),
            unseen,
            is_visible: visible,
        }
    }

    /// (column, text, width) of every visible cell.
    fn cells(line: &Line) -> Vec<(usize, String, usize)> {
        line.visible_cells()
            .map(|c| (c.cell_index(), c.str().to_string(), c.width()))
            .collect()
    }

    #[test]
    fn strip_lays_out_tabs_with_close_buttons() {
        let s = layout_strip(
            &[tab(1, "a", false, false), tab(2, "bb", true, false)],
            true,
            40,
            &style(),
        );
        assert_eq!(s.line.len(), 40);
        assert_eq!(s.line.as_str().trim_end(), " a ×  bb ×");
        assert_eq!(
            s.hits,
            vec![
                StripHit {
                    kind: StripHitKind::Tab(1),
                    x: 0,
                    width: 5
                },
                StripHit {
                    kind: StripHitKind::Close(1),
                    x: 3,
                    width: 1
                },
                StripHit {
                    kind: StripHitKind::Tab(2),
                    x: 5,
                    width: 6
                },
                StripHit {
                    kind: StripHitKind::Close(2),
                    x: 9,
                    width: 1
                },
            ]
        );
    }

    #[test]
    fn strip_marks_unseen_hidden_tabs_only() {
        let s = layout_strip(
            &[tab(1, "ab", false, true), tab(2, "cd", true, true)],
            true,
            40,
            &style(),
        );
        // Hidden unseen gets the marker; the visible one does not even
        // though it is flagged unseen.
        assert_eq!(s.line.as_str().trim_end(), " • ab ×  cd ×");
    }

    #[test]
    fn strip_styles_visible_tab_by_leaf_focus() {
        let st = style();
        let tabs = [tab(1, "a", false, false), tab(2, "b", true, false)];
        for (active, expect) in [(true, &st.active), (false, &st.visible)] {
            let s = layout_strip(&tabs, active, 20, &st);
            let all = cells(&s.line);
            for (col, _, _) in &all {
                let cell = s.line.get_cell(*col).unwrap();
                let a = cell.attrs();
                if *col < 5 {
                    assert_eq!(a, &st.hidden, "col {col}");
                } else if *col < 10 {
                    assert_eq!(a, expect, "col {col} active={active}");
                } else {
                    assert_eq!(a, &st.background, "col {col}");
                }
            }
            // Make sure the loop above was not vacuous.
            assert_eq!(all.len(), 20);
        }
    }

    #[test]
    fn strip_truncates_wide_titles_by_cells() {
        let t = "中文中文中文";
        let s = layout_strip(
            &[tab(1, t, false, false), tab(2, t, true, false)],
            true,
            22,
            &style(),
        );
        assert_eq!(s.line.len(), 22);
        // Each tab is 11 cells: " " + 3 wide chars (6) + 1 pad + " × ".
        let expect_tab = " 中文中  × ";
        assert_eq!(unicode_column_width(expect_tab, None), 11);
        assert_eq!(s.line.as_str(), format!("{expect_tab}{expect_tab}"));
        let all = cells(&s.line);
        // No wide cell may straddle a tab boundary or be cut: every
        // width-2 cell is followed by a skipped column.
        for (i, (col, text, w)) in all.iter().enumerate() {
            if *w == 2 {
                let next = all.get(i + 1).map(|n| n.0).unwrap_or(22);
                assert_eq!(next, col + 2, "wide cell {text:?} at {col}");
            }
        }
        // Column 7 is the padding space, column 8..11 is " × ".
        let c7 = s.line.get_cell(7).unwrap();
        assert_eq!(c7.str(), " ");
        assert_eq!(s.line.get_cell(9).unwrap().str(), "×");
        assert_eq!(s.line.get_cell(10).unwrap().str(), " ");
        assert_eq!(
            s.hits[0],
            StripHit {
                kind: StripHitKind::Tab(1),
                x: 0,
                width: 11
            }
        );
        assert_eq!(
            s.hits[1],
            StripHit {
                kind: StripHitKind::Close(1),
                x: 9,
                width: 1
            }
        );
        assert_eq!(
            s.hits[2],
            StripHit {
                kind: StripHitKind::Tab(2),
                x: 11,
                width: 11
            }
        );
        assert_eq!(
            s.hits[3],
            StripHit {
                kind: StripHitKind::Close(2),
                x: 20,
                width: 1
            }
        );
    }

    #[test]
    fn strip_narrow_keeps_visible_tab_with_markers() {
        let tabs: Vec<_> = (0..10)
            .map(|i| tab(i, &format!("t{i}"), i == 6, false))
            .collect();
        let s = layout_strip(&tabs, true, 30, &style());
        assert_eq!(s.line.len(), 30);
        assert_eq!(s.line.get_cell(0).unwrap().str(), "‹");
        assert_eq!(s.line.get_cell(29).unwrap().str(), "›");
        assert_eq!(s.line.get_cell(0).unwrap().attrs(), &style().hidden);
        assert_eq!(s.line.get_cell(29).unwrap().attrs(), &style().hidden);
        // 30 cells hold 3 tabs of MIN_TAB = 8. The window starts
        // (3 - 1) / 2 = 1 tab before the visible tab 6, so tab 6 is the
        // middle one, drawn with its own title.
        assert_eq!(s.line.as_str(), "‹t5   ×  t6   ×  t7   ×      ›");
        let tab_hits: Vec<_> = s
            .hits
            .iter()
            .filter(|h| matches!(h.kind, StripHitKind::Tab(_)))
            .map(|h| (h.kind, h.x, h.width))
            .collect();
        assert_eq!(
            tab_hits,
            vec![
                (StripHitKind::Tab(5), 0, 8),
                (StripHitKind::Tab(6), 8, 8),
                (StripHitKind::Tab(7), 16, 8),
            ]
        );
        for h in &s.hits {
            assert!(h.x + h.width <= 30, "{:?}", h);
        }
    }

    #[test]
    fn strip_ignores_zero_width_graphemes() {
        for title in [
            "a\u{200B}b",
            "a\u{200B}\u{200B}\u{200B}b",
            "a\tb",
            "\u{0301}ab",
            "a\u{200E}b",
            // One grapheme of 8 bytes, which Cell stores on the heap.
            "\u{0301}\u{0301}\u{0301}\u{0301}ab",
        ] {
            for width in [20, 12, 9] {
                let s = layout_strip(
                    &[tab(1, title, true, false), tab(2, "z", false, false)],
                    true,
                    width,
                    &style(),
                );
                assert_eq!(s.line.len(), width, "{:?}", title);
                let closes: Vec<_> = s
                    .hits
                    .iter()
                    .filter(|h| matches!(h.kind, StripHitKind::Close(_)))
                    .collect();
                assert!(!closes.is_empty(), "{:?}", title);
                for h in closes {
                    let c = s.line.get_cell(h.x).unwrap();
                    assert_eq!(c.str(), "×", "{:?} w={}", title, width);
                }
            }
        }
    }

    #[test]
    fn strip_degenerate_inputs_do_not_panic() {
        let s = layout_strip(&[], true, 10, &style());
        assert_eq!(s.line.len(), 10);
        assert!(s.hits.is_empty());
        let s = layout_strip(&[tab(1, "a", true, false)], true, 0, &style());
        assert_eq!(s.line.len(), 0);
        assert!(s.hits.is_empty());
        for w in 1..8 {
            let s = layout_strip(
                &[tab(1, "abc", false, false), tab(2, "d", true, false)],
                true,
                w,
                &style(),
            );
            assert_eq!(s.line.len(), w);
            for h in &s.hits {
                assert!(h.x + h.width <= w);
            }
        }
    }
}
