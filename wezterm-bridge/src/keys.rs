use wezterm_term::{KeyCode, KeyModifiers};

/// Turn a key name into the key it names.
///
/// Text alone cannot drive a terminal. Every selection list, confirmation
/// box and menu a CLI puts up wants arrows and Enter, and a remote that can
/// only paste characters is stuck the moment one appears -- with no way out,
/// since the pane it is stuck in has no window attached either.
///
/// An unrecognised name is refused rather than guessed at: sending the wrong
/// key into a confirmation dialog answers a question nobody read.
pub fn parse_key(name: &str) -> Option<KeyCode> {
    let lowered = name.trim().to_ascii_lowercase();
    Some(match lowered.as_str() {
        "up" | "uparrow" => KeyCode::UpArrow,
        "down" | "downarrow" => KeyCode::DownArrow,
        "left" | "leftarrow" => KeyCode::LeftArrow,
        "right" | "rightarrow" => KeyCode::RightArrow,
        "enter" | "return" | "cr" => KeyCode::Enter,
        "escape" | "esc" => KeyCode::Escape,
        "tab" => KeyCode::Tab,
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "space" => KeyCode::Char(' '),
        other => {
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => return None,
            }
        }
    })
}

/// Collect modifier names into a set.
///
/// An unknown name is dropped rather than rejected: the key itself is what
/// matters, and refusing the whole request over a typo in a modifier would
/// leave the caller stuck for no good reason.
pub fn parse_modifiers(names: &[String]) -> KeyModifiers {
    let mut mods = KeyModifiers::NONE;
    for name in names {
        match name.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= KeyModifiers::CTRL,
            "alt" | "option" => mods |= KeyModifiers::ALT,
            "shift" => mods |= KeyModifiers::SHIFT,
            "super" | "cmd" | "win" => mods |= KeyModifiers::SUPER,
            _ => {}
        }
    }
    mods
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn arrows_by_name() {
        assert_eq!(parse_key("up"), Some(KeyCode::UpArrow));
        assert_eq!(parse_key("down"), Some(KeyCode::DownArrow));
        assert_eq!(parse_key("left"), Some(KeyCode::LeftArrow));
        assert_eq!(parse_key("right"), Some(KeyCode::RightArrow));
    }

    #[test]
    fn names_are_case_insensitive() {
        assert_eq!(parse_key("Down"), Some(KeyCode::DownArrow));
        assert_eq!(parse_key("ESCAPE"), Some(KeyCode::Escape));
    }

    #[test]
    fn the_common_aliases_work() {
        assert_eq!(parse_key("esc"), Some(KeyCode::Escape));
        assert_eq!(parse_key("return"), Some(KeyCode::Enter));
        assert_eq!(parse_key("cr"), Some(KeyCode::Enter));
    }

    #[test]
    fn a_single_character_is_that_character() {
        assert_eq!(parse_key("a"), Some(KeyCode::Char('a')));
        assert_eq!(parse_key("1"), Some(KeyCode::Char('1')));
    }

    #[test]
    fn a_name_that_means_nothing_is_refused() {
        assert_eq!(
            parse_key("banana"),
            None,
            "guessing would send something the caller did not ask for"
        );
        assert_eq!(parse_key(""), None);
    }

    #[test]
    fn modifiers_combine() {
        let mods = parse_modifiers(&["ctrl".to_string(), "shift".to_string()]);
        assert!(mods.contains(KeyModifiers::CTRL));
        assert!(mods.contains(KeyModifiers::SHIFT));
        assert!(!mods.contains(KeyModifiers::ALT));
    }

    #[test]
    fn no_modifiers_is_none() {
        assert_eq!(parse_modifiers(&[]), KeyModifiers::NONE);
    }

    #[test]
    fn an_unknown_modifier_is_ignored_rather_than_failing() {
        let mods = parse_modifiers(&["ctrl".to_string(), "hyper".to_string()]);
        assert!(mods.contains(KeyModifiers::CTRL));
    }
}
