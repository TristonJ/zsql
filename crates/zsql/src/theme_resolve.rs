//! Resolves the configured theme name into a [`Theme`] the workspace
//! renders with: a built-in Catppuccin flavor, else a user theme file on
//! disk, else the default dark theme -- never blocking or panicking
//! startup.

use std::path::{Path, PathBuf};

use zsql_ui::theme::{Colors, Theme, built_in_theme};

/// Resolve `theme_name` into the [`Theme`] to render with.
///
/// Tries, in order: a built-in Catppuccin flavor matched by name; a JSON
/// file named `<theme_name>.json` inside `themes_dir` (if a themes
/// directory was resolvable); and finally [`Theme::default`]. A missing
/// `themes_dir`, a missing or unreadable theme file, malformed JSON, an
/// unknown color-role key, or a malformed hex value all fall back to the
/// default and are logged via `tracing::warn!` naming the theme and the
/// failure reason -- this never panics and never blocks startup on a bad
/// theme.
#[tracing::instrument(name = "theme_resolve", skip(themes_dir))]
pub fn resolve(theme_name: &str, themes_dir: Option<&Path>) -> Theme {
    if let Some(theme) = built_in_theme(theme_name) {
        tracing::info!(theme = theme_name, "resolved a built-in theme");
        return theme;
    }

    let Some(dir) = themes_dir else {
        tracing::warn!(
            theme = theme_name,
            "no themes directory is available and the name does not match a built-in flavor; \
             falling back to the default theme"
        );
        return Theme::default();
    };

    let path = theme_file_path(dir, theme_name);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            tracing::warn!(
                theme = theme_name,
                path = %path.display(),
                error = %err,
                "failed to read theme file; falling back to the default theme"
            );
            return Theme::default();
        }
    };

    match serde_json::from_str::<Colors>(&text) {
        Ok(colors) => {
            tracing::info!(theme = theme_name, path = %path.display(), "loaded theme from disk");
            Theme { colors }
        }
        Err(err) => {
            tracing::warn!(
                theme = theme_name,
                path = %path.display(),
                error = %err,
                "failed to parse theme file; falling back to the default theme"
            );
            Theme::default()
        }
    }
}

/// The JSON theme file `theme_name` resolves to inside `themes_dir`.
fn theme_file_path(themes_dir: &Path, theme_name: &str) -> PathBuf {
    themes_dir.join(format!("{theme_name}.json"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use zsql_ui::theme::Theme;
    use zsql_ui::theme::catppuccin::{
        FRAPPE_NAME, LATTE_NAME, MACCHIATO_NAME, MOCHA_NAME, frappe, latte, macchiato, mocha,
    };

    use super::resolve;

    /// A themes directory owned exclusively by one test, removed (with any
    /// files written into it) on drop.
    struct TestThemesDir(PathBuf);

    impl TestThemesDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "zsql-theme-resolve-test-{label}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("must be able to create a test themes dir");
            Self(dir)
        }

        fn write(&self, theme_name: &str, contents: &str) {
            std::fs::write(self.0.join(format!("{theme_name}.json")), contents)
                .expect("must be able to write a test theme file");
        }
    }

    impl Drop for TestThemesDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn each_built_in_flavor_name_resolves_to_its_own_colors_without_touching_disk() {
        assert_eq!(resolve(LATTE_NAME, None).colors, latte());
        assert_eq!(resolve(FRAPPE_NAME, None).colors, frappe());
        assert_eq!(resolve(MACCHIATO_NAME, None).colors, macchiato());
        assert_eq!(resolve(MOCHA_NAME, None).colors, mocha());
    }

    #[test]
    fn an_unknown_name_with_no_themes_dir_falls_back_to_the_default_theme() {
        assert_eq!(resolve("nonexistent", None), Theme::default());
    }

    #[test]
    fn a_missing_theme_file_falls_back_to_the_default_theme() {
        let dir = TestThemesDir::new("missing-file");
        assert_eq!(resolve("nonexistent", Some(&dir.0)), Theme::default());
    }

    #[test]
    fn malformed_json_falls_back_to_the_default_theme() {
        let dir = TestThemesDir::new("malformed-json");
        dir.write("broken", "{ this is not json");
        assert_eq!(resolve("broken", Some(&dir.0)), Theme::default());
    }

    #[test]
    fn an_unknown_color_role_key_falls_back_to_the_default_theme() {
        let dir = TestThemesDir::new("unknown-key");
        dir.write("typo", "{\"accentt\": \"#33c2ac\"}");
        assert_eq!(resolve("typo", Some(&dir.0)), Theme::default());
    }

    #[test]
    fn a_malformed_hex_value_falls_back_to_the_default_theme() {
        let dir = TestThemesDir::new("bad-hex");
        dir.write("bad-hex", "{\"accent\": \"not-a-color\"}");
        assert_eq!(resolve("bad-hex", Some(&dir.0)), Theme::default());
    }

    #[test]
    fn a_valid_partial_theme_file_overrides_only_the_fields_it_sets() {
        let dir = TestThemesDir::new("partial");
        dir.write("warmer", "{\"accent\": \"#e0a33f\"}");

        let theme = resolve("warmer", Some(&dir.0));

        let expected = Theme {
            colors: zsql_ui::theme::Colors {
                accent: 0xe0_a3_3f,
                ..Theme::default().colors
            },
        };
        assert_eq!(theme, expected);
    }

    #[test]
    fn a_built_in_flavor_name_wins_over_a_same_named_file_on_disk() {
        let dir = TestThemesDir::new("shadowed");
        dir.write(MOCHA_NAME, "{\"accent\": \"#000000\"}");

        assert_eq!(resolve(MOCHA_NAME, Some(&dir.0)).colors, mocha());
    }
}
