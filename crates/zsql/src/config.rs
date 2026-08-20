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
/// File name of the legacy, pre-migration per-connection tab-session store,
/// under `APP_CONFIG_DIR_NAME`. See [`crate::session_store::migration`].
const TAB_SESSIONS_FILE_NAME: &str = "tab_sessions.json";
/// Directory name under `APP_CONFIG_DIR_NAME` that holds user theme files,
/// one JSON file per theme named after [`ThemeConfig::name`]. See
/// [`crate::theme_resolve`].
const THEMES_DIR_NAME: &str = "themes";
/// Directory name under the OS data dir that holds every zsql session
/// directory: `dirs::data_dir().join(APP_DATA_DIR_NAME)`.
const APP_DATA_DIR_NAME: &str = "zsql";
/// Directory name under `APP_DATA_DIR_NAME` that holds one subdirectory per
/// connection's tab session. See [`crate::session_store`].
const SESSIONS_DIR_NAME: &str = "sessions";
/// Directory name under `APP_DATA_DIR_NAME` that holds the shared library's
/// flat pool of `.sql` files. See [`crate::session_store::library`].
const LIBRARY_DIR_NAME: &str = "library";

/// The standard SSH port, used to fill in a connection form's SSH tunnel
/// port when left empty.
pub const DEFAULT_SSH_TUNNEL_PORT: u16 = 22;

/// Fallback tab title for a file opened via Browse whose path carries no
/// usable file name.
pub const UNTITLED_SCRIPT_NAME: &str = "untitled.sql";

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
    /// Timing for transient status-bar messages.
    pub status: StatusConfig,
    /// Timing for autosave/draft persistence.
    pub autosave: AutosaveConfig,
    /// Timing for the sidebar's own periodic housekeeping.
    pub sidebar: SidebarConfig,
    /// The results grid's staged-changes queue: the keybinding that applies
    /// it.
    pub staging: StagingConfig,
}

/// Timing for autosave/draft persistence
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutosaveConfig {
    /// Debounce interval, in milliseconds, between an edit and the notify
    /// it schedules.
    pub edit_debounce_ms: u64,
}

const DEFAULT_EDIT_DEBOUNCE_MS: u64 = 400;

impl Default for AutosaveConfig {
    fn default() -> Self {
        Self {
            edit_debounce_ms: DEFAULT_EDIT_DEBOUNCE_MS,
        }
    }
}

impl AutosaveConfig {
    /// [`Self::edit_debounce_ms`] as a [`Duration`].
    #[must_use]
    pub fn edit_debounce(&self) -> Duration {
        Duration::from_millis(self.edit_debounce_ms)
    }
}

/// Timing for transient status-bar messages, e.g. the "saved <file>"
/// confirmation shown after an explicit script save.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusConfig {
    /// How long the post-save confirmation stays visible before clearing
    /// itself, in milliseconds.
    pub save_confirmation_duration_ms: u64,
}

const DEFAULT_SAVE_CONFIRMATION_DURATION_MS: u64 = 2_500;

impl Default for StatusConfig {
    fn default() -> Self {
        Self {
            save_confirmation_duration_ms: DEFAULT_SAVE_CONFIRMATION_DURATION_MS,
        }
    }
}

impl StatusConfig {
    /// [`Self::save_confirmation_duration_ms`] as a [`Duration`].
    #[must_use]
    pub fn save_confirmation_duration(&self) -> Duration {
        Duration::from_millis(self.save_confirmation_duration_ms)
    }
}

/// Timing for the sidebar's own periodic housekeeping.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SidebarConfig {
    /// How often the Scripts pane recomputes every row's relative-modified-
    /// time label ("2m", "3h", ...), in milliseconds
    pub scripts_relative_time_refresh_ms: u64,
}

const DEFAULT_SCRIPTS_RELATIVE_TIME_REFRESH_MS: u64 = 30_000;

impl Default for SidebarConfig {
    fn default() -> Self {
        Self {
            scripts_relative_time_refresh_ms: DEFAULT_SCRIPTS_RELATIVE_TIME_REFRESH_MS,
        }
    }
}

