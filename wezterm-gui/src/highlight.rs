//! Keyword highlighting: colour words like "error" and "ok" in terminal
//! output, the way MobaXterm's "syntax highlighting" does.
//!
//! This only ever changes what is drawn. The pane's own lines are left alone,
//! so copying, searching and quick select see the text exactly as the program
//! wrote it; the renderer is handed a recoloured copy instead.

use config::KeywordHighlightRule;
use termwiz::cell::Intensity;
use termwiz::color::ColorAttribute;
use wezterm_term::Line;

pub struct CompiledRule {
    regex: regex::Regex,
    color: ColorAttribute,
    bold: bool,
}

/// Compile the configured rules. One that doesn't compile is reported and
/// left out rather than taking the others down with it.
pub fn compile_rules(rules: &[KeywordHighlightRule]) -> Vec<CompiledRule> {
    rules
        .iter()
        .filter_map(|rule| match regex::Regex::new(&rule.regex) {
            Ok(regex) => Some(CompiledRule {
                regex,
                color: rule.color.into(),
                bold: rule.bold,
            }),
            Err(err) => {
                log::error!(
                    "keyword_highlight_rules: ignoring {:?}: {err:#}",
                    rule.regex
                );
                None
            }
        })
        .collect()
}

/// A copy of `line` with the rules' colours applied, or None if nothing in it
/// matched.
///
/// Only cells still in the default foreground colour and not in reverse video
/// are touched: whatever a program coloured itself stays as it is, and a
/// reversed cell's foreground is really its background. Where rules overlap,
/// the earlier one wins.
pub fn highlight_line(line: &Line, rules: &[CompiledRule]) -> Option<Line> {
    if rules.is_empty() {
        return None;
    }
    let text = line.as_str();

    // The byte offset in `text` at which each visible cell starts. `as_str`
    // joins exactly these cells' text, so the two line up.
    let mut starts = vec![];
    let mut offset = 0;
    for cell in line.visible_cells() {
        starts.push((offset, cell.cell_index()));
        offset += cell.str().len();
    }

    // For each cell, the first rule that covers it.
    let mut assigned: Vec<Option<usize>> = vec![None; starts.len()];
    for (r, rule) in rules.iter().enumerate() {
        for m in rule.regex.find_iter(&text) {
            for (slot, (start, _)) in starts.iter().enumerate() {
                if *start >= m.start() && *start < m.end() && assigned[slot].is_none() {
                    assigned[slot] = Some(r);
                }
            }
        }
    }
    if assigned.iter().all(|a| a.is_none()) {
        return None;
    }

    let mut out = line.clone();
    let mut changed = false;
    {
        let cells = out.cells_mut_for_attr_changes_only();
        for (slot, rule) in assigned.into_iter().enumerate() {
            let Some(rule) = rule else { continue };
            let cell_index = starts[slot].1;
            let Some(cell) = cells.get_mut(cell_index) else {
                continue;
            };
            let attrs = cell.attrs_mut();
            if attrs.foreground() != ColorAttribute::Default || attrs.reverse() {
                continue;
            }
            attrs.set_foreground(rules[rule].color);
            if rules[rule].bold {
                attrs.set_intensity(Intensity::Bold);
            }
            changed = true;
        }
    }
    // The copy carries the original's cached render state; drop it so that
    // nothing mistakes one for the other.
    out.clear_appdata();
    changed.then_some(out)
}

#[cfg(test)]
mod test {
    use super::*;
    use config::{ColorSpec, KeywordHighlightRule};
    use termwiz::cell::{Cell, CellAttributes};
    use termwiz::color::AnsiColor;
    use termwiz::surface::SEQ_ZERO;

    fn rules() -> Vec<CompiledRule> {
        compile_rules(&config::default_keyword_highlight_rules())
    }

    fn line(text: &str) -> Line {
        Line::from_text(text, &CellAttributes::default(), SEQ_ZERO, None)
    }

    fn fg_at(line: &Line, idx: usize) -> ColorAttribute {
        line.get_cell(idx).unwrap().attrs().foreground()
    }

    fn colored_range(line: &Line, color: ColorAttribute) -> Vec<usize> {
        line.visible_cells()
            .filter(|c| c.attrs().foreground() == color)
            .map(|c| c.cell_index())
            .collect()
    }

