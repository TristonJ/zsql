//! Mapping from `sqlx::Error` to the driver-agnostic [`zsql_core::CoreError`].
//!
//! `zsql-core` must never see a `sqlx` type, so every fallible sqlx call in
//! this crate funnels its error through [`map_connect_error`] before it can
//! cross the trait boundary. The mapped message is built from sqlx's own
//! `Display` output plus a short category prefix; the mapper itself never
//! appends the connection string. It does not scrub the underlying sqlx
//! error's own text, so if some future sqlx error variant embedded a DSN in
//! its `Display` output, that text would still surface here unredacted.

use zsql_core::CoreError;

/// Convert a `sqlx::Error` encountered while establishing (or verifying) a
/// connection into a [`CoreError::Connection`], with a short, useful
/// description. The mapper adds no connection string of its own; it only
/// prepends a category prefix to sqlx's own `Display` text.
///
/// Takes ownership (rather than `&sqlx::Error`) because its only call site is
/// `Result::map_err`, which hands over the error by value.
pub(crate) fn map_connect_error(err: sqlx::Error) -> CoreError {
    CoreError::Connection(describe(err))
}

/// Render a short, useful description of a connect-phase sqlx error. Adds a
/// category prefix but introduces no connection string of its own.
fn describe(err: sqlx::Error) -> String {
    match err {
        sqlx::Error::Database(db_err) => {
            format!("database rejected connection: {}", db_err.message())
        }
        sqlx::Error::Io(io_err) => format!("network error: {io_err}"),
        sqlx::Error::Tls(tls_err) => format!("TLS error: {tls_err}"),
        sqlx::Error::Configuration(cfg_err) => {
            format!("invalid connection configuration: {cfg_err}")
        }
        sqlx::Error::PoolTimedOut => "timed out waiting for a pooled connection".to_owned(),
        sqlx::Error::PoolClosed => "connection pool is closed".to_owned(),
        sqlx::Error::WorkerCrashed => "connection pool background worker crashed".to_owned(),
        // `sqlx::Error` is `#[non_exhaustive]`; every named variant relevant
        // to the connect phase is matched above, so this is a real catch-all
        // for whatever sqlx adds later, not dead code.
        other => format!("connection failed: {other}"),
    }
}

/// Convert a `sqlx::Error` encountered while streaming a query's results
/// into a [`CoreError::Query`], with a short, useful description. Like
/// [`map_connect_error`], it adds no SQL text or connection string of its
/// own; it only summarizes the sqlx error itself with a category prefix.
pub(crate) fn map_query_error(err: sqlx::Error) -> CoreError {
    CoreError::Query(describe_query(err))
}

/// Render a short, useful description of a query-phase sqlx error.
fn describe_query(err: sqlx::Error) -> String {
    match err {
        sqlx::Error::Database(db_err) => {
            format!("database rejected query: {}", db_err.message())
        }
        sqlx::Error::Io(io_err) => format!("network error: {io_err}"),
        sqlx::Error::Protocol(msg) => format!("protocol error: {msg}"),
        sqlx::Error::RowNotFound => "query returned no rows".to_owned(),
        sqlx::Error::ColumnNotFound(name) => format!("column not found: {name}"),
        sqlx::Error::ColumnDecode { index, source } => {
            format!("failed to decode column {index}: {source}")
        }
        sqlx::Error::PoolTimedOut => "timed out waiting for a pooled connection".to_owned(),
        sqlx::Error::PoolClosed => "connection pool is closed".to_owned(),
        sqlx::Error::WorkerCrashed => "connection pool background worker crashed".to_owned(),
        // `sqlx::Error` is `#[non_exhaustive]`; every named variant relevant
        // to the query phase is matched above, so this is a real catch-all
        // for whatever sqlx adds later, not dead code.
        other => format!("query failed: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{map_connect_error, map_query_error};
    use zsql_core::CoreError;

    #[test]
    fn pool_timed_out_maps_to_connection_error_with_useful_message() {
        let mapped = map_connect_error(sqlx::Error::PoolTimedOut);
        match mapped {
            CoreError::Connection(msg) => assert!(msg.contains("timed out")),
            other => panic!("expected CoreError::Connection, got {other:?}"),
        }
    }

    #[test]
    fn pool_closed_maps_to_connection_error() {
        let mapped = map_connect_error(sqlx::Error::PoolClosed);
        assert!(matches!(mapped, CoreError::Connection(_)));
    }

    #[test]
    fn io_error_maps_to_connection_error_with_network_prefix_and_no_added_dsn() {
        // The mapper adds no connection string of its own: it only prepends
        // a fixed category prefix to whatever the underlying io error says.
        // This test uses a DSN-free io error precisely so the assertion can
        // prove that property -- if the mapper appended a DSN, it would show
        // up here even though this io error never mentioned one.
        let io_err = std::io::Error::other("connect failed: connection refused");
        let mapped = map_connect_error(sqlx::Error::Io(io_err));
        match mapped {
            CoreError::Connection(msg) => {
                assert_eq!(msg, "network error: connect failed: connection refused");
                assert!(!msg.contains("postgres://"));
            }
            other => panic!("expected CoreError::Connection, got {other:?}"),
        }
    }

    #[test]
    fn configuration_error_maps_to_connection_error() {
        let cfg_err: sqlx::error::BoxDynError = "bad ssl mode".into();
        let mapped = map_connect_error(sqlx::Error::Configuration(cfg_err));
        match mapped {
            CoreError::Connection(msg) => assert!(msg.contains("invalid connection configuration")),
            other => panic!("expected CoreError::Connection, got {other:?}"),
        }
    }

    #[test]
    fn row_not_found_maps_to_query_error() {
        let mapped = map_query_error(sqlx::Error::RowNotFound);
        match mapped {
            CoreError::Query(msg) => assert!(msg.contains("no rows")),
            other => panic!("expected CoreError::Query, got {other:?}"),
        }
    }

    #[test]
    fn column_not_found_maps_to_query_error_with_column_name() {
        let mapped = map_query_error(sqlx::Error::ColumnNotFound("email".to_owned()));
        match mapped {
            CoreError::Query(msg) => assert!(msg.contains("email")),
            other => panic!("expected CoreError::Query, got {other:?}"),
        }
    }

    #[test]
    fn io_error_maps_to_query_error_with_network_prefix_and_no_added_dsn() {
        // Same guarantee as the connect-side test above: the mapper adds no
        // connection string of its own, only a fixed category prefix.
        let io_err = std::io::Error::other("connect failed: connection refused");
        let mapped = map_query_error(sqlx::Error::Io(io_err));
        match mapped {
            CoreError::Query(msg) => {
                assert_eq!(msg, "network error: connect failed: connection refused");
                assert!(!msg.contains("postgres://"));
            }
            other => panic!("expected CoreError::Query, got {other:?}"),
        }
    }

    #[test]
    fn pool_timed_out_maps_to_query_error() {
        let mapped = map_query_error(sqlx::Error::PoolTimedOut);
        assert!(matches!(mapped, CoreError::Query(_)));
    }
}
