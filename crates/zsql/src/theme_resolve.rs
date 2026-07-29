//! Resolves the configured theme name into a [`Theme`] the workspace
//! renders with: a built-in Catppuccin flavor, else a user theme file on
//! disk, else the default dark theme -- never blocking or panicking
//! startup. Also enumerates every selectable theme for the Appearance
//! modal, and applies a chosen theme to the running app.

use std::path::{Path, PathBuf};

use zsql_ui::theme::{
    Colors, Theme, built_in_theme, builtin_theme_names,
    catppuccin::{FRAPPE_NAME, LATTE_NAME, MACCHIATO_NAME, MOCHA_NAME},
};

use crate::config::Config;

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
pub fn resolve(theme_name: &str, themes_dir: Option<&Path>) -> Colors {
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
        return Colors::default();
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
            return Colors::default();
        }
    };

    match serde_json::from_str::<Colors>(&text) {
        Ok(colors) => {
            tracing::info!(theme = theme_name, path = %path.display(), "loaded theme from disk");
            colors
        }
        Err(err) => {
            tracing::warn!(
                theme = theme_name,
                path = %path.display(),
                error = %err,
                "failed to parse theme file; falling back to the default theme"
            );
            Colors::default()
        }
    }
}

/// The JSON theme file `theme_name` resolves to inside `themes_dir`.
fn theme_file_path(themes_dir: &Path, theme_name: &str) -> PathBuf {
    themes_dir.join(format!("{theme_name}.json"))
}

/// A selectable theme's broad visual character, shown as a label on its
/// Appearance-modal card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// A dark-background palette. Every built-in flavor except
    /// [`LATTE_NAME`].
    Dark,
    /// A light-background palette: [`LATTE_NAME`], the only built-in one.
    Light,
    /// A user-supplied theme file discovered on disk, of unknown tone.
    Custom,
}

/// One theme selectable from the Appearance modal: its config-facing name,
/// human-readable display name, resolved colors, and tone label.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeEntry {
    /// The name persisted to [`crate::config::ThemeConfig::name`] and passed
    /// to [`resolve`].
    pub name: String,
    /// A human-readable label for [`Self::name`], shown on its card.
    pub display_name: String,
    /// This theme's resolved color palette.
    pub colors: Colors,
    /// This theme's tone label.
    pub tone: Tone,
}

/// A human-readable label for a config-facing theme name: a built-in's
/// documented display form, or a user theme's file stem title-cased word by
/// word (`"solarized-dark"` -> `"Solarized Dark"`).
#[must_use]
pub(crate) fn display_name_for(name: &str) -> String {
    match name {
        zsql_ui::theme::ZSQL_DARK_NAME => "zsql dark".to_owned(),
        LATTE_NAME => "Catppuccin Latte".to_owned(),
        FRAPPE_NAME => "Catppuccin Frappe".to_owned(),
        MACCHIATO_NAME => "Catppuccin Macchiato".to_owned(),
        MOCHA_NAME => "Catppuccin Mocha".to_owned(),
        other => other
            .split(['-', '_'])
            .filter(|word| !word.is_empty())
            .map(title_case_word)
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// `word` with its first character uppercased and the rest left as-is.
fn title_case_word(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Every theme selectable from the Appearance modal: the five built-ins,
/// always present and listed first in [`builtin_theme_names`] order, then
/// every `<name>.json` file discovered in `themes_dir` (if resolvable), in
/// alphabetical order by name.
///
/// A user theme file whose name collides with a built-in is discovered but
/// never produces its own entry: a built-in name always wins, matching
/// [`resolve`]'s own precedence, so a card in the modal is never shown for a
/// file that `resolve` would never actually read. A file that fails to
/// parse is skipped with a `tracing::warn!` naming the file and the parse
/// error; enumeration itself never fails or panics on a bad file.
#[tracing::instrument(name = "theme_catalog", skip(themes_dir))]
pub fn list_themes(themes_dir: Option<&Path>) -> Vec<ThemeEntry> {
    let mut entries: Vec<ThemeEntry> = builtin_theme_names()
        .iter()
        .map(|&name| ThemeEntry {
            name: name.to_owned(),
            display_name: display_name_for(name),
            colors: built_in_theme(name).unwrap_or_default(),
            tone: if name == LATTE_NAME {
                Tone::Light
            } else {
                Tone::Dark
            },
        })
        .collect();
    tracing::info!(count = entries.len(), "enumerated built-in themes");

    let Some(dir) = themes_dir else {
        tracing::debug!("no themes directory available; only built-ins are selectable");
        return entries;
    };
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        tracing::debug!(dir = %dir.display(), "themes directory unreadable; only built-ins are selectable");
        return entries;
    };

    let mut user_entries = Vec::new();
    for dir_entry in read_dir.filter_map(Result::ok) {
        let path = dir_entry.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if built_in_theme(name).is_some() {
            // A built-in name wins over a same-named file on disk, matching
            // `resolve`'s precedence -- `resolve` would never read this file
            // either, so it must not appear as its own card.
            continue;
        }

        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "failed to read user theme file; skipping");
                continue;
            }
        };
        match serde_json::from_str::<Colors>(&text) {
            Ok(colors) => user_entries.push(ThemeEntry {
                name: name.to_owned(),
                display_name: display_name_for(name),
                colors,
                tone: Tone::Custom,
            }),
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "failed to parse user theme file; skipping");
            }
        }
    }
    user_entries.sort_by(|a, b| a.name.cmp(&b.name));
    tracing::info!(
        count = user_entries.len(),
        "enumerated user themes from disk"
    );

    entries.extend(user_entries);
    entries
}

