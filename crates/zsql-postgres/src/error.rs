//! Mapping from `sqlx::Error` to the driver-agnostic [`zsql_core::CoreError`].

use zsql_core::CoreError;

/// Convert a `sqlx::Error` encountered while establishing (or verifying) a
/// connection into a [`CoreError::Connection`], with a short, useful
/// description
pub(crate) fn map_connect_error(err: sqlx::Error) -> CoreError {
    CoreError::Connection(describe(err))
}

/// Render a short, useful description of a connect-phase sqlx error.
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
        other => format!("connection failed: {other}"),
    }
}

/// Convert a `sqlx::Error` encountered while streaming a query's results
/// into a [`CoreError::Query`], with a short, useful description.
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
        other => format!("query failed: {other}"),
    }
}

/// Convert a `sqlx::Error` encountered while introspecting the schema into a
/// [`CoreError::Introspection`], with a short, useful description.
pub(crate) fn map_introspect_error(err: sqlx::Error) -> CoreError {
    CoreError::Introspection(describe_introspect(err))
}

/// Render a short, useful description of an introspection-phase sqlx error.
fn describe_introspect(err: sqlx::Error) -> String {
    match err {
        sqlx::Error::Database(db_err) => {
            format!(
                "database rejected introspection query: {}",
                db_err.message()
            )
        }
        sqlx::Error::Io(io_err) => format!("network error: {io_err}"),
        sqlx::Error::Protocol(msg) => format!("protocol error: {msg}"),
        sqlx::Error::RowNotFound => "introspection query returned no rows".to_owned(),
        sqlx::Error::ColumnNotFound(name) => format!("column not found: {name}"),
        sqlx::Error::ColumnDecode { index, source } => {
            format!("failed to decode column {index}: {source}")
        }
        sqlx::Error::PoolTimedOut => "timed out waiting for a pooled connection".to_owned(),
        sqlx::Error::PoolClosed => "connection pool is closed".to_owned(),
        sqlx::Error::WorkerCrashed => "connection pool background worker crashed".to_owned(),
        other => format!("introspection failed: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{map_connect_error, map_introspect_error, map_query_error};
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

    #[test]
    fn pool_timed_out_maps_to_introspection_error_with_useful_message() {
        let mapped = map_introspect_error(sqlx::Error::PoolTimedOut);
        match mapped {
            CoreError::Introspection(msg) => assert!(msg.contains("timed out")),
            other => panic!("expected CoreError::Introspection, got {other:?}"),
        }
    }

    #[test]
    fn io_error_maps_to_introspection_error_with_network_prefix_and_no_added_dsn() {
        let io_err = std::io::Error::other("connect failed: connection refused");
        let mapped = map_introspect_error(sqlx::Error::Io(io_err));
        match mapped {
            CoreError::Introspection(msg) => {
                assert_eq!(msg, "network error: connect failed: connection refused");
                assert!(!msg.contains("postgres://"));
            }
            other => panic!("expected CoreError::Introspection, got {other:?}"),
        }
    }
}
