//! The runtime theme: the [`Colors`] every view reads instead of a hardcoded
//! literal, held as `gpui` global state so it can be swapped without touching
//! a single call site.

pub mod catppuccin;
mod colors;
mod fonts;

use std::sync::LazyLock;

pub use colors::Colors;
pub use fonts::{DEFAULT_FONT_DATA, DEFAULT_FONT_UI, Fonts, get_builtin_fonts};

/// The active visual theme: a color palette. A single-field wrapper today so
/// the palette is reached as `cx.theme().colors.<role>`, leaving room for a
/// theme to grow other dimensions (sizing, spacing) later without rewriting
/// every call site.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Theme {
    /// The active color palette.
    pub colors: Colors,
    /// The active fonts.
    pub fonts: Fonts,
}

impl gpui::Global for Theme {}

/// Config-facing name of the built-in zsql Dark theme: the app's original
/// palette, reachable through the same lookup as every other built-in
/// flavor instead of only as [`Colors::default`]'s fallback.
pub const ZSQL_DARK_NAME: &str = "zsql-dark";

/// Every built-in theme's config-facing name, in the order an enumeration UI
/// should list them: the default zsql Dark palette first, then the four
/// Catppuccin flavors light-to-dark.
const BUILTIN_THEME_NAMES: [&str; 5] = [
    ZSQL_DARK_NAME,
    catppuccin::LATTE_NAME,
    catppuccin::FRAPPE_NAME,
    catppuccin::MACCHIATO_NAME,
    catppuccin::MOCHA_NAME,
];

/// Every selectable built-in theme's config-facing name, in the documented
/// display order (see [`BUILTIN_THEME_NAMES`]).
#[must_use]
pub fn builtin_theme_names() -> &'static [&'static str] {
    &BUILTIN_THEME_NAMES
}

/// The built-in [`Colors`] palette named `name` (the zsql Dark default or
/// one of the [`catppuccin`] flavors), or `None` if `name` does not match a
/// built-in.
#[must_use]
pub fn built_in_theme(name: &str) -> Option<Colors> {
    if name == ZSQL_DARK_NAME {
        return Some(Colors::default());
    }
    catppuccin::built_in_by_name(name)
}

/// Ergonomic access to the active [`Theme`]: any type that derefs to
/// `gpui::App` (every `Context<T>` and `Window` context does) gets
/// `cx.theme()` for free.
pub trait ActiveTheme {
    /// The active theme, read from `gpui`'s global state, or
    /// [`Theme::default`] if no global has been set yet -- e.g. in a test
    /// that never opens the real app's window (see `main.rs`, the sole
    /// place a real run sets one).
    fn theme(&self) -> &Theme;
}

// To safely support the below implementation, keep around a static
// Theme so we can pass around references to it.
static DEFAULT_THEME: LazyLock<Theme> = LazyLock::new(Theme::default);

