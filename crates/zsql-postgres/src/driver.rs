//! The Postgres [`Driver`] and its live [`Connection`] implementation, built
//! on sqlx with the **smol** runtime so its futures await directly on gpui's
//! executor — no tokio runtime, no bridge thread.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{AssertSqlSafe, Executor as _, Row as _, SqlSafeStr as _, Statement as _};
use zsql_core::{
    BatchSink, ConnConfig, Connection, CoreError, Driver, QueryEvent, QueryHandle, RowBatch,
    SchemaTree,
};

use crate::error::{map_connect_error, map_query_error};
use crate::values::{column_metas, decode_row};

/// Rows are grouped into batches of at most this many rows before a
/// [`QueryEvent::Batch`] is pushed into the sink. Bounded so a large result
/// set streams to the UI incrementally instead of arriving as one huge
/// allocation. This is an internal placeholder default: threading the app's
/// configured batch size through from `Config` into the driver happens at
/// the layer that wires a session's `Connection` up to its `Config`, not
/// here.
const DEFAULT_QUERY_BATCH_SIZE: usize = 500;

/// Bounded pool size for a single desktop client. Small on purpose: this app
/// drives at most a handful of concurrent operations (one running query plus
/// occasional introspection), and a modest ceiling avoids hammering the
/// server from a client that only ever has one user.
const MAX_POOL_CONNECTIONS: u32 = 5;

/// How long to wait for the initial connection (and later, a free pooled
/// connection) before giving up. Bounded so a stuck DSN fails fast instead of
/// hanging the caller indefinitely.
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounded size of the dedicated pool [`issue_server_side_cancel`] draws
/// from. Deliberately separate from `MAX_POOL_CONNECTIONS` and small: a
/// `pg_cancel_backend` call is a single scalar query, never more than a
/// couple of which are ever in flight at once for this single-user desktop
/// client, and keeping it off the query pool entirely is the point (see the
/// doc comment on [`PostgresDriver::build_cancel_pool`]).
const CANCEL_POOL_CONNECTIONS: u32 = 2;

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

    /// Build the small dedicated pool used only for issuing `SELECT
    /// pg_cancel_backend($pid)` (see [`issue_server_side_cancel`]).
    ///
    /// This is deliberately a *different* pool from the one query
    /// connections are drawn from (see [`build_pool`](Self::build_pool)),
    /// not a second handle onto the same one. A query connection that is
    /// cooperatively cancelled while blocked server-side does not
    /// necessarily free its permit back to the query pool right away:
    /// returning a pooled connection first pings it to confirm it is idle
    /// before the permit is released, and that ping itself blocks until the
    /// backend actually responds -- exactly the kind of response a
    /// still-blocked query withholds until a server-side cancel reaches it.
    /// If cancellation drew its own connection from that same pool, several
    /// blocked-then-cancelled queries saturating every permit could starve
    /// the very connection needed to unblock any of them, deadlocking the
    /// pool for the queries' full natural duration. Drawing from an
    /// entirely separate, independently-bounded pool means issuing a cancel
    /// can never be blocked by query-pool saturation.
    ///
    /// Connects lazily: parsing/validating `url` cannot fail asynchronously
    /// here, so this is synchronous, and no network round trip happens
    /// against this pool until the first cancel is actually issued.
    ///
    /// # Errors
    /// Returns [`CoreError::Connection`] if `url` cannot be parsed.
    fn build_cancel_pool(url: &str) -> Result<PgPool, CoreError> {
        PgPoolOptions::new()
            .max_connections(CANCEL_POOL_CONNECTIONS)
            .acquire_timeout(POOL_ACQUIRE_TIMEOUT)
            .connect_lazy(url)
            .map_err(map_connect_error)
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
        let cancel_pool = Self::build_cancel_pool(&cfg.url)?;
        tracing::info!("postgres connection established");
        Ok(Box::new(PgConnection { pool, cancel_pool }))
    }
}

/// A live Postgres connection, backed by a bounded sqlx connection pool.
pub struct PgConnection {
    pool: PgPool,
    /// Separate, independently-bounded pool used only for server-side
    /// cancellation (`SELECT pg_cancel_backend($pid)`). Never draws from
    /// `pool` -- see [`PostgresDriver::build_cancel_pool`] for why sharing
    /// one pool between running queries and their own cancellation is
    /// unsafe.
    cancel_pool: PgPool,
}

/// Issue `SELECT pg_cancel_backend($pid)` on a connection acquired fresh
/// from `cancel_pool` -- deliberately *not* the connection running the query
/// being cancelled, which may be blocked server-side and unable to answer
/// anything until the cancel itself takes effect, and deliberately *not* the
/// shared query pool either (see [`PostgresDriver::build_cancel_pool`]) so
/// this can never be starved by other queries that pool is busy running or
/// cancelling. Best-effort: any failure (including the target backend having
/// already finished on its own) is logged and swallowed here, never
/// surfaced to the query's sink -- a query that finishes naturally in the
/// small window before this fires is a success, not an error.
#[tracing::instrument(name = "pg_cancel_backend", skip(cancel_pool))]
async fn issue_server_side_cancel(cancel_pool: &PgPool, pid: i32) {
    match sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
        .bind(pid)
        .fetch_one(cancel_pool)
        .await
    {
        Ok(signalled) => {
            tracing::info!(pid, signalled, "server-side cancel issued");
        }
        Err(err) => {
            tracing::warn!(pid, error = %err, "server-side cancel request failed");
        }
    }
}

