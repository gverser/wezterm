//! Rules for colouring keywords in terminal output.
use crate::color::ColorSpec;
use termwiz::color::AnsiColor;
use wezterm_dynamic::{FromDynamic, ToDynamic};

/// Colour the text matched by `regex` (Rust `regex` syntax), wherever the
/// program did not choose a colour of its own.
#[derive(Debug, Clone, PartialEq, FromDynamic, ToDynamic)]
pub struct KeywordHighlightRule {
    pub regex: String,
    pub color: ColorSpec,
    #[dynamic(default)]
    pub bold: bool,
}

/// The default rules, after MobaXterm's keyword highlighting. Earlier rules
/// win where two match the same text.
///
/// The colours are palette slots rather than RGB values, so that they follow
/// the colour scheme.
pub fn default_keyword_highlight_rules() -> Vec<KeywordHighlightRule> {
    let rule = |regex: &str, color: AnsiColor| KeywordHighlightRule {
        regex: regex.to_string(),
        color: ColorSpec::AnsiColor(color),
        bold: false,
    };
    vec![
        rule(
            r"(?i)\b(?:errors?|fail(?:s|ed|ure)?|fatal|critical|denied|refused|panic(?:ked)?|exceptions?|abort(?:ed)?|unreachable)\b",
            AnsiColor::Red,
        ),
        rule(
            r"(?i)\b(?:warn(?:s|ing|ings)?|deprecated)\b",
            AnsiColor::Yellow,
        ),
        rule(
            r"(?i)\b(?:ok|success(?:ful(?:ly)?)?|succeeded|done|passed|complete(?:d)?)\b",
            AnsiColor::Lime,
        ),
        rule(
            r"\b(?:(?:25[0-5]|2[0-4]\d|1?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|1?\d?\d)(?::\d{1,5})?\b",
            AnsiColor::Aqua,
        ),
    ]
}
