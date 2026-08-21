//! [`parse_keystrokes`]: turn a resolved keystroke list into the
//! `KeybindingKeystroke`s a view matches key events against.

use gpui::{KeybindingKeystroke, Keystroke};

/// Parse each of `keystrokes` into a [`KeybindingKeystroke`], logging and
/// skipping any entry that fails to parse rather than panicking. `config_key`
/// names the resolved config field in the warning, e.g.
/// `"keybindings.parameters_modal.next_field"`.
pub(crate) fn parse_keystrokes(
    keystrokes: &[String],
    config_key: &str,
) -> Vec<KeybindingKeystroke> {
    keystrokes
        .iter()
        .filter_map(|s| match Keystroke::parse(s) {
            Ok(keystroke) => Some(KeybindingKeystroke::from_keystroke(keystroke)),
            Err(error) => {
                tracing::warn!(config_key, keystroke = s.as_str(), %error, "invalid keystroke, ignoring");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_keystrokes;

    #[test]
    fn valid_keystrokes_all_parse() {
        let parsed = parse_keystrokes(&["tab".to_owned(), "shift-tab".to_owned()], "test.key");
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn an_invalid_keystroke_is_skipped_and_does_not_panic() {
        let parsed = parse_keystrokes(&["not-a-key".to_owned()], "test.key");
        assert!(parsed.is_empty());
    }

    #[test]
    fn a_mixed_list_keeps_only_the_valid_entries() {
        let parsed = parse_keystrokes(&["tab".to_owned(), "not-a-key".to_owned()], "test.key");
        assert_eq!(parsed.len(), 1);
    }
}
