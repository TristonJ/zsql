//! Engine-neutral connection configuration.

use std::net::SocketAddr;

use crate::error::CoreError;

/// Connection configuration. For now this wraps a URL/URL string as-is
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnConfig {
    /// The connection URL (e.g. `postgres://user:pass@host:5432/db`).
    pub url: String,
    /// When set, the caller has already opened a local tunnel and this is
    /// its loopback address: a network driver dials this instead of `url`'s
    /// own host:port, per that driver's own tunneled-connect translation.
    /// `None` for an untunneled connect, which is behaviorally identical to
    /// this field never having existed.
    pub tunnel_local_addr: Option<SocketAddr>,
}

impl ConnConfig {
    /// Build a config from a URL string, with no tunnel.
    ///
    /// # Errors
    /// Returns [`CoreError::Url`] if the URL is empty.
    pub fn from_url(url: &str) -> Result<Self, CoreError> {
        if url.trim().is_empty() {
            return Err(CoreError::Url("empty URL".into()));
        }
        Ok(Self {
            url: url.to_owned(),
            tunnel_local_addr: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ConnConfig;

    #[test]
    fn from_url_rejects_empty() {
        assert!(ConnConfig::from_url("   ").is_err());
    }

    #[test]
    fn from_url_keeps_url() {
        let cfg = ConnConfig::from_url("postgres://localhost/db").unwrap();
        assert_eq!(cfg.url, "postgres://localhost/db");
    }
}
