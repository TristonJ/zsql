//! Registers this binary's concrete [`Driver`] implementations and resolves
//! a connection URL to one of them via [`zsql_core::select_driver`].
//!
//! This is the only module in the `zsql` binary that names
//! `zsql-postgres`, `zsql-sqlite`, `zsql-mssql`, and `zsql-mysql`;
//! everything downstream (`session.rs`, the connection-manager UI) goes
//! through [`connect`] and never picks a driver directly.

use std::net::SocketAddr;
use std::sync::{Arc, LazyLock};

use zsql_core::{Connection, CoreError, Driver};
use zsql_mssql::MssqlDriver;
use zsql_mysql::MysqlDriver;
use zsql_postgres::PostgresDriver;
use zsql_sqlite::SqliteDriver;

/// Fallback driver id when a connection URL cannot be resolved to a
/// registered driver (e.g. nothing is connected yet): standard SQL string
/// literal escaping and `:name`-only parameter detection, never a specific
/// driver's own dialect.
pub const UNKNOWN_DRIVER_ID: &str = "unknown";

/// This binary's registered drivers, built once and shared by every caller.
static REGISTERED_DRIVERS: LazyLock<Vec<Arc<dyn Driver>>> = LazyLock::new(|| {
    vec![
        Arc::new(PostgresDriver),
        Arc::new(SqliteDriver),
        Arc::new(MssqlDriver),
        Arc::new(MysqlDriver),
    ]
});

/// The list of drivers this binary registers.
#[must_use]
pub fn registered_drivers() -> Vec<Arc<dyn Driver>> {
    REGISTERED_DRIVERS.clone()
}

/// Resolve `url`'s scheme to a registered driver and connect through it,
/// with the resulting connection's row-batching set to `batch_size`
/// (typically the app's configured `query.batch_size`).
///
/// `url` is taken by value (rather than `&str`) so the returned future is
/// self-contained and can be driven on a background executor without
/// borrowing anything from its caller's stack frame.
///
/// # Errors
/// Returns [`CoreError::Url`] if `url` is empty or its scheme has no
/// registered driver, or whatever error the selected driver's own
/// `parse_url`/`connect` returns.
#[tracing::instrument(name = "connect_via_selected_driver", skip_all)]
pub async fn connect(url: String, batch_size: usize) -> Result<Box<dyn Connection>, CoreError> {
    let drivers = registered_drivers();
    let driver = zsql_core::select_driver(&drivers, &url)?;
    tracing::info!(driver = driver.id(), "driver selected for connection");
    let mut cfg = driver.parse_url(&url)?;
    cfg.batch_size = batch_size;
    driver.connect(&cfg).await
}

/// Resolve `url`'s scheme to a registered driver and connect through it,
/// dialing `tunnel_addr` (an already-open local tunnel's loopback address)
/// instead of `url`'s own host:port, with the resulting connection's
/// row-batching set to `batch_size`. Each registered driver translates the
/// tunnel dial into its own client library's terms -- see each driver
/// crate's `tunnel` module for the specifics.
///
/// # Errors
/// Returns [`CoreError::Url`] if `url` is empty or its scheme has no
/// registered driver, or whatever error the selected driver's own
/// `parse_url`/`connect` returns.
#[tracing::instrument(name = "connect_via_selected_driver_tunneled", skip_all)]
pub async fn connect_tunneled(
    url: String,
    tunnel_addr: SocketAddr,
    batch_size: usize,
) -> Result<Box<dyn Connection>, CoreError> {
    let drivers = registered_drivers();
    let driver = zsql_core::select_driver(&drivers, &url)?;
    tracing::info!(
        driver = driver.id(),
        "driver selected for tunneled connection"
    );
    let mut cfg = driver.parse_url(&url)?;
    cfg.tunnel_local_addr = Some(tunnel_addr);
    cfg.batch_size = batch_size;
    driver.connect(&cfg).await
}

/// Detect the driver id `url` would resolve to
///
/// # Errors
/// Returns [`CoreError::Url`] if `url` has no recognizable scheme, or its
/// scheme has no registered driver.
pub fn detect_driver_id(url: &str) -> Result<&'static str, CoreError> {
    let drivers = registered_drivers();
    zsql_core::select_driver(&drivers, url).map(|driver| driver.id())
}

/// Detect the driver name `url` would resolve to
///
/// # Errors
/// Returns [`CoreError::Url`] if `url` has no recognizable scheme, or its
/// scheme has no registered driver.
pub fn detect_driver_name(url: &str) -> Result<&'static str, CoreError> {
    let drivers = registered_drivers();
    zsql_core::select_driver(&drivers, url).map(|driver| driver.display_name())
}

