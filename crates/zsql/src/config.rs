//! Application configuration: theme placeholder, query limits, and the other
//! constants

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{Pixels, px};
use serde::{Deserialize, Serialize};
use zsql_ui::theme::{DEFAULT_FONT_DATA, DEFAULT_FONT_UI};

/// Directory name under the OS config dir that holds every zsql config/data
/// file: `dirs::config_dir().join(APP_CONFIG_DIR_NAME)`.
const APP_CONFIG_DIR_NAME: &str = "zsql";
/// File name of the top-level app config, under `APP_CONFIG_DIR_NAME`.
const CONFIG_FILE_NAME: &str = "config.toml";
/// File name of the persisted connection store, under `APP_CONFIG_DIR_NAME`.
/// See [`crate::connections::ConnectionStore`].
const CONNECTIONS_FILE_NAME: &str = "connections.toml";
/// File name of the persisted per-connection tab-session store, under
/// `APP_CONFIG_DIR_NAME`. See [`crate::tab_session`].
const TAB_SESSIONS_FILE_NAME: &str = "tab_sessions.json";
/// Directory name under `APP_CONFIG_DIR_NAME` that holds user theme files,
/// one JSON file per theme named after [`ThemeConfig::name`]. See
/// [`crate::theme_resolve`].
const THEMES_DIR_NAME: &str = "themes";

/// Top-level application config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Theme/appearance settings (placeholder until real theming lands).
    pub theme: ThemeConfig,
    /// Query execution limits and defaults.
    pub query: QueryConfig,
    /// Connection defaults.
    pub connection: ConnectionConfig,
    /// Connection liveliness probe timing.
    pub liveness: LivenessConfig,
    /// Sizing bounds for the resizable workspace panes.
    pub layout: LayoutConfig,
}

/// Font settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontConfig {
    /// The font used in code blocks, values, tables, etc.
    pub data: String,
    /// The font used in UI elements, e.g. buttons, labels, etc.
    pub ui: String,
}

/// Appearance settings. Placeholder for the eventual theming system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    /// Named theme (e.g. `dark`).
    pub name: String,
    /// Font used for data, code, and other similar content. Typically a monospace font.
    pub fonts: FontConfig,
}

/// Query execution limits and defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct QueryConfig {
    /// Rows accumulated per streamed batch before pushing to the UI.
    pub batch_size: usize,
    /// Default `LIMIT` applied to table quick-previews.
    pub preview_limit: u64,
    /// Server-side statement timeout in milliseconds (`0` disables it).
    pub statement_timeout_ms: u64,
    /// Upper bound on rows accumulated for a single streamed query result.
    /// Once a result reaches this many rows the query is cancelled and the
    /// session reports a truncated result rather than continuing to grow
    /// without bound.
    pub max_result_rows: u64,
}

/// Connection defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionConfig {
    /// Optional default URL; the `DATABASE_URL` env var overrides it.
    pub default_url: Option<String>,
}

/// Timing for the recurring connection liveliness probe that runs once a
/// [`crate::session::Session`] is connected.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LivenessConfig {
    /// How often the liveliness probe fires, in milliseconds, while a
    /// connection is idle. The probe never overlaps itself: a slow probe
    /// defers, rather than duplicates, the next tick.
    pub probe_interval_ms: u64,
    /// How long a single probe may run before it is treated as a failure, in
    /// milliseconds.
    pub probe_timeout_ms: u64,
}

/// Sizing bounds for the workspace's resizable panes: the schema sidebar,
/// the SQL editor, and the results grid. The divider between two panes lets
/// the user drag past the default size but never past `min`/`max`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutConfig {
    /// Sidebar width when the workspace first opens.
    pub sidebar_default_width: Pixels,
    /// Narrowest the sidebar can be dragged to.
    pub sidebar_min_width: Pixels,
    /// Widest the sidebar can be dragged to.
    pub sidebar_max_width: Pixels,
    /// Editor pane height when the workspace first opens.
    pub editor_default_height: Pixels,
    /// Shortest the editor pane can be dragged to.
    pub editor_min_height: Pixels,
    /// Shortest the results pane can be dragged to; the editor/results
    /// divider refuses to shrink the results pane past this even when the
    /// requested drag would otherwise push the editor further down.
    pub results_min_height: Pixels,
    /// Hit-target thickness of a draggable divider between two panes.
    pub divider_thickness: Pixels,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            sidebar_default_width: px(300.0),
            sidebar_min_width: px(180.0),
            sidebar_max_width: px(560.0),
            editor_default_height: px(500.0),
            editor_min_height: px(120.0),
            results_min_height: px(120.0),
            divider_thickness: px(4.0),
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: "dark".to_owned(),
            fonts: FontConfig::default(),
        }
    }
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            data: DEFAULT_FONT_DATA.to_string(),
            ui: DEFAULT_FONT_UI.to_string(),
        }
    }
}

