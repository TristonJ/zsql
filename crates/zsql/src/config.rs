//! Application configuration: theme placeholder, query limits, and the other
//! constants

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

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
}

/// Appearance settings. Placeholder for the eventual theming system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    /// Named theme (e.g. `dark`).
    pub name: String,
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
}

/// Connection defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConnectionConfig {
    /// Optional default DSN; the `DATABASE_URL` env var overrides it.
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

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: "dark".to_owned(),
        }
    }
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            batch_size: 500,
            preview_limit: 200,
            statement_timeout_ms: 30_000,
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
        dirs::config_dir().map(|d| d.join("zsql").join("config.toml"))
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
}
