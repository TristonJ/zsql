//! Typed errors for the core contract.

use std::sync::Arc;

use thiserror::Error;

/// Errors surfaced across the driver boundary.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Failed to establish or maintain a connection.
    #[error("connection error: {message}")]
    Connection {
        message: String,
        /// Whether retying without user action could plausibly succeed
        transient: bool,
        #[source]
        source: Option<Arc<dyn std::error::Error + Send + Sync>>,
    },

    /// A query failed to execute.
    #[error("query error: {message}")]
    Query {
        message: String,
        /// Backend error code, if available.
        code: Option<String>,
        /// Character offset into the SQL statement where the error occurred, if available.
        position: Option<usize>,
        #[source]
        source: Option<Arc<dyn std::error::Error + Send + Sync>>,
    },

    /// Schema introspection failed.
    #[error("introspection error: {message}")]
    Introspection {
        message: String,
        #[source]
        source: Option<Arc<dyn std::error::Error + Send + Sync>>,
    },

    /// A URL could not be parsed for the target backend.
    #[error("invalid URL: {0}")]
    Url(String),
}

impl CoreError {
    /// Construct a [`CoreError::Connection`] with the given message and no source.
    pub fn connection(message: impl Into<String>, transient: bool) -> Self {
        CoreError::Connection {
            message: message.into(),
            transient,
            source: None,
        }
    }

    /// Construct a [`CoreError::Query`] with the given message and no source.
    pub fn query(message: impl Into<String>) -> Self {
        CoreError::Query {
            message: message.into(),
            code: None,
            position: None,
            source: None,
        }
    }

    /// Construct a [`CoreError::Introspection`] with the given message and no source.
    pub fn introspection(message: impl Into<String>) -> Self {
        CoreError::Introspection {
            message: message.into(),
            source: None,
        }
    }
}
