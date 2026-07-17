//! The Postgres [`Driver`] and its live [`Connection`] implementation, built
//! on sqlx with the **smol** runtime so its futures await directly on gpui's
//! executor — no tokio runtime, no bridge thread.

use std::time::Duration;

use async_trait::async_trait;
use sqlx::Row as _;
use sqlx::postgres::{PgPool, PgPoolOptions};
use zsql_core::{BatchSink, ConnConfig, Connection, CoreError, Driver, QueryHandle, SchemaTree};

use crate::error::map_connect_error;

/// Bounded pool size for a single desktop client. Small on purpose: this app
/// drives at most a handful of concurrent operations (one running query plus
/// occasional introspection), and a modest ceiling avoids hammering the
/// server from a client that only ever has one user.
const MAX_POOL_CONNECTIONS: u32 = 5;

/// How long to wait for the initial connection (and later, a free pooled
/// connection) before giving up. Bounded so a stuck DSN fails fast instead of
/// hanging the caller indefinitely.
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// The Postgres [`Driver`].
#[derive(Debug, Default, Clone, Copy)]
pub struct PostgresDriver;

impl PostgresDriver {
    /// Build a bounded connection pool for `url` and verify it is reachable
    /// with a trivial liveness query before returning it.
    ///
    /// # Errors
    /// Returns [`CoreError::Connection`] if the pool cannot be built or the
    /// liveness query fails.
    async fn build_pool(url: &str) -> Result<PgPool, CoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(MAX_POOL_CONNECTIONS)
            .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
            .connect(url)
            .await
            .map_err(map_connect_error)?;
        liveness_check(&pool).await?;
        Ok(pool)
    }
}

/// Run a trivial `SELECT 1` against `pool` to confirm the connection is
/// actually usable, not just accepted. Returns the decoded value.
async fn liveness_check(pool: &PgPool) -> Result<i64, CoreError> {
    let row = sqlx::query("SELECT 1 AS one")
        .fetch_one(pool)
        .await
        .map_err(map_connect_error)?;
    let one: i32 = row.try_get("one").map_err(map_connect_error)?;
    Ok(i64::from(one))
}

#[async_trait]
impl Driver for PostgresDriver {
    fn id(&self) -> &'static str {
        "postgres"
    }

    fn display_name(&self) -> &'static str {
        "PostgreSQL"
    }

    fn parse_dsn(&self, dsn: &str) -> Result<ConnConfig, CoreError> {
        ConnConfig::from_dsn(dsn)
    }

    #[tracing::instrument(name = "pg_connect", skip_all, fields(driver = self.id()))]
    async fn connect(&self, cfg: &ConnConfig) -> Result<Box<dyn Connection>, CoreError> {
        // Never log `cfg.url`: it may embed a password. Only non-secret
        // fields (the driver id, above) are attached to this span.
        let pool = Self::build_pool(&cfg.url).await?;
        tracing::info!("postgres connection established");
        Ok(Box::new(PgConnection { pool }))
    }
}

/// A live Postgres connection, backed by a bounded sqlx connection pool.
pub struct PgConnection {
    pool: PgPool,
}

#[async_trait]
impl Connection for PgConnection {
    fn stream_query(&self, _sql: String, sink: BatchSink) -> QueryHandle {
        let (cancel_tx, _cancel_rx) = flume::unbounded();
        // TODO: query streaming is not implemented yet; this placeholder
        // exists so the trait compiles and callers get a clear typed error
        // instead of a panic or a silently empty result.
        let err = CoreError::Query("stream_query is not implemented yet".to_owned());
        // Best-effort: if the receiver was already dropped, there is no one
        // left to observe the error and nothing more to do.
        let _ = sink.send(Err(err));
        QueryHandle::new(cancel_tx)
    }

