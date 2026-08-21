//! The `[keybindings]` config section and the pure resolution that turns it
//! (plus each site's built-in defaults) into the keystroke lists every
//! registration site consumes. See [`config::KeybindingsConfig`] for the
//! TOML shape and [`resolve::resolve`] for how a `Config` becomes the
//! effective bindings.

mod bind;
pub mod config;
mod keystrokes;
mod resolve;

pub(crate) use bind::bind_all;
pub use config::KeybindingsConfig;
pub use keystrokes::Keystrokes;
pub use resolve::resolve;

#[cfg(test)]
mod tests {
    use crate::config::Config;

    #[test]
    fn a_default_config_serializes_with_no_keybindings_section_at_all() {
        let text = toml::to_string(&Config::default()).unwrap();
        assert!(
            !text.contains("[keybindings"),
            "a default config must never write a [keybindings.*] table:\n{text}"
        );
        assert!(
            !text.contains("keybindings"),
            "a default config must never mention keybindings at all:\n{text}"
        );
    }

    #[test]
    fn a_default_configs_serialized_form_round_trips_through_load_or_default() {
        let text = toml::to_string(&Config::default()).unwrap();
        let temp = std::env::temp_dir().join(format!(
            "zsql-keybindings-round-trip-test-{}.toml",
            std::process::id()
        ));
        std::fs::write(&temp, &text).unwrap();

        let reloaded = Config::load_or_default(&temp).unwrap();
        std::fs::remove_file(&temp).ok();

        assert_eq!(reloaded, Config::default());
    }
}
