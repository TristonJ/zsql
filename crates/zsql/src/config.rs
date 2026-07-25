//! Application configuration: theme, query limits, and the other
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

/// The standard SSH port, used to fill in a connection form's SSH tunnel
/// port when left empty.
pub const DEFAULT_SSH_TUNNEL_PORT: u16 = 22;

/// Top-level application config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Theme/appearance settings: the active theme name and data/UI fonts.
    pub theme: ThemeConfig,
    /// Query execution limits and defaults.
    pub query: QueryConfig,
    /// Connection liveliness probe timing.
    pub liveness: LivenessConfig,
    /// Sizing bounds for the resizable workspace panes.
    pub layout: LayoutConfig,
    /// Thresholds and layout tunables for the results grid's value panel.
    pub value_panel: ValuePanelConfig,
}

/// Font settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontConfig {
    /// The font used in code blocks, values, tables, etc.
    pub data: String,
    /// The font used in UI elements, e.g. buttons, labels, etc.
    pub ui: String,
}

/// Appearance settings: the active theme name and data/UI fonts.
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
    /// Rows accumulated per streamed batch before pushing to the UI. Threaded
    /// through to each sqlx-based driver's connection at connect time (see
    /// `zsql_core::ConnConfig::batch_size`).
    pub batch_size: usize,
    /// Default `LIMIT` applied to table quick-previews.
    pub preview_limit: u64,
    /// Upper bound on rows accumulated for a single streamed query result.
    /// Once a result reaches this many rows the query is cancelled and the
    /// session reports a truncated result rather than continuing to grow
    /// without bound.
    pub max_result_rows: u64,
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
    /// Layout options for the value panel
    pub value_panel: ValuePanelLayout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
// All three fields end in `_width` because they describe the same
// dimension (default/min/max), matching `LayoutConfig`'s own naming.
#[allow(clippy::struct_field_names)]
pub struct ValuePanelLayout {
    /// Value panel width when it first opens.
    pub default_width: Pixels,
    /// Narrowest the value panel can be dragged to.
    pub min_width: Pixels,
    /// Widest the value panel can be dragged to.
    pub max_width: Pixels,
}

/// Thresholds and layout tunables for the results grid's value panel (see
/// `crate::ui::value_panel`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ValuePanelConfig {
    /// Largest a JSON value's source text may be for the panel to parse it
    /// eagerly into the Tree/Pretty views on open. Past this, the panel opens
    /// in Raw with only the first `json_oversized_preview_bytes` shown, plus
    /// a "Load full value" action that parses the rest off the render path.
    pub json_eager_parse_threshold_bytes: usize,
    /// How many bytes of an oversized JSON value's source text the panel
    /// shows in Raw mode before the value has been fully loaded.
    pub json_oversized_preview_bytes: usize,
    /// Bytes shown per row in the Bytes renderer's hex dump.
    pub hex_bytes_per_row: usize,
}

/// Default [`ValuePanelConfig::json_eager_parse_threshold_bytes`]: large
/// enough that an ordinary JSON cell always parses eagerly, small enough that
/// a pathological value cannot block a render on a multi-megabyte parse.
const DEFAULT_JSON_EAGER_PARSE_THRESHOLD_BYTES: usize = 2 * 1024 * 1024;
/// Default [`ValuePanelConfig::json_oversized_preview_bytes`]: enough text to
/// orient the user in an oversized value's shape without holding a
/// multi-megabyte string in the preview itself.
const DEFAULT_JSON_OVERSIZED_PREVIEW_BYTES: usize = 64 * 1024;
/// Default [`ValuePanelConfig::hex_bytes_per_row`]: the conventional `hexdump`/
/// `xxd` row width.
const DEFAULT_HEX_BYTES_PER_ROW: usize = 16;

impl Default for ValuePanelConfig {
    fn default() -> Self {
        Self {
            json_eager_parse_threshold_bytes: DEFAULT_JSON_EAGER_PARSE_THRESHOLD_BYTES,
            json_oversized_preview_bytes: DEFAULT_JSON_OVERSIZED_PREVIEW_BYTES,
            hex_bytes_per_row: DEFAULT_HEX_BYTES_PER_ROW,
        }
    }
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
            value_panel: ValuePanelLayout::default(),
        }
    }
}

impl Default for ValuePanelLayout {
    fn default() -> Self {
        Self {
            default_width: px(360.0),
            min_width: px(240.0),
            max_width: px(720.0),
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: zsql_ui::theme::ZSQL_DARK_NAME.to_owned(),
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
            batch_size: zsql_core::DEFAULT_QUERY_BATCH_SIZE,
            preview_limit: 200,
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

    /// Write the full config to `path`, creating its parent directory if
    /// needed. Serializes every section (not just whichever one a caller
    /// changed), so a caller that wants to change one setting must load,
    /// mutate, then save, rather than losing the rest of the file.
    ///
    /// # Errors
    /// Returns [`ConfigError`] if the config cannot be serialized or the
    /// file cannot be written.
    #[tracing::instrument(name = "config_save", skip(self))]
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(ConfigError::Write)?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text).map_err(ConfigError::Write)?;
        tracing::info!(path = %path.display(), "config saved");
        Ok(())
    }
}