    #[tracing::instrument(name = "pg_introspect", skip_all, fields(pool_size = self.pool.size()))]
    async fn introspect(&self) -> Result<SchemaTree, CoreError> {
        // TODO: schema introspection (pg_catalog / information_schema ->
        // SchemaTree) is not implemented yet; this placeholder exists so the
        // trait compiles and callers get a clear typed error instead of a
        // panic or a silently empty tree.
        Err(CoreError::Introspection(
            "introspect is not implemented yet".to_owned(),
        ))
    }
}

/// Build a pool for `url` and run a trivial liveness query while driven by a
/// non-tokio executor. Returns the value of `SELECT 1`. Used by the app shell
/// as a lightweight standalone connectivity check independent of the
/// `Driver`/`Connection` trait objects.
///
/// # Errors
/// Returns an error if the connection or query fails.
#[tracing::instrument(skip_all)]
pub async fn spike_select_one(url: &str) -> anyhow::Result<i64> {
    let pool = PostgresDriver::build_pool(url).await?;
    let one = liveness_check(&pool).await?;
    pool.close().await;
    Ok(one)
}

#[cfg(test)]
mod tests {
    use zsql_core::{ConnConfig, Driver};

    use super::PostgresDriver;

    /// A host that can never resolve (`.invalid` is reserved by RFC 2606),
    /// so DNS lookup fails immediately. Deliberately not a "connection
    /// refused" address: sqlx's pool treats a refused connection as the
    /// server still starting up and retries with backoff until the acquire
    /// timeout, which would make this test slow for no benefit.
    const UNREACHABLE_DSN: &str = "postgres://user:pass@zsql-test-nonexistent-host.invalid/db";

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        futures::executor::block_on(fut)
    }

    #[test]
    fn connect_maps_unreachable_host_to_core_connection_error() {
        let driver = PostgresDriver;
        let cfg = ConnConfig::from_dsn(UNREACHABLE_DSN).unwrap();
        let result = block_on(driver.connect(&cfg));
        match result {
            Err(zsql_core::CoreError::Connection(msg)) => {
                assert!(!msg.is_empty(), "error message should not be empty");
            }
            Err(other) => panic!("expected CoreError::Connection, got {other:?}"),
            Ok(_) => panic!("connecting to an unreachable host must fail"),
        }
    }

    #[test]
    fn connect_maps_malformed_dsn_to_core_connection_error() {
        let driver = PostgresDriver;
        // Not a valid postgres URL at all (no scheme).
        let cfg = ConnConfig {
            url: "not a valid dsn".to_owned(),
        };
        let result = block_on(driver.connect(&cfg));
        assert!(matches!(result, Err(zsql_core::CoreError::Connection(_))));
    }

    #[test]
    fn parse_dsn_rejects_empty_string() {
        let driver = PostgresDriver;
        assert!(driver.parse_dsn("   ").is_err());
    }

    #[test]
    fn driver_ids_are_stable() {
        let driver = PostgresDriver;
        assert_eq!(driver.id(), "postgres");
        assert_eq!(driver.display_name(), "PostgreSQL");
    }

    /// Live-database tests are gated on `ZSQL_TEST_DATABASE_URL` so
    /// `cargo test` passes with no database present.
    #[test]
    fn connect_succeeds_against_a_live_database_when_configured() {
        let Ok(url) = std::env::var("ZSQL_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping connect_succeeds_against_a_live_database_when_configured: \
                 ZSQL_TEST_DATABASE_URL not set"
            );
            return;
        };
        let driver = PostgresDriver;
        let cfg = ConnConfig::from_dsn(&url).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        // stream_query and introspect are placeholders in this scope; confirm
        // they surface typed errors rather than panicking.
        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT 1".to_owned(), tx);
        let evt = rx.recv().expect("placeholder should push one event");
        assert!(matches!(evt, Err(zsql_core::CoreError::Query(_))));

        let introspect_result = block_on(conn.introspect());
        assert!(matches!(
            introspect_result,
            Err(zsql_core::CoreError::Introspection(_))
        ));
    }
}