impl SidebarConfig {
    /// [`Self::scripts_relative_time_refresh_ms`] as a [`Duration`].
    #[must_use]
    pub fn scripts_relative_time_refresh(&self) -> Duration {
        Duration::from_millis(self.scripts_relative_time_refresh_ms)
    }
}

/// The results grid's staged-changes queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StagingConfig {
    /// The key chord that applies the staged-changes queue, in `gpui`
    /// keystroke syntax (e.g. `ctrl-shift-enter`).
    pub apply_keybinding: String,
}

const DEFAULT_STAGING_APPLY_KEYBINDING: &str = "ctrl-shift-enter";

impl Default for StagingConfig {
    fn default() -> Self {
        Self {
            apply_keybinding: DEFAULT_STAGING_APPLY_KEYBINDING.to_owned(),
        }
    }
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
    /// Default `LIMIT` applied to table quick-previews, and the page size a
    /// fresh preview's pager starts at.
    pub preview_limit: u64,
    /// The page sizes a preview's pager cycles through, in order. The
    /// results bar's page-size control never hardcodes these numbers.
    pub preview_page_sizes: Vec<u64>,
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
    /// Width every entry in the scrollable tab strip renders at, and the
    /// unit the strip's active-tab-scroll-into-view logic uses to locate a
    /// tab's position along the strip.
    pub tab_width: Pixels,
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
            tab_width: px(160.0),
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
            preview_page_sizes: vec![100, 200, 500, 1000],
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