/// Persist `theme_name` into the config file at `config_path` (loading it
/// first, so no other section is touched), if a path is given. A missing
/// `config_path`, a config that fails to load, or a save that fails to
/// write are all logged via `tracing::warn!` and otherwise ignored: this
/// never returns an error, since a persistence failure must never block or
/// undo the (session-only) live apply that already happened before this
/// runs.
#[tracing::instrument(name = "theme_apply_persist", skip(config_path))]
pub(crate) fn persist_theme_choice(theme_name: &str, config_path: Option<&Path>) {
    let Some(path) = config_path else {
        tracing::warn!(
            theme = theme_name,
            "no config path is resolvable; the theme choice will not persist across restarts"
        );
        return;
    };

    let mut cfg = match Config::load_or_default(path) {
        Ok(cfg) => cfg,
        Err(err) => {
            tracing::warn!(
                theme = theme_name,
                path = %path.display(),
                error = %err,
                "failed to load config while persisting theme choice; the theme choice will not persist"
            );
            return;
        }
    };
    theme_name.clone_into(&mut cfg.theme.name);
    if let Err(err) = cfg.save(path) {
        tracing::warn!(
            theme = theme_name,
            path = %path.display(),
            error = %err,
            "failed to persist theme choice"
        );
    }
}

/// Swap the active [`Theme`] global to `colors` and refresh every open
/// window so it repaints with the new palette on the next frame. This is the
/// session-only live apply: it neither resolves a name nor persists a choice,
/// so a caller applies exactly the palette it already holds (e.g. the one the
/// Appearance modal enumerated for a card), and the applied palette can never
/// disagree with the swatch that was shown. Persistence is a separate step
/// ([`persist_theme_choice`]) a caller runs when it commits a choice.
#[tracing::instrument(name = "theme_apply", skip(colors, cx))]
pub fn apply_colors(colors: Colors, cx: &mut gpui::App) {
    if cx.try_global::<Theme>().is_some() {
        cx.global_mut::<Theme>().colors = colors;
    } else {
        cx.set_global(Theme {
            colors,
            ..Theme::default()
        });
    }
    cx.refresh_windows();
    tracing::info!("theme applied to every open window");
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use gpui::TestAppContext;
    use zsql_ui::theme::catppuccin::{
        FRAPPE_NAME, LATTE_NAME, MACCHIATO_NAME, MOCHA_NAME, frappe, latte, macchiato, mocha,
    };
    use zsql_ui::theme::{ActiveTheme, Colors, Theme, ZSQL_DARK_NAME, builtin_theme_names};

    use super::{Tone, list_themes, resolve};

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
            if let Err(err) = std::fs::remove_dir_all(&self.0) {
                tracing::debug!(dir = %self.0.display(), %err, "failed to clean up test themes dir");
            }
        }
    }

    #[test]
    fn each_built_in_flavor_name_resolves_to_its_own_colors_without_touching_disk() {
        assert_eq!(resolve(LATTE_NAME, None), latte());
        assert_eq!(resolve(FRAPPE_NAME, None), frappe());
        assert_eq!(resolve(MACCHIATO_NAME, None), macchiato());
        assert_eq!(resolve(MOCHA_NAME, None), mocha());
    }

    #[test]
    fn an_unknown_name_with_no_themes_dir_falls_back_to_the_default_theme() {
        assert_eq!(resolve("nonexistent", None), Colors::default());
    }

    #[test]
    fn a_missing_theme_file_falls_back_to_the_default_theme() {
        let dir = TestThemesDir::new("missing-file");
        assert_eq!(resolve("nonexistent", Some(&dir.0)), Colors::default());
    }

    #[test]
    fn malformed_json_falls_back_to_the_default_theme() {
        let dir = TestThemesDir::new("malformed-json");
        dir.write("broken", "{ this is not json");
        assert_eq!(resolve("broken", Some(&dir.0)), Colors::default());
    }

    #[test]
    fn an_unknown_color_role_key_falls_back_to_the_default_theme() {
        let dir = TestThemesDir::new("unknown-key");
        dir.write("typo", "{\"accentt\": \"#33c2ac\"}");
        assert_eq!(resolve("typo", Some(&dir.0)), Colors::default());
    }

    #[test]
    fn a_malformed_hex_value_falls_back_to_the_default_theme() {
        let dir = TestThemesDir::new("bad-hex");
        dir.write("bad-hex", "{\"accent\": \"not-a-color\"}");
        assert_eq!(resolve("bad-hex", Some(&dir.0)), Colors::default());
    }

    #[test]
    fn a_valid_partial_theme_file_overrides_only_the_fields_it_sets() {
        let dir = TestThemesDir::new("partial");
        dir.write("warmer", "{\"accent\": \"#e0a33f\"}");

        let theme = resolve("warmer", Some(&dir.0));

        let expected = zsql_ui::theme::Colors {
            accent: 0xe0_a3_3f,
            ..Theme::default().colors
        };

        assert_eq!(theme, expected);
    }

    #[test]
    fn a_built_in_flavor_name_wins_over_a_same_named_file_on_disk() {
        let dir = TestThemesDir::new("shadowed");
        dir.write(MOCHA_NAME, "{\"accent\": \"#000000\"}");

        assert_eq!(resolve(MOCHA_NAME, Some(&dir.0)), mocha());
    }

    // -- list_themes ----------------------------------------------------

    #[test]
    fn list_themes_with_no_themes_dir_lists_exactly_the_five_builtins_in_order() {
        let entries = list_themes(None);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, builtin_theme_names());
    }

    #[test]
    fn list_themes_labels_catppuccin_latte_light_and_every_other_builtin_dark() {
        let entries = list_themes(None);
        for entry in &entries {
            let expected = if entry.name == LATTE_NAME {
                Tone::Light
            } else {
                Tone::Dark
            };
            assert_eq!(
                entry.tone, expected,
                "{} has the wrong tone label",
                entry.name
            );
        }
    }

    #[test]
    fn list_themes_scans_the_directory_and_skips_a_malformed_file_but_keeps_a_valid_one() {
        let dir = TestThemesDir::new("scan-mixed");
        dir.write("nord", "{\"accent\": \"#88c0d0\"}");
        dir.write("broken", "{ not json");

        let entries = list_themes(Some(&dir.0));

        assert!(
            entries.iter().any(|e| e.name == "nord"),
            "the valid user theme file must be discovered"
        );
        assert!(
            !entries.iter().any(|e| e.name == "broken"),
            "the malformed user theme file must be skipped, not surfaced as a broken card"
        );
    }

    #[test]
    fn list_themes_labels_every_discovered_user_theme_custom() {
        let dir = TestThemesDir::new("scan-custom-tone");
        dir.write("nord", "{\"accent\": \"#88c0d0\"}");

        let entries = list_themes(Some(&dir.0));
        let nord = entries
            .iter()
            .find(|e| e.name == "nord")
            .expect("the user theme must be discovered");
        assert_eq!(nord.tone, Tone::Custom);
    }

    #[test]
    fn list_themes_never_produces_a_second_entry_for_a_builtin_shadowed_on_disk() {
        let dir = TestThemesDir::new("scan-shadowed");
        dir.write(MOCHA_NAME, "{\"accent\": \"#000000\"}");

        let entries = list_themes(Some(&dir.0));
        let matching: Vec<_> = entries.iter().filter(|e| e.name == MOCHA_NAME).collect();
        assert_eq!(
            matching.len(),
            1,
            "a built-in name must win over a same-named file on disk, matching resolve()'s \
             precedence, rather than producing two cards"
        );
        assert_eq!(matching[0].colors, mocha());
    }

    #[test]
    fn list_themes_sorts_user_themes_alphabetically_after_the_builtins() {
        let dir = TestThemesDir::new("scan-sort");
        dir.write("zeta", "{}");
        dir.write("alpha", "{}");

        let entries = list_themes(Some(&dir.0));
        let user_names: Vec<&str> = entries
            .iter()
            .skip(builtin_theme_names().len())
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(user_names, ["alpha", "zeta"]);
    }

    #[test]
    fn list_themes_ignores_non_json_files_in_the_themes_dir() {
        let dir = TestThemesDir::new("scan-non-json");
        std::fs::write(dir.0.join("readme.txt"), "not a theme").expect("setup write must succeed");

        let entries = list_themes(Some(&dir.0));
        assert!(!entries.iter().any(|e| e.name == "readme"));
    }

    #[test]
    fn display_name_for_known_builtins_matches_the_documented_labels() {
        assert_eq!(super::display_name_for(ZSQL_DARK_NAME), "zsql dark");
        assert_eq!(super::display_name_for(LATTE_NAME), "Catppuccin Latte");
        assert_eq!(super::display_name_for(FRAPPE_NAME), "Catppuccin Frappe");
        assert_eq!(
            super::display_name_for(MACCHIATO_NAME),
            "Catppuccin Macchiato"
        );
        assert_eq!(super::display_name_for(MOCHA_NAME), "Catppuccin Mocha");
    }

    #[test]
    fn display_name_for_a_user_theme_title_cases_each_hyphenated_word() {
        assert_eq!(super::display_name_for("nord"), "Nord");
        assert_eq!(super::display_name_for("solarized-dark"), "Solarized Dark");
    }

    #[test]
    fn list_themes_returns_only_the_builtins_when_the_themes_dir_is_unreadable() {
        // A path that is a regular file (not a directory) makes `read_dir`
        // fail; enumeration must still yield exactly the built-ins and never
        // panic, matching the guarantee that the built-ins survive an
        // unreadable themes directory.
        let dir = TestThemesDir::new("unreadable-themes-dir");
        let not_a_dir = dir.0.join("regular-file");
        std::fs::write(&not_a_dir, "not a directory")
            .expect("must be able to seed the blocking file");

        let entries = list_themes(Some(&not_a_dir));
        assert_eq!(entries.len(), builtin_theme_names().len());
        for (entry, &name) in entries.iter().zip(builtin_theme_names()) {
            assert_eq!(entry.name, name);
        }
    }

    // -- persist_theme_choice ---------------------------------------------

    #[test]
    fn persist_theme_choice_with_no_config_path_does_not_panic() {
        super::persist_theme_choice(MOCHA_NAME, None);
    }

    #[test]
    fn persist_theme_choice_writes_the_chosen_theme_name_to_a_fresh_config_file() {
        let config_dir = TestThemesDir::new("persist-fresh-config-dir");
        let config_path = config_dir.0.join("config.toml");

        super::persist_theme_choice(MOCHA_NAME, Some(&config_path));

        let reloaded = crate::config::Config::load_or_default(&config_path)
            .expect("the freshly written config must load back");
        assert_eq!(reloaded.theme.name, MOCHA_NAME);
    }

    #[test]
    fn persist_theme_choice_preserves_every_other_section_of_an_existing_config() {
        let config_dir = TestThemesDir::new("persist-preserve-config-dir");
        let config_path = config_dir.0.join("config.toml");

        let mut existing = crate::config::Config::default();
        existing.query.max_result_rows = 4_242;
        existing
            .save(&config_path)
            .expect("seeding the config file must succeed");

        super::persist_theme_choice(LATTE_NAME, Some(&config_path));

        let reloaded = crate::config::Config::load_or_default(&config_path)
            .expect("the updated config must load back");
        assert_eq!(reloaded.theme.name, LATTE_NAME);
        assert_eq!(
            reloaded.query.max_result_rows, 4_242,
            "persisting a theme choice must not clobber unrelated config sections"
        );
    }

    #[test]
    fn persist_theme_choice_does_not_panic_when_the_existing_config_is_malformed() {
        let config_dir = TestThemesDir::new("persist-malformed-config-dir");
        let config_path = config_dir.0.join("config.toml");
        let malformed = "this = is not [valid toml";
        std::fs::write(&config_path, malformed)
            .expect("must be able to seed a malformed config file");

        // A config that fails to load is logged and left untouched, never a
        // panic and never silently overwritten, so a bad file neither blocks
        // the live apply nor destroys the user's (recoverable) config.
        super::persist_theme_choice(MOCHA_NAME, Some(&config_path));

        assert_eq!(
            std::fs::read_to_string(&config_path).expect("the config file must still exist"),
            malformed,
            "a config that fails to load must be left byte-for-byte untouched"
        );
    }

    // -- apply_colors -------------------------------------------------------

    #[gpui::test]
    fn apply_colors_swaps_the_already_set_global_palette(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(Theme {
                colors: Colors::default(),
                ..Theme::default()
            });
        });

        cx.update(|cx| {
            super::apply_colors(mocha(), cx);
        });

        cx.update(|cx| {
            assert_eq!(cx.theme().colors, mocha());
        });
    }

    #[gpui::test]
    fn apply_colors_sets_the_global_when_none_was_set_before(cx: &mut TestAppContext) {
        cx.update(|cx| {
            super::apply_colors(latte(), cx);
        });

        cx.update(|cx| {
            assert_eq!(cx.theme().colors, latte());
        });
    }
}
