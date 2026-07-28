//! Engine-neutral connection configuration.

use std::net::SocketAddr;

use crate::error::CoreError;

/// Rows a sqlx-based driver's query stream groups into one
/// [`crate::driver::QueryEvent::Batch`] before pushing it to the sink, absent
/// an app-level override. Bounded so a large result set streams to the UI
/// incrementally instead of arriving as one huge allocation.
pub const DEFAULT_QUERY_BATCH_SIZE: usize = 500;

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
    /// Rows a streamed query batches at a time, typically the app's
    /// configured `query.batch_size`. Defaults to [`DEFAULT_QUERY_BATCH_SIZE`]
    /// for a caller that has no app-level `Config` to draw from.
    pub batch_size: usize,
}

impl ConnConfig {
    /// Build a config from a URL string, with no tunnel, at the default
    /// batch size.
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
            batch_size: DEFAULT_QUERY_BATCH_SIZE,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnConfig, DEFAULT_QUERY_BATCH_SIZE};

    #[test]
    fn from_url_rejects_empty() {
        assert!(ConnConfig::from_url("   ").is_err());
    }

    #[test]
    fn from_url_keeps_url() {
        let cfg = ConnConfig::from_url("postgres://localhost/db").unwrap();
        assert_eq!(cfg.url, "postgres://localhost/db");
    }

    #[test]
    fn from_url_defaults_batch_size() {
        let cfg = ConnConfig::from_url("postgres://localhost/db").unwrap();
        assert_eq!(cfg.batch_size, DEFAULT_QUERY_BATCH_SIZE);
    }
}
