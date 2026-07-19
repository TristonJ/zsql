//! The `SQLite` [`Driver`] and its live [`Connection`] implementation, built on
//! sqlx with the **smol** runtime so its futures await directly on gpui's
//! executor
//!
//! `SQLite` has no separate backend server process, so unlike the Postgres
//! driver's `pg_cancel_backend` side channel there is nothing to signal
//! server-side: cancellation here is cooperative-only. Dropping (or calling
//! `cancel()` on) a [`QueryHandle`] simply stops the background query task
//! from reading further rows out of the `SQLite` connection worker, and no
//! mechanism analogous to `pg_cancel_backend` is attempted or promised.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, Executor as _, Row as _, SqlSafeStr as _, Statement as _};
use zsql_core::{
    BatchSink, ConnConfig, Connection, CoreError, Driver, QueryEvent, QueryHandle, RelationSchema,
    RowBatch, RowCount, SchemaTree, quote_ident,
};

use crate::error::{map_connect_error, map_query_error};
use crate::values::{column_metas, decode_row};

/// Rows are grouped into batches of at most this many rows before a
/// [`QueryEvent::Batch`] is pushed into the sink. Bounded so a large result
/// set streams to the UI incrementally instead of arriving as one huge
/// allocation. Mirrors `zsql-postgres`'s batch bound; this is an internal
/// placeholder default, not yet wired to `Config`.
const DEFAULT_QUERY_BATCH_SIZE: usize = 500;

/// Pool size for a `SQLite` connection.
///
/// Deliberately capped at one connection, for two reasons that go beyond
/// the "single desktop user" reasoning `zsql-postgres` uses for its own
/// (larger) pool bound:
/// - An in-memory (`:memory:`) database exists only for the lifetime of the
///   connection that opened it: a second pooled connection would see an
///   entirely separate, empty database, silently corrupting every query that
///   happened to land on it.
/// - `SQLite` serializes writers at the file level regardless of how many
///   connections a client opens, so a bigger pool buys no real write
///   concurrency here, only a bigger chance of a `SQLITE_BUSY` contention
///   error between this process's own connections.
const MAX_POOL_CONNECTIONS: u32 = 1;

/// How long to wait for the initial connection (and later, a free pooled
/// connection) before giving up.
const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// The `SQLite` [`Driver`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SqliteDriver;

impl SqliteDriver {
    /// Build a single-connection pool for `url` and verify it is reachable
    /// with a trivial liveness query before returning it.
    ///
    /// # Errors
    /// Returns [`CoreError::Connection`] if the pool cannot be built or the
    /// liveness query fails.
    async fn build_pool(url: &str) -> Result<SqlitePool, CoreError> {
        let pool = SqlitePoolOptions::new()
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
async fn liveness_check(pool: &SqlitePool) -> Result<i64, CoreError> {
    let row = sqlx::query("SELECT 1 AS one")
        .fetch_one(pool)
        .await
        .map_err(map_connect_error)?;
    let one: i64 = row.try_get("one").map_err(map_connect_error)?;
    Ok(one)
}

#[async_trait]
impl Driver for SqliteDriver {
    fn id(&self) -> &'static str {
        "sqlite"
    }

    fn display_name(&self) -> &'static str {
        "SQLite"
    }

    fn parse_url(&self, url: &str) -> Result<ConnConfig, CoreError> {
        ConnConfig::from_url(url)
    }

    #[tracing::instrument(name = "sqlite_connect", skip_all, fields(driver = self.id()))]
    async fn connect(&self, cfg: &ConnConfig) -> Result<Box<dyn Connection>, CoreError> {
        // Unlike a Postgres URL, a SQLite URL is normally just a file path
        // (or `:memory:`) with no embedded credentials, but it is still kept
        // out of the span for consistency with the other driver.
        let pool = Self::build_pool(&cfg.url).await?;
        tracing::info!("sqlite connection established");
        Ok(Box::new(SqliteConnectionImpl { pool }))
    }
}

/// A live `SQLite` connection, backed by a single-connection sqlx pool (see
/// [`MAX_POOL_CONNECTIONS`]).
pub struct SqliteConnectionImpl {
    pool: SqlitePool,
}

