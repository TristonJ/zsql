//! The `MySQL`/`MariaDB` [`Driver`] and its live [`Connection`]
//! implementation, built on sqlx's `MySql` backend with the **smol**
//! runtime so its futures await directly on gpui's executor. sqlx has no
//! separate `MariaDB` backend; `MariaDB` speaks the `MySQL` wire protocol,
//! so this one driver serves both (see [`crate::url::normalize_for_sqlx`]
//! for the `mariadb://` -> `mysql://` scheme rewrite this relies on).

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;
use sqlx::mysql::{MySqlPool, MySqlPoolOptions};
use sqlx::{AssertSqlSafe, Executor as _, Row as _, SqlSafeStr as _, Statement as _};
use zsql_core::{
    BatchSink, ConnConfig, Connection, CoreError, Driver, QueryEvent, QueryHandle, RelationSchema,
    RowBatch, RowCount, SchemaTree,
};

use crate::error::{map_connect_error, map_query_error};
use crate::quoting::backtick_quote_ident;
use crate::values::{column_metas, decode_row};

/// Rows are grouped into batches of at most this many rows before a
/// [`QueryEvent::Batch`] is pushed into the sink. Mirrors `zsql-postgres`'s
/// batch bound; bounded so a large result set streams to the UI
/// incrementally instead of arriving as one huge allocation.
const DEFAULT_QUERY_BATCH_SIZE: usize = 500;

/// Bounded pool size for a single desktop client. Mirrors
/// `zsql-postgres::MAX_POOL_CONNECTIONS`: this app drives at most a handful
/// of concurrent operations, and a modest ceiling avoids hammering the
/// server from a client that only ever has one user.
const MAX_POOL_CONNECTIONS: u32 = 5;

/// How long to wait for the initial connection (and later, a free pooled
/// connection) before giving up.
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounded size of the dedicated pool [`issue_server_side_cancel`] draws
/// from. Deliberately separate from `MAX_POOL_CONNECTIONS` and small: a
/// `KILL QUERY` call is a single statement, never more than a couple of
/// which are ever in flight at once for this single-user desktop client, and
/// keeping it off the query pool entirely means cancellation is never queued
/// behind the very query it is trying to stop.
const CANCEL_POOL_CONNECTIONS: u32 = 2;

/// Bounded size of the dedicated pool [`MySqlConnection::ping`] draws from.
/// Separate from both `MAX_POOL_CONNECTIONS` and `CANCEL_POOL_CONNECTIONS` so
/// a liveness probe can never be blocked behind an in-flight query, nor
/// blocked behind (or itself block) a cancel request.
const PROBE_POOL_CONNECTIONS: u32 = 2;

/// The MySQL/MariaDB [`Driver`].
#[derive(Debug, Default, Clone, Copy)]
pub struct MysqlDriver;

impl MysqlDriver {
    /// Build a bounded connection pool for `url` and verify it is reachable
    /// with a trivial liveness query before returning it.
    ///
    /// # Errors
    /// Returns [`CoreError::Connection`] if `url` cannot be normalized, the
    /// pool cannot be built, or the liveness query fails.
    async fn build_pool(url: &str) -> Result<MySqlPool, CoreError> {
        let url = crate::url::normalize_for_sqlx(url)?;
        let pool = MySqlPoolOptions::new()
            .max_connections(MAX_POOL_CONNECTIONS)
            .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
            .connect(&url)
            .await
            .map_err(map_connect_error)?;
        liveness_check(&pool).await?;
        Ok(pool)
    }

    /// Build a small side pool of `max_connections`, used for an operation
    /// that must never share a connection with the main query pool (see
    /// [`build_pool`](Self::build_pool)). Connects lazily: parsing/
    /// validating `url` cannot fail asynchronously here, so this is
    /// synchronous, and no network round trip happens against this pool
    /// until its first query is actually issued.
    ///
    /// # Errors
    /// Returns [`CoreError::Connection`] if `url` cannot be normalized or
    /// parsed.
    fn build_side_pool(url: &str, max_connections: u32) -> Result<MySqlPool, CoreError> {
        let url = crate::url::normalize_for_sqlx(url)?;
        MySqlPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
            .connect_lazy(&url)
            .map_err(map_connect_error)
    }

    /// Build the small dedicated pool [`MySqlConnection::ping`] draws from.
    ///
    /// Deliberately disables sqlx's default `test_before_acquire`: sqlx's
    /// pool otherwise pings an idle connection *before* handing it back and,
    /// on failure, silently discards it and hands the caller a freshly
    /// opened one instead -- exactly right for the query pool, but wrong
    /// here, since a probe's entire purpose is to be the thing that notices
    /// staleness, so this pool must hand back whatever connection it has,
    /// dead or not, and let the probe's own query surface the failure.
    ///
    /// # Errors
    /// Returns [`CoreError::Connection`] if `url` cannot be normalized or
    /// parsed.
    fn build_probe_pool(url: &str) -> Result<MySqlPool, CoreError> {
        let url = crate::url::normalize_for_sqlx(url)?;
        MySqlPoolOptions::new()
            .max_connections(PROBE_POOL_CONNECTIONS)
            .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
            .test_before_acquire(false)
            .connect_lazy(&url)
            .map_err(map_connect_error)
    }
}

/// Run a trivial `SELECT 1` against `pool` to confirm the connection is
/// actually usable, not just accepted. Returns the decoded value.
async fn liveness_check(pool: &MySqlPool) -> Result<i64, CoreError> {
    let row = sqlx::query("SELECT 1 AS one")
        .fetch_one(pool)
        .await
        .map_err(map_connect_error)?;
    let one: i32 = row.try_get("one").map_err(map_connect_error)?;
    Ok(i64::from(one))
}

#[async_trait]
impl Driver for MysqlDriver {
    fn id(&self) -> &'static str {
        "mysql"
    }

    fn display_name(&self) -> &'static str {
        "MySQL / MariaDB"
    }

    fn parse_url(&self, url: &str) -> Result<ConnConfig, CoreError> {
        ConnConfig::from_url(url)
    }

    #[tracing::instrument(name = "mysql_connect", skip_all, fields(driver = self.id()))]
    async fn connect(&self, cfg: &ConnConfig) -> Result<Box<dyn Connection>, CoreError> {
        // Never log `cfg.url`: it may embed a password. Only non-secret
        // fields (the driver id, above) are attached to this span.
        let pool = Self::build_pool(&cfg.url).await?;
        let cancel_pool = Self::build_side_pool(&cfg.url, CANCEL_POOL_CONNECTIONS)?;
        let probe_pool = Self::build_probe_pool(&cfg.url)?;
        tracing::info!("mysql connection established");
        Ok(Box::new(MySqlConnection {
            pool,
            cancel_pool,
            probe_pool,
        }))
    }
}

/// A live MySQL/MariaDB connection, backed by a bounded sqlx connection pool.
pub struct MySqlConnection {
    pool: MySqlPool,
    /// Separate, independently-bounded pool used only for server-side
    /// cancellation (`KILL QUERY`).
    cancel_pool: MySqlPool,
    /// Separate, independently-bounded pool used only for the liveness
    /// probe (see [`MySqlConnection::ping`]), so a probe can never be
    /// blocked behind an in-flight query or a cancel request, nor block
    /// either of those in turn.
    probe_pool: MySqlPool,
}