/// Detect the driver default port `url` would resolve to
pub fn detect_driver_default_port(url: &str) -> Result<Option<u16>, CoreError> {
    let drivers = registered_drivers();
    zsql_core::select_driver(&drivers, url).map(|driver| driver.default_port())
}

/// Whether `driver_id` names a network driver (i.e. one that uses a TCP port such
/// as postgres) not a file-based driver (such as sqlite). None for an unrecognized
/// driver id.
#[must_use]
pub fn is_network(driver_id: &str) -> Option<bool> {
    let drivers = registered_drivers();
    drivers
        .iter()
        .find(|driver| driver.id() == driver_id)
        .map(|driver| driver.is_networked())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{connect, registered_drivers};

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        futures::executor::block_on(fut)
    }

    #[test]
    fn registered_drivers_include_postgres_sqlite_mssql_and_mysql() {
        let ids: Vec<&str> = registered_drivers().iter().map(|d| d.id()).collect();
        assert!(
            ids.contains(&"postgres"),
            "expected a registered postgres driver: {ids:?}"
        );
        assert!(
            ids.contains(&"sqlite"),
            "expected a registered sqlite driver: {ids:?}"
        );
        assert!(
            ids.contains(&"mssql"),
            "expected a registered mssql driver: {ids:?}"
        );
        assert!(
            ids.contains(&"mysql"),
            "expected a registered mysql driver: {ids:?}"
        );
    }

    #[test]
    fn connect_opens_a_sqlite_in_memory_database_through_selection() {
        let _guard = crate::test_support::serialize_real_io();
        let conn = block_on(connect(
            "sqlite::memory:".to_owned(),
            zsql_core::DEFAULT_QUERY_BATCH_SIZE,
        ))
        .expect("sqlite connect through selection should succeed");
        drop(conn);
    }

    #[test]
    fn connect_routes_a_mariadb_scheme_to_the_registered_mysql_driver() {
        // An unreachable host still proves routing: a `CoreError::Url`
        // would mean `mariadb://` never resolved to a registered driver at
        // all, whereas a `CoreError::Connection` means it resolved (to the
        // `mysql` driver, the only one registered for that scheme) and then
        // failed to actually reach the host.
        let result = block_on(connect(
            "mariadb://user:pass@zsql-test-nonexistent-host.invalid/db".to_owned(),
            zsql_core::DEFAULT_QUERY_BATCH_SIZE,
        ));
        match result {
            Err(zsql_core::CoreError::Connection { .. }) => {}
            Err(other) => panic!("expected a CoreError::Connection, got {other:?}"),
            Ok(_) => panic!("connecting to an unreachable host must fail"),
        }
    }

    #[test]
    fn connect_rejects_an_unrecognized_scheme_without_naming_the_full_url() {
        let result = block_on(connect(
            "cassandra://secret-password@host/db".to_owned(),
            zsql_core::DEFAULT_QUERY_BATCH_SIZE,
        ));
        match result {
            Err(zsql_core::CoreError::Url(message)) => {
                assert!(message.contains("cassandra"), "message: {message}");
                assert!(
                    !message.contains("secret-password"),
                    "message must not leak the full URL: {message}"
                );
            }
            Err(other) => {
                panic!("expected CoreError::Url for an unrecognized scheme, got {other:?}")
            }
            Ok(_) => panic!("expected an unrecognized scheme to fail"),
        }
    }

    #[test]
    fn connect_rejects_an_empty_url_with_a_url_error() {
        let result = block_on(connect(String::new(), zsql_core::DEFAULT_QUERY_BATCH_SIZE));
        assert!(matches!(result, Err(zsql_core::CoreError::Url(_))));
    }

    /// Live-database test
    #[test]
    fn connect_opens_a_live_sqlite_database_through_selection_when_configured() {
        let url = "sqlite::memory:".to_string();
        let _guard = crate::test_support::serialize_real_io();

        let conn = block_on(connect(url, zsql_core::DEFAULT_QUERY_BATCH_SIZE))
            .expect("connect through selection should succeed");

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT 1 AS one".to_owned(), tx);

        let mut observed_value = None;
        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(zsql_core::QueryEvent::Batch(batch))) => {
                    observed_value = batch.rows.first().map(|row| row.0[0].clone());
                }
                Ok(Ok(zsql_core::QueryEvent::Done { .. })) => break,
                Ok(Ok(zsql_core::QueryEvent::Columns(_))) => {}
                Ok(Err(err)) => panic!("SELECT 1 failed: {err}"),
                Err(err) => panic!("SELECT 1 did not complete: {err}"),
            }
        }

        assert_eq!(
            observed_value,
            Some(zsql_core::Value::Int(1)),
            "SELECT 1 through the selection-based connect path must actually return 1"
        );
    }
}