    #[test]
    fn default_rules_all_compile() {
        assert_eq!(rules().len(), 4);
    }

    #[test]
    fn error_word_gets_red() {
        let hl = highlight_line(&line("build ERROR here"), &rules()).unwrap();
        assert_eq!(
            colored_range(&hl, ColorAttribute::PaletteIndex(9)),
            (6..11).collect::<Vec<_>>()
        );
        assert_eq!(fg_at(&hl, 0), ColorAttribute::Default);
        assert_eq!(fg_at(&hl, 12), ColorAttribute::Default);
    }

    #[test]
    fn nothing_to_color_returns_none() {
        assert!(highlight_line(&line("plain text"), &rules()).is_none());
    }

    #[test]
    fn explicit_color_is_kept() {
        let mut blue = CellAttributes::default();
        blue.set_foreground(ColorAttribute::PaletteIndex(4));
        let cells: Vec<Cell> = "error"
            .chars()
            .map(|c| Cell::new(c, blue.clone()))
            .collect();
        let l = Line::from_cells(cells, SEQ_ZERO);
        assert!(highlight_line(&l, &rules()).is_none());
    }

    #[test]
    fn reverse_cells_are_skipped() {
        let mut rev = CellAttributes::default();
        rev.set_reverse(true);
        let cells: Vec<Cell> = "failed"
            .chars()
            .map(|c| Cell::new(c, rev.clone()))
            .collect();
        let l = Line::from_cells(cells, SEQ_ZERO);
        assert!(highlight_line(&l, &rules()).is_none());
    }

    #[test]
    fn substring_does_not_match() {
        assert!(highlight_line(&line("terrorist okay"), &rules()).is_none());
    }

    #[test]
    fn ipv4_with_port() {
        let hl = highlight_line(&line("connect 10.0.0.5:22 ok"), &rules()).unwrap();
        assert_eq!(
            colored_range(&hl, ColorAttribute::PaletteIndex(14)),
            (8..19).collect::<Vec<_>>()
        );
        assert_eq!(
            colored_range(&hl, ColorAttribute::PaletteIndex(10)),
            vec![20, 21]
        );
    }

    #[test]
    fn wide_chars_map_to_right_cells() {
        // 中 and 文 take two cells each, then a space: "error" starts at cell 5.
        let hl = highlight_line(&line("中文 error"), &rules()).unwrap();
        assert_eq!(
            colored_range(&hl, ColorAttribute::PaletteIndex(9)),
            (5..10).collect::<Vec<_>>()
        );
    }

    #[test]
    fn first_rule_wins() {
        let rules = compile_rules(&[
            KeywordHighlightRule {
                regex: "abc".to_string(),
                color: ColorSpec::AnsiColor(AnsiColor::Red),
                bold: false,
            },
            KeywordHighlightRule {
                regex: "bcd".to_string(),
                color: ColorSpec::AnsiColor(AnsiColor::Lime),
                bold: true,
            },
        ]);
        let hl = highlight_line(&line("abcd"), &rules).unwrap();
        assert_eq!(fg_at(&hl, 1), ColorAttribute::PaletteIndex(9));
        assert_eq!(fg_at(&hl, 2), ColorAttribute::PaletteIndex(9));
        assert_eq!(fg_at(&hl, 3), ColorAttribute::PaletteIndex(10));
        assert_eq!(
            hl.get_cell(3).unwrap().attrs().intensity(),
            termwiz::cell::Intensity::Bold
        );
        assert_eq!(
            hl.get_cell(0).unwrap().attrs().intensity(),
            termwiz::cell::Intensity::Normal
        );
    }

    #[test]
    fn bad_regex_is_skipped() {
        let rules = compile_rules(&[KeywordHighlightRule {
            regex: "(unclosed".to_string(),
            color: ColorSpec::AnsiColor(AnsiColor::Red),
            bold: false,
        }]);
        assert!(rules.is_empty());
    }

    #[test]
    fn original_line_untouched() {
        let original = line("fatal: denied");
        let before = original.clone();
        let hl = highlight_line(&original, &rules()).unwrap();
        assert_eq!(original.as_str(), before.as_str());
        for (a, b) in original.visible_cells().zip(before.visible_cells()) {
            assert_eq!(a.attrs(), b.attrs());
        }
        assert_eq!(hl.as_str(), original.as_str());
    }
}
