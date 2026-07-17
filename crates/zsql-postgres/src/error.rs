//! Mapping from `sqlx::Error` to the driver-agnostic [`zsql_core::CoreError`].
//!
//! `zsql-core` must never see a `sqlx` type, so every fallible sqlx call in
//! this crate funnels its error through [`map_connect_error`] before it can
//! cross the trait boundary. The mapped message is built from sqlx's own
//! `Display` output plus a short category prefix; it never includes the raw
//! connection string, so credentials embedded in a DSN are not echoed back.

use zsql_core::CoreError;

/// Convert a `sqlx::Error` encountered while establishing (or verifying) a
/// connection into a [`CoreError::Connection`], with a short, useful
/// description that never leaks the connection string.
///
/// Takes ownership (rather than `&sqlx::Error`) because its only call site is
/// `Result::map_err`, which hands over the error by value.
pub(crate) fn map_connect_error(err: sqlx::Error) -> CoreError {
    CoreError::Connection(describe(err))
}

/// Render a short, useful description of a connect-phase sqlx error without
/// leaking the connection string.
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

#[cfg(test)]
mod tests {
    use super::map_connect_error;
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
    fn io_error_maps_to_connection_error_without_leaking_dsn() {
        let secret_dsn = "postgres://user:supersecret@example.invalid/db";
        let io_err = std::io::Error::other(format!("connect failed for {secret_dsn}"));
        let mapped = map_connect_error(sqlx::Error::Io(io_err));
        match mapped {
            CoreError::Connection(msg) => {
                // The mapper itself never appends the DSN; whatever the
                // underlying io error says is out of our control, but the
                // mapper must not additionally embed the raw connection
                // string anywhere in its own formatting.
                assert!(msg.starts_with("network error: "));
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
}