/// Spawn [`issue_server_side_cancel`] as a detached background task so
/// neither the query task (which may itself be about to return) nor the
/// caller of `cancel()` has to wait for the cancel round-trip to complete.
fn spawn_server_side_cancel(cancel_pool: &PgPool, pid: i32) {
    let cancel_pool = cancel_pool.clone();
    async_global_executor::spawn(async move { issue_server_side_cancel(&cancel_pool, pid).await })
        .detach();
}

#[async_trait]
impl Connection for PgConnection {
    fn stream_query(&self, sql: String, sink: BatchSink) -> QueryHandle {
        let (cancel_tx, cancel_rx) = flume::unbounded();
        let pool = self.pool.clone();
        let cancel_pool = self.cancel_pool.clone();
        // Run on the smol-based executor sqlx's `runtime-smol` feature
        // already drives its own futures with, so this never needs (or
        // creates) a tokio runtime. The returned `Task` is detached: the
        // caller drives the query lifecycle through `sink` and `cancel_rx`
        // (via the returned `QueryHandle`), not by awaiting this task.
        async_global_executor::spawn(run_query(pool, cancel_pool, sql, sink, cancel_rx)).detach();
        QueryHandle::new(cancel_tx)
    }

    #[tracing::instrument(name = "pg_introspect", skip_all, fields(pool_size = self.pool.size()))]
    async fn introspect(&self) -> Result<SchemaTree, CoreError> {
        crate::introspect::introspect(&self.pool).await
    }
}