/// Errors saving the top-level app config.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The in-memory config could not be serialized to TOML.
    #[error("failed to serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// The config file could not be written.
    #[error("failed to write config: {0}")]
    Write(std::io::Error),
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
    fn batch_size_defaults_to_the_shared_zsql_core_constant() {
        assert_eq!(
            Config::default().query.batch_size,
            zsql_core::DEFAULT_QUERY_BATCH_SIZE
        );
    }

    #[test]
    fn query_config_batch_size_round_trips_through_toml() {
        let mut cfg = Config::default();
        cfg.query.batch_size = 42;

        let text = toml::to_string(&cfg).expect("config must serialize to toml");
        let parsed: Config = toml::from_str(&text).expect("config must parse back from toml");

        assert_eq!(parsed.query.batch_size, 42);
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

    #[test]
    fn value_panel_layout_defaults_are_ordered_min_default_max() {
        let cfg = Config::default();
        assert!(cfg.layout.value_panel.min_width <= cfg.layout.value_panel.default_width);
        assert!(cfg.layout.value_panel.max_width >= cfg.layout.value_panel.default_width);
    }

    #[test]
    fn value_panel_config_defaults_are_all_positive() {
        let cfg = Config::default();
        assert!(cfg.value_panel.json_eager_parse_threshold_bytes > 0);
        assert!(cfg.value_panel.json_oversized_preview_bytes > 0);
        assert!(cfg.value_panel.hex_bytes_per_row > 0);
        assert!(
            cfg.value_panel.json_oversized_preview_bytes
                < cfg.value_panel.json_eager_parse_threshold_bytes,
            "the oversized preview must be smaller than the threshold that triggers it"
        );
    }

    #[test]
    fn value_panel_config_round_trips_through_toml() {
        let mut cfg = Config::default();
        cfg.layout.value_panel.default_width = gpui::px(400.0);
        cfg.layout.value_panel.min_width = gpui::px(260.0);
        cfg.layout.value_panel.max_width = gpui::px(800.0);
        cfg.value_panel.json_eager_parse_threshold_bytes = 1_000;
        cfg.value_panel.json_oversized_preview_bytes = 100;
        cfg.value_panel.hex_bytes_per_row = 8;

        let text = toml::to_string(&cfg).expect("config must serialize to toml");
        let parsed: Config = toml::from_str(&text).expect("config must parse back from toml");

        assert_eq!(parsed.layout.value_panel.default_width, gpui::px(400.0));
        assert_eq!(parsed.layout.value_panel.min_width, gpui::px(260.0));
        assert_eq!(parsed.layout.value_panel.max_width, gpui::px(800.0));
        assert_eq!(parsed.value_panel.json_eager_parse_threshold_bytes, 1_000);
        assert_eq!(parsed.value_panel.json_oversized_preview_bytes, 100);
        assert_eq!(parsed.value_panel.hex_bytes_per_row, 8);
    }

    #[test]
    fn value_panel_section_is_optional_in_toml_and_falls_back_to_defaults() {
        let parsed: Config =
            toml::from_str("[connection]\ndefault_url = \"postgres://localhost/db\"\n")
                .expect("config without a [value_panel] section must still parse");
        assert_eq!(
            parsed.value_panel.hex_bytes_per_row,
            Config::default().value_panel.hex_bytes_per_row
        );
    }

    #[test]
    fn default_theme_name_matches_a_builtin_theme() {
        assert!(
            zsql_ui::theme::builtin_theme_names().contains(&Config::default().theme.name.as_str()),
            "a fresh install's configured theme name must be a real built-in, \
             so the Appearance modal can mark a card active on first run"
        );
    }

    /// A temp file path this test owns exclusively, removed on drop so tests
    /// never leak files into the real temp dir.
    struct TempConfigPath(std::path::PathBuf);

    impl TempConfigPath {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "zsql-config-test-{label}-{}-{n}.toml",
                std::process::id()
            ));
            Self(path)
        }
    }

    impl Drop for TempConfigPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn saving_then_loading_preserves_every_touched_section_not_just_theme() {
        let temp = TempConfigPath::new("round-trip");

        let mut cfg = Config::default();
        cfg.theme.name = "catppuccin-mocha".to_owned();
        cfg.query.max_result_rows = 42;
        cfg.query.batch_size = 17;
        cfg.layout.sidebar_default_width = gpui::px(321.0);
        cfg.liveness.probe_interval_ms = 9_999;
        cfg.value_panel.hex_bytes_per_row = 4;

        cfg.save(&temp.0).expect("save must succeed");
        let reloaded = Config::load_or_default(&temp.0).expect("reload must succeed");

        assert_eq!(reloaded.theme.name, "catppuccin-mocha");
        assert_eq!(reloaded.query.max_result_rows, 42);
        assert_eq!(reloaded.query.batch_size, 17);
        assert_eq!(reloaded.layout.sidebar_default_width, gpui::px(321.0));
        assert_eq!(reloaded.liveness.probe_interval_ms, 9_999);
        assert_eq!(reloaded.value_panel.hex_bytes_per_row, 4);
    }

    #[test]
    fn saving_creates_missing_parent_directories() {
        let base = std::env::temp_dir().join(format!(
            "zsql-config-test-nested-parent-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let path = base.join("nested").join("config.toml");

        Config::default()
            .save(&path)
            .expect("save must create parent dirs");
        assert!(path.exists());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn saving_twice_overwrites_rather_than_appends() {
        let temp = TempConfigPath::new("overwrite");

        let mut first = Config::default();
        first.theme.name = "catppuccin-latte".to_owned();
        first.save(&temp.0).expect("first save must succeed");

        let mut second = Config::default();
        second.theme.name = "catppuccin-mocha".to_owned();
        second.save(&temp.0).expect("second save must succeed");

        let reloaded = Config::load_or_default(&temp.0).expect("reload must succeed");
        assert_eq!(reloaded.theme.name, "catppuccin-mocha");
    }
}
