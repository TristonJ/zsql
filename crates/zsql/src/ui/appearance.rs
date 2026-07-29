//! The Appearance modal: a grid of every selectable theme (see
//! [`crate::theme_resolve::list_themes`]), each card a mini zsql preview
//! painted in that theme's own colors. Selecting a card applies the theme
//! live to every open window without closing the modal (see
//! [`crate::theme_resolve::apply_colors`]); the choice is persisted once, on
//! dismiss.

use std::path::PathBuf;

use gpui::{Context, FocusHandle, KeyDownEvent, Window};

use crate::theme_resolve::{self, ThemeEntry};

mod card;
mod render;

/// The Appearance modal's state: the enumerated theme list, which card is
/// focused/checked, and whether the modal overlay is currently mounted.
pub struct AppearanceModalView {
    open: bool,
    modal_focus: FocusHandle,
    themes: Vec<ThemeEntry>,
    /// The config-facing name of the currently active theme -- kept here
    /// (rather than derived from the global palette, which carries no name)
    /// as the single source of truth the status-bar trigger and this
    /// modal's ACTIVE pill both read.
    active_name: String,
    /// Index into `themes` of the checked/keyboard-focused card.
    focused_index: usize,
    /// Parallel to `themes`: one focus handle per card, so arrow-key
    /// navigation can move real window focus and each card has a stable,
    /// unique element id to track it by.
    card_focus_handles: Vec<FocusHandle>,
    /// Where user theme files are discovered from, rescanned every time the
    /// modal opens (see [`Self::open`]) so a file dropped in while the app
    /// was already running becomes selectable.
    themes_dir: Option<PathBuf>,
    /// Where the active theme's name is persisted on dismiss (typically
    /// [`crate::config::Config::default_path`]), via
    /// [`theme_resolve::persist_theme_choice`] from [`Self::close`]. `None`
    /// disables persistence for the session (the live apply still happens) --
    /// tests inject their own temp path here rather than persisting to a
    /// developer's real config file.
    config_path: Option<PathBuf>,
    /// The theme name currently written to `config_path`. Tracked so
    /// persistence can skip a redundant rewrite when the choice has not
    /// changed since the modal opened -- merely opening and dismissing the
    /// modal must not reformat (or strip comments from) the user's config.
    persisted_name: String,
}

impl AppearanceModalView {
    /// Build a modal over the theme currently applied (`active_theme_name`,
    /// typically `cfg.theme.name`), enumerating themes from `themes_dir`
    /// (typically [`crate::config::Config::themes_dir`]) and persisting a
    /// selection to `config_path` (typically
    /// [`crate::config::Config::default_path`]). Starts closed.
    #[must_use]
    pub fn new(
        active_theme_name: String,
        themes_dir: Option<PathBuf>,
        config_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let themes = theme_resolve::list_themes(themes_dir.as_deref());
        let focused_index = index_of(&themes, &active_theme_name);
        let card_focus_handles = themes.iter().map(|_| cx.focus_handle()).collect();
        Self {
            open: false,
            modal_focus: cx.focus_handle(),
            themes,
            persisted_name: active_theme_name.clone(),
            active_name: active_theme_name,
            focused_index,
            card_focus_handles,
            themes_dir,
            config_path,
        }
    }

    /// Whether the modal overlay is currently mounted/visible.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The focus handle of the checked card ([`Self::focused_index`]), for a
    /// caller opening the modal to focus. Arrow-key navigation is handled by
    /// a listener on the card grid's container, a descendant of the modal
    /// overlay, so key events only reach it once a descendant (a card)
    /// actually holds focus; the overlay stays on the key-dispatch path
    /// either way (it is an ancestor of every card), so `Escape` still
    /// closes the modal from a focused card.
    #[must_use]
    pub fn focused_card_handle(&self) -> FocusHandle {
        self.card_focus_handles
            .get(self.focused_index)
            .cloned()
            .unwrap_or_else(|| self.modal_focus.clone())
    }

    /// The config-facing name of the currently active theme. Test helper:
    /// production code reads [`Self::active_theme_display_name`] instead.
    #[cfg(test)]
    #[must_use]
    pub fn active_theme_name(&self) -> &str {
        &self.active_name
    }

