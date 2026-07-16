//! Typed errors for the core contract.

use thiserror::Error;

/// Errors surfaced across the driver boundary.
#[derive(Debug, Error)]
pub enum CoreError {
    /// Failed to establish or maintain a connection.
    #[error("connection error: {0}")]
    Connection(String),

    /// A query failed to execute.
    #[error("query error: {0}")]
    Query(String),

    /// Schema introspection failed.
    #[error("introspection error: {0}")]
    Introspection(String),

    /// A DSN could not be parsed for the target backend.
    #[error("invalid DSN: {0}")]
    Dsn(String),
}