/// Issue `KILL QUERY <id>` on a connection acquired fresh from
/// `cancel_pool`. Best-effort: any failure (including the target connection
/// having already finished on its own) is logged and swallowed here.
#[tracing::instrument(name = "mysql_kill_query", skip(cancel_pool))]
async fn issue_server_side_cancel(cancel_pool: &MySqlPool, connection_id: u64) {
    // `KILL QUERY` is a server admin command, not reliably preparable as a
    // parameterized statement across both engines, so `connection_id` (a
    // `u64` this driver itself just read back from `SELECT CONNECTION_ID()`,
    // never externally supplied text) is formatted directly into the raw SQL
    // text instead of bound.
    let sql = format!("KILL QUERY {connection_id}");
    match sqlx::raw_sql(AssertSqlSafe(sql)).execute(cancel_pool).await {
        Ok(_) => tracing::info!(connection_id, "server-side cancel issued"),
        Err(err) => {
            tracing::warn!(connection_id, error = %err, "server-side cancel request failed");
        }
    }
}

/// Spawn [`issue_server_side_cancel`] as a detached background task so
/// neither the query task (which may itself be about to return) nor the
/// caller of `cancel()` has to wait for the cancel round-trip to complete.
fn spawn_server_side_cancel(cancel_pool: &MySqlPool, connection_id: u64) {
    let cancel_pool = cancel_pool.clone();
    async_global_executor::spawn(async move {
        issue_server_side_cancel(&cancel_pool, connection_id).await;
    })
    .detach();
}

#[async_trait]
impl Connection for MySqlConnection {
    fn stream_query(&self, sql: String, sink: BatchSink) -> QueryHandle {
        let (cancel_tx, cancel_rx) = flume::unbounded();
        let pool = self.pool.clone();
        let cancel_pool = self.cancel_pool.clone();
        async_global_executor::spawn(run_query(pool, cancel_pool, sql, sink, cancel_rx)).detach();
        QueryHandle::new(cancel_tx)
    }

    #[tracing::instrument(name = "mysql_introspect", skip_all, fields(pool_size = self.pool.size()))]
    async fn introspect(&self) -> Result<SchemaTree, CoreError> {
        crate::introspect::introspect(&self.pool).await
    }

    #[tracing::instrument(name = "mysql_ping", skip_all, fields(pool_size = self.probe_pool.size()))]
    async fn ping(&self) -> Result<(), CoreError> {
        liveness_check(&self.probe_pool).await?;
        Ok(())
    }

    #[tracing::instrument(name = "mysql_count_rows", skip(self), fields(pool_size = self.pool.size()))]
    async fn count_rows(&self, schema: &str, relation: &str) -> Result<RowCount, CoreError> {
        if let Some(estimate) = fetch_table_rows_estimate(&self.pool, schema, relation).await? {
            tracing::debug!(
                estimate,
                "using information_schema.TABLES row-count estimate"
            );
            return Ok(RowCount::Estimated(estimate));
        }
        tracing::debug!(
            "no reliable TABLE_ROWS estimate (view, or relation not found); \
             falling back to an exact count"
        );
        let exact = exact_row_count(&self.pool, schema, relation).await?;
        Ok(RowCount::Exact(exact))
    }

    #[tracing::instrument(
        name = "mysql_describe_relation",
        skip(self),
        fields(pool_size = self.pool.size())
    )]
    async fn describe_relation(
        &self,
        schema: &str,
        relation: &str,
    ) -> Result<RelationSchema, CoreError> {
        crate::describe::describe_relation(&self.pool, schema, relation).await
    }

    /// The click-to-preview query for `relation` in `schema`, capped at
    /// `limit` rows, in this dialect's syntax: backtick-quoted identifiers.
    fn preview_query(&self, schema: &str, relation: &str, limit: u64) -> String {
        format!(
            "SELECT * FROM {}.{} LIMIT {limit}",
            backtick_quote_ident(schema),
            backtick_quote_ident(relation)
        )
    }
}

/// Look up `information_schema.TABLES.TABLE_ROWS` for `schema.relation`,
/// bind-parameterized (never string-interpolated). `TABLE_ROWS` is `NULL`
/// for a view (and for any relation this query finds no row for at all), in
/// which case this returns `None` so the caller falls back to an exact
/// count; for a base table it is `InnoDB`'s own approximate row count,
/// exactly analogous to Postgres's `pg_class.reltuples`.
async fn fetch_table_rows_estimate(
    pool: &MySqlPool,
    schema: &str,
    relation: &str,
) -> Result<Option<u64>, CoreError> {
    let row = sqlx::query(
        "SELECT TABLE_ROWS FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?",
    )
    .bind(schema)
    .bind(relation)
    .fetch_optional(pool)
    .await
    .map_err(map_query_error)?;

    let Some(row) = row else {
        return Ok(None);
    };
    let table_rows: Option<u64> = row.try_get("TABLE_ROWS").map_err(map_query_error)?;
    Ok(table_rows)
}

/// Build a `SELECT COUNT(*)` statement against `schema.relation`, with both
/// identifiers backtick-quoted so an adversarial schema/relation name cannot
/// break out of the identifier position.
fn exact_count_sql(schema: &str, relation: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM {}.{}",
        backtick_quote_ident(schema),
        backtick_quote_ident(relation)
    )
}