impl SqliteConnectionImpl {
    /// Close the underlying pool.
    ///
    /// `zsql_core::Connection` has no `close` method today, so this is an
    /// inherent method reachable only on the concrete type (not through
    /// `Box<dyn Connection>`); it exists for callers that hold the concrete
    /// connection and want to release the file handle deterministically
    /// rather than relying on drop order. Dropping the connection (or the
    /// pool inside it) is equally sufficient in every other case.
    #[tracing::instrument(name = "sqlite_close", skip_all)]
    pub async fn close(self) {
        self.pool.close().await;
        tracing::info!("sqlite connection closed");
    }
}

#[async_trait]
impl Connection for SqliteConnectionImpl {
    fn stream_query(&self, sql: String, sink: BatchSink) -> QueryHandle {
        let (cancel_tx, cancel_rx) = flume::unbounded();
        let pool = self.pool.clone();
        // Run on the smol-based executor sqlx's `runtime-smol` feature
        async_global_executor::spawn(run_query(pool, sql, sink, cancel_rx)).detach();
        QueryHandle::new(cancel_tx)
    }

    #[tracing::instrument(name = "sqlite_introspect", skip_all, fields(pool_size = self.pool.size()))]
    async fn introspect(&self) -> Result<SchemaTree, CoreError> {
        crate::introspect::introspect(&self.pool).await
    }

    async fn ping(&self) -> Result<(), CoreError> {
        liveness_check(&self.pool).await.map(|_| ())
    }

    /// `SQLite` has no analogue of Postgres's `pg_class.reltuples`: it keeps
    /// no running per-table row-count statistic, and `ANALYZE`'s own
    /// `sqlite_stat1`/`sqlite_stat4` tables estimate index selectivity, not
    /// a table's total row count, and only exist at all once `ANALYZE` has
    /// been run. There is therefore no cheap estimate to prefer here; this
    /// always executes an exact `COUNT(*)` and reports [`RowCount::Exact`].
    #[tracing::instrument(name = "sqlite_count_rows", skip(self), fields(pool_size = self.pool.size()))]
    async fn count_rows(&self, schema: &str, relation: &str) -> Result<RowCount, CoreError> {
        let sql = count_sql(schema, relation);
        // `sql` is built entirely from `quote_ident`-escaped identifiers via
        // `count_sql`, never from unescaped runtime text.
        let count: i64 = sqlx::query_scalar(AssertSqlSafe(sql))
            .fetch_one(&self.pool)
            .await
            .map_err(map_query_error)?;
        Ok(RowCount::Exact(u64::try_from(count).unwrap_or(0)))
    }

    #[tracing::instrument(
        name = "sqlite_describe_relation",
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
}

/// Build `SELECT COUNT(*) FROM <quoted schema>.<quoted relation>`, quoting
/// both identifiers so an adversarial schema/relation name cannot break out
/// of the identifier position.
fn count_sql(schema: &str, relation: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM {}.{}",
        quote_ident(schema),
        quote_ident(relation)
    )
}