    /// A human-readable label for [`Self::active_theme_name`], for the
    /// status-bar trigger to show. Falls back to the raw name if the active
    /// theme is no longer among the enumerated entries (e.g. its file was
    /// deleted after being selected).
    #[must_use]
    pub fn active_theme_display_name(&self) -> String {
        self.themes
            .iter()
            .find(|entry| entry.name == self.active_name)
            .map_or_else(
                || self.active_name.clone(),
                |entry| entry.display_name.clone(),
            )
    }

    /// Open the modal, re-scanning the themes directory first so a file
    /// dropped in since the last time it was open becomes selectable.
    pub fn open(&mut self, cx: &mut Context<Self>) {
        self.refresh_themes(cx);
        self.open = true;
        cx.notify();
    }

    /// Close the modal, committing the live-applied choice to disk on the way
    /// out. Navigating cards only previews live (see [`Self::select`]), so
    /// dismissal is the one place the config file is touched, keeping arrow-key
    /// navigation free of per-keystroke disk I/O. There is nothing to revert --
    /// the persisted choice is exactly what is already applied.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.persist_if_changed();
        self.open = false;
        cx.notify();
    }

    /// Persist a live-applied-but-uncommitted theme choice on app quit, so
    /// selecting a theme and quitting without dismissing the modal still
    /// persists it. See `main.rs`'s `App::on_app_quit` hook.
    pub fn flush_theme_on_quit(&mut self) {
        self.persist_if_changed();
    }

    /// Write the active theme name to `config_path`, but only when it differs
    /// from what is already there, so an unchanged open/dismiss never rewrites
    /// the config file. A persistence failure is logged, never raised.
    fn persist_if_changed(&mut self) {
        if self.active_name == self.persisted_name {
            return;
        }
        theme_resolve::persist_theme_choice(&self.active_name, self.config_path.as_deref());
        self.persisted_name = self.active_name.clone();
    }

    /// Re-enumerate `themes` from disk and resync `focused_index`/
    /// `card_focus_handles` to the (possibly changed) list.
    fn refresh_themes(&mut self, cx: &mut Context<Self>) {
        self.themes = theme_resolve::list_themes(self.themes_dir.as_deref());
        self.card_focus_handles = self.themes.iter().map(|_| cx.focus_handle()).collect();
        self.focused_index = index_of(&self.themes, &self.active_name);
    }

    /// Select the theme at `index`: apply its already-enumerated palette live
    /// to every window and mark that card both checked and active. This is a
    /// session-only preview -- applying the palette the card was enumerated
    /// with (rather than re-resolving the name) keeps what is applied in step
    /// with what is shown, and the choice is written to disk only when the
    /// modal is dismissed (see [`Self::close`]), so navigating cards never
    /// touches the config file.
    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.themes.get(index) else {
            return;
        };
        let name = entry.name.clone();
        let colors = entry.colors;
        theme_resolve::apply_colors(colors, cx);
        self.active_name = name;
        self.focused_index = index;
        if let Some(handle) = self.card_focus_handles.get(index) {
            window.focus(handle);
        }
        cx.notify();
    }

    /// Left/Up moves the checked card to the previous one, Right/Down to
    /// the next, wrapping at either end -- matching a native radiogroup's
    /// arrow-key behavior, including that moving focus also selects.
    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let len = self.themes.len();
        if len == 0 {
            return;
        }
        let next_index = match event.keystroke.key.as_str() {
            "right" | "down" => Some((self.focused_index + 1) % len),
            "left" | "up" => Some((self.focused_index + len - 1) % len),
            _ => None,
        };
        if let Some(index) = next_index {
            cx.stop_propagation();
            self.select(index, window, cx);
        }
    }
}

