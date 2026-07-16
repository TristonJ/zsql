//! Engine-neutral connection configuration.

use crate::error::CoreError;

/// Connection configuration. For v0 this wraps a DSN/URL string; fielded parsing
/// (host / port / user / sslmode / …) lands in M1 alongside the real driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnConfig {
    /// The connection URL (e.g. `postgres://user:pass@host:5432/db`).
    pub url: String,
}

impl ConnConfig {
    /// Build a config from a DSN string.
    ///
    /// # Errors
    /// Returns [`CoreError::Dsn`] if the DSN is empty.
    pub fn from_dsn(dsn: &str) -> Result<Self, CoreError> {
        if dsn.trim().is_empty() {
            return Err(CoreError::Dsn("empty DSN".into()));
        }
        Ok(Self {
            url: dsn.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ConnConfig;

    #[test]
    fn from_dsn_rejects_empty() {
        assert!(ConnConfig::from_dsn("   ").is_err());
    }

    #[test]
    fn from_dsn_keeps_url() {
        let cfg = ConnConfig::from_dsn("postgres://localhost/db").unwrap();
        assert_eq!(cfg.url, "postgres://localhost/db");
    }
}