/// Stream a query's results into `sink`. `sql` may hold several statements;
/// each result-producing statement emits its own [`QueryEvent::Columns`]
/// followed by that set's [`QueryEvent::Batch`]es, and the whole stream ends
/// with exactly one [`QueryEvent::Done`] - or, on any failure, a single `Err`
/// in place of `Done`. Every statement still executes (so all side effects
/// happen); a fresh `Columns` event marks each set boundary so the consumer
/// can keep only the last set rather than concatenating mismatched rows.
///
/// Column metadata for `Columns` is taken from the first row any statement
/// in `sql` produces (a [`sqlx::sqlite::SqliteRow`] carries its own column
/// list), not from an upfront describe. If no statement ever produces a row
/// (DDL, DML without `RETURNING`, or a zero-row `SELECT`), a describe is run
/// as a fallback *after* execution has already completed successfully,
/// purely to recover a zero-row `SELECT`'s column list; if that fallback
/// describe itself fails, the query has already succeeded, so this degrades
/// to reporting no columns rather than failing an otherwise-successful
/// query.
///
/// Cancellation here is cooperative-only (see the module doc comment): the
/// `select` below simply stops calling `rows.next()` once cancelled, which
/// stops issuing further step requests to `SQLite`'s connection worker. There
/// is no `SQLite`-side equivalent of `pg_cancel_backend` to also interrupt a
/// step already in flight.
#[tracing::instrument(name = "sqlite_stream_query", skip_all, fields(pool_size = pool.size()))]
async fn run_query(pool: SqlitePool, sql: String, sink: BatchSink, cancel_rx: flume::Receiver<()>) {
    tracing::debug!(sql = %sql, "streaming query");

    let mut conn = match pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => {
            let _ = sink.send_async(Err(map_query_error(err))).await;
            return;
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
                // latch so a following statement's rows form a new result set
                // (the consumer keeps only the last) instead of being appended
                // onto this one's columns.
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

    // Release the dedicated connection this query ran on *before* the
    // fallback describe below might need to acquire one of its own: the pool
    // holds only [`MAX_POOL_CONNECTIONS`] connection, so describing on the
    // pool while still holding this one would deadlock forever waiting for
    // a connection only this same task could free.
    drop(rows);
    drop(conn);

    // A statement with no output columns (DDL, or DML without `RETURNING`)
    // reports its row count as `affected` in `Done`. A statement that does
    // produce columns (SELECT, or DML with `RETURNING`) instead lets the
    // caller derive a count from the rows it already streamed, and reports
    // `affected: None`
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use zsql_core::{ConnConfig, Connection, Driver};

    use super::{SqliteConnectionImpl, SqliteDriver};

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        futures::executor::block_on(fut)
    }

    /// A path under the process temp dir that no `SQLite` connection can open
    /// (its parent directory does not exist and `SQLite` is not asked to
    /// create it), used to exercise the connect/introspect error paths
    /// without touching any real filesystem state.
    fn unopenable_url() -> String {
        format!(
            "sqlite:{}/zsql-sqlite-test-nonexistent-dir/db.sqlite3",
            std::env::temp_dir().display()
        )
    }

    /// A fresh temp-file path this test owns exclusively, cleaned up on drop.
    struct TempDbPath(std::path::PathBuf);

    impl TempDbPath {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "zsql-sqlite-test-{label}-{}-{n}.sqlite3",
                std::process::id()
            ));
            Self(path)
        }

        fn url(&self) -> String {
            format!("sqlite://{}?mode=rwc", self.0.display())
        }
    }

    impl Drop for TempDbPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn driver_ids_are_stable() {
        let driver = SqliteDriver;
        assert_eq!(driver.id(), "sqlite");
        assert_eq!(driver.display_name(), "SQLite");
    }

    #[test]
    fn parse_url_rejects_empty_string() {
        let driver = SqliteDriver;
        assert!(driver.parse_url("   ").is_err());
    }

    #[test]
    fn connect_succeeds_against_an_in_memory_database() {
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url("sqlite::memory:").unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");
        drop(conn);
    }

    #[test]
    fn preview_query_matches_the_shared_default_limit_form() {
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url("sqlite::memory:").unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");
        assert_eq!(
            conn.preview_query("public", "orders", 200),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200"
        );
    }

    #[test]
    fn preview_query_is_safe_against_an_injection_shaped_relation_name() {
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url("sqlite::memory:").unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");
        let sql = conn.preview_query("public", "orders\"; DROP TABLE users; --", 200);
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\"\"; DROP TABLE users; --\" LIMIT 200"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }

    #[test]
    fn close_shuts_down_the_underlying_pool() {
        // `close` is reachable only on the concrete type (see its doc
        // comment), so this builds one directly instead of going through
        // `Driver::connect`, which returns `Box<dyn Connection>`.
        let pool = block_on(
            sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:"),
        )
        .expect("in-memory connect should succeed");
        let pool_handle = pool.clone();
        let conn = SqliteConnectionImpl { pool };

        block_on(conn.close());

        assert!(
            pool_handle.is_closed(),
            "close() should close the underlying pool"
        );
    }

    #[test]
    fn connect_succeeds_against_a_fresh_temp_file_database() {
        let db = TempDbPath::new("connect");
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url(&db.url()).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");
        drop(conn);
    }

    #[test]
    fn connect_maps_an_unopenable_path_to_core_connection_error() {
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url(&unopenable_url()).unwrap();
        let result = block_on(driver.connect(&cfg));
        match result {
            Err(zsql_core::CoreError::Connection(msg)) => {
                assert!(!msg.is_empty(), "error message should not be empty");
            }
            Err(other) => panic!("expected CoreError::Connection, got {other:?}"),
            Ok(_) => panic!("connecting to an unopenable path must fail"),
        }
    }

    #[test]
    fn introspect_maps_an_unopenable_path_to_core_introspection_error() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_lazy(&unopenable_url())
            .expect("connect_lazy only parses the URL; it must not touch the filesystem");
        let conn = SqliteConnectionImpl { pool };

        let result = block_on(conn.introspect());
        match result {
            Err(zsql_core::CoreError::Introspection(msg)) => {
                assert!(!msg.is_empty(), "error message should not be empty");
            }
            Err(other) => panic!("expected CoreError::Introspection, got {other:?}"),
            Ok(_) => panic!("introspecting an unopenable path must fail"),
        }
    }

    #[test]
    fn introspect_builds_a_schema_tree_from_a_seeded_temp_file_database() {
        let db = TempDbPath::new("introspect-shape");
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url(&db.url()).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        run_ddl(
            &*conn,
            "CREATE TABLE users(\
                 id INTEGER PRIMARY KEY, \
                 email TEXT NOT NULL, \
                 display_name TEXT\
             ); \
             CREATE VIEW active_users AS SELECT * FROM users",
        );

        let tree = block_on(conn.introspect()).expect("introspect should succeed");

        assert_eq!(
            tree.catalogs.len(),
            1,
            "a sqlite connection has one catalog"
        );
        let catalog = &tree.catalogs[0];
        assert_eq!(
            catalog.schemas.len(),
            1,
            "v0 sqlite introspection surfaces exactly the main schema"
        );
        let main = &catalog.schemas[0];
        assert_eq!(main.name, "main");

        let users = main
            .tables
            .iter()
            .find(|r| r.name == "users")
            .expect("the seeded users table is present");
        assert_eq!(users.kind, zsql_core::RelationKind::Table);

        let active_users = main
            .tables
            .iter()
            .find(|r| r.name == "active_users")
            .expect("the seeded active_users view is present");
        assert_eq!(active_users.kind, zsql_core::RelationKind::View);

        let email = users
            .columns
            .iter()
            .find(|c| c.name == "email")
            .expect("users.email column is present");
        assert!(!email.nullable, "users.email is declared NOT NULL");
        assert!(
            email.type_name.to_uppercase().contains("TEXT"),
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

    #[test]
    fn introspect_names_the_catalog_after_the_memory_pseudo_file_for_an_in_memory_database() {
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url("sqlite::memory:").unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let tree = block_on(conn.introspect()).expect("introspect should succeed");

        assert_eq!(
            tree.catalogs[0].name, ":memory:",
            "an in-memory connection has no backing file, so pragma_database_list \
             reports an empty file column, which introspection renders as \":memory:\""
        );
    }

    #[test]
    fn introspect_orders_relations_and_columns_deterministically() {
        let db = TempDbPath::new("introspect-order");
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url(&db.url()).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        // Table names deliberately out of alphabetical creation order.
        run_ddl(
            &*conn,
            "CREATE TABLE zebras(id INTEGER PRIMARY KEY); \
             CREATE TABLE apples(id INTEGER PRIMARY KEY); \
             CREATE TABLE mangoes(\
                 id INTEGER PRIMARY KEY, \
                 email TEXT, \
                 display_name TEXT, \
                 is_active INTEGER, \
                 created_at TEXT\
             )",
        );

        let tree = block_on(conn.introspect()).expect("introspect should succeed");
        let main = &tree.catalogs[0].schemas[0];

        let relation_names: Vec<&str> = main.tables.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            relation_names,
            vec!["apples", "mangoes", "zebras"],
            "relations must be sorted by name"
        );

        let mangoes = main
            .tables
            .iter()
            .find(|r| r.name == "mangoes")
            .expect("mangoes table is present");
        let column_names: Vec<&str> = mangoes.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            column_names,
            vec!["id", "email", "display_name", "is_active", "created_at"],
            "columns must be in table/ordinal-position order, not alphabetical"
        );
    }

    #[test]
    fn introspect_includes_a_view_with_no_tables_alongside_it_when_none_exist() {
        let db = TempDbPath::new("introspect-view-only");
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url(&db.url()).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        run_ddl(
            &*conn,
            "CREATE TABLE source(id INTEGER PRIMARY KEY); \
             CREATE VIEW only_view AS SELECT id FROM source",
        );

        let tree = block_on(conn.introspect()).expect("introspect should succeed");
        let main = &tree.catalogs[0].schemas[0];
        assert_eq!(main.tables.len(), 2, "the table and the view both appear");
    }

    #[test]
    fn describe_relation_reports_columns_indexes_and_constraints() {
        let db = TempDbPath::new("describe-relation");
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url(&db.url()).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        run_ddl(
            &*conn,
            "CREATE TABLE parents(id INTEGER PRIMARY KEY); \
             CREATE TABLE children(\
                 id INTEGER PRIMARY KEY, \
                 parent_id INTEGER NOT NULL REFERENCES parents(id), \
                 code TEXT NOT NULL, \
                 total INTEGER NOT NULL DEFAULT 0, \
                 CHECK (total >= 0)\
             ); \
             CREATE UNIQUE INDEX children_code_idx ON children(code)",
        );

        let detail = block_on(conn.describe_relation("main", "children"))
            .expect("describe_relation must succeed");

        let id = detail
            .columns
            .iter()
            .find(|c| c.name == "id")
            .expect("id column is present");
        assert!(id.is_primary_key);

        let parent_id = detail
            .columns
            .iter()
            .find(|c| c.name == "parent_id")
            .expect("parent_id column is present");
        assert!(!parent_id.nullable);
        let fk = parent_id
            .foreign_key
            .as_ref()
            .expect("parent_id must carry a foreign key");
        assert_eq!(fk.table, "parents");
        assert_eq!(fk.columns, vec!["id".to_owned()]);

        let code = detail
            .columns
            .iter()
            .find(|c| c.name == "code")
            .expect("code column is present");
        assert!(code.is_unique);

        let total = detail
            .columns
            .iter()
            .find(|c| c.name == "total")
            .expect("total column is present");
        assert_eq!(total.default.as_deref(), Some("0"));

        assert_eq!(detail.indexes.len(), 1);
        assert_eq!(detail.indexes[0].name, "children_code_idx");
        assert!(detail.indexes[0].unique);

        assert!(
            detail
                .constraints
                .iter()
                .any(|c| c.kind == zsql_core::ConstraintKind::PrimaryKey),
            "the primary key must be reported as a constraint"
        );
        assert!(
            detail
                .constraints
                .iter()
                .any(|c| c.kind == zsql_core::ConstraintKind::ForeignKey),
            "the foreign key must be reported as a constraint"
        );
        assert!(
            detail
                .constraints
                .iter()
                .all(|c| c.kind != zsql_core::ConstraintKind::Check),
            "sqlite describe_relation cannot introspect CHECK constraints and must not \
             fabricate one"
        );
    }

    #[test]
    fn describe_relation_errors_for_a_relation_that_does_not_exist() {
        let db = TempDbPath::new("describe-relation-missing");
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url(&db.url()).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let result = block_on(conn.describe_relation("main", "does_not_exist"));
        match result {
            Err(zsql_core::CoreError::Introspection(msg)) => {
                assert!(!msg.is_empty(), "error message should not be empty");
            }
            Err(other) => panic!("expected CoreError::Introspection, got {other:?}"),
            Ok(detail) => panic!("describing a nonexistent relation must fail, got {detail:?}"),
        }
    }

    /// Run `sql` (typically DDL) to completion against `conn` and panic on
    /// any error, discarding whatever events it produces.
    fn run_ddl(conn: &dyn Connection, sql: &str) {
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
    fn count_sql_quotes_both_identifiers() {
        assert_eq!(
            super::count_sql("main", "items"),
            "SELECT COUNT(*) FROM \"main\".\"items\""
        );
    }

    #[test]
    fn count_sql_is_safe_against_an_injection_shaped_relation_name() {
        let sql = super::count_sql("main", "items\"; DROP TABLE users; --");
        assert_eq!(
            sql,
            "SELECT COUNT(*) FROM \"main\".\"items\"\"; DROP TABLE users; --\""
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }

    #[test]
    fn count_rows_returns_the_exact_seeded_row_count() {
        let db = TempDbPath::new("count-rows");
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url(&db.url()).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        run_ddl(
            &*conn,
            "CREATE TABLE items(id INTEGER PRIMARY KEY); \
             INSERT INTO items DEFAULT VALUES; \
             INSERT INTO items DEFAULT VALUES; \
             INSERT INTO items DEFAULT VALUES; \
             INSERT INTO items DEFAULT VALUES; \
             INSERT INTO items DEFAULT VALUES",
        );

        let row_count =
            block_on(conn.count_rows("main", "items")).expect("count_rows should succeed");
        assert_eq!(row_count, zsql_core::RowCount::Exact(5));
    }

    #[test]
    fn count_rows_returns_exact_zero_for_an_empty_table() {
        let db = TempDbPath::new("count-rows-empty");
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url(&db.url()).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        run_ddl(&*conn, "CREATE TABLE empty_items(id INTEGER PRIMARY KEY)");

        let row_count =
            block_on(conn.count_rows("main", "empty_items")).expect("count_rows should succeed");
        assert_eq!(row_count, zsql_core::RowCount::Exact(0));
    }

    #[test]
    fn count_rows_errors_for_a_relation_that_does_not_exist() {
        let db = TempDbPath::new("count-rows-missing-relation");
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url(&db.url()).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let result = block_on(conn.count_rows("main", "zsql_test_relation_that_does_not_exist"));
        match result {
            Err(zsql_core::CoreError::Query(msg)) => {
                assert!(!msg.is_empty(), "error message should not be empty");
            }
            Err(other) => panic!("expected CoreError::Query, got {other:?}"),
            Ok(row_count) => {
                panic!("counting a nonexistent relation must fail, got {row_count:?}")
            }
        }
    }

    #[test]
    fn stream_query_pushes_single_error_for_invalid_sql() {
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url("sqlite::memory:").unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("THIS IS NOT SQL".to_owned(), tx);

        let evt = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("stream_query must push exactly one event, not hang");
        match evt {
            Err(zsql_core::CoreError::Query(msg)) => assert!(!msg.is_empty()),
            other => panic!("expected a single CoreError::Query, got {other:?}"),
        }

        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "no further events should follow the error"
        );
    }

    #[test]
    fn stream_query_maps_a_representative_type_spread() {
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url("sqlite::memory:").unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let sql = "SELECT \
            42 AS i, \
            2.5 AS f, \
            'hi' AS t, \
            x'0102' AS b, \
            NULL AS n"
            .to_owned();

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(sql, tx);

        let columns = match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => columns,
            other => panic!("expected Columns first, got {other:?}"),
        };
        assert_eq!(columns.len(), 5, "one column per selected expression");

        let mut rows = Vec::new();
        let affected = loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { affected }) => break affected,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        };
        assert_eq!(affected, None);

        assert_eq!(rows.len(), 1, "the query returns exactly one row");
        let cells = &rows[0].0;
        assert_eq!(cells[0], zsql_core::Value::Int(42));
        assert_eq!(cells[1], zsql_core::Value::Float(2.5));
        assert_eq!(cells[2], zsql_core::Value::Text("hi".to_owned()));
        assert_eq!(cells[3], zsql_core::Value::Bytes(vec![0x01, 0x02]));
        assert_eq!(cells[4], zsql_core::Value::Null);
    }

    #[test]
    fn stream_query_keeps_statements_as_separate_result_sets() {
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url("sqlite::memory:").unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        // Both statements name their column "n", so a naive concatenation
        // would look structurally valid while silently mixing two statements'
        // rows. Each statement must instead open its own result set.
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
    fn stream_query_batches_large_result_sets() {
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url("sqlite::memory:").unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let row_count = super::DEFAULT_QUERY_BATCH_SIZE * 2 + 7;
        let sql = format!(
            "WITH RECURSIVE cnt(g) AS (\
                 SELECT 1 UNION ALL SELECT g + 1 FROM cnt WHERE g < {row_count}\
             ) SELECT g FROM cnt"
        );
        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(sql, tx);

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

    #[test]
    fn stream_query_reports_affected_rows_for_dml() {
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url("sqlite::memory:").unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let (setup_tx, setup_rx) = flume::unbounded();
        let _setup = conn.stream_query(
            "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT); \
             INSERT INTO users(name) VALUES ('a'), ('b'), ('c')"
                .to_owned(),
            setup_tx,
        );
        drain(&setup_rx);

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("UPDATE users SET name = name".to_owned(), tx);

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

    #[test]
    fn stream_query_emits_columns_for_a_zero_row_result() {
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url("sqlite::memory:").unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT 1 AS one WHERE 0".to_owned(), tx);

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
    fn dropping_the_query_handle_stops_further_rows() {
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url("sqlite::memory:").unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let (tx, rx) = flume::unbounded();
        let handle = conn.stream_query(slow_recursive_query(), tx);

        // Let the query get started (past `Columns`) before cancelling.
        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(_)) => {}
            other => panic!("expected Columns first, got {other:?}"),
        }
        drop(handle);

        assert_no_further_completion(&rx);
    }

    #[test]
    fn calling_cancel_stops_further_rows() {
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url("sqlite::memory:").unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let (tx, rx) = flume::unbounded();
        let handle = conn.stream_query(slow_recursive_query(), tx);

        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(_)) => {}
            other => panic!("expected Columns first, got {other:?}"),
        }
        handle.cancel();

        assert_no_further_completion(&rx);
        drop(handle);
    }

    #[test]
    fn cancelling_one_query_leaves_the_connection_usable_for_the_next() {
        // Cooperative-only cancellation must stop *this* query's stream
        // without tearing down the underlying connection: SQLite has no
        // separate backend process a cancel could target, so the only
        // thing that could go wrong here is the shared connection itself
        // ending up wedged or closed.
        let driver = SqliteDriver;
        let cfg = ConnConfig::from_url("sqlite::memory:").unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let (tx, rx) = flume::unbounded();
        let handle = conn.stream_query(slow_recursive_query(), tx);
        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(_)) => {}
            other => panic!("expected Columns first, got {other:?}"),
        }
        handle.cancel();
        assert_no_further_completion(&rx);
        drop(handle);

        let (tx2, rx2) = flume::unbounded();
        let _handle2 = conn.stream_query("SELECT 1 AS one".to_owned(), tx2);
        match recv(&rx2) {
            Ok(zsql_core::QueryEvent::Columns(columns)) => assert_eq!(columns.len(), 1),
            other => panic!("expected Columns first, got {other:?}"),
        }
        let mut rows = Vec::new();
        loop {
            match recv(&rx2) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => rows.extend(batch.rows),
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0[0], zsql_core::Value::Int(1));
    }

    /// A `WITH RECURSIVE` query with no practical upper bound, used to
    /// exercise cooperative cancellation: `SQLite` computes one row per step
    /// rather than materializing the whole recursion up front, so a cancel
    /// signal genuinely races an in-progress stream instead of a query that
    /// already finished producing all its rows.
    fn slow_recursive_query() -> String {
        "WITH RECURSIVE cnt(g) AS (\
             SELECT 1 UNION ALL SELECT g + 1 FROM cnt WHERE g < 100000000\
         ) SELECT g FROM cnt"
            .to_owned()
    }

    /// Drain `rx` until the query it belongs to has stopped producing events
    /// after a cancellation, asserting it never reaches `Done`.
    fn assert_no_further_completion(
        rx: &flume::Receiver<Result<zsql_core::QueryEvent, zsql_core::CoreError>>,
    ) {
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

    /// Drain every event from `rx` until the channel disconnects, ignoring
    /// their content; used after fixture-setup statements this test does not
    /// otherwise assert on.
    fn drain(rx: &flume::Receiver<Result<zsql_core::QueryEvent, zsql_core::CoreError>>) {
        while rx.recv_timeout(Duration::from_secs(10)).is_ok() {}
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
