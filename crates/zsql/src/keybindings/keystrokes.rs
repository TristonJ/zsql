//! [`Keystrokes`]: the TOML-facing shape of a keybinding config value.

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One or more keystrokes bound to a single action, in `gpui` keystroke
/// syntax (e.g. `"secondary-c"`). Deserializes from either a bare string or
/// an array of strings, so a config author never has to wrap a single
/// keystroke in brackets. Serializing a single-element list re-emits a bare
/// string, keeping a saved config file as close to hand-written as possible.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Keystrokes(pub Vec<String>);

impl Serialize for Keystrokes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.0.as_slice() {
            [single] => serializer.serialize_str(single),
            many => many.serialize(serializer),
        }
    }
}

struct KeystrokesVisitor;

impl<'de> Visitor<'de> for KeystrokesVisitor {
    type Value = Keystrokes;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a keystroke string or an array of keystroke strings")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(Keystrokes(vec![value.to_owned()]))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: de::SeqAccess<'de>,
    {
        let mut entries = Vec::new();
        while let Some(entry) = seq.next_element::<String>()? {
            entries.push(entry);
        }
        Ok(Keystrokes(entries))
    }
}

impl<'de> Deserialize<'de> for Keystrokes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(KeystrokesVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::Keystrokes;

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Wrapper {
        value: Keystrokes,
    }

    #[test]
    fn a_bare_string_deserializes_to_a_one_element_vec() {
        let parsed: Wrapper = toml::from_str("value = \"secondary-c\"").unwrap();
        assert_eq!(parsed.value.0, vec!["secondary-c".to_owned()]);
    }

    #[test]
    fn an_array_deserializes_to_the_matching_vec() {
        let parsed: Wrapper = toml::from_str("value = [\"cmd-enter\", \"ctrl-enter\"]").unwrap();
        assert_eq!(
            parsed.value.0,
            vec!["cmd-enter".to_owned(), "ctrl-enter".to_owned()]
        );
    }

    #[test]
    fn an_empty_array_deserializes_to_an_empty_vec() {
        let parsed: Wrapper = toml::from_str("value = []").unwrap();
        assert!(parsed.value.0.is_empty());
    }

    #[test]
    fn a_single_element_vec_serializes_back_to_a_bare_string() {
        let wrapper = Wrapper {
            value: Keystrokes(vec!["secondary-c".to_owned()]),
        };
        let text = toml::to_string(&wrapper).unwrap();
        assert_eq!(text.trim(), "value = \"secondary-c\"");
    }

    #[test]
    fn a_multi_element_vec_serializes_as_an_array() {
        let wrapper = Wrapper {
            value: Keystrokes(vec!["cmd-enter".to_owned(), "ctrl-enter".to_owned()]),
        };
        let text = toml::to_string(&wrapper).unwrap();
        assert_eq!(text.trim(), "value = [\"cmd-enter\", \"ctrl-enter\"]");
    }

    #[test]
    fn round_tripping_a_bare_string_through_serialize_and_deserialize_preserves_it() {
        let original = Wrapper {
            value: Keystrokes(vec!["secondary-f".to_owned()]),
        };
        let text = toml::to_string(&original).unwrap();
        let reparsed: Wrapper = toml::from_str(&text).unwrap();
        assert_eq!(reparsed.value.0, original.value.0);
    }
}