    /// Path to the legacy, pre-migration tab-session store
    /// (`$XDG_CONFIG_HOME/zsql/tab_sessions.json`), if resolvable. Lives
    /// alongside [`Config::default_path`], under the same app config
    /// directory. Superseded by [`Config::sessions_dir`]; this path is only
    /// ever consulted by the one-time migration in
    /// [`crate::session_store::migration`].
    #[must_use]
    pub fn tab_sessions_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join(APP_CONFIG_DIR_NAME).join(TAB_SESSIONS_FILE_NAME))
    }

    /// Directory holding every connection's tab-session subdirectory
    /// (`$XDG_DATA_HOME/zsql/sessions/`), if resolvable
    #[must_use]
    pub fn sessions_dir() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join(APP_DATA_DIR_NAME).join(SESSIONS_DIR_NAME))
    }

    /// Directory holding the shared library's flat pool of `.sql` files
    /// (`$XDG_DATA_HOME/zsql/library/`), if resolvable
    #[must_use]
    pub fn library_dir() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join(APP_DATA_DIR_NAME).join(LIBRARY_DIR_NAME))
    }

    /// Directory holding user theme files
    /// (`$XDG_CONFIG_HOME/zsql/themes/`), if resolvable
    #[must_use]
    pub fn themes_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join(APP_CONFIG_DIR_NAME).join(THEMES_DIR_NAME))
    }

    /// The "Somewhere else..." save dialog's default starting directory
    #[must_use]
    pub fn default_export_dir() -> Option<PathBuf> {
        dirs::document_dir().or_else(dirs::home_dir)
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
    fn sessions_dir_ends_in_zsql_sessions_when_resolvable() {
        let Some(path) = Config::sessions_dir() else {
            return;
        };
        assert!(path.ends_with("zsql/sessions"));
    }

    #[test]
    fn sessions_dir_lives_under_the_data_dir_not_the_config_dir() {
        let (Some(sessions_dir), Some(config_dir)) = (Config::sessions_dir(), dirs::config_dir())
        else {
            return;
        };
        if let Some(data_dir) = dirs::data_dir() {
            assert!(sessions_dir.starts_with(&data_dir));
        }
        if data_dir_differs_from_config_dir(&config_dir) {
            assert!(!sessions_dir.starts_with(&config_dir));
        }
    }

    /// Some platforms (notably plain XDG setups without `XDG_DATA_HOME` set)
    /// can resolve the same directory for both roots; the "not under the
    /// config dir" assertion only makes sense when they actually differ.
    fn data_dir_differs_from_config_dir(config_dir: &std::path::Path) -> bool {
        dirs::data_dir().is_some_and(|data_dir| data_dir != config_dir)
    }

    #[test]
    fn library_dir_ends_in_zsql_library_when_resolvable() {
        let Some(path) = Config::library_dir() else {
            return;
        };
        assert!(path.ends_with("zsql/library"));
    }

    #[test]
    fn library_dir_lives_under_the_data_dir_not_the_config_dir() {
        let (Some(library_dir), Some(config_dir)) = (Config::library_dir(), dirs::config_dir())
        else {
            return;
        };
        if let Some(data_dir) = dirs::data_dir() {
            assert!(library_dir.starts_with(&data_dir));
        }
        if data_dir_differs_from_config_dir(&config_dir) {
            assert!(!library_dir.starts_with(&config_dir));
        }
    }

    #[test]
    fn library_dir_is_a_sibling_of_sessions_dir() {
        let (Some(library_dir), Some(sessions_dir)) =
            (Config::library_dir(), Config::sessions_dir())
        else {
            return;
        };
        assert_eq!(library_dir.parent(), sessions_dir.parent());
    }

    #[test]
    fn save_confirmation_duration_defaults_to_a_positive_span() {
        let cfg = Config::default();
        assert!(cfg.status.save_confirmation_duration_ms > 0);
        assert_eq!(
            cfg.status.save_confirmation_duration(),
            Duration::from_millis(cfg.status.save_confirmation_duration_ms)
        );
    }

    #[test]
    fn status_config_round_trips_through_toml() {
        let mut cfg = Config::default();
        cfg.status.save_confirmation_duration_ms = 9_001;

        let text = toml::to_string(&cfg).expect("config must serialize to toml");
        let parsed: Config = toml::from_str(&text).expect("config must parse back from toml");

        assert_eq!(parsed.status.save_confirmation_duration_ms, 9_001);
    }

    #[test]
    fn status_section_is_optional_in_toml_and_falls_back_to_defaults() {
        let parsed: Config =
            toml::from_str("[connection]\ndefault_url = \"postgres://localhost/db\"\n")
                .expect("config without a [status] section must still parse");
        assert_eq!(
            parsed.status.save_confirmation_duration_ms,
            Config::default().status.save_confirmation_duration_ms
        );
    }

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
    fn preview_page_sizes_default_to_the_four_documented_choices_in_order() {
        assert_eq!(
            Config::default().query.preview_page_sizes,
            vec![100, 200, 500, 1000]
        );
    }

    #[test]
    fn preview_page_sizes_round_trip_through_toml() {
        let mut cfg = Config::default();
        cfg.query.preview_page_sizes = vec![50, 250];

        let text = toml::to_string(&cfg).expect("config must serialize to toml");
        let parsed: Config = toml::from_str(&text).expect("config must parse back from toml");

        assert_eq!(parsed.query.preview_page_sizes, vec![50, 250]);
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
        cfg.layout.tab_width = gpui::px(180.0);

        let text = toml::to_string(&cfg).expect("config must serialize to toml");
        let parsed: Config = toml::from_str(&text).expect("config must parse back from toml");

        assert_eq!(parsed.layout.sidebar_default_width, gpui::px(340.0));
        assert_eq!(parsed.layout.sidebar_min_width, gpui::px(200.0));
        assert_eq!(parsed.layout.sidebar_max_width, gpui::px(600.0));
        assert_eq!(parsed.layout.editor_default_height, gpui::px(420.0));
        assert_eq!(parsed.layout.editor_min_height, gpui::px(150.0));
        assert_eq!(parsed.layout.results_min_height, gpui::px(140.0));
        assert_eq!(parsed.layout.divider_thickness, gpui::px(8.0));
        assert_eq!(parsed.layout.tab_width, gpui::px(180.0));
    }

    #[test]
    fn tab_width_defaults_to_a_positive_size() {
        assert!(Config::default().layout.tab_width > gpui::px(0.0));
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
