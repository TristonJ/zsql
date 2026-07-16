//! Application configuration: theme placeholder, query limits, and the other
//! constants that must never be hardcoded at call sites. Loaded from TOML with
//! defaults; missing file is not an error.

use std::path::{Path, PathBuf};

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
