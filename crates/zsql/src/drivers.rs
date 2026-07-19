//! Registers this binary's concrete [`Driver`] implementations and resolves
//! a connection URL to one of them via [`zsql_core::select_driver`].
//!
//! This is the only module in the `zsql` binary that names
//! `zsql-postgres`, `zsql-sqlite`, and `zsql-mssql`; everything downstream
//! (`session.rs`, the connection-manager UI) goes through [`connect`] and
//! never picks a driver directly.

use std::sync::Arc;

use zsql_core::{Connection, CoreError, Driver};
use zsql_mssql::MssqlDriver;
use zsql_postgres::PostgresDriver;
use zsql_sqlite::SqliteDriver;

/// Build the list of drivers this binary registers.
#[must_use]
pub fn registered_drivers() -> Vec<Arc<dyn Driver>> {
    vec![
        Arc::new(PostgresDriver),
        Arc::new(SqliteDriver),
        Arc::new(MssqlDriver),
    ]
}

/// Resolve `url`'s scheme to a registered driver and connect through it.
///
/// `url` is taken by value (rather than `&str`) so the returned future is
/// self-contained and can be driven on a background executor without
/// borrowing anything from its caller's stack frame.
///
/// # Errors
/// Returns [`CoreError::Dsn`] if `url` is empty or its scheme has no
/// registered driver, or whatever error the selected driver's own
/// `parse_dsn`/`connect` returns.
#[tracing::instrument(name = "connect_via_selected_driver", skip_all)]
pub async fn connect(url: String) -> Result<Box<dyn Connection>, CoreError> {
    let drivers = registered_drivers();
    let driver = zsql_core::select_driver(&drivers, &url)?;
    tracing::info!(driver = driver.id(), "driver selected for connection");
    let cfg = driver.parse_dsn(&url)?;
    driver.connect(&cfg).await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{connect, registered_drivers};

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        futures::executor::block_on(fut)
    }

    #[test]
    fn registered_drivers_include_postgres_and_sqlite() {
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
    }

    #[test]
    fn connect_opens_a_sqlite_in_memory_database_through_selection() {
        let _guard = crate::test_support::serialize_real_io();
        let conn = block_on(connect("sqlite::memory:".to_owned()))
            .expect("sqlite connect through selection should succeed");
        drop(conn);
    }

    #[test]
    fn connect_rejects_an_unrecognized_scheme_without_naming_the_full_url() {
        let result = block_on(connect("cassandra://secret-password@host/db".to_owned()));
        match result {
            Err(zsql_core::CoreError::Dsn(message)) => {
                assert!(message.contains("cassandra"), "message: {message}");
                assert!(
                    !message.contains("secret-password"),
                    "message must not leak the full URL: {message}"
                );
            }
            Err(other) => {
                panic!("expected CoreError::Dsn for an unrecognized scheme, got {other:?}")
            }
            Ok(_) => panic!("expected an unrecognized scheme to fail"),
        }
    }

    #[test]
    fn connect_rejects_an_empty_url_with_a_dsn_error() {
        let result = block_on(connect(String::new()));
        assert!(matches!(result, Err(zsql_core::CoreError::Dsn(_))));
    }

    /// Live-database test, gated on `ZSQL_TEST_DATABASE_URL` so `cargo test`
    /// passes with no database present.
    #[test]
    fn connect_opens_a_live_postgres_database_through_selection_when_configured() {
        let Ok(url) = std::env::var("ZSQL_TEST_DATABASE_URL") else {
            eprintln!("skipping live test: ZSQL_TEST_DATABASE_URL not set");
            return;
        };
        let _guard = crate::test_support::serialize_real_io();

        let conn =
            block_on(connect(url)).expect("postgres connect through selection should succeed");

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
