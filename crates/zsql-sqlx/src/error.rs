use std::sync::Arc;

use zsql_core::CoreError;

/// Map a [`sqlx::Error`] into a [`CoreError::Connection`]
pub fn map_sqlx_connection_error(err: sqlx::Error) -> CoreError {
    CoreError::Connection {
        message: describe_connection_error(&err),
        transient: is_connection_error_transient(&err),
        source: Some(Arc::new(err)),
    }
}

/// Render a short, useful description of a connect-phase sqlx error.
fn describe_connection_error(err: &sqlx::Error) -> String {
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

/// Whether a connection error is possibly transient
fn is_connection_error_transient(err: &sqlx::Error) -> bool {
    matches!(
        err,
        sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::PoolTimedOut
            | sqlx::Error::WorkerCrashed
    )
}

/// Convert a [`sqlx::Error`] into a [`CoreError::Query`]
pub fn map_sqlx_query_error(err: sqlx::Error) -> CoreError {
    CoreError::Query {
        message: describe_query_error(&err),
        code: err
            .as_database_error()
            .and_then(|e| e.code())
            .map(|c| c.to_string()),
        position: query_error_position(&err),
        source: Some(Arc::new(err)),
    }
}

/// Render a short, useful description of a query-phase sqlx error.
fn describe_query_error(err: &sqlx::Error) -> String {
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

/// Try to get the character offset of the error from a [`sqlx::Error`], if available.
fn query_error_position(err: &sqlx::Error) -> Option<usize> {
    if let Some(pg_err) = err
        .as_database_error()
        .and_then(|e| e.try_downcast_ref::<sqlx::postgres::PgDatabaseError>())
    {
        return pg_err.position().map(|pos| match pos {
            sqlx::postgres::PgErrorPosition::Original(n) => n,
            sqlx::postgres::PgErrorPosition::Internal { position, .. } => position,
        });
    }

    None
}

/// Convert a `sqlx::Error` encountered while introspecting the schema into a
/// [`CoreError::Introspection`], with a short, useful description.
pub fn map_sqlx_introspect_error(err: sqlx::Error) -> CoreError {
    CoreError::Introspection {
        message: describe_introspect_error(&err),
        source: Some(Arc::new(err)),
    }
}

/// Render a short, useful description of an introspection-phase sqlx error.
fn describe_introspect_error(err: &sqlx::Error) -> String {
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
    use crate::error::map_sqlx_query_error;

    use super::{map_sqlx_connection_error, map_sqlx_introspect_error};
    use zsql_core::CoreError;

    #[test]
    fn pool_timed_out_maps_to_connection_error_with_useful_message() {
        let mapped = map_sqlx_connection_error(sqlx::Error::PoolTimedOut);
        match mapped {
            CoreError::Connection { message, .. } => assert!(message.contains("timed out")),
            other => panic!("expected CoreError::Connection, got {other:?}"),
        }
    }

    #[test]
    fn pool_closed_maps_to_connection_error() {
        let mapped = map_sqlx_connection_error(sqlx::Error::PoolClosed);
        assert!(matches!(mapped, CoreError::Connection { .. }));
    }

    #[test]
    fn io_error_maps_to_connection_error_with_network_prefix_and_no_added_url() {
        let io_err = std::io::Error::other("connect failed: connection refused");
        let mapped = map_sqlx_connection_error(sqlx::Error::Io(io_err));
        match mapped {
            CoreError::Connection { message, .. } => {
                assert_eq!(message, "network error: connect failed: connection refused");
                assert!(!message.contains("postgres://"));
            }
            other => panic!("expected CoreError::Connection, got {other:?}"),
        }
    }

    #[test]
    fn configuration_error_maps_to_connection_error() {
        let cfg_err: sqlx::error::BoxDynError = "bad ssl mode".into();
        let mapped = map_sqlx_connection_error(sqlx::Error::Configuration(cfg_err));
        match mapped {
            CoreError::Connection { message, .. } => {
                assert!(message.contains("invalid connection configuration"))
            }
            other => panic!("expected CoreError::Connection, got {other:?}"),
        }
    }

    #[test]
    fn row_not_found_maps_to_query_error() {
        let mapped = map_sqlx_query_error(sqlx::Error::RowNotFound);
        match mapped {
            CoreError::Query { message, .. } => assert!(message.contains("no rows")),
            other => panic!("expected CoreError::Query, got {other:?}"),
        }
    }

    #[test]
    fn column_not_found_maps_to_query_error_with_column_name() {
        let mapped = map_sqlx_query_error(sqlx::Error::ColumnNotFound("email".to_owned()));
        match mapped {
            CoreError::Query { message, .. } => assert!(message.contains("email")),
            other => panic!("expected CoreError::Query, got {other:?}"),
        }
    }

    #[test]
    fn io_error_maps_to_query_error_with_network_prefix_and_no_added_url() {
        let io_err = std::io::Error::other("connect failed: connection refused");
        let mapped = map_sqlx_query_error(sqlx::Error::Io(io_err));
        match mapped {
            CoreError::Query { message, .. } => {
                assert_eq!(message, "network error: connect failed: connection refused");
                assert!(!message.contains("postgres://"));
            }
            other => panic!("expected CoreError::Query, got {other:?}"),
        }
    }

    #[test]
    fn pool_timed_out_maps_to_query_error() {
        let mapped = map_sqlx_query_error(sqlx::Error::PoolTimedOut);
        assert!(matches!(mapped, CoreError::Query { .. }));
    }

    #[test]
    fn pool_timed_out_maps_to_introspection_error_with_useful_message() {
        let mapped = map_sqlx_introspect_error(sqlx::Error::PoolTimedOut);
        match mapped {
            CoreError::Introspection { message, .. } => assert!(message.contains("timed out")),
            other => panic!("expected CoreError::Introspection, got {other:?}"),
        }
    }

    #[test]
    fn io_error_maps_to_introspection_error_with_network_prefix_and_no_added_url() {
        let io_err = std::io::Error::other("connect failed: connection refused");
        let mapped = map_sqlx_introspect_error(sqlx::Error::Io(io_err));
        match mapped {
            CoreError::Introspection { message, .. } => {
                assert_eq!(message, "network error: connect failed: connection refused");
                assert!(!message.contains("postgres://"));
            }
            other => panic!("expected CoreError::Introspection, got {other:?}"),
        }
    }
}