/// The index of `themes`' entry named `name`, or `0` if none matches (the
/// list always has at least the five built-ins, so this never has to
/// contend with an empty list in practice).
fn index_of(themes: &[ThemeEntry], name: &str) -> usize {
    themes
        .iter()
        .position(|entry| entry.name == name)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use zsql_ui::theme::ActiveTheme;

    use super::AppearanceModalView;

    /// A themes directory owned exclusively by one test, removed (with any
    /// files written into it) on drop.
    struct TestThemesDir(std::path::PathBuf);

    impl TestThemesDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "zsql-appearance-modal-test-{label}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("must be able to create a test themes dir");
            Self(dir)
        }
    }

    impl Drop for TestThemesDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[gpui::test]
    fn a_fresh_modal_starts_closed_and_checks_the_active_theme(cx: &mut TestAppContext) {
        let (view, vcx) = cx.add_window_view(|_window, cx| {
            AppearanceModalView::new("catppuccin-mocha".to_owned(), None, None, cx)
        });
        vcx.run_until_parked();

        assert!(!view.read_with(vcx, |v, _| v.is_open()));
        assert_eq!(
            view.read_with(vcx, |v, _| v.active_theme_name().to_owned()),
            "catppuccin-mocha"
        );
        assert_eq!(
            view.read_with(vcx, |v, _| v.focused_index),
            view.read_with(vcx, |v, _| super::index_of(&v.themes, "catppuccin-mocha"))
        );
    }

    #[gpui::test]
    fn opening_then_closing_toggles_is_open(cx: &mut TestAppContext) {
        let (view, vcx) = cx.add_window_view(|_window, cx| {
            AppearanceModalView::new("zsql-dark".to_owned(), None, None, cx)
        });

        view.update(vcx, AppearanceModalView::open);
        assert!(view.read_with(vcx, |v, _| v.is_open()));

        view.update(vcx, AppearanceModalView::close);
        assert!(!view.read_with(vcx, |v, _| v.is_open()));
    }

    #[gpui::test]
    fn selecting_a_card_updates_the_active_name_and_focused_index(cx: &mut TestAppContext) {
        let (view, vcx) = cx.add_window_view(|_window, cx| {
            AppearanceModalView::new("zsql-dark".to_owned(), None, None, cx)
        });
        vcx.run_until_parked();

        vcx.update(|window, cx| {
            view.update(cx, |v, cx| v.select(2, window, cx));
        });

        let themes = view.read_with(vcx, |v, _| v.themes.clone());
        assert_eq!(
            view.read_with(vcx, |v, _| v.active_name.clone()),
            themes[2].name
        );
        assert_eq!(view.read_with(vcx, |v, _| v.focused_index), 2);
    }

    #[gpui::test]
    fn arrow_key_navigation_wraps_at_both_ends(cx: &mut TestAppContext) {
        let (view, vcx) = cx.add_window_view(|_window, cx| {
            AppearanceModalView::new("zsql-dark".to_owned(), None, None, cx)
        });
        vcx.run_until_parked();

        let len = view.read_with(vcx, |v, _| v.themes.len());

        vcx.update(|window, cx| {
            view.update(cx, |v, cx| v.select(0, window, cx));
        });
        vcx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.handle_key_down(
                    &gpui::KeyDownEvent {
                        keystroke: gpui::Keystroke {
                            modifiers: gpui::Modifiers::default(),
                            key: "left".to_owned(),
                            key_char: None,
                        },
                        is_held: false,
                    },
                    window,
                    cx,
                );
            });
        });
        assert_eq!(
            view.read_with(vcx, |v, _| v.focused_index),
            len - 1,
            "left from the first card must wrap to the last"
        );

        vcx.update(|window, cx| {
            view.update(cx, |v, cx| {
                v.handle_key_down(
                    &gpui::KeyDownEvent {
                        keystroke: gpui::Keystroke {
                            modifiers: gpui::Modifiers::default(),
                            key: "right".to_owned(),
                            key_char: None,
                        },
                        is_held: false,
                    },
                    window,
                    cx,
                );
            });
        });
        assert_eq!(
            view.read_with(vcx, |v, _| v.focused_index),
            0,
            "right from the last card must wrap to the first"
        );
    }

    #[gpui::test]
    fn a_theme_file_dropped_in_after_construction_becomes_selectable_the_next_time_it_opens(
        cx: &mut TestAppContext,
    ) {
        let dir = TestThemesDir::new("late-drop");
        let themes_dir = dir.0.clone();
        let (view, vcx) = cx.add_window_view(|_window, cx| {
            AppearanceModalView::new("zsql-dark".to_owned(), Some(themes_dir), None, cx)
        });
        vcx.run_until_parked();

        assert!(
            !view
                .read_with(vcx, |v, _| v.themes.clone())
                .iter()
                .any(|entry| entry.name == "nord"),
            "a theme dropped in after construction must not appear before the modal reopens"
        );

        std::fs::write(dir.0.join("nord.json"), "{\"accent\": \"#88c0d0\"}")
            .expect("writing the late-dropped theme file must succeed");

        view.update(vcx, AppearanceModalView::open);
        assert!(
            view.read_with(vcx, |v, _| v.themes.clone())
                .iter()
                .any(|entry| entry.name == "nord"),
            "the theme dropped in while the app was running must be selectable once the modal opens"
        );
    }

    #[gpui::test]
    fn selecting_a_card_then_closing_persists_the_choice_to_the_injected_config_path(
        cx: &mut TestAppContext,
    ) {
        let config_dir = TestThemesDir::new("select-persist-config-dir");
        let config_path = config_dir.0.join("config.toml");
        let (view, vcx) = cx.add_window_view(|_window, cx| {
            AppearanceModalView::new("zsql-dark".to_owned(), None, Some(config_path.clone()), cx)
        });
        vcx.run_until_parked();

        vcx.update(|window, cx| {
            view.update(cx, |v, cx| v.select(1, window, cx));
        });
        vcx.run_until_parked();
        assert!(
            !config_path.exists(),
            "selecting a card previews live but must not write the config file until dismiss"
        );

        view.update(vcx, AppearanceModalView::close);
        vcx.run_until_parked();

        let themes = view.read_with(vcx, |v, _| v.themes.clone());
        let reloaded = crate::config::Config::load_or_default(&config_path)
            .expect("the modal's own config path must have been written to, not the real one");
        assert_eq!(reloaded.theme.name, themes[1].name);
    }

    #[gpui::test]
    fn closing_without_changing_the_theme_does_not_rewrite_the_config(cx: &mut TestAppContext) {
        let config_dir = TestThemesDir::new("close-noop-config-dir");
        let config_path = config_dir.0.join("config.toml");
        let (view, vcx) = cx.add_window_view(|_window, cx| {
            AppearanceModalView::new("zsql-dark".to_owned(), None, Some(config_path.clone()), cx)
        });
        vcx.run_until_parked();

        view.update(vcx, AppearanceModalView::open);
        view.update(vcx, AppearanceModalView::close);
        vcx.run_until_parked();

        assert!(
            !config_path.exists(),
            "opening then dismissing without changing the theme must not touch the config"
        );
    }

    #[gpui::test]
    fn flushing_on_quit_persists_a_selected_but_undismissed_theme(cx: &mut TestAppContext) {
        let config_dir = TestThemesDir::new("quit-flush-config-dir");
        let config_path = config_dir.0.join("config.toml");
        let (view, vcx) = cx.add_window_view(|_window, cx| {
            AppearanceModalView::new("zsql-dark".to_owned(), None, Some(config_path.clone()), cx)
        });
        vcx.run_until_parked();

        vcx.update(|window, cx| {
            view.update(cx, |v, cx| v.select(1, window, cx));
        });
        // The modal is never dismissed -- only the app-quit flush runs, which
        // must still commit the live-applied choice.
        view.update(vcx, |v, _cx| v.flush_theme_on_quit());
        vcx.run_until_parked();

        let themes = view.read_with(vcx, |v, _| v.themes.clone());
        let reloaded = crate::config::Config::load_or_default(&config_path)
            .expect("the quit flush must have written the injected config path");
        assert_eq!(reloaded.theme.name, themes[1].name);
    }

    #[gpui::test]
    fn selecting_a_user_theme_card_applies_that_files_own_palette(cx: &mut TestAppContext) {
        let themes_dir = TestThemesDir::new("select-custom-apply-themes-dir");
        // A user theme overriding a single role; every other role inherits the
        // default through the `Colors` deserializer.
        std::fs::write(themes_dir.0.join("nord.json"), "{\"accent\": \"#88c0d0\"}")
            .expect("writing the user theme file must succeed");
        let expected = crate::theme_resolve::resolve("nord", Some(&themes_dir.0));

        let (view, vcx) = cx.add_window_view(|_window, cx| {
            AppearanceModalView::new("zsql-dark".to_owned(), Some(themes_dir.0.clone()), None, cx)
        });
        vcx.run_until_parked();

        let nord_index = view.read_with(vcx, |v, _| super::index_of(&v.themes, "nord"));
        vcx.update(|window, cx| {
            view.update(cx, |v, cx| v.select(nord_index, window, cx));
        });
        vcx.run_until_parked();

        vcx.update(|_window, cx| {
            assert_eq!(
                cx.theme().colors,
                expected,
                "selecting a user theme card must apply that file's own palette, not the default"
            );
        });
    }
}