/// Run an exact `SELECT COUNT(*)` against `schema.relation`.
async fn exact_row_count(pool: &MySqlPool, schema: &str, relation: &str) -> Result<u64, CoreError> {
    let sql = exact_count_sql(schema, relation);
    // `sql` is built entirely from `backtick_quote_ident`-escaped
    // identifiers via `exact_count_sql`, never from unescaped runtime text.
    let count: i64 = sqlx::query_scalar(AssertSqlSafe(sql))
        .fetch_one(pool)
        .await
        .map_err(map_query_error)?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Stream a query's results into `sink`. `sql` may hold several statements;
/// each result-producing statement emits its own [`QueryEvent::Columns`]
/// followed by that set's [`QueryEvent::Batch`]es, and the whole stream ends
/// with exactly one [`QueryEvent::Done`] -- or, on any failure, a single
/// `Err` in place of `Done`. Every statement still executes (so all side
/// effects happen); a fresh `Columns` event marks each set boundary so the
/// consumer can keep only the last set rather than concatenating mismatched
/// rows.
///
/// Runs on a single connection acquired from `pool` for the lifetime of this
/// call (not the pool directly), so that connection's own id can be captured
/// via `SELECT CONNECTION_ID()` *before* the row-streaming loop below ever
/// starts checking `cancel_rx`.
///
/// Column metadata for `Columns` is taken from the first row any statement
/// in `sql` produces. If no statement ever produces a row (DDL, DML without
/// an output, or a zero-row `SELECT`), `MySQL`'s own wire protocol never sends
/// column metadata separately from a row, so a describe is run as a
/// fallback *after* execution has already completed successfully, purely to
/// recover a zero-row `SELECT`'s column list; if that fallback describe
/// itself fails, the query has already succeeded, so this degrades to
/// reporting no columns rather than failing an otherwise-successful query.
#[tracing::instrument(name = "mysql_stream_query", skip_all, fields(pool_size = pool.size()))]
#[allow(clippy::too_many_lines)]
async fn run_query(
    pool: MySqlPool,
    cancel_pool: MySqlPool,
    sql: String,
    sink: BatchSink,
    cancel_rx: flume::Receiver<()>,
) {
    // The SQL text itself carries no connection secrets (those live only in
    // the URL, never logged here), so it is fine to record at debug level.
    tracing::debug!(sql = %sql, "streaming query");

    let mut conn = match pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => {
            let _ = sink.send_async(Err(map_query_error(err))).await;
            return;
        }
    };

    // Capture this connection's own id so a later cancel can target it via
    // `KILL QUERY` on `cancel_pool`.
    let connection_id = match sqlx::query_scalar::<_, u64>("SELECT CONNECTION_ID()")
        .fetch_one(&mut *conn)
        .await
    {
        Ok(id) => {
            tracing::debug!(connection_id = id, "dedicated connection id captured");
            Some(id)
        }
        Err(err) => {
            // Cooperative cancellation (the `select` loop below) still works
            // without a known id; only the server-side `KILL QUERY` path is
            // unavailable for this one query. Never fatal to the query
            // itself.
            tracing::warn!(
                error = %err,
                "failed to capture connection id; server-side cancel unavailable for this query"
            );
            None
        }
    };

    let mut rows = sqlx::raw_sql(AssertSqlSafe(sql.clone())).fetch_many(&mut *conn);
    let mut batch = RowBatch::new();
    let mut affected: u64 = 0;
    // Whether the statement currently streaming has already announced its
    // columns. Reset at each statement boundary so a following statement
    // starts a new result set.
    let mut columns_sent = false;
    // Whether any statement in `sql` produced columns at all.
    let mut any_columns_sent = false;

    loop {
        let step = futures::future::select(cancel_rx.recv_async(), rows.next());
        match step.await {
            futures::future::Either::Left(_) => {
                // Cancelled: either an explicit `cancel()` call or every
                // `QueryHandle` clone (hence every `cancel_tx`) was dropped.
                tracing::debug!("query cancelled");
                if let Some(connection_id) = connection_id {
                    spawn_server_side_cancel(&cancel_pool, connection_id);
                }
                return;
            }
            futures::future::Either::Right((None, _)) => break,
            futures::future::Either::Right((Some(Ok(sqlx::Either::Right(row))), _)) => {
                if !columns_sent {
                    let columns = column_metas(row.columns());
                    if sink
                        .send_async(Ok(QueryEvent::Columns(columns)))
                        .await
                        .is_err()
                    {
                        // Receiver already gone; no one left to stream rows to.
                        return;
                    }
                    columns_sent = true;
                    any_columns_sent = true;
                }
                batch.push(decode_row(&row));
                if batch.len() >= DEFAULT_QUERY_BATCH_SIZE {
                    let full = std::mem::take(&mut batch);
                    if sink.send_async(Ok(QueryEvent::Batch(full))).await.is_err() {
                        return;
                    }
                }
            }
            futures::future::Either::Right((Some(Ok(sqlx::Either::Left(result))), _)) => {
                // End of one statement. Flush its rows and reset the per-set
                // latch so a following statement's rows form a new result
                // set (the consumer keeps only the last) instead of being
                // appended onto this one's columns.
                if !batch.is_empty() {
                    let full = std::mem::take(&mut batch);
                    if sink.send_async(Ok(QueryEvent::Batch(full))).await.is_err() {
                        return;
                    }
                }
                affected += result.rows_affected();
                columns_sent = false;
            }
            futures::future::Either::Right((Some(Err(err)), _)) => {
                let _ = sink.send_async(Err(map_query_error(err))).await;
                return;
            }
        }
    }

    // A statement with no output columns (DDL, or DML without an output
    // clause) reports its row count as `affected` in `Done`. A statement
    // that does produce columns (SELECT, or DML with an output clause)
    // instead lets the caller derive a count from the rows it already
    // streamed, and reports `affected: None`.
    let reports_affected = if any_columns_sent {
        false
    } else {
        let columns = match pool.prepare(AssertSqlSafe(sql).into_sql_str()).await {
            Ok(statement) => column_metas(statement.columns()),
            Err(_) => Vec::new(),
        };
        let reports_affected = columns.is_empty();
        if sink
            .send_async(Ok(QueryEvent::Columns(columns)))
            .await
            .is_err()
        {
            return;
        }
        reports_affected
    };

    if !batch.is_empty() && sink.send_async(Ok(QueryEvent::Batch(batch))).await.is_err() {
        return;
    }

    let affected = reports_affected.then_some(affected);
    let _ = sink.send_async(Ok(QueryEvent::Done { affected })).await;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use zsql_core::{ConnConfig, Connection, Driver};

    use super::{MySqlConnection, MysqlDriver};

    const UNREACHABLE_URL: &str = "mysql://user:pass@zsql-test-nonexistent-host.invalid/db";

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        futures::executor::block_on(fut)
    }

    #[test]
    fn connect_maps_unreachable_host_to_core_connection_error() {
        let driver = MysqlDriver;
        let cfg = ConnConfig::from_url(UNREACHABLE_URL).unwrap();
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
    fn connect_maps_a_mariadb_scheme_unreachable_host_to_core_connection_error() {
        let driver = MysqlDriver;
        let cfg = ConnConfig::from_url("mariadb://user:pass@zsql-test-nonexistent-host.invalid/db")
            .unwrap();
        let result = block_on(driver.connect(&cfg));
        assert!(matches!(result, Err(zsql_core::CoreError::Connection(_))));
    }

    #[test]
    fn connect_maps_malformed_url_to_core_connection_error() {
        let driver = MysqlDriver;
        let cfg = ConnConfig {
            url: "not a valid url".to_owned(),
        };
        let result = block_on(driver.connect(&cfg));
        assert!(matches!(result, Err(zsql_core::CoreError::Connection(_))));
    }

    #[test]
    fn parse_url_rejects_empty_string() {
        let driver = MysqlDriver;
        assert!(driver.parse_url("   ").is_err());
    }

    #[test]
    fn driver_ids_are_stable() {
        let driver = MysqlDriver;
        assert_eq!(driver.id(), "mysql");
        assert_eq!(driver.display_name(), "MySQL / MariaDB");
    }

    #[test]
    fn introspect_maps_unreachable_host_to_core_introspection_error() {
        let conn = connection_for_test();
        let result = block_on(conn.introspect());
        match result {
            Err(zsql_core::CoreError::Introspection(msg)) => {
                assert!(!msg.is_empty(), "error message should not be empty");
            }
            Err(other) => panic!("expected CoreError::Introspection, got {other:?}"),
            Ok(_) => panic!("introspecting an unreachable host must fail"),
        }
    }

    #[test]
    fn ping_maps_unreachable_host_to_core_connection_error() {
        let conn = connection_for_test();
        let result = block_on(conn.ping());
        match result {
            Err(zsql_core::CoreError::Connection(msg)) => {
                assert!(!msg.is_empty(), "error message should not be empty");
            }
            Err(other) => panic!("expected CoreError::Connection, got {other:?}"),
            Ok(()) => panic!("pinging an unreachable host must fail"),
        }
    }

    /// Builds a [`MySqlConnection`] whose pools only ever parse
    /// `UNREACHABLE_URL` (`connect_lazy` never touches the network), so a
    /// test can exercise `preview_query` -- pure string-building, no I/O --
    /// without a live database.
    fn connection_for_test() -> MySqlConnection {
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .connect_lazy(UNREACHABLE_URL)
            .expect("connect_lazy only parses the URL; it must not touch the network");
        let cancel_pool = pool.clone();
        let probe_pool = pool.clone();
        MySqlConnection {
            pool,
            cancel_pool,
            probe_pool,
        }
    }

    #[test]
    fn preview_query_quotes_both_identifiers_with_backticks_and_applies_the_limit() {
        let conn = connection_for_test();
        assert_eq!(
            conn.preview_query("zsql", "orders", 200),
            "SELECT * FROM `zsql`.`orders` LIMIT 200"
        );
    }

    #[test]
    fn preview_query_is_safe_against_an_injection_shaped_relation_name() {
        let conn = connection_for_test();
        let sql = conn.preview_query("zsql", "orders`; DROP TABLE users; --", 200);
        assert_eq!(
            sql,
            "SELECT * FROM `zsql`.`orders``; DROP TABLE users; --` LIMIT 200"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }

    #[test]
    fn stream_query_pushes_single_error_when_pool_is_unreachable() {
        let conn = connection_for_test();

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT 1".to_owned(), tx);

        let evt = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("stream_query must push exactly one event, not hang");
        match evt {
            Err(zsql_core::CoreError::Query(msg)) => assert!(!msg.is_empty()),
            other => panic!("expected a single CoreError::Query, got {other:?}"),
        }

        // No `Done` (or anything else) follows the error.
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "no further events should follow the error"
        );
    }

    #[test]
    fn exact_count_sql_quotes_both_identifiers() {
        assert_eq!(
            super::exact_count_sql("zsql", "orders"),
            "SELECT COUNT(*) FROM `zsql`.`orders`"
        );
    }

    #[test]
    fn exact_count_sql_is_safe_against_an_injection_shaped_relation_name() {
        let sql = super::exact_count_sql("zsql", "orders`; DROP TABLE users; --");
        assert_eq!(
            sql,
            "SELECT COUNT(*) FROM `zsql`.`orders``; DROP TABLE users; --`"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }

    #[test]
    fn exact_count_sql_is_safe_against_an_injection_shaped_schema_name() {
        let sql = super::exact_count_sql("zsql`; DROP TABLE users; --", "orders");
        assert_eq!(
            sql,
            "SELECT COUNT(*) FROM `zsql``; DROP TABLE users; --`.`orders`"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }
}

#[cfg(test)]
#[cfg(feature = "driver-integration-tests")]
mod database_tests {
    use std::time::Duration;

    use zsql_core::{ConnConfig, Driver};

    use super::MysqlDriver;

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        futures::executor::block_on(fut)
    }

    /// Reads `ZSQL_TEST_MYSQL_URL` for database-tests. The very same URL
    /// (and every test in this module) is run twice by CI/local scripts: once
    /// pointed at a live `MySQL` 8 instance and once at a live `MariaDB`
    /// instance, proving the single-driver decision this crate exists to
    /// validate.
    fn live_database_url() -> String {
        std::env::var("ZSQL_TEST_MYSQL_URL")
            .expect("ZSQL_TEST_MYSQL_URL must be set to run database tests")
    }

    /// Connects to `ZSQL_TEST_MYSQL_URL` via [`live_database_url`].
    fn live_connection() -> Box<dyn zsql_core::Connection> {
        let url = live_database_url();
        let driver = MysqlDriver;
        let cfg = ConnConfig::from_url(&url).unwrap();
        block_on(driver.connect(&cfg)).expect("connect should succeed")
    }

    /// Rewrite `ZSQL_TEST_MYSQL_URL`'s scheme to `mariadb://`, proving the
    /// driver accepts that scheme too, not just whichever one the
    /// environment happened to be configured with.
    fn live_connection_via_mariadb_scheme() -> Box<dyn zsql_core::Connection> {
        let url = live_database_url();
        let mariadb_url = url.replacen("mysql://", "mariadb://", 1);
        assert_ne!(mariadb_url, url, "expected the URL to start with mysql://");
        let driver = MysqlDriver;
        let cfg = ConnConfig::from_url(&mariadb_url).unwrap();
        block_on(driver.connect(&cfg)).expect("connect via a mariadb:// URL should succeed")
    }

    /// Receive one event with a generous timeout so a broken implementation
    /// fails the test instead of hanging it.
    fn recv(
        rx: &flume::Receiver<Result<zsql_core::QueryEvent, zsql_core::CoreError>>,
    ) -> Result<zsql_core::QueryEvent, zsql_core::CoreError> {
        rx.recv_timeout(Duration::from_secs(10))
            .expect("expected an event within the timeout")
    }

    /// Seed a small, cheap-to-scan 10-row helper table named `table`, used
    /// only to build the large self cross joins
    /// [`cross_join_rows_sql`]/[`effectively_unbounded_rows_sql`] need.
    /// Plain in-memory table cross joins, not `WITH RECURSIVE`: `MySQL` and
    /// `MariaDB` disagree on the session variable controlling recursion depth
    /// (`cte_max_recursion_depth` vs `max_recursive_iterations`), and
    /// `information_schema.COLUMNS` was tried as a ready-made large table
    /// and rejected -- it is itself an expensive dynamic view recomputed on
    /// every scan, so cross-joining it repeatedly took tens of seconds just
    /// to return one row. A nested-loop join over an ordinary, cheap
    /// in-memory table has neither problem.
    fn seed_cross_join_source_table(conn: &dyn zsql_core::Connection, table: &str) {
        run_ddl(conn, &format!("DROP TABLE IF EXISTS {table}"));
        run_ddl(conn, &format!("CREATE TABLE {table} (n INT)"));
        run_ddl(
            conn,
            &format!("INSERT INTO {table} (n) VALUES (1),(2),(3),(4),(5),(6),(7),(8),(9),(10)"),
        );
    }

    /// A plain self cross join of `table` (seeded by
    /// [`seed_cross_join_source_table`] with 10 rows) across `ways`
    /// aliases, capped at `limit` rows -- an exact, bounded row count a test
    /// wants to see stream in full.
    fn cross_join_rows_sql(table: &str, ways: usize, limit: usize) -> String {
        let aliases = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
        let from_clause = aliases[..ways]
            .iter()
            .map(|alias| format!("{table} {alias}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("SELECT 1 AS g FROM {from_clause} LIMIT {limit}")
    }

    /// A plain nine-way self cross join of `table`, producing 10^9 rows --
    /// effectively unbounded for a test's purposes (no `LIMIT`, unlike
    /// [`cross_join_rows_sql`]): a nested-loop join over an ordinary, cheap
    /// in-memory table starts streaming its first row immediately, so this
    /// is the right shape for a query a test cancels mid-stream rather than
    /// lets run to completion.
    fn effectively_unbounded_rows_sql(table: &str) -> String {
        let aliases = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
        let from_clause = aliases
            .iter()
            .map(|alias| format!("{table} {alias}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("SELECT 1 AS g FROM {from_clause}")
    }

    /// Run `sql` (typically DDL/DML setup) to completion against `conn`,
    /// panicking on any error and discarding whatever events it produces.
    fn run_ddl(conn: &dyn zsql_core::Connection, sql: &str) {
        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(sql.to_owned(), tx);
        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(zsql_core::QueryEvent::Done { .. })) => break,
                Ok(Ok(_)) => {}
                Ok(Err(err)) => panic!("ddl setup failed: {err:?}"),
                Err(err) => panic!("ddl setup did not complete: {err:?}"),
            }
        }
    }

    #[test]
    fn connect_succeeds_against_a_live_database_when_configured() {
        live_connection();
    }

    #[test]
    fn connect_succeeds_via_a_mariadb_scheme_url_when_configured() {
        // Proves the scheme is normalized before it ever reaches sqlx's
        // `MySqlConnectOptions`: a bare `mariadb://` URL, not just
        // `mysql://`, must actually connect.
        live_connection_via_mariadb_scheme();
    }

    #[test]
    fn ping_succeeds_against_a_live_database_when_configured() {
        let conn = live_connection();
        block_on(conn.ping()).expect("ping should succeed against a reachable database");
    }

    #[test]
    fn ping_completes_while_a_slow_query_is_streaming_when_configured() {
        let conn = live_connection();

        let (tx, rx) = flume::unbounded();
        let handle = conn.stream_query("SELECT SLEEP(3)".to_owned(), tx);

        // Let `SLEEP` actually start server-side before probing, so the ping
        // genuinely races a query that is mid-flight.
        std::thread::sleep(Duration::from_millis(300));

        let ping_started = std::time::Instant::now();
        block_on(conn.ping()).expect("ping must succeed independently of the slow query");
        assert!(
            ping_started.elapsed() < Duration::from_secs(2),
            "ping took {:?}, which suggests it was blocked behind the slow query",
            ping_started.elapsed()
        );

        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Columns(_) | zsql_core::QueryEvent::Batch(_)) => {}
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                Err(err) => panic!("slow query must not fail alongside a probe: {err:?}"),
            }
        }
        drop(handle);
    }

    #[test]
    fn stream_query_keeps_statements_as_separate_result_sets_when_configured() {
        let conn = live_connection();

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT 1 AS n; SELECT 2 AS n".to_owned(), tx);

        let mut columns_events = 0usize;
        let mut rows_per_set: Vec<Vec<zsql_core::Value>> = Vec::new();
        let affected = loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Columns(columns)) => {
                    assert_eq!(columns.len(), 1);
                    assert_eq!(columns[0].name, "n");
                    columns_events += 1;
                    rows_per_set.push(Vec::new());
                }
                Ok(zsql_core::QueryEvent::Batch(batch)) => {
                    let current = rows_per_set
                        .last_mut()
                        .expect("a Columns event must precede any Batch");
                    current.extend(batch.rows.into_iter().map(|row| row.0[0].clone()));
                }
                Ok(zsql_core::QueryEvent::Done { affected }) => break affected,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        };

        assert_eq!(affected, None);
        assert_eq!(
            columns_events, 2,
            "each statement must announce its own result set"
        );
        assert_eq!(
            rows_per_set,
            vec![
                vec![zsql_core::Value::Int(1)],
                vec![zsql_core::Value::Int(2)]
            ],
            "each statement's single row stays within its own result set, not concatenated"
        );
    }

    #[test]
    fn stream_query_batches_large_result_sets_when_configured() {
        let conn = live_connection();

        let row_count = super::DEFAULT_QUERY_BATCH_SIZE * 2 + 7;
        let table = "zsql_test_batch_rows";
        seed_cross_join_source_table(&*conn, table);
        // A 4-way cross join of the 10-row helper table yields 10^4 =
        // 10,000 possible rows, comfortably above `row_count`; `LIMIT` caps
        // it at exactly the count this test wants to see in full.
        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(cross_join_rows_sql(table, 4, row_count), tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 1),
            other => panic!("expected Columns first, got {other:?}"),
        }

        let mut total_rows = 0usize;
        let mut batch_count = 0usize;
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => {
                    assert!(
                        batch.len() <= super::DEFAULT_QUERY_BATCH_SIZE,
                        "batch of {} rows exceeds the bound",
                        batch.len()
                    );
                    assert!(!batch.is_empty(), "a sent Batch must never be empty");
                    total_rows += batch.len();
                    batch_count += 1;
                }
                Ok(zsql_core::QueryEvent::Done { affected }) => {
                    assert_eq!(affected, None);
                    break;
                }
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        assert_eq!(total_rows, row_count);
        assert!(
            batch_count >= 3,
            "expected at least 3 batches for {row_count} rows at a bound of \
             {}, got {batch_count}",
            super::DEFAULT_QUERY_BATCH_SIZE
        );

        run_ddl(&*conn, &format!("DROP TABLE {table}"));
    }

    #[test]
    fn stream_query_reports_affected_rows_for_dml_when_configured() {
        let conn = live_connection();

        run_ddl(&*conn, "DROP TABLE IF EXISTS zsql_test_dml_rowcount");
        run_ddl(&*conn, "CREATE TABLE zsql_test_dml_rowcount (n INT)");
        run_ddl(
            &*conn,
            "INSERT INTO zsql_test_dml_rowcount (n) VALUES (1), (2), (3)",
        );

        let (tx, rx) = flume::unbounded();
        let _handle =
            conn.stream_query("UPDATE zsql_test_dml_rowcount SET n = n + 0".to_owned(), tx);
        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => {
                assert!(columns.is_empty(), "DML has no output columns");
            }
            other => panic!("expected Columns first, got {other:?}"),
        }
        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Done { affected }) => assert_eq!(affected, Some(3)),
            other => panic!("expected Done with no Batch in between, got {other:?}"),
        }

        run_ddl(&*conn, "DROP TABLE zsql_test_dml_rowcount");
    }

    #[test]
    fn stream_query_emits_columns_for_a_zero_row_result_when_configured() {
        let conn = live_connection();

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT 1 AS one FROM DUAL WHERE 1 = 0".to_owned(), tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 1),
            other => panic!("expected Columns first, got {other:?}"),
        }
        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Done { affected }) => assert_eq!(affected, None),
            other => panic!("expected Done with no Batch in between, got {other:?}"),
        }
    }

    #[test]
    fn preview_query_executes_against_a_live_seeded_table_when_configured() {
        let conn = live_connection();

        let sql = conn.preview_query("zsql", "users", 5);
        assert!(
            sql.contains("LIMIT 5"),
            "expected a LIMIT-bounded preview: {sql}"
        );

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(sql, tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert!(!columns.is_empty()),
            other => panic!("a syntax error would arrive as Err; expected Columns, got {other:?}"),
        }
        let mut rows = 0usize;
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows += batch.len(),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        assert!(
            rows >= 1,
            "the seeded users table should return at least one row"
        );
        assert!(rows <= 5, "LIMIT 5 must cap the result at five rows");
    }

    #[test]
    fn dropping_the_query_handle_stops_further_rows_when_configured() {
        let conn = live_connection();
        let table = "zsql_test_cross_join_drop";
        seed_cross_join_source_table(&*conn, table);

        let (tx, rx) = flume::unbounded();
        let handle = conn.stream_query(effectively_unbounded_rows_sql(table), tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(_)) => {}
            other => panic!("expected Columns first, got {other:?}"),
        }
        drop(handle);

        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(zsql_core::QueryEvent::Batch(_))) => {}
                Ok(Ok(zsql_core::QueryEvent::Done { .. })) => {
                    panic!("a cancelled query must not reach Done")
                }
                Ok(Err(err)) => panic!("unexpected error after cancellation: {err:?}"),
                Ok(Ok(zsql_core::QueryEvent::Columns(_))) => {
                    panic!("Columns must only be sent once")
                }
                Err(flume::RecvTimeoutError::Disconnected) => break,
                Err(flume::RecvTimeoutError::Timeout) => {
                    panic!("cancellation did not stop the background task promptly")
                }
            }
        }

        run_ddl(&*conn, &format!("DROP TABLE {table}"));
    }

    #[test]
    fn calling_cancel_stops_further_rows_when_configured() {
        let conn = live_connection();
        let table = "zsql_test_cross_join_cancel";
        seed_cross_join_source_table(&*conn, table);

        let (tx, rx) = flume::unbounded();
        let handle = conn.stream_query(effectively_unbounded_rows_sql(table), tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(_)) => {}
            other => panic!("expected Columns first, got {other:?}"),
        }
        handle.cancel();

        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(zsql_core::QueryEvent::Batch(_))) => {}
                Ok(Ok(zsql_core::QueryEvent::Done { .. })) => {
                    panic!("a cancelled query must not reach Done")
                }
                Ok(Err(err)) => panic!("unexpected error after cancellation: {err:?}"),
                Ok(Ok(zsql_core::QueryEvent::Columns(_))) => {
                    panic!("Columns must only be sent once")
                }
                Err(flume::RecvTimeoutError::Disconnected) => break,
                Err(flume::RecvTimeoutError::Timeout) => {
                    panic!("cancellation did not stop the background task promptly")
                }
            }
        }
        drop(handle);

        run_ddl(&*conn, &format!("DROP TABLE {table}"));
    }

    #[test]
    fn introspect_builds_schema_tree_matching_the_seeded_database_when_configured() {
        let conn = live_connection();

        let tree = block_on(conn.introspect()).expect("introspect should succeed");
        assert_eq!(tree.catalogs.len(), 1);
        let catalog = &tree.catalogs[0];
        assert_eq!(catalog.name, "def");

        assert!(
            catalog.schemas.iter().all(|s| {
                s.name != "information_schema"
                    && s.name != "performance_schema"
                    && s.name != "mysql"
                    && s.name != "sys"
            }),
            "system schemas must be excluded, got schemas: {:?}",
            catalog.schemas.iter().map(|s| &s.name).collect::<Vec<_>>()
        );

        let zsql = catalog
            .schemas
            .iter()
            .find(|s| s.name == "zsql")
            .expect("the seeded zsql database is present");

        let users = zsql
            .tables
            .iter()
            .find(|r| r.name == "users")
            .expect("the seeded users table is present");
        assert_eq!(users.kind, zsql_core::RelationKind::Table);

        let recent_orders = zsql
            .tables
            .iter()
            .find(|r| r.name == "recent_orders")
            .expect("the seeded recent_orders view is present");
        assert_eq!(recent_orders.kind, zsql_core::RelationKind::View);

        let email = users
            .columns
            .iter()
            .find(|c| c.name == "email")
            .expect("users.email column is present");
        assert!(!email.nullable, "users.email is declared NOT NULL");

        let display_name = users
            .columns
            .iter()
            .find(|c| c.name == "display_name")
            .expect("users.display_name column is present");
        assert!(
            display_name.nullable,
            "users.display_name has no NOT NULL constraint in the seed"
        );
    }

    #[test]
    fn introspect_orders_schemas_relations_and_columns_deterministically_when_configured() {
        let conn = live_connection();

        let tree = block_on(conn.introspect()).expect("introspect should succeed");
        let catalog = &tree.catalogs[0];

        let schema_names: Vec<&str> = catalog.schemas.iter().map(|s| s.name.as_str()).collect();
        let mut sorted_schema_names = schema_names.clone();
        sorted_schema_names.sort_unstable();
        assert_eq!(schema_names, sorted_schema_names);

        let zsql = catalog
            .schemas
            .iter()
            .find(|s| s.name == "zsql")
            .expect("the seeded zsql database is present");
        let relation_names: Vec<&str> = zsql.tables.iter().map(|r| r.name.as_str()).collect();
        let mut sorted_relation_names = relation_names.clone();
        sorted_relation_names.sort_unstable();
        assert_eq!(relation_names, sorted_relation_names);

        let users = zsql
            .tables
            .iter()
            .find(|r| r.name == "users")
            .expect("the seeded users table is present");
        let column_names: Vec<&str> = users.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            column_names,
            vec!["id", "email", "display_name", "is_active", "created_at"],
            "columns must be in ordinal position, not alphabetical"
        );
    }

    #[test]
    fn introspect_includes_a_second_database_including_an_empty_one_when_configured() {
        let conn = live_connection();

        let tree = block_on(conn.introspect()).expect("introspect should succeed");
        let catalog = &tree.catalogs[0];

        let analytics = catalog
            .schemas
            .iter()
            .find(|s| s.name == "zsql_analytics")
            .expect("the seeded zsql_analytics database is present");
        let page_views = analytics
            .tables
            .iter()
            .find(|r| r.name == "page_views")
            .expect("the seeded zsql_analytics.page_views table is present");
        assert_eq!(page_views.kind, zsql_core::RelationKind::Table);

        let empty = catalog
            .schemas
            .iter()
            .find(|s| s.name == "zsql_empty")
            .expect("the seeded zsql_empty database is present even though it holds nothing");
        assert!(empty.tables.is_empty());
    }

    #[test]
    fn count_rows_returns_an_estimated_count_for_a_seeded_table_when_configured() {
        let conn = live_connection();
        let row_count = block_on(conn.count_rows("zsql", "users")).expect("count_rows must run");
        tracing::info!(
            ?row_count,
            "count_rows_returns_an_estimated_count_for_a_seeded_table_when_configured executed \
             against the live database"
        );
        // `information_schema.TABLES.TABLE_ROWS` is an estimate, so its exact
        // value is not guaranteed; what this test pins is that a base table
        // takes the estimate branch (the sibling view test pins `Exact`).
        assert!(
            matches!(row_count, zsql_core::RowCount::Estimated(_)),
            "expected an estimate from information_schema.TABLES.TABLE_ROWS, got {row_count:?}"
        );
    }

    #[test]
    fn count_rows_falls_back_to_exact_for_a_view_when_configured() {
        let conn = live_connection();
        // A view has no `information_schema.TABLES.TABLE_ROWS` value of its
        // own (it is always `NULL`), so `count_rows` must fall back to an
        // exact `COUNT(*)`.
        let row_count =
            block_on(conn.count_rows("zsql", "recent_orders")).expect("count_rows must run");
        assert!(
            matches!(row_count, zsql_core::RowCount::Exact(_)),
            "expected Exact for a view, got {row_count:?}"
        );
    }

    /// Seed a parent/child table pair (unique-suffixed so parallel test runs
    /// cannot collide) exercising a primary key, a foreign key, a named
    /// non-PK unique index, and a check constraint, and describe the child
    /// table.
    fn describe_seeded_child(
        conn: &dyn zsql_core::Connection,
        suffix: &str,
    ) -> zsql_core::RelationSchema {
        let parent = format!("zsql_test_describe_parent_{suffix}");
        let child = format!("zsql_test_describe_child_{suffix}");
        let status_index = format!("idx_{child}_status");

        run_ddl(conn, &format!("DROP TABLE IF EXISTS {child}"));
        run_ddl(conn, &format!("DROP TABLE IF EXISTS {parent}"));
        run_ddl(
            conn,
            &format!(
                "CREATE TABLE {parent} ( \
                     id INT AUTO_INCREMENT PRIMARY KEY, \
                     code VARCHAR(50) NOT NULL, \
                     CONSTRAINT uq_{parent}_code UNIQUE (code) \
                 )"
            ),
        );
        run_ddl(
            conn,
            &format!(
                "CREATE TABLE {child} ( \
                     id INT AUTO_INCREMENT PRIMARY KEY, \
                     parent_id INT NOT NULL, \
                     qty INT NOT NULL DEFAULT 1, \
                     status VARCHAR(20) NOT NULL DEFAULT 'open', \
                     CONSTRAINT fk_{child}_parent FOREIGN KEY (parent_id) REFERENCES {parent} (id), \
                     CONSTRAINT ck_{child}_qty CHECK (qty > 0) \
                 )"
            ),
        );
        run_ddl(
            conn,
            &format!("CREATE UNIQUE INDEX {status_index} ON {child} (status)"),
        );

        let schema = block_on(conn.describe_relation("zsql", &child))
            .expect("describe_relation should succeed for a seeded table");

        run_ddl(conn, &format!("DROP TABLE {child}"));
        run_ddl(conn, &format!("DROP TABLE {parent}"));
        schema
    }

    #[test]
    fn describe_relation_reports_column_key_and_default_detail_when_configured() {
        let conn = live_connection();
        let schema = describe_seeded_child(&*conn, "cols");

        let id = schema
            .columns
            .iter()
            .find(|c| c.name == "id")
            .expect("id column present");
        assert!(id.is_primary_key);
        assert!(!id.nullable);
        assert!(id.foreign_key.is_none());

        let parent_id = schema
            .columns
            .iter()
            .find(|c| c.name == "parent_id")
            .expect("parent_id column present");
        assert!(!parent_id.is_primary_key);
        assert!(!parent_id.nullable);
        let fk = parent_id
            .foreign_key
            .as_ref()
            .expect("parent_id carries a foreign key");
        assert_eq!(fk.table, "zsql_test_describe_parent_cols");
        assert_eq!(fk.columns, vec!["id".to_owned()]);

        let status = schema
            .columns
            .iter()
            .find(|c| c.name == "status")
            .expect("status column present");
        assert!(
            status
                .default
                .as_deref()
                .is_some_and(|d| d.contains("open")),
            "status default should mention 'open', got {:?}",
            status.default
        );
    }

    #[test]
    fn describe_relation_reports_the_primary_and_unique_indexes_when_configured() {
        let conn = live_connection();
        let schema = describe_seeded_child(&*conn, "idx");

        let pk_index = schema
            .indexes
            .iter()
            .find(|i| i.name == "PRIMARY")
            .expect("the primary key's backing index is listed");
        assert!(pk_index.unique);

        let status_index = schema
            .indexes
            .iter()
            .find(|i| i.name == "idx_zsql_test_describe_child_idx_status")
            .expect("the named non-PK unique index is listed");
        assert!(status_index.unique);
    }

    #[test]
    fn describe_relation_reports_the_primary_foreign_and_check_constraints_when_configured() {
        let conn = live_connection();
        let schema = describe_seeded_child(&*conn, "con");

        let pk_constraint = schema
            .constraints
            .iter()
            .find(|c| c.name == "PRIMARY")
            .expect("primary key constraint present");
        assert_eq!(pk_constraint.kind, zsql_core::ConstraintKind::PrimaryKey);

        let fk_constraint = schema
            .constraints
            .iter()
            .find(|c| c.name == "fk_zsql_test_describe_child_con_parent")
            .expect("foreign key constraint present");
        assert_eq!(fk_constraint.kind, zsql_core::ConstraintKind::ForeignKey);
        assert!(
            fk_constraint.definition.contains("parent_id"),
            "fk definition should mention parent_id, got {}",
            fk_constraint.definition
        );

        let check_constraint = schema
            .constraints
            .iter()
            .find(|c| c.name == "ck_zsql_test_describe_child_con_qty")
            .expect("check constraint present");
        assert_eq!(check_constraint.kind, zsql_core::ConstraintKind::Check);
        assert!(
            check_constraint.definition.contains("qty"),
            "check definition should mention qty, got {}",
            check_constraint.definition
        );
    }

    #[test]
    fn describe_relation_reports_a_single_column_unique_constraint_when_configured() {
        let conn = live_connection();

        run_ddl(&*conn, "DROP TABLE IF EXISTS zsql_test_describe_unique");
        run_ddl(
            &*conn,
            "CREATE TABLE zsql_test_describe_unique ( \
                 id INT AUTO_INCREMENT PRIMARY KEY, \
                 code VARCHAR(50) NOT NULL, \
                 CONSTRAINT uq_zsql_test_describe_unique_code UNIQUE (code) \
             )",
        );

        let schema = block_on(conn.describe_relation("zsql", "zsql_test_describe_unique"))
            .expect("describe_relation should succeed for a seeded table");

        let code = schema
            .columns
            .iter()
            .find(|c| c.name == "code")
            .expect("code column present");
        assert!(code.is_unique);
        assert!(!code.is_primary_key);

        let unique_constraint = schema
            .constraints
            .iter()
            .find(|c| c.name == "uq_zsql_test_describe_unique_code")
            .expect("unique constraint present");
        assert_eq!(unique_constraint.kind, zsql_core::ConstraintKind::Unique);

        run_ddl(&*conn, "DROP TABLE zsql_test_describe_unique");
    }

    #[test]
    fn describe_relation_returns_err_for_a_nonexistent_relation_when_configured() {
        let conn = live_connection();

        let result = block_on(conn.describe_relation("zsql", "zsql_test_describe_missing"));
        assert!(
            matches!(result, Err(zsql_core::CoreError::Introspection(_))),
            "expected a CoreError::Introspection, got {result:?}"
        );
    }

    #[test]
    fn stream_query_maps_a_representative_type_spread_when_configured() {
        let conn = live_connection();

        let sql = "SELECT \
            CAST(5 AS SIGNED) AS bi, \
            CAST(1.5 AS DECIMAL(10,1)) AS dec_val, \
            CAST('hi' AS CHAR(10)) AS c, \
            CAST(NULL AS CHAR(10)) AS nothing, \
            CAST(x'0102' AS BINARY(2)) AS bin, \
            CAST('2024-01-15' AS DATE) AS d, \
            CAST('2024-01-15 13:45:30' AS DATETIME) AS dt, \
            CAST('13:45:30' AS TIME) AS t"
            .to_owned();

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(sql, tx);

        let columns = match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => columns,
            other => panic!("expected Columns first, got {other:?}"),
        };
        assert_eq!(columns.len(), 8, "one column per selected expression");

        let mut rows = Vec::new();
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        assert_eq!(rows.len(), 1, "the query returns exactly one row");
        let cells = &rows[0].0;
        assert_eq!(cells[0], zsql_core::Value::Int(5));
        assert_eq!(cells[1], zsql_core::Value::Numeric("1.5".to_owned()));
        assert_eq!(cells[2], zsql_core::Value::Text("hi".to_owned()));
        assert_eq!(cells[3], zsql_core::Value::Null);
        assert_eq!(cells[4], zsql_core::Value::Bytes(vec![0x01, 0x02]));
        assert_eq!(
            cells[5],
            zsql_core::Value::Timestamp("2024-01-15".to_owned())
        );
        assert_eq!(
            cells[6],
            zsql_core::Value::Timestamp("2024-01-15T13:45:30".to_owned())
        );
        assert_eq!(cells[7], zsql_core::Value::Timestamp("13:45:30".to_owned()));
    }

    #[test]
    fn stream_query_maps_json_when_configured() {
        let conn = live_connection();

        // MariaDB has no native JSON wire type: JSON is a LONGTEXT alias
        // there (MySQL's JSON is a distinct type), so `JSON_OBJECT(...)`
        // decodes as `Value::Text` on MariaDB and `Value::Json` on MySQL.
        // Both carry the same text; this test accepts either variant
        // rather than assuming MySQL's richer typing.
        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT JSON_OBJECT('a', 1) AS j".to_owned(), tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 1),
            other => panic!("expected Columns first, got {other:?}"),
        }
        let mut rows = Vec::new();
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        let text = match &rows[0].0[0] {
            zsql_core::Value::Json(text) | zsql_core::Value::Text(text) => text.clone(),
            other => panic!("expected Value::Json or Value::Text, got {other:?}"),
        };
        assert!(
            text.contains('a') && text.contains('1'),
            "expected the encoded key/value to survive decode, got {text}"
        );
    }

    #[test]
    fn stream_query_maps_every_signed_integer_width_to_int_when_configured() {
        let conn = live_connection();

        run_ddl(&*conn, "DROP TABLE IF EXISTS zsql_test_signed_widths");
        run_ddl(
            &*conn,
            "CREATE TABLE zsql_test_signed_widths ( \
                 s_tiny TINYINT, \
                 s_small SMALLINT, \
                 s_medium MEDIUMINT, \
                 s_int INT, \
                 s_big BIGINT \
             )",
        );
        run_ddl(
            &*conn,
            "INSERT INTO zsql_test_signed_widths VALUES (-128, -32768, -8388608, -2147483648, -9223372036854775808)",
        );

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT * FROM zsql_test_signed_widths".to_owned(), tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 5),
            other => panic!("expected Columns first, got {other:?}"),
        }
        let mut rows = Vec::new();
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        let cells = &rows[0].0;
        assert_eq!(cells[0], zsql_core::Value::Int(-128));
        assert_eq!(cells[1], zsql_core::Value::Int(-32768));
        assert_eq!(cells[2], zsql_core::Value::Int(-8_388_608));
        assert_eq!(cells[3], zsql_core::Value::Int(-2_147_483_648));
        assert_eq!(cells[4], zsql_core::Value::Int(-9_223_372_036_854_775_808));

        run_ddl(&*conn, "DROP TABLE zsql_test_signed_widths");
    }

    #[test]
    fn stream_query_maps_a_timestamp_column_to_a_timestamp_value_when_configured() {
        let conn = live_connection();

        // `users.created_at` is seeded as a `TIMESTAMP` column (distinct
        // from `DATETIME`, which the representative-type-spread test
        // already covers); this proves the dedicated `"TIMESTAMP"` dispatch
        // arm in `values.rs`, not just `"DATETIME"`'s.
        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(
            "SELECT created_at FROM users ORDER BY id LIMIT 1".to_owned(),
            tx,
        );

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => {
                assert_eq!(columns.len(), 1);
                assert!(
                    columns[0].type_name.eq_ignore_ascii_case("TIMESTAMP"),
                    "expected the seeded column to still be declared TIMESTAMP, got {}",
                    columns[0].type_name
                );
            }
            other => panic!("expected Columns first, got {other:?}"),
        }
        let mut rows = Vec::new();
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        match &rows[0].0[0] {
            zsql_core::Value::Timestamp(text) => {
                assert!(!text.is_empty(), "timestamp text must not be empty");
            }
            other => panic!("expected Value::Timestamp, got {other:?}"),
        }
    }

    #[test]
    fn stream_query_maps_tinyint_one_to_bool_but_other_tinyint_widths_to_int_when_configured() {
        let conn = live_connection();

        run_ddl(&*conn, "DROP TABLE IF EXISTS zsql_test_tinyint_widths");
        run_ddl(
            &*conn,
            "CREATE TABLE zsql_test_tinyint_widths (flag BOOLEAN, small TINYINT)",
        );
        run_ddl(
            &*conn,
            "INSERT INTO zsql_test_tinyint_widths (flag, small) VALUES (1, 42)",
        );

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(
            "SELECT flag, small FROM zsql_test_tinyint_widths".to_owned(),
            tx,
        );

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 2),
            other => panic!("expected Columns first, got {other:?}"),
        }
        let mut rows = Vec::new();
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        assert_eq!(rows[0].0[0], zsql_core::Value::Bool(true));
        assert_eq!(rows[0].0[1], zsql_core::Value::Int(42));

        run_ddl(&*conn, "DROP TABLE zsql_test_tinyint_widths");
    }

    #[test]
    fn stream_query_maps_unsigned_integers_including_a_bigint_overflow_when_configured() {
        let conn = live_connection();

        run_ddl(&*conn, "DROP TABLE IF EXISTS zsql_test_unsigned");
        run_ddl(
            &*conn,
            "CREATE TABLE zsql_test_unsigned ( \
                 u_tiny TINYINT UNSIGNED, \
                 u_small SMALLINT UNSIGNED, \
                 u_medium MEDIUMINT UNSIGNED, \
                 u_int INT UNSIGNED, \
                 u_big_fits BIGINT UNSIGNED, \
                 u_big_overflow BIGINT UNSIGNED \
             )",
        );
        run_ddl(
            &*conn,
            "INSERT INTO zsql_test_unsigned VALUES (255, 65535, 16777215, 4294967295, 9223372036854775807, 18446744073709551615)",
        );

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT * FROM zsql_test_unsigned".to_owned(), tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 6),
            other => panic!("expected Columns first, got {other:?}"),
        }
        let mut rows = Vec::new();
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        let cells = &rows[0].0;
        assert_eq!(cells[0], zsql_core::Value::Int(255));
        assert_eq!(cells[1], zsql_core::Value::Int(65535));
        assert_eq!(cells[2], zsql_core::Value::Int(16_777_215));
        assert_eq!(cells[3], zsql_core::Value::Int(4_294_967_295));
        assert_eq!(cells[4], zsql_core::Value::Int(9_223_372_036_854_775_807));
        assert_eq!(
            cells[5],
            zsql_core::Value::Numeric("18446744073709551615".to_owned()),
            "a BIGINT UNSIGNED value beyond i64::MAX must round-trip exactly as text, \
             never silently truncate or wrap"
        );

        run_ddl(&*conn, "DROP TABLE zsql_test_unsigned");
    }

    #[test]
    fn stream_query_maps_decimal_at_full_precision_when_configured() {
        let conn = live_connection();

        // Wider than an f64 can represent exactly: verifies precision is
        // preserved as text, not rounded through a float.
        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(
            "SELECT CAST('123456789012345678901234567890.123456789' AS DECIMAL(41,9)) AS n"
                .to_owned(),
            tx,
        );

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 1),
            other => panic!("expected Columns first, got {other:?}"),
        }
        let mut rows = Vec::new();
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        assert_eq!(
            rows[0].0[0],
            zsql_core::Value::Numeric("123456789012345678901234567890.123456789".to_owned())
        );
    }

    #[test]
    fn stream_query_maps_float_and_double_when_configured() {
        let conn = live_connection();

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(
            "SELECT CAST(1.5 AS FLOAT) AS f, CAST(2.5 AS DOUBLE) AS d".to_owned(),
            tx,
        );

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 2),
            other => panic!("expected Columns first, got {other:?}"),
        }
        let mut rows = Vec::new();
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        assert_eq!(rows[0].0[0], zsql_core::Value::Float(1.5));
        assert_eq!(rows[0].0[1], zsql_core::Value::Float(2.5));
    }

    #[test]
    fn stream_query_maps_year_and_bit_to_defined_non_unknown_values_when_configured() {
        let conn = live_connection();

        run_ddl(&*conn, "DROP TABLE IF EXISTS zsql_test_year_bit");
        run_ddl(
            &*conn,
            "CREATE TABLE zsql_test_year_bit (y YEAR, flags BIT(8))",
        );
        run_ddl(
            &*conn,
            "INSERT INTO zsql_test_year_bit (y, flags) VALUES (2024, b'00000101')",
        );

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT y, flags FROM zsql_test_year_bit".to_owned(), tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 2),
            other => panic!("expected Columns first, got {other:?}"),
        }
        let mut rows = Vec::new();
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        assert_eq!(rows[0].0[0], zsql_core::Value::Int(2024));
        assert_eq!(rows[0].0[1], zsql_core::Value::Int(5));
        assert!(
            !matches!(rows[0].0[0], zsql_core::Value::Unknown(_)),
            "YEAR must not decode to Unknown"
        );
        assert!(
            !matches!(rows[0].0[1], zsql_core::Value::Unknown(_)),
            "BIT must not decode to Unknown"
        );

        run_ddl(&*conn, "DROP TABLE zsql_test_year_bit");
    }

    #[test]
    fn stream_query_maps_null_to_null_regardless_of_declared_type_when_configured() {
        let conn = live_connection();

        // `CAST(... AS JSON)` is deliberately not exercised here: MariaDB
        // has no `JSON` cast target at all (JSON is a `LONGTEXT` alias
        // there, not a real type), so that syntax is MySQL-only.
        //
        // `ST_GeomFromText(NULL)` exercises the raw_fallback NULL branch: its
        // declared column type is GEOMETRY, a type this module never maps,
        // so unlike the other three columns here it can only resolve to
        // Value::Null via raw_fallback's own is_null() check rather than a
        // scalar::<T, _> Ok(None) arm.
        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(
            "SELECT CAST(NULL AS SIGNED) AS n, CAST(NULL AS DATETIME) AS d, \
                    CAST(NULL AS DECIMAL(10,2)) AS dec_val, \
                    ST_GeomFromText(NULL) AS geom"
                .to_owned(),
            tx,
        );

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 4),
            other => panic!("expected Columns first, got {other:?}"),
        }
        let mut rows = Vec::new();
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        for cell in &rows[0].0 {
            assert_eq!(*cell, zsql_core::Value::Null);
        }
    }

    #[test]
    fn stream_query_maps_an_unmapped_type_to_unknown_carrying_its_type_name_when_configured() {
        let conn = live_connection();

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(
            "SELECT ST_GeomFromText('POINT(1 1)') AS geom".to_owned(),
            tx,
        );

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 1),
            other => panic!("expected Columns first, got {other:?}"),
        }
        let mut rows = Vec::new();
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        match &rows[0].0[0] {
            zsql_core::Value::Unknown(type_name) => {
                assert!(!type_name.is_empty(), "type name must not be empty");
            }
            other => panic!("expected Value::Unknown for an unmapped geometry type, got {other:?}"),
        }
    }
}