impl ActiveTheme for gpui::App {
    fn theme(&self) -> &Theme {
        if let Some(t) = self.try_global::<Theme>() {
            return t;
        }
        &DEFAULT_THEME
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::Theme;

    #[test]
    fn default_theme_carries_the_default_colors() {
        let theme = Theme::default();
        assert_eq!(theme.colors, super::Colors::default());
    }

    #[test]
    fn builtin_theme_names_lists_exactly_the_five_flavors_in_display_order() {
        assert_eq!(
            super::builtin_theme_names(),
            [
                "zsql-dark",
                "catppuccin-latte",
                "catppuccin-frappe",
                "catppuccin-macchiato",
                "catppuccin-mocha",
            ]
        );
    }

    #[test]
    fn built_in_theme_resolves_zsql_dark_to_the_default_palette() {
        assert_eq!(
            super::built_in_theme(super::ZSQL_DARK_NAME),
            Some(super::Colors::default())
        );
    }

    #[test]
    fn built_in_theme_still_resolves_every_catppuccin_flavor() {
        for name in super::builtin_theme_names() {
            assert!(
                super::built_in_theme(name).is_some(),
                "{name} must resolve to a built-in palette"
            );
        }
    }

    #[test]
    fn built_in_theme_returns_none_for_an_unknown_name() {
        assert_eq!(super::built_in_theme("not-a-real-theme"), None);
    }

    /// Every color a view paints with must come from the active [`Theme`],
    /// not a literal baked into the call site: this scans every source file
    /// in the workspace's `gpui`-facing crates and fails on a hardcoded
    /// `rgb(0x...)`/`rgba(0x...)` literal outside the theme module (which
    /// legitimately owns the palette) or a test module (which legitimately
    /// pins expected values against literals for comparison).
    #[test]
    fn guard_no_hardcoded_color_literals_outside_the_theme_module() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."); // crates/zsql-ui -> workspace root
        let scanned_crate_src_dirs = [
            "crates/zsql-ui/src",
            "crates/zsql-editor/src",
            "crates/zsql/src",
        ];

        let mut offenders = Vec::new();
        let mut files_scanned = 0usize;
        for crate_src_dir in scanned_crate_src_dirs {
            scan_dir_for_hardcoded_colors(
                &workspace_root.join(crate_src_dir),
                &mut offenders,
                &mut files_scanned,
            );
        }

        // A directory-resolution bug (a wrong `workspace_root`, a typo'd
        // crate dir) would make the walk silently visit nothing and this
        // guard would then pass by scanning zero files. Requiring a floor
        // well under the real source tree's file count catches that without
        // hardcoding an exact, easily-stale count.
        assert!(
            files_scanned > 20,
            "expected to scan more than 20 .rs files across {scanned_crate_src_dirs:?}, but only \
             scanned {files_scanned} -- the walk is probably not finding the real source tree"
        );

        assert!(
            offenders.is_empty(),
            "found hardcoded rgb(0x../rgba(0x.. color literals outside the theme module: \
             {offenders:#?}"
        );
    }

    /// The detector this guard runs on every file must actually flag a
    /// hardcoded color literal, and must not flag ordinary code -- checked
    /// directly against planted strings so the guard test above cannot pass
    /// vacuously by scanning files whose content it fails to inspect
    /// correctly.
    #[test]
    fn hardcoded_color_detector_flags_literals_and_ignores_plain_code() {
        assert!(file_contains_hardcoded_color(
            "fn paint() -> Div { div().bg(rgb(0xff00ff)) }"
        ));
        assert!(file_contains_hardcoded_color(
            "fn paint() -> Div { div().bg(rgba(0xff00ff33)) }"
        ));
        assert!(!file_contains_hardcoded_color(
            "fn paint(theme: &Theme) -> Div { div().bg(rgb(theme.colors.accent)) }"
        ));
        assert!(!file_contains_hardcoded_color(""));
    }

    /// Recursively collect `path`'s (and its descendants') `.rs` files whose
    /// non-test code contains a hardcoded `rgb(0x`/`rgba(0x` literal, into
    /// `offenders`, incrementing `files_scanned` for every `.rs` file
    /// actually read (including ones skipped by policy), so a caller can
    /// verify the walk really visited the tree instead of finding nothing.
    /// Skips the theme module itself (own path contains `/theme/` or
    /// `theme.rs`, where the palette legitimately lives).
    fn scan_dir_for_hardcoded_colors(
        dir: &Path,
        offenders: &mut Vec<String>,
        files_scanned: &mut usize,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                scan_dir_for_hardcoded_colors(&path, offenders, files_scanned);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            *files_scanned += 1;
            let path_str = path.to_string_lossy();
            if path_str.contains("/theme/") || path_str.ends_with("theme.rs") {
                continue;
            }

            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let production_code = production_code_before_tests(&content);
            if file_contains_hardcoded_color(production_code) {
                offenders.push(path_str.into_owned());
            }
        }
    }

    /// Whether `content` contains a hardcoded `rgb(0x`/`rgba(0x` color
    /// literal, as opposed to a call built from a theme field or method.
    fn file_contains_hardcoded_color(content: &str) -> bool {
        content.contains("rgb(0x") || content.contains("rgba(0x")
    }

    /// `content` up to (excluding) its first `#[cfg(test)]` marker -- every
    /// file in this workspace keeps all its `#[cfg(test)]` modules
    /// contiguous at the end, after all production code, so this reliably
    /// excludes test-only code (which legitimately pins literal ARGB values
    /// to assert against) without needing a real Rust parser.
    fn production_code_before_tests(content: &str) -> &str {
        content
            .find("#[cfg(test)]")
            .map_or(content, |index| &content[..index])
    }
}