/// Stream one query's results into `sink` as: exactly one
/// [`QueryEvent::Columns`], then zero or more [`QueryEvent::Batch`], then
/// exactly one [`QueryEvent::Done`] — or, on any failure, a single `Err` in
/// place of `Done`.
///
/// Runs on a single connection acquired from `pool` for the lifetime of this
/// call (not the pool directly), so that connection's backend PID can be
/// captured via `SELECT pg_backend_pid()` *before* the row-streaming loop
/// below ever starts checking `cancel_rx`. That ordering is what makes the
/// PID capture and cancellation-observed steps purely sequential within this
/// one task rather than a race to guard against: by the time this task can
/// possibly observe a cancellation, the PID is either already known (the
/// common case) or capture already failed and never will be (in which case
/// only cooperative cancellation applies below; the server-side cancel is
/// simply unavailable for this one query). Cancelling means running `SELECT
/// pg_cancel_backend($pid)` for that PID on `cancel_pool` -- a separate pool
/// from `pool` (see [`PostgresDriver::build_cancel_pool`]) -- which is the
/// only way to actually interrupt work already blocked server-side
/// (dropping/ignoring this task alone only stops the client from reading
/// further rows).
///
/// The rows themselves are fetched with [`sqlx::raw_sql`] (Postgres's simple
/// query protocol) rather than a prepared statement, which keeps every
/// column's wire representation in text format — see [`crate::values`] for
/// why that matters for decoding types this driver does not explicitly map.
/// The simple query protocol also accepts `sql` containing more than one
/// `;`-separated statement (their results concatenate), unlike a `PREPARE`
/// / describe cycle, which can only parse one statement at a time.
///
/// Column metadata for `Columns` is therefore taken from the first row any
/// statement in `sql` produces (a [`sqlx::postgres::PgRow`] carries its own
/// column list), not from an upfront describe: an upfront describe would
/// reject any multi-statement `sql` even though the simple protocol executes
/// it fine. If no statement ever produces a row (DDL, DML without
/// `RETURNING`, or a zero-row `SELECT`), a describe is run as a fallback
/// *after* execution has already completed successfully, purely to recover
/// a zero-row `SELECT`'s column list; if that fallback describe itself
/// fails (for instance because `sql` was multiple statements and none of
/// them produced a row), the query has already succeeded, so this degrades
/// to reporting no columns rather than failing an otherwise-successful
/// query at this late stage.
#[tracing::instrument(name = "pg_stream_query", skip_all, fields(pool_size = pool.size()))]
async fn run_query(
    pool: PgPool,
    cancel_pool: PgPool,
    sql: String,
    sink: BatchSink,
    cancel_rx: flume::Receiver<()>,
) {
    // The SQL text itself carries no connection secrets (those live only in
    // the DSN, never logged here), so it is fine to record at debug level.
    tracing::debug!(sql = %sql, "streaming query");

    let mut conn = match pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => {
            let _ = sink.send_async(Err(map_query_error(err))).await;
            return;
        }
    };

    // Capture this connection's backend PID so a later cancel can target it
    // via `pg_cancel_backend` on `cancel_pool`. This always runs to
    // completion (success or failure) before the row-streaming loop below
    // ever checks `cancel_rx`, so `pid` is settled -- known or permanently
    // unknown -- before cancellation can be observed; see this function's
    // doc comment for why that ordering means no race needs guarding here.
    let pid = match sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
        .fetch_one(&mut *conn)
        .await
    {
        Ok(pid) => {
            tracing::debug!(pid, "dedicated connection backend pid captured");
            Some(pid)
        }
        Err(err) => {
            // Cooperative cancellation (the `select` loop below) still
            // works without a known pid; only the server-side
            // `pg_cancel_backend` path is unavailable for this one query.
            // Never fatal to the query itself.
            tracing::warn!(
                error = %err,
                "failed to capture backend pid; server-side cancel unavailable for this query"
            );
            None
        }
    };

    let mut rows = sqlx::raw_sql(AssertSqlSafe(sql.clone())).fetch_many(&mut *conn);
    let mut batch = RowBatch::new();
    let mut affected: u64 = 0;
    let mut columns_sent = false;

    loop {
        // `futures::future::select` polls its first argument before its
        // second, and only polls the second if the first is `Pending` — so
        // the cancellation check goes first here. `rows.next()` can stay
        // synchronously `Ready` for many consecutive polls once the client
        // has a chunk of the wire response already buffered (no real
        // `Pending` point to yield at), which would starve a cancellation
        // future placed second indefinitely, up to the whole result set
        // draining before cancellation is ever observed.
        let step = futures::future::select(cancel_rx.recv_async(), rows.next());
        match step.await {
            futures::future::Either::Left(_) => {
                // Cancelled: either an explicit `cancel()` call or every
                // `QueryHandle` clone (hence every `cancel_tx`) was dropped.
                // Stop fetching further rows; `rows` (and then `conn`) are
                // dropped when this function returns, releasing the
                // connection back to the pool. That alone only stops this
                // client from reading further rows -- it does not interrupt
                // work already running server-side, which is why the
                // server-side cancel below is also needed.
                tracing::debug!("query cancelled");
                if let Some(pid) = pid {
                    spawn_server_side_cancel(&cancel_pool, pid);
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
                affected += result.rows_affected();
            }
            futures::future::Either::Right((Some(Err(err)), _)) => {
                let _ = sink.send_async(Err(map_query_error(err))).await;
                return;
            }
        }
    }

    // A statement with no output columns (DDL, or DML without `RETURNING`)
    // reports its row count as `affected` in `Done`. A statement that does
    // produce columns (SELECT, or DML with `RETURNING`) instead lets the
    // caller derive a count from the rows it already streamed, and reports
    // `affected: None` — matching `QueryEvent::Done`'s doc comment that
    // `affected` is for non-SELECT statements. When no row was ever seen,
    // that distinction is only recoverable via the describe fallback below.
    let reports_affected = if columns_sent {
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
    use std::time::Duration;

    use zsql_core::{ConnConfig, Connection, Driver};

    use super::{PgConnection, PostgresDriver};

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
        let Some(_conn) = live_connection() else {
            return;
        };
    }

    /// `introspect` must map a `sqlx` failure to `CoreError::Introspection`
    /// without panicking or hanging, exactly like `stream_query` does above:
    /// a lazily-connected pool to an unreachable host never touches the
    /// network until the first real query, which here is inside
    /// `introspect` itself. Deliberately not gated on
    /// `ZSQL_TEST_DATABASE_URL`: this must pass with no database present.
    #[test]
    fn introspect_maps_unreachable_host_to_core_introspection_error() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy(UNREACHABLE_DSN)
            .expect("connect_lazy only parses the DSN; it must not touch the network");
        let cancel_pool = pool.clone();
        let conn = PgConnection { pool, cancel_pool };

        let result = block_on(conn.introspect());
        match result {
            Err(zsql_core::CoreError::Introspection(msg)) => {
                assert!(!msg.is_empty(), "error message should not be empty");
            }
            Err(other) => panic!("expected CoreError::Introspection, got {other:?}"),
            Ok(_) => panic!("introspecting an unreachable host must fail"),
        }
    }

    /// Builds a [`SchemaTree`] against the seeded dev database
    /// (`scripts/pg-dev.sh` + `dev/seed.sql`) and checks it against what that
    /// seed is known to contain: a `public` schema with `users` and `orders`
    /// tables, a `recent_orders` view, a `recent_orders_mv` materialized
    /// view, and a partitioned `events` table; no system schemas; and a
    /// couple of columns whose nullability is known from the seed's DDL.
    #[test]
    fn introspect_builds_schema_tree_matching_the_seeded_database_when_configured() {
        let Some(url) = live_database_url() else {
            return;
        };
        let driver = PostgresDriver;
        let cfg = ConnConfig::from_dsn(&url).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let tree = block_on(conn.introspect()).expect("introspect should succeed");

        assert_eq!(
            tree.catalogs.len(),
            1,
            "a postgres connection sees exactly one catalog"
        );
        let catalog = &tree.catalogs[0];
        assert_eq!(
            catalog.name,
            database_name_from_url(&url),
            "catalog name must be the connected database"
        );

        assert!(
            catalog.schemas.iter().all(|s| {
                s.name != "pg_catalog" && s.name != "information_schema" && s.name != "pg_toast"
            }),
            "system schemas must be excluded, got schemas: {:?}",
            catalog.schemas.iter().map(|s| &s.name).collect::<Vec<_>>()
        );

        let public = catalog
            .schemas
            .iter()
            .find(|s| s.name == "public")
            .expect("the seeded database has a public schema");

        let users = public
            .tables
            .iter()
            .find(|r| r.name == "users")
            .expect("the seeded users table is present");
        assert_eq!(users.kind, zsql_core::RelationKind::Table);

        let orders = public
            .tables
            .iter()
            .find(|r| r.name == "orders")
            .expect("the seeded orders table is present");
        assert_eq!(orders.kind, zsql_core::RelationKind::Table);

        let recent_orders = public
            .tables
            .iter()
            .find(|r| r.name == "recent_orders")
            .expect("the seeded recent_orders view is present");
        assert_eq!(recent_orders.kind, zsql_core::RelationKind::View);

        // The only seeded object with `pg_class.relkind = 'm'`: proves the
        // materialized-view arm of the mapping actually fires against a
        // live server, not just in the offline `relation_kind` unit test.
        let recent_orders_mv = public
            .tables
            .iter()
            .find(|r| r.name == "recent_orders_mv")
            .expect("the seeded recent_orders_mv materialized view is present");
        assert_eq!(recent_orders_mv.kind, zsql_core::RelationKind::MatView);

        // The only seeded object with `pg_class.relkind = 'p'`: proves the
        // partitioned-table arm of the mapping surfaces as an ordinary
        // `Table`, and that its partition is enumerated as its own table.
        let events = public
            .tables
            .iter()
            .find(|r| r.name == "events")
            .expect("the seeded partitioned events table is present");
        assert_eq!(events.kind, zsql_core::RelationKind::Table);

        let events_2024 = public
            .tables
            .iter()
            .find(|r| r.name == "events_2024")
            .expect("the seeded events_2024 partition is present");
        assert_eq!(events_2024.kind, zsql_core::RelationKind::Table);

        let email = users
            .columns
            .iter()
            .find(|c| c.name == "email")
            .expect("users.email column is present");
        assert!(!email.nullable, "users.email is declared NOT NULL");
        assert!(
            email.type_name.to_lowercase().contains("char")
                || email.type_name.to_lowercase().contains("text"),
            "users.email should be a text-family type, got {}",
            email.type_name
        );

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

    /// Schemas must be sorted by name, relations sorted by name within a
    /// schema, and columns in ordinal-position order (not alphabetical) —
    /// otherwise the sidebar this feeds would reorder on every refresh.
    #[test]
    fn introspect_orders_schemas_relations_and_columns_deterministically_when_configured() {
        let Some(conn) = live_connection() else {
            return;
        };

        let tree = block_on(conn.introspect()).expect("introspect should succeed");
        let catalog = &tree.catalogs[0];

        let schema_names: Vec<&str> = catalog.schemas.iter().map(|s| s.name.as_str()).collect();
        let mut sorted_schema_names = schema_names.clone();
        sorted_schema_names.sort_unstable();
        assert_eq!(
            schema_names, sorted_schema_names,
            "schemas must be sorted by name"
        );

        let public = catalog
            .schemas
            .iter()
            .find(|s| s.name == "public")
            .expect("the seeded database has a public schema");
        let relation_names: Vec<&str> = public.tables.iter().map(|r| r.name.as_str()).collect();
        let mut sorted_relation_names = relation_names.clone();
        sorted_relation_names.sort_unstable();
        assert_eq!(
            relation_names, sorted_relation_names,
            "relations must be sorted by name within a schema"
        );

        let users = public
            .tables
            .iter()
            .find(|r| r.name == "users")
            .expect("the seeded users table is present");
        let column_names: Vec<&str> = users.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            column_names,
            vec!["id", "email", "display_name", "is_active", "created_at"],
            "columns must be in table/ordinal-position order, not alphabetical"
        );
    }

    /// Introspection must walk every non-system schema, not just `public`:
    /// the seeded `analytics` schema (with a table) and `empty_ns` schema
    /// (with none) must both appear, `empty_ns` with an empty relation list
    /// rather than being dropped for having nothing in it.
    #[test]
    fn introspect_includes_non_public_schemas_including_an_empty_one_when_configured() {
        let Some(conn) = live_connection() else {
            return;
        };

        let tree = block_on(conn.introspect()).expect("introspect should succeed");
        let catalog = &tree.catalogs[0];

        let analytics = catalog
            .schemas
            .iter()
            .find(|s| s.name == "analytics")
            .expect("the seeded analytics schema is present");
        let page_views = analytics
            .tables
            .iter()
            .find(|r| r.name == "page_views")
            .expect("the seeded analytics.page_views table is present");
        assert_eq!(page_views.kind, zsql_core::RelationKind::Table);
        assert!(
            page_views.columns.iter().any(|c| c.name == "path"),
            "analytics.page_views should carry its columns too"
        );

        let empty_ns = catalog
            .schemas
            .iter()
            .find(|s| s.name == "empty_ns")
            .expect("the seeded empty_ns schema is present even though it holds nothing");
        assert!(
            empty_ns.tables.is_empty(),
            "empty_ns has no tables/views in the seed"
        );
    }

    /// Column attribution keys columns by `(schema, relation)`, not relation
    /// name alone. `public.users` and `analytics.users` are two different
    /// tables that only share a name (see `dev/seed.sql`); if columns were
    /// ever matched back by relation name alone instead of the full
    /// `(schema, relation)` pair, one of these two tables would silently end
    /// up with the other's columns (or none at all) the moment a name
    /// collision like this exists, even though every other seeded relation
    /// name is unique and so would not catch that regression.
    #[test]
    fn introspect_attributes_columns_by_schema_and_relation_not_name_alone_when_configured() {
        let Some(conn) = live_connection() else {
            return;
        };

        let tree = block_on(conn.introspect()).expect("introspect should succeed");
        let catalog = &tree.catalogs[0];

        let public_users = catalog
            .schemas
            .iter()
            .find(|s| s.name == "public")
            .and_then(|s| s.tables.iter().find(|r| r.name == "users"))
            .expect("public.users is present");
        let analytics_users = catalog
            .schemas
            .iter()
            .find(|s| s.name == "analytics")
            .and_then(|s| s.tables.iter().find(|r| r.name == "users"))
            .expect("analytics.users is present");

        let public_names: Vec<&str> = public_users
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        let analytics_names: Vec<&str> = analytics_users
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();

        assert_eq!(
            public_names,
            vec!["id", "email", "display_name", "is_active", "created_at"],
            "public.users must keep its own columns"
        );
        assert_eq!(
            analytics_names,
            vec!["user_id", "username"],
            "analytics.users must keep its own distinct columns, not public.users's"
        );
        assert_ne!(
            public_names, analytics_names,
            "same-named tables in different schemas must not share a column set"
        );
    }

    /// Extract the database name (last path segment, query string stripped)
    /// from a Postgres URL, to compare against a live-introspected catalog
    /// name without hardcoding the seeded dev database's name in the test.
    fn database_name_from_url(url: &str) -> &str {
        let after_slash = url.rsplit('/').next().unwrap_or_default();
        after_slash.split('?').next().unwrap_or(after_slash)
    }

    /// A pool that connects lazily never touches the network until first
    /// use, so this builds a `PgConnection` directly (bypassing
    /// `PostgresDriver::connect`, which would fail during its own eager
    /// liveness check) to exercise `stream_query`'s error path in isolation:
    /// the background task's first real query — `pool.prepare(..)` — is what
    /// discovers the host is unreachable.
    #[test]
    fn stream_query_pushes_single_error_when_pool_is_unreachable() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy(UNREACHABLE_DSN)
            .expect("connect_lazy only parses the DSN; it must not touch the network");
        let cancel_pool = pool.clone();
        let conn = PgConnection { pool, cancel_pool };

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

    /// Runs a query with one column of each mapped type plus a NULL, an
    /// array, and a JSON value, and asserts both the event sequence
    /// (`Columns` -> `Batch`(es) -> `Done`) and the decoded `Value`s.
    #[test]
    fn stream_query_maps_a_representative_type_spread_when_configured() {
        let Some(conn) = live_connection() else {
            return;
        };

        let sql = "SELECT \
            true AS b, \
            2::int2 AS i2, \
            4::int4 AS i4, \
            8::int8 AS i8, \
            1.5::float4 AS f4, \
            2.5::float8 AS f8, \
            123.456::numeric AS num, \
            'hi'::text AS t, \
            NULL::text AS nothing, \
            '11111111-1111-1111-1111-111111111111'::uuid AS u, \
            ARRAY[1, NULL, 3]::int4[] AS arr, \
            '{\"a\": 1}'::jsonb AS j, \
            '\\x0102'::bytea AS by, \
            '2024-01-15'::date AS d, \
            '13:45:30'::time AS tm, \
            '2024-01-15 13:45:30'::timestamp AS ts, \
            '2024-01-15 13:45:30+00'::timestamptz AS tstz, \
            '1 day'::interval AS iv"
            .to_owned();

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(sql, tx);

        let columns = match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => columns,
            other => panic!("expected Columns first, got {other:?}"),
        };
        assert_eq!(columns.len(), 18, "one column per selected expression");

        let mut rows = Vec::new();
        let affected = loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { affected }) => break affected,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        };
        // A SELECT reports its row count through the streamed rows
        // themselves, not `affected`.
        assert_eq!(affected, None);

        assert_eq!(rows.len(), 1, "the query returns exactly one row");
        let cells = &rows[0].0;
        assert_eq!(cells[0], zsql_core::Value::Bool(true));
        assert_eq!(cells[1], zsql_core::Value::Int(2));
        assert_eq!(cells[2], zsql_core::Value::Int(4));
        assert_eq!(cells[3], zsql_core::Value::Int(8));
        assert_eq!(cells[4], zsql_core::Value::Float(1.5));
        assert_eq!(cells[5], zsql_core::Value::Float(2.5));
        assert_eq!(cells[6], zsql_core::Value::Numeric("123.456".to_owned()));
        assert_eq!(cells[7], zsql_core::Value::Text("hi".to_owned()));
        assert_eq!(cells[8], zsql_core::Value::Null);
        assert_eq!(
            cells[9],
            zsql_core::Value::Uuid("11111111-1111-1111-1111-111111111111".to_owned())
        );
        assert_eq!(
            cells[10],
            zsql_core::Value::Array(vec![
                zsql_core::Value::Int(1),
                zsql_core::Value::Null,
                zsql_core::Value::Int(3),
            ])
        );
        // Postgres's own `jsonb` text output (`{"a": 1}`, with a space after
        // the colon), not a serde_json re-serialization: decoding jsonb as a
        // raw string preserves exactly what the server holds instead of
        // risking precision loss or key reordering from a round-trip through
        // `serde_json::Value`.
        assert_eq!(cells[11], zsql_core::Value::Json("{\"a\": 1}".to_owned()));
        assert_eq!(cells[12], zsql_core::Value::Bytes(vec![0x01, 0x02]));
        assert_eq!(
            cells[13],
            zsql_core::Value::Timestamp("2024-01-15".to_owned())
        );
        assert_eq!(
            cells[14],
            zsql_core::Value::Timestamp("13:45:30".to_owned())
        );
        assert_eq!(
            cells[15],
            zsql_core::Value::Timestamp("2024-01-15T13:45:30".to_owned()),
            "fractionless timestamp uses a T separator, not chrono's default space"
        );
        assert_eq!(
            cells[16],
            zsql_core::Value::Timestamp("2024-01-15T13:45:30+00:00".to_owned()),
            "timestamptz renders as RFC3339"
        );
        // interval is not an explicitly mapped type: it must degrade to
        // Value::Unknown via the raw-text fallback rather than erroring the
        // whole query. This is the "never errors on an unmapped type" guarantee.
        assert!(
            matches!(cells[17], zsql_core::Value::Unknown(_)),
            "an unmapped type (interval) must decode to Value::Unknown, got {:?}",
            cells[17],
        );
    }

    /// `json`/`jsonb` scalars and their 1-D array forms (`json[]`/`jsonb[]`)
    /// must decode as `Value::Json` holding the server's own text, never a
    /// `serde_json`-reserialized copy. This is checked three ways: (1) a
    /// `jsonb` object whose two keys sort differently by length-then-byte
    /// order (Postgres's own canonical `jsonb` key order) than
    /// alphabetically — a `serde_json::Value` round trip through its
    /// default `BTreeMap`-backed object would alphabetize and so diverge
    /// from Postgres's actual canonical text; (2) a `jsonb` integer wider
    /// than `i64`/`f64`, which a `serde_json::Value` round trip (no
    /// `arbitrary_precision` feature enabled in this workspace) would
    /// corrupt; (3) a `json` (not `jsonb`) scalar with irregular whitespace,
    /// which `json` (unlike `jsonb`) preserves verbatim — any reformatting
    /// proves a reserialization happened.
    #[test]
    fn stream_query_maps_json_and_jsonb_scalars_and_arrays_when_configured() {
        let Some(conn) = live_connection() else {
            return;
        };

        let sql = "SELECT \
            '{\"aa\": 1, \"b\": 2}'::jsonb AS jsonb_key_order, \
            '{\"n\": 123456789012345678901234567890}'::jsonb AS jsonb_big_int, \
            '{ \"a\" : 1 }'::json AS json_preserves_whitespace, \
            NULL::jsonb AS jsonb_null, \
            ARRAY['{\"a\":1}'::jsonb, NULL, '{\"b\":2}'::jsonb] AS jsonb_array, \
            ARRAY['{\"a\":1}'::json, NULL] AS json_array"
            .to_owned();

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(sql, tx);

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
        assert_eq!(rows.len(), 1);
        let cells = &rows[0].0;

        // Postgres's own canonical order for these two keys is length-then-
        // byte-order ("b" before "aa"), not alphabetical ("aa" before "b").
        // Getting this exact string proves no `BTreeMap`-backed
        // `serde_json::Value` round trip happened.
        assert_eq!(
            cells[0],
            zsql_core::Value::Json("{\"b\": 2, \"aa\": 1}".to_owned()),
            "jsonb key order must match Postgres's own canonical order, not alphabetical"
        );
        assert_eq!(
            cells[1],
            zsql_core::Value::Json("{\"n\": 123456789012345678901234567890}".to_owned()),
            "an integer wider than i64/f64 must survive with no precision loss"
        );
        assert_eq!(
            cells[2],
            zsql_core::Value::Json("{ \"a\" : 1 }".to_owned()),
            "json (unlike jsonb) preserves the original whitespace exactly"
        );
        assert_eq!(cells[3], zsql_core::Value::Null);
        assert_eq!(
            cells[4],
            zsql_core::Value::Array(vec![
                zsql_core::Value::Json("{\"a\": 1}".to_owned()),
                zsql_core::Value::Null,
                zsql_core::Value::Json("{\"b\": 2}".to_owned()),
            ]),
            "jsonb[] must decode to an Array of Json values, NULL element included"
        );
        assert_eq!(
            cells[5],
            zsql_core::Value::Array(vec![
                zsql_core::Value::Json("{\"a\":1}".to_owned()),
                zsql_core::Value::Null,
            ]),
            "json[] must preserve each element's original text exactly"
        );
    }

    /// Two `;`-separated statements must both run and have their rows
    /// concatenated into the stream, proving `Columns` no longer comes from
    /// an upfront `PREPARE`-style describe (which can only parse a single
    /// statement and would error on input like this before either statement
    /// ever executes).
    #[test]
    fn stream_query_supports_multiple_semicolon_separated_statements_when_configured() {
        let Some(conn) = live_connection() else {
            return;
        };

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT 1 AS n; SELECT 2 AS n".to_owned(), tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => {
                assert_eq!(columns.len(), 1);
                assert_eq!(columns[0].name, "n");
            }
            other => panic!("expected Columns first, got {other:?}"),
        }

        let mut rows = Vec::new();
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { affected }) => {
                    assert_eq!(affected, None);
                    break;
                }
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        assert_eq!(
            rows.iter().map(|row| row.0[0].clone()).collect::<Vec<_>>(),
            vec![zsql_core::Value::Int(1), zsql_core::Value::Int(2)],
            "rows from both statements must concatenate in order"
        );
    }

    /// A result set larger than the batch size must arrive as multiple
    /// `Batch` events, each bounded by `DEFAULT_QUERY_BATCH_SIZE`, whose rows
    /// concatenate back to the full result.
    #[test]
    fn stream_query_batches_large_result_sets_when_configured() {
        let Some(conn) = live_connection() else {
            return;
        };

        let row_count = super::DEFAULT_QUERY_BATCH_SIZE * 2 + 7;
        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(
            format!("SELECT g FROM generate_series(1, {row_count}) AS g"),
            tx,
        );

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
    }

    /// A statement with no output columns (here, an `UPDATE`) reports its
    /// row count through `Done { affected }` instead of streaming rows.
    #[test]
    fn stream_query_reports_affected_rows_for_dml_when_configured() {
        let Some(conn) = live_connection() else {
            return;
        };

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(
            "UPDATE users SET display_name = display_name".to_owned(),
            tx,
        );

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => {
                assert!(columns.is_empty(), "DML has no output columns");
            }
            other => panic!("expected Columns first, got {other:?}"),
        }
        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Done { affected }) => {
                assert_eq!(affected, Some(3), "the seeded fixture has 3 users");
            }
            other => panic!("expected Done with no Batch in between, got {other:?}"),
        }
    }

    /// A zero-row result set must still emit `Columns` before `Done`, with
    /// no `Batch` events in between.
    #[test]
    fn stream_query_emits_columns_for_a_zero_row_result_when_configured() {
        let Some(conn) = live_connection() else {
            return;
        };

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT 1 AS one WHERE false".to_owned(), tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 1),
            other => panic!("expected Columns first, got {other:?}"),
        }
        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Done { affected }) => assert_eq!(affected, None),
            other => panic!("expected Done with no Batch in between, got {other:?}"),
        }
    }

    /// Dropping the `QueryHandle` (here, its only clone) must promptly stop
    /// the background task from fetching further rows: a long-running query
    /// should not be able to push a `Done` after its handle is gone.
    #[test]
    fn dropping_the_query_handle_stops_further_rows_when_configured() {
        let Some(conn) = live_connection() else {
            return;
        };

        let (tx, rx) = flume::unbounded();
        let handle =
            conn.stream_query("SELECT * FROM generate_series(1, 100000000)".to_owned(), tx);

        // Let the query get started (past `Columns`) before cancelling.
        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(_)) => {}
            other => panic!("expected Columns first, got {other:?}"),
        }
        drop(handle);

        // Drain whatever was already in flight; a `Done` must never appear,
        // and the channel must settle (close) promptly instead of hanging.
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
    }

    /// Calling `QueryHandle::cancel()` explicitly (handle kept alive, unlike
    /// the drop case above) must also promptly stop the background task from
    /// fetching further rows: this exercises the live-sender-delivers-a-value
    /// path through `cancel_rx.recv_async()`, distinct from the
    /// sender-all-dropped path the drop test covers.
    #[test]
    fn calling_cancel_stops_further_rows_when_configured() {
        let Some(conn) = live_connection() else {
            return;
        };

        let (tx, rx) = flume::unbounded();
        let handle =
            conn.stream_query("SELECT * FROM generate_series(1, 100000000)".to_owned(), tx);

        // Let the query get started (past `Columns`) before cancelling.
        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(_)) => {}
            other => panic!("expected Columns first, got {other:?}"),
        }
        handle.cancel();

        // Drain whatever was already in flight; a `Done` must never appear,
        // and the channel must settle (close) promptly instead of hanging.
        // `handle` (and its `cancel_tx`) is kept alive for the whole loop, so
        // this only passes if the explicit `cancel()` send is itself what
        // stops the task, not a subsequent drop of the handle.
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
    }

    /// Proves cancellation is server-side, not merely client-side: starts a
    /// `pg_sleep(30)` (a query that only a signal delivered on the server can
    /// interrupt -- no amount of the client simply not reading rows stops
    /// it), cancels it, and then confirms via a *separate* connection's
    /// `pg_stat_activity` that no backend is still actively running that
    /// `pg_sleep` well within the 30s sleep bound. Also asserts the
    /// streaming side never reaches `Done`, so both halves of the
    /// cancellation contract (cooperative + server-side) are checked
    /// together.
    #[test]
    fn cancel_stops_a_server_side_blocking_query_when_configured() {
        let Some(url) = live_database_url() else {
            return;
        };
        let driver = PostgresDriver;
        let cfg = ConnConfig::from_dsn(&url).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let (tx, rx) = flume::unbounded();
        let handle = conn.stream_query("SELECT pg_sleep(30)".to_owned(), tx);

        // The dedicated connection always captures its backend pid before
        // this task ever checks for cancellation (see `run_query`'s doc
        // comment), so this delay is not needed for that ordering. It exists
        // so `pg_sleep` is genuinely in progress server-side by the time
        // `cancel()` fires below, proving the assertions after this prove a
        // cancel interrupting already-running server-side work, not a query
        // that never got the chance to start.
        std::thread::sleep(Duration::from_millis(500));
        handle.cancel();

        // No `Done` (or anything else past what was already in flight) may
        // ever arrive: cooperative cancellation must still hold.
        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(Ok(zsql_core::QueryEvent::Done { .. })) => {
                    panic!("a cancelled query must not reach Done")
                }
                Ok(_) => {}
                Err(flume::RecvTimeoutError::Disconnected) => break,
                Err(flume::RecvTimeoutError::Timeout) => {
                    panic!("cancellation did not stop the stream promptly")
                }
            }
        }
        drop(handle);

        // Now prove the *server* actually stopped executing pg_sleep, using
        // a connection independent of the one the query ran on. Poll with a
        // bound far short of the 30s sleep: if `pg_cancel_backend` was never
        // issued, this loop will still see the backend active at the
        // deadline and fail, whereas cooperative-only cancellation (just not
        // reading more rows) would leave the server-side sleep running the
        // entire 30 seconds.
        let check_pool = block_on(sqlx::postgres::PgPoolOptions::new().connect(&url))
            .expect("a separate verification connection must succeed");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut still_running = true;
        while std::time::Instant::now() < deadline {
            // `pid <> pg_backend_pid()` excludes this very SELECT's own
            // backend: without it, this query would always match itself
            // (its own `query` text contains the literal substring
            // `pg_sleep` from the `LIKE` pattern below, and `pg_stat_activity`
            // reports it as `state = 'active'` while it executes), making
            // `still_running` never go false regardless of whether the
            // real `pg_sleep` backend was cancelled.
            let count: i64 = block_on(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM pg_stat_activity \
                     WHERE query LIKE '%pg_sleep%' AND state = 'active' \
                     AND pid <> pg_backend_pid()",
                )
                .fetch_one(&check_pool),
            )
            .expect("pg_stat_activity query should succeed");
            if count == 0 {
                still_running = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        assert!(
            !still_running,
            "the pg_sleep backend should have been server-side cancelled \
             well within the 30s sleep, but is still active after 10s"
        );
    }

    /// Reads `ZSQL_TEST_DATABASE_URL`, or returns `None` (after printing why)
    /// so callers can skip. Centralizes the skip-when-unset behavior all live
    /// tests in this module share.
    fn live_database_url() -> Option<String> {
        let Ok(url) = std::env::var("ZSQL_TEST_DATABASE_URL") else {
            eprintln!("skipping live test: ZSQL_TEST_DATABASE_URL not set");
            return None;
        };
        Some(url)
    }

    /// Connects to `ZSQL_TEST_DATABASE_URL` via [`live_database_url`], or
    /// returns `None` so callers can skip.
    fn live_connection() -> Option<Box<dyn zsql_core::Connection>> {
        let url = live_database_url()?;
        let driver = PostgresDriver;
        let cfg = ConnConfig::from_dsn(&url).unwrap();
        Some(block_on(driver.connect(&cfg)).expect("connect should succeed"))
    }

    /// Receive one event with a generous timeout so a broken implementation
    /// fails the test instead of hanging it.
    fn recv(
        rx: &flume::Receiver<Result<zsql_core::QueryEvent, zsql_core::CoreError>>,
    ) -> Result<zsql_core::QueryEvent, zsql_core::CoreError> {
        rx.recv_timeout(Duration::from_secs(10))
            .expect("expected an event within the timeout")
    }
}