/// Default [`QueryConfig::max_result_rows`]: large enough that an ordinary
/// result set never comes close, but bounded so a runaway query cannot grow
/// the in-memory result set (and the UI grid) without limit.
const DEFAULT_MAX_RESULT_ROWS: u64 = 5_000_000;

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            batch_size: 500,
            preview_limit: 200,
            statement_timeout_ms: 30_000,
            max_result_rows: DEFAULT_MAX_RESULT_ROWS,
        }
    }
}

impl Default for LivenessConfig {
    fn default() -> Self {
        Self {
            probe_interval_ms: 5_000,
            probe_timeout_ms: 2_000,
        }
    }
}

impl LivenessConfig {
    /// The probe interval as a [`Duration`].
    #[must_use]
    pub fn probe_interval(&self) -> Duration {
        Duration::from_millis(self.probe_interval_ms)
    }

    /// The probe timeout as a [`Duration`].
    #[must_use]
    pub fn probe_timeout(&self) -> Duration {
        Duration::from_millis(self.probe_timeout_ms)
    }
}

impl Config {
    /// Standard config path (`$XDG_CONFIG_HOME/zsql/config.toml`), if resolvable.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join(APP_CONFIG_DIR_NAME).join(CONFIG_FILE_NAME))
    }

    /// Path to the persisted connection store
    /// (`$XDG_CONFIG_HOME/zsql/connections.toml`), if resolvable. Lives
    /// alongside [`Config::default_path`], under the same app config
    /// directory.
    #[must_use]
    pub fn connections_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join(APP_CONFIG_DIR_NAME).join(CONNECTIONS_FILE_NAME))
    }

    /// Path to the persisted tab-session store
    /// (`$XDG_CONFIG_HOME/zsql/tab_sessions.json`), if resolvable. Lives
    /// alongside [`Config::default_path`], under the same app config
    /// directory.
    #[must_use]
    pub fn tab_sessions_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join(APP_CONFIG_DIR_NAME).join(TAB_SESSIONS_FILE_NAME))
    }

    /// Directory holding user theme files
    /// (`$XDG_CONFIG_HOME/zsql/themes/`), if resolvable. Lives alongside
    /// [`Config::default_path`], under the same app config directory.
    #[must_use]
    pub fn themes_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join(APP_CONFIG_DIR_NAME).join(THEMES_DIR_NAME))
    }

    /// Load config from `path`, falling back to defaults if it does not exist.
    ///
    /// # Errors
    /// Returns an error if the file exists but cannot be read or parsed.
    pub fn load_or_default(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    /// Resolve the effective connection URL: `DATABASE_URL` wins, else the
    /// configured default.
    #[must_use]
    pub fn resolve_url(&self) -> Option<String> {
        std::env::var("DATABASE_URL")
            .ok()
            .or_else(|| self.connection.default_url.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::Config;

    #[test]
    fn liveness_defaults_are_positive_and_the_timeout_is_shorter_than_the_interval() {
        let cfg = Config::default();
        assert!(cfg.liveness.probe_interval_ms > 0);
        assert!(cfg.liveness.probe_timeout_ms > 0);
        assert!(
            cfg.liveness.probe_timeout_ms < cfg.liveness.probe_interval_ms,
            "a probe must be able to time out within a single interval"
        );
    }

    #[test]
    fn liveness_duration_helpers_convert_from_the_configured_milliseconds() {
        let mut cfg = Config::default();
        cfg.liveness.probe_interval_ms = 7_500;
        cfg.liveness.probe_timeout_ms = 1_200;
        assert_eq!(cfg.liveness.probe_interval(), Duration::from_millis(7_500));
        assert_eq!(cfg.liveness.probe_timeout(), Duration::from_millis(1_200));
    }

    #[test]
    fn liveness_config_round_trips_through_toml() {
        let mut cfg = Config::default();
        cfg.liveness.probe_interval_ms = 9_000;
        cfg.liveness.probe_timeout_ms = 1_500;

        let text = toml::to_string(&cfg).expect("config must serialize to toml");
        let parsed: Config = toml::from_str(&text).expect("config must parse back from toml");

        assert_eq!(parsed.liveness.probe_interval_ms, 9_000);
        assert_eq!(parsed.liveness.probe_timeout_ms, 1_500);
    }

    #[test]
    fn liveness_section_is_optional_in_toml_and_falls_back_to_defaults() {
        let parsed: Config =
            toml::from_str("[connection]\ndefault_url = \"postgres://localhost/db\"\n")
                .expect("config without a [liveness] section must still parse");
        assert_eq!(
            parsed.liveness.probe_interval_ms,
            Config::default().liveness.probe_interval_ms
        );
    }

    #[test]
    fn max_result_rows_defaults_to_five_million() {
        assert_eq!(Config::default().query.max_result_rows, 5_000_000);
    }

    #[test]
    fn query_config_max_result_rows_round_trips_through_toml() {
        let mut cfg = Config::default();
        cfg.query.max_result_rows = 100;

        let text = toml::to_string(&cfg).expect("config must serialize to toml");
        let parsed: Config = toml::from_str(&text).expect("config must parse back from toml");

        assert_eq!(parsed.query.max_result_rows, 100);
    }

    #[test]
    fn query_section_is_optional_in_toml_and_falls_back_to_the_default_row_limit() {
        let parsed: Config =
            toml::from_str("[connection]\ndefault_url = \"postgres://localhost/db\"\n")
                .expect("config without a [query] section must still parse");
        assert_eq!(
            parsed.query.max_result_rows,
            Config::default().query.max_result_rows
        );
    }

    #[test]
    fn layout_defaults_match_todays_fixed_sidebar_width_and_editor_height() {
        let cfg = Config::default();
        assert_eq!(cfg.layout.sidebar_default_width, gpui::px(300.0));
        assert_eq!(cfg.layout.editor_default_height, gpui::px(500.0));
    }

    #[test]
    fn layout_mins_are_at_or_below_their_defaults_and_maxes_are_at_or_above() {
        let cfg = Config::default();
        assert!(cfg.layout.sidebar_min_width <= cfg.layout.sidebar_default_width);
        assert!(cfg.layout.sidebar_max_width >= cfg.layout.sidebar_default_width);
        assert!(cfg.layout.editor_min_height <= cfg.layout.editor_default_height);
    }

    #[test]
    fn layout_config_round_trips_through_toml() {
        let mut cfg = Config::default();
        cfg.layout.sidebar_default_width = gpui::px(340.0);
        cfg.layout.sidebar_min_width = gpui::px(200.0);
        cfg.layout.sidebar_max_width = gpui::px(600.0);
        cfg.layout.editor_default_height = gpui::px(420.0);
        cfg.layout.editor_min_height = gpui::px(150.0);
        cfg.layout.results_min_height = gpui::px(140.0);
        cfg.layout.divider_thickness = gpui::px(8.0);

        let text = toml::to_string(&cfg).expect("config must serialize to toml");
        let parsed: Config = toml::from_str(&text).expect("config must parse back from toml");

        assert_eq!(parsed.layout.sidebar_default_width, gpui::px(340.0));
        assert_eq!(parsed.layout.sidebar_min_width, gpui::px(200.0));
        assert_eq!(parsed.layout.sidebar_max_width, gpui::px(600.0));
        assert_eq!(parsed.layout.editor_default_height, gpui::px(420.0));
        assert_eq!(parsed.layout.editor_min_height, gpui::px(150.0));
        assert_eq!(parsed.layout.results_min_height, gpui::px(140.0));
        assert_eq!(parsed.layout.divider_thickness, gpui::px(8.0));
    }

    #[test]
    fn layout_section_is_optional_in_toml_and_falls_back_to_defaults() {
        let parsed: Config =
            toml::from_str("[connection]\ndefault_url = \"postgres://localhost/db\"\n")
                .expect("config without a [layout] section must still parse");
        assert_eq!(
            parsed.layout.sidebar_default_width,
            Config::default().layout.sidebar_default_width
        );
        assert_eq!(
            parsed.layout.editor_default_height,
            Config::default().layout.editor_default_height
        );
    }
}
