//! The Postgres [`Driver`] and its live [`Connection`] implementation, built
//! on sqlx with the **smol** runtime so its futures await directly on gpui's
//! executor

use async_trait::async_trait;
use sqlx::postgres::PgPool;
use sqlx::{AssertSqlSafe, Column, Postgres, Row as _, TypeInfo};
use zsql_core::{
    BatchSink, ColumnMeta, ConnConfig, Connection, CoreError, Driver, QueryHandle, RelationSchema,
    RowCount, SchemaTree, quote_ident,
};
use zsql_sqlx::error::map_sqlx_query_error;
use zsql_sqlx::pool::{
    CANCEL_POOL_CONNECTIONS, MAX_POOL_CONNECTIONS, POOL_ACQUIRE_TIMEOUT, PROBE_POOL_CONNECTIONS,
    build_pool, build_probe_pool, build_side_pool, liveness_check,
};
use zsql_sqlx::{CancelHandle, SqlxZsqlDriver};

use crate::values::decode_row;

/// Below this, `pg_class.reltuples` cannot be trusted as a row-count
/// estimate. Modern Postgres (this driver has been verified against 18)
/// reports `reltuples = -1` as an explicit sentinel for a relation that has
/// never been `ANALYZE`d (whether or not it holds any rows), and this
/// `reltuples >= threshold` check is what actually catches that sentinel and
/// routes `count_rows` to the exact `COUNT(*)` fallback -- it is not dead
/// defensive code guarding against a case Postgres cannot produce.
const RELTUPLES_UNRELIABLE_THRESHOLD: f32 = 0.0;

/// The Postgres [`Driver`].
#[derive(Debug, Default, Clone, Copy)]
pub struct PostgresDriver;

#[async_trait]
impl Driver for PostgresDriver {
    fn id(&self) -> &'static str {
        "postgres"
    }

    fn display_name(&self) -> &'static str {
        "PostgreSQL"
    }

    fn default_port(&self) -> Option<u16> {
        Some(5432)
    }

    fn url_schemes(&self) -> &[&'static str] {
        &["postgres", "postgresql"]
    }

    fn parse_url(&self, url: &str) -> Result<ConnConfig, CoreError> {
        ConnConfig::from_url(url)
    }

    #[tracing::instrument(
        name = "pg_connect",
        skip_all,
        fields(driver = self.id(), tls_mode = tracing::field::Empty)
    )]
    async fn connect(&self, cfg: &ConnConfig) -> Result<Box<dyn Connection>, CoreError> {
        // Never log `cfg.url`: it may embed a password. Only non-secret
        // fields (the driver id and TLS mode, never the URL) are attached to
        // this span.
        let (url, tls_mode) = match cfg.tunnel_local_addr {
            Some(tunnel_addr) => crate::tunnel::tunneled_connect_url(&cfg.url, tunnel_addr)?,
            None => (cfg.url.clone(), zsql_core::TlsVerify::Off),
        };
        tracing::Span::current().record("tls_mode", tls_mode.label());

        let pool: PgPool = build_pool(&url, MAX_POOL_CONNECTIONS, POOL_ACQUIRE_TIMEOUT).await?;
        let cancel_pool: PgPool =
            build_side_pool(&url, CANCEL_POOL_CONNECTIONS, POOL_ACQUIRE_TIMEOUT)?;
        let probe_pool: PgPool =
            build_probe_pool(&url, PROBE_POOL_CONNECTIONS, POOL_ACQUIRE_TIMEOUT)?;
        tracing::info!("postgres connection established");
        Ok(Box::new(PgConnection(zsql_sqlx::SqlxConnection::new(
            pool,
            cancel_pool,
            probe_pool,
            cfg.batch_size,
        ))))
    }
}

impl SqlxZsqlDriver<Postgres> for PostgresDriver {
    const NAME: &'static str = "postgres";

    type Cancel = PgCancelHandle;

    fn column_metas(columns: &[<Postgres as sqlx::Database>::Column]) -> Vec<ColumnMeta> {
        columns
            .iter()
            .map(|column| ColumnMeta {
                name: column.name().to_owned(),
                type_name: column.type_info().name().to_owned(),
                nullable: true,
            })
            .collect()
    }

    fn decode_row(row: &<Postgres as sqlx::Database>::Row) -> zsql_core::Row {
        decode_row(row)
    }

    fn rows_affected(result: &<Postgres as sqlx::Database>::QueryResult) -> u64 {
        result.rows_affected()
    }

    async fn cancel_handle(
        conn: &mut <Postgres as sqlx::Database>::Connection,
    ) -> Result<Self::Cancel, sqlx::Error> {
        let pid = sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
            .fetch_one(&mut *conn)
            .await?;
        Ok(PgCancelHandle { pid })
    }
}

pub struct PgConnection(zsql_sqlx::SqlxConnection<Postgres, PostgresDriver>);

#[async_trait]
impl Connection for PgConnection {
    fn stream_query(&self, sql: String, sink: BatchSink) -> QueryHandle {
        self.0.stream_query(sql, sink)
    }

    #[tracing::instrument(name = "pg_introspect", skip_all, fields(pool_size = self.0.pool().size()))]
    async fn introspect(&self) -> Result<SchemaTree, CoreError> {
        crate::introspect::introspect(self.0.pool()).await
    }

    #[tracing::instrument(name = "pg_ping", skip_all, fields(pool_size = self.0.probe_pool().size()))]
    async fn ping(&self) -> Result<(), CoreError> {
        liveness_check(self.0.probe_pool()).await?;
        Ok(())
    }

    #[tracing::instrument(name = "pg_count_rows", skip(self), fields(pool_size = self.0.pool().size()))]
    async fn count_rows(&self, schema: &str, relation: &str) -> Result<RowCount, CoreError> {
        if let Some(reltuples) = fetch_reltuples(self.0.pool(), schema, relation).await? {
            if reltuples_is_reliable(reltuples) {
                tracing::debug!(reltuples, "using planner row-count estimate");
                return Ok(RowCount::Estimated(reltuples_to_row_count(reltuples)));
            }
            tracing::debug!(
                reltuples,
                "planner estimate unreliable (relation never analyzed); \
                 falling back to an exact count"
            );
        } else {
            tracing::debug!("no pg_class row found; falling back to an exact count");
        }
        let exact = exact_row_count(self.0.pool(), schema, relation).await?;
        Ok(RowCount::Exact(exact))
    }

    #[tracing::instrument(
        name = "pg_describe_relation",
        skip(self),
        fields(pool_size = self.0.pool().size())
    )]
    async fn describe_relation(
        &self,
        schema: &str,
        relation: &str,
    ) -> Result<RelationSchema, CoreError> {
        crate::describe::describe_relation(self.0.pool(), schema, relation).await
    }

    #[tracing::instrument(name = "pg_close", skip_all)]
    async fn close(&self) {
        self.0.close().await;
    }
}

pub struct PgCancelHandle {
    pid: i32,
}

impl CancelHandle<Postgres> for PgCancelHandle {
    async fn cancel(self, cancel_pool: &sqlx::Pool<Postgres>) -> Result<(), sqlx::Error> {
        sqlx::query_scalar::<_, bool>("SELECT pg_cancel_backend($1)")
            .bind(self.pid)
            .fetch_one(cancel_pool)
            .await?;
        Ok(())
    }
}

/// Look up `pg_class.reltuples` for `schema.relation`, bind-parameterized
/// (never string-interpolated) against `pg_namespace`/`pg_class`. Returns
/// `None` if no matching catalog row exists (e.g. the relation was dropped,
/// or the caller passed a name that doesn't exist).
async fn fetch_reltuples(
    pool: &PgPool,
    schema: &str,
    relation: &str,
) -> Result<Option<f32>, CoreError> {
    let row = sqlx::query(
        "SELECT c.reltuples \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = $2",
    )
    .bind(schema)
    .bind(relation)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_query_error)?;

    let Some(row) = row else {
        return Ok(None);
    };
    let reltuples: f32 = row.try_get("reltuples").map_err(map_sqlx_query_error)?;
    Ok(Some(reltuples))
}

/// Whether `reltuples` is trustworthy enough to report as a
/// [`RowCount::Estimated`]. Postgres uses a negative `reltuples` as an
/// explicit sentinel for "this relation has never been `ANALYZE`d" (verified
/// against Postgres 18: a freshly created, unanalyzed table reports
/// `reltuples = -1` regardless of how many rows it actually holds); once
/// `ANALYZE` has run at least once, `reltuples` is a nonnegative planner
/// estimate, including `0` for a genuinely empty analyzed table, which is
/// reliable and reported as-is.
fn reltuples_is_reliable(reltuples: f32) -> bool {
    reltuples >= RELTUPLES_UNRELIABLE_THRESHOLD
}

/// Convert a nonnegative `pg_class.reltuples` estimate to a row count.
/// `reltuples_is_reliable` guarantees `reltuples >= 0.0` before this is
/// called, so the cast never loses sign, and Postgres's own `reltuples` is
/// itself only ever an approximation, so rounding-to-nearest is at least as
/// precise as the estimate it comes from.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
fn reltuples_to_row_count(reltuples: f32) -> u64 {
    reltuples.round() as u64
}

/// Build `SELECT COUNT(*) FROM <quoted schema>.<quoted relation>`, quoting
/// both identifiers so an adversarial schema/relation name cannot break out
/// of the identifier position.
fn exact_count_sql(schema: &str, relation: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM {}.{}",
        quote_ident(schema),
        quote_ident(relation)
    )
}

/// Run an exact `SELECT COUNT(*)` against `schema.relation`.
async fn exact_row_count(pool: &PgPool, schema: &str, relation: &str) -> Result<u64, CoreError> {
    let sql = exact_count_sql(schema, relation);
    // `sql` is built entirely from `quote_ident`-escaped identifiers via
    // `exact_count_sql`, never from unescaped runtime text.
    let count: i64 = sqlx::query_scalar(AssertSqlSafe(sql))
        .fetch_one(pool)
        .await
        .map_err(map_sqlx_query_error)?;
    Ok(u64::try_from(count).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use zsql_core::{ConnConfig, Connection, Driver, SortDirection};

    use super::{PgConnection, PostgresDriver};

    const UNREACHABLE_URL: &str = "postgres://user:pass@zsql-test-nonexistent-host.invalid/db";

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        futures::executor::block_on(fut)
    }

    #[test]
    fn connect_maps_unreachable_host_to_core_connection_error() {
        let driver = PostgresDriver;
        let cfg = ConnConfig::from_url(UNREACHABLE_URL).unwrap();
        let result = block_on(driver.connect(&cfg));
        match result {
            Err(zsql_core::CoreError::Connection { message, .. }) => {
                assert!(!message.is_empty(), "error message should not be empty");
            }
            Err(other) => panic!("expected CoreError::Connection, got {other:?}"),
            Ok(_) => panic!("connecting to an unreachable host must fail"),
        }
    }

    #[test]
    fn connect_maps_malformed_url_to_core_connection_error() {
        let driver = PostgresDriver;
        // Not a valid postgres URL at all (no scheme).
        let cfg = ConnConfig {
            url: "not a valid url".to_owned(),
            tunnel_local_addr: None,
            batch_size: zsql_core::DEFAULT_QUERY_BATCH_SIZE,
        };
        let result = block_on(driver.connect(&cfg));
        assert!(matches!(
            result,
            Err(zsql_core::CoreError::Connection { .. })
        ));
    }

    #[test]
    fn parse_url_rejects_empty_string() {
        let driver = PostgresDriver;
        assert!(driver.parse_url("   ").is_err());
    }

    #[test]
    fn driver_ids_are_stable() {
        let driver = PostgresDriver;
        assert_eq!(driver.id(), "postgres");
        assert_eq!(driver.display_name(), "PostgreSQL");
    }

    #[test]
    fn introspect_maps_unreachable_host_to_core_introspection_error() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy(UNREACHABLE_URL)
            .expect("connect_lazy only parses the URL; it must not touch the network");
        let cancel_pool = pool.clone();
        let probe_pool = pool.clone();
        let conn = PgConnection(zsql_sqlx::SqlxConnection::new(
            pool,
            cancel_pool,
            probe_pool,
            zsql_core::DEFAULT_QUERY_BATCH_SIZE,
        ));

        let result = block_on(conn.introspect());
        match result {
            Err(zsql_core::CoreError::Introspection { message, .. }) => {
                assert!(!message.is_empty(), "error message should not be empty");
            }
            Err(other) => panic!("expected CoreError::Introspection, got {other:?}"),
            Ok(_) => panic!("introspecting an unreachable host must fail"),
        }
    }

    #[test]
    fn ping_maps_unreachable_host_to_core_connection_error() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy(UNREACHABLE_URL)
            .expect("connect_lazy only parses the URL; it must not touch the network");
        let cancel_pool = pool.clone();
        let probe_pool = pool.clone();
        let conn = PgConnection(zsql_sqlx::SqlxConnection::new(
            pool,
            cancel_pool,
            probe_pool,
            zsql_core::DEFAULT_QUERY_BATCH_SIZE,
        ));

        let result = block_on(conn.ping());
        match result {
            Err(zsql_core::CoreError::Connection { message, .. }) => {
                assert!(!message.is_empty(), "error message should not be empty");
            }
            Err(other) => panic!("expected CoreError::Connection, got {other:?}"),
            Ok(()) => panic!("pinging an unreachable host must fail"),
        }
    }

    /// Builds a [`PgConnection`] whose pools only ever parse `UNREACHABLE_URL`
    /// (`connect_lazy` never touches the network), so a test can exercise
    /// `preview_query` -- pure string-building, no I/O -- without a live
    /// database.
    fn connection_for_test() -> PgConnection {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy(UNREACHABLE_URL)
            .expect("connect_lazy only parses the URL; it must not touch the network");
        let cancel_pool = pool.clone();
        let probe_pool = pool.clone();
        PgConnection(zsql_sqlx::SqlxConnection::new(
            pool,
            cancel_pool,
            probe_pool,
            zsql_core::DEFAULT_QUERY_BATCH_SIZE,
        ))
    }

    #[test]
    fn preview_query_quotes_both_identifiers_and_applies_the_limit() {
        let conn = connection_for_test();
        assert_eq!(
            conn.preview_query("public", "orders", 200),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200"
        );
    }

    #[test]
    fn preview_query_is_safe_against_an_injection_shaped_relation_name() {
        let conn = connection_for_test();
        let sql = conn.preview_query("public", "orders\"; DROP TABLE users; --", 200);
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\"\"; DROP TABLE users; --\" LIMIT 200"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }

    #[test]
    fn preview_query_windowed_with_no_sort_and_no_offset_matches_the_plain_preview() {
        let conn = connection_for_test();
        assert_eq!(
            conn.preview_query_windowed("public", "orders", None, 200, 0),
            conn.preview_query("public", "orders", 200)
        );
    }

    #[test]
    fn preview_query_windowed_applies_ascending_and_descending_sorts() {
        let conn = connection_for_test();
        assert_eq!(
            conn.preview_query_windowed(
                "public",
                "orders",
                Some(("total_cents", SortDirection::Asc)),
                200,
                0
            ),
            "SELECT * FROM \"public\".\"orders\" ORDER BY \"total_cents\" ASC LIMIT 200"
        );
        assert_eq!(
            conn.preview_query_windowed(
                "public",
                "orders",
                Some(("total_cents", SortDirection::Desc)),
                200,
                0
            ),
            "SELECT * FROM \"public\".\"orders\" ORDER BY \"total_cents\" DESC LIMIT 200"
        );
    }

    #[test]
    fn preview_query_windowed_omits_offset_on_page_one_and_applies_it_from_page_two() {
        let conn = connection_for_test();
        let page_one = conn.preview_query_windowed("public", "orders", None, 200, 0);
        assert!(!page_one.contains("OFFSET"), "page one: {page_one}");
        assert_eq!(
            conn.preview_query_windowed("public", "orders", None, 200, 200),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200 OFFSET 200"
        );
        assert_eq!(
            conn.preview_query_windowed("public", "orders", None, 200, 800),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200 OFFSET 800"
        );
    }

    #[test]
    fn preview_query_windowed_supports_every_configured_page_size() {
        let conn = connection_for_test();
        for page_size in [100_u64, 200, 500, 1000] {
            assert_eq!(
                conn.preview_query_windowed("public", "orders", None, page_size, 0),
                format!("SELECT * FROM \"public\".\"orders\" LIMIT {page_size}")
            );
        }
    }

    #[test]
    fn preview_query_windowed_is_safe_against_an_injection_shaped_sort_column() {
        let conn = connection_for_test();
        let sql = conn.preview_query_windowed(
            "public",
            "orders",
            Some(("total\"; DROP TABLE users; --", SortDirection::Asc)),
            200,
            0,
        );
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\" ORDER BY \"total\"\"; DROP TABLE users; --\" ASC LIMIT 200"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }

    #[test]
    fn stream_query_pushes_single_error_when_pool_is_unreachable() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy(UNREACHABLE_URL)
            .expect("connect_lazy only parses the URL; it must not touch the network");
        let cancel_pool = pool.clone();
        let probe_pool = pool.clone();
        let conn = PgConnection(zsql_sqlx::SqlxConnection::new(
            pool,
            cancel_pool,
            probe_pool,
            zsql_core::DEFAULT_QUERY_BATCH_SIZE,
        ));

        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query("SELECT 1".to_owned(), tx);

        let evt = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("stream_query must push exactly one event, not hang");
        match evt {
            Err(zsql_core::CoreError::Query { message, .. }) => assert!(!message.is_empty()),
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
            super::exact_count_sql("public", "orders"),
            "SELECT COUNT(*) FROM \"public\".\"orders\""
        );
    }

    #[test]
    fn exact_count_sql_is_safe_against_an_injection_shaped_relation_name() {
        let sql = super::exact_count_sql("public", "orders\"; DROP TABLE users; --");
        assert_eq!(
            sql,
            "SELECT COUNT(*) FROM \"public\".\"orders\"\"; DROP TABLE users; --\""
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }

    #[test]
    fn exact_count_sql_is_safe_against_an_injection_shaped_schema_name() {
        let sql = super::exact_count_sql("public\"; DROP TABLE users; --", "orders");
        assert_eq!(
            sql,
            "SELECT COUNT(*) FROM \"public\"\"; DROP TABLE users; --\".\"orders\""
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }

    #[test]
    fn reltuples_is_reliable_rejects_the_never_analyzed_sentinel() {
        // Postgres reports reltuples == -1 for a relation that has never
        // been ANALYZE'd, regardless of how many rows it actually holds.
        assert!(!super::reltuples_is_reliable(-1.0));
    }

    #[test]
    fn reltuples_is_reliable_accepts_a_positive_estimate() {
        assert!(super::reltuples_is_reliable(1234.0));
    }

    #[test]
    fn reltuples_is_reliable_accepts_a_genuinely_empty_analyzed_table() {
        // Once ANALYZE has run, reltuples == 0 means "zero rows", a
        // trustworthy estimate rather than an unanalyzed placeholder (the
        // placeholder is the negative sentinel above, not zero).
        assert!(super::reltuples_is_reliable(0.0));
    }

    #[test]
    fn reltuples_to_row_count_rounds_to_the_nearest_integer() {
        assert_eq!(super::reltuples_to_row_count(1234.4), 1234);
        assert_eq!(super::reltuples_to_row_count(1234.6), 1235);
        assert_eq!(super::reltuples_to_row_count(0.0), 0);
    }
}

#[cfg(test)]
#[cfg(feature = "driver-integration-tests")]
mod database_tests {
    use std::time::Duration;

    use zsql_core::{ConnConfig, Driver};

    use super::PostgresDriver;

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        futures::executor::block_on(fut)
    }

    /// Reads `ZSQL_TEST_POSTGRES_URL` for database-tests
    fn live_database_url() -> String {
        std::env::var("ZSQL_TEST_POSTGRES_URL")
            .expect("ZSQL_TEST_POSTGRES_URL must be set to run database tests")
    }

    /// Connects to `ZSQL_TEST_POSTGRES_URL` via [`live_database_url`]
    fn live_connection() -> Box<dyn zsql_core::Connection> {
        let url = live_database_url();
        let driver = PostgresDriver;
        let cfg = ConnConfig::from_url(&url).unwrap();
        block_on(driver.connect(&cfg)).expect("connect should succeed")
    }

    /// Table names [`seed_describe_relation_tables`] creates: a referenced
    /// parent table and the described child table.
    const DESCRIBE_RELATION_PARENTS_TABLE: &str = "zsql_test_describe_parents";
    const DESCRIBE_RELATION_CHILDREN_TABLE: &str = "zsql_test_describe_children";

    /// Seeds a self-contained pair of tables (a referenced parents table and
    /// the described children table) with a primary key, a foreign key, a
    /// named non-PK unique index, and a check constraint. Ad hoc DDL run
    /// through `conn` rather than `dev/seed.sql`, so a test using this owns
    /// and cleans up its own tables independently of the shared dev seed.
    fn seed_describe_relation_tables(conn: &dyn zsql_core::Connection) {
        let parents = DESCRIBE_RELATION_PARENTS_TABLE;
        let children = DESCRIBE_RELATION_CHILDREN_TABLE;
        run_ddl(conn, &format!("DROP TABLE IF EXISTS {children}"));
        run_ddl(conn, &format!("DROP TABLE IF EXISTS {parents}"));
        run_ddl(
            conn,
            &format!("CREATE TABLE {parents} (id bigserial PRIMARY KEY)"),
        );
        run_ddl(
            conn,
            &format!(
                "CREATE TABLE {children} (\
                     id bigserial PRIMARY KEY, \
                     parent_id bigint NOT NULL REFERENCES {parents} (id), \
                     code text NOT NULL, \
                     total integer NOT NULL DEFAULT 0, \
                     label text NOT NULL DEFAULT 'pending', \
                     CONSTRAINT zsql_test_describe_children_total_check CHECK (total >= 0)\
                 )"
            ),
        );
        run_ddl(
            conn,
            &format!(
                "CREATE UNIQUE INDEX zsql_test_describe_children_code_idx ON {children} (code)"
            ),
        );
    }

    /// Drops the tables [`seed_describe_relation_tables`] created.
    fn drop_describe_relation_tables(conn: &dyn zsql_core::Connection) {
        run_ddl(
            conn,
            &format!("DROP TABLE {DESCRIBE_RELATION_CHILDREN_TABLE}"),
        );
        run_ddl(
            conn,
            &format!("DROP TABLE {DESCRIBE_RELATION_PARENTS_TABLE}"),
        );
    }

    /// Receive one event with a generous timeout so a broken implementation
    /// fails the test instead of hanging it.
    fn recv(
        rx: &flume::Receiver<Result<zsql_core::QueryEvent, zsql_core::CoreError>>,
    ) -> Result<zsql_core::QueryEvent, zsql_core::CoreError> {
        rx.recv_timeout(Duration::from_secs(10))
            .expect("expected an event within the timeout")
    }

    /// How long [`wait_until_active_in_pg_stat_activity`] polls before
    /// giving up.
    const QUERY_START_POLL_DEADLINE: Duration = Duration::from_secs(5);
    /// How often [`wait_until_active_in_pg_stat_activity`] re-checks
    /// `pg_stat_activity` while waiting.
    const QUERY_START_POLL_INTERVAL: Duration = Duration::from_millis(50);

    /// Polls `pg_stat_activity`, on a connection independent of the one
    /// running `needle`, until some backend is actively running a query
    /// containing it. Used to prove a query dispatched via `stream_query`
    /// has genuinely started executing server-side before racing something
    /// (a probe, a cancel) against it.
    ///
    /// `stream_query`'s own event stream cannot serve this purpose: this
    /// driver only emits a statement's `Columns` event once that
    /// statement's first row has actually arrived, and Postgres does not
    /// flush a multi-statement simple query's earlier results ahead of the
    /// whole batch completing -- so a query like `pg_sleep(n)`, which
    /// produces its single row only once the sleep itself finishes, gives
    /// no earlier observable signal that way.
    fn wait_until_active_in_pg_stat_activity(url: &str, needle: &str) {
        let check_pool = block_on(sqlx::postgres::PgPoolOptions::new().connect(url))
            .expect("a separate verification connection must succeed");
        let deadline = std::time::Instant::now() + QUERY_START_POLL_DEADLINE;
        loop {
            let count: i64 = block_on(
                sqlx::query_scalar::<_, i64>(
                    "SELECT count(*) FROM pg_stat_activity \
                     WHERE query LIKE $1 AND state = 'active' AND pid <> pg_backend_pid()",
                )
                .bind(format!("%{needle}%"))
                .fetch_one(&check_pool),
            )
            .expect("pg_stat_activity query should succeed");
            if count > 0 {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no backend running a query containing {needle:?} appeared in \
                 pg_stat_activity within {QUERY_START_POLL_DEADLINE:?}"
            );
            std::thread::sleep(QUERY_START_POLL_INTERVAL);
        }
    }

    #[test]
    fn connect_succeeds_against_a_live_database_when_configured() {
        live_connection();
    }

    #[test]
    fn ping_succeeds_against_a_live_database_when_configured() {
        let conn = live_connection();
        block_on(conn.ping()).expect("ping should succeed against a reachable database");
    }

    #[test]
    fn ping_completes_while_a_slow_query_is_streaming_when_configured() {
        let url = live_database_url();
        let driver = PostgresDriver;
        let cfg = ConnConfig::from_url(&url).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let (tx, rx) = flume::unbounded();
        let handle = conn.stream_query("SELECT pg_sleep(3)".to_owned(), tx);
        wait_until_active_in_pg_stat_activity(&url, "pg_sleep");

        // The probe uses its own pool, so it must complete promptly instead
        // of waiting behind the slow query's connection.
        let ping_started = std::time::Instant::now();
        block_on(conn.ping()).expect("ping must succeed independently of the slow query");
        assert!(
            ping_started.elapsed() < Duration::from_secs(2),
            "ping took {:?}, which suggests it was blocked behind the slow query",
            ping_started.elapsed()
        );

        // The slow query must still reach its normal terminal state,
        // unaffected by the probe that ran alongside it.
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
    fn introspect_builds_schema_tree_matching_the_seeded_database_when_configured() {
        let url = live_database_url();
        let driver = PostgresDriver;
        let cfg = ConnConfig::from_url(&url).unwrap();
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

        let recent_orders_mv = public
            .tables
            .iter()
            .find(|r| r.name == "recent_orders_mv")
            .expect("the seeded recent_orders_mv materialized view is present");
        assert_eq!(recent_orders_mv.kind, zsql_core::RelationKind::MatView);

        let events = public
            .tables
            .iter()
            .find(|r| r.name == "events")
            .expect("the seeded partitioned events table is present");
        assert_eq!(events.kind, zsql_core::RelationKind::Partitioned);

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

    #[test]
    fn introspect_orders_schemas_relations_and_columns_deterministically_when_configured() {
        let conn = live_connection();
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

    #[test]
    fn introspect_includes_non_public_schemas_including_an_empty_one_when_configured() {
        let conn = live_connection();
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

    #[test]
    fn introspect_attributes_columns_by_schema_and_relation_not_name_alone_when_configured() {
        let conn = live_connection();

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

    fn database_name_from_url(url: &str) -> &str {
        let after_slash = url.rsplit('/').next().unwrap_or_default();
        after_slash.split('?').next().unwrap_or(after_slash)
    }

    #[test]
    fn stream_query_maps_a_representative_type_spread_when_configured() {
        let conn = live_connection();

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
        assert!(
            matches!(cells[17], zsql_core::Value::Unknown(_)),
            "an unmapped type (interval) must decode to Value::Unknown, got {:?}",
            cells[17],
        );
    }

    #[test]
    fn stream_query_maps_json_and_jsonb_scalars_and_arrays_when_configured() {
        let conn = live_connection();

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

    #[test]
    fn stream_query_keeps_statements_as_separate_result_sets_when_configured() {
        let conn = live_connection();

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
    fn stream_query_batches_large_result_sets_when_configured() {
        let conn = live_connection();

        let row_count = zsql_core::DEFAULT_QUERY_BATCH_SIZE * 2 + 7;
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
                        batch.len() <= zsql_core::DEFAULT_QUERY_BATCH_SIZE,
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
            zsql_core::DEFAULT_QUERY_BATCH_SIZE
        );
    }

    #[test]
    fn stream_query_honors_a_connection_configured_custom_batch_size_when_configured() {
        let url = live_database_url();
        let driver = PostgresDriver;
        let mut cfg = ConnConfig::from_url(&url).unwrap();
        let custom_batch_size = 3;
        cfg.batch_size = custom_batch_size;
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let row_count = 10;
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
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Batch(batch)) => {
                    assert!(
                        batch.len() <= custom_batch_size,
                        "batch of {} rows exceeds the configured bound of {custom_batch_size}",
                        batch.len()
                    );
                    total_rows += batch.len();
                }
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        assert_eq!(total_rows, row_count);
    }

    #[test]
    fn stream_query_reports_affected_rows_for_dml_when_configured() {
        let conn = live_connection();

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

    #[test]
    fn stream_query_emits_columns_for_a_zero_row_result_when_configured() {
        let conn = live_connection();

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

    #[test]
    fn dropping_the_query_handle_stops_further_rows_when_configured() {
        let conn = live_connection();

        let (tx, rx) = flume::unbounded();
        let handle =
            conn.stream_query("SELECT * FROM generate_series(1, 100000000)".to_owned(), tx);

        // Let the query get started (past `Columns`) before cancelling.
        match recv(&rx) {
            Ok(zsql_core::QueryEvent::Columns(_)) => {}
            other => panic!("expected Columns first, got {other:?}"),
        }
        drop(handle);

        // Drain whatever was already in flight
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

    #[test]
    fn calling_cancel_stops_further_rows_when_configured() {
        let conn = live_connection();

        let (tx, rx) = flume::unbounded();
        let handle =
            conn.stream_query("SELECT * FROM generate_series(1, 100000000)".to_owned(), tx);

        // Let the query get started (past `Columns`) before cancelling.
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
    }

    #[test]
    fn cancel_stops_a_server_side_blocking_query_when_configured() {
        let url = live_database_url();
        let driver = PostgresDriver;
        let cfg = ConnConfig::from_url(&url).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let (tx, rx) = flume::unbounded();
        let handle = conn.stream_query("SELECT pg_sleep(30)".to_owned(), tx);
        wait_until_active_in_pg_stat_activity(&url, "pg_sleep");
        handle.cancel();

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
            // making `still_running` never go false regardless of whether
            // the real `pg_sleep` backend was cancelled.
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

    /// Runs `ANALYZE` on a freshly seeded table, then asserts `count_rows`
    /// reports a `RowCount::Estimated` within a generous tolerance of the
    /// table's true row count. Postgres's own planner statistics come from a
    /// sample, not a full scan, so a wide (20%) relative tolerance is used
    /// even though a table this small is typically counted exactly by
    /// `ANALYZE`.
    #[test]
    fn count_rows_returns_an_estimated_count_within_tolerance_after_analyze_when_configured() {
        let url = live_database_url();
        let driver = PostgresDriver;
        let cfg = ConnConfig::from_url(&url).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let table = "zsql_test_count_rows_estimated";
        let seeded_rows: i64 = 500;
        run_ddl(&*conn, &format!("DROP TABLE IF EXISTS {table}"));
        run_ddl(&*conn, &format!("CREATE TABLE {table} (n integer)"));
        run_ddl(
            &*conn,
            &format!("INSERT INTO {table} SELECT * FROM generate_series(1, {seeded_rows})"),
        );
        run_ddl(&*conn, &format!("ANALYZE {table}"));

        let row_count = block_on(conn.count_rows("public", table)).expect("count_rows must run");
        tracing::info!(
            ?row_count,
            "count_rows_returns_an_estimated_count_within_tolerance_after_analyze_when_configured executed against the live database"
        );

        match row_count {
            zsql_core::RowCount::Estimated(estimate) => {
                let diff = (i64::try_from(estimate).unwrap() - seeded_rows).abs();
                let tolerance = seeded_rows / 5; // 20%
                assert!(
                    diff <= tolerance,
                    "estimate {estimate} too far from the true count {seeded_rows} \
                     (tolerance {tolerance})"
                );
            }
            zsql_core::RowCount::Exact(n) => {
                panic!("expected an Estimated count after ANALYZE, got Exact({n})")
            }
        }

        run_ddl(&*conn, &format!("DROP TABLE {table}"));
    }

    /// A freshly created, never-`ANALYZE`d table reports `reltuples = -1` in
    /// `pg_class` (Postgres's own sentinel for "never analyzed") regardless
    /// of how many rows it actually holds, so `count_rows` must fall back to
    /// an exact `COUNT(*)` rather than trusting that sentinel as an
    /// estimate.
    #[test]
    fn count_rows_falls_back_to_exact_for_an_unanalyzed_table_when_configured() {
        let url = live_database_url();
        let driver = PostgresDriver;
        let cfg = ConnConfig::from_url(&url).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let table = "zsql_test_count_rows_unanalyzed";
        let seeded_rows = 7;
        run_ddl(&*conn, &format!("DROP TABLE IF EXISTS {table}"));
        run_ddl(&*conn, &format!("CREATE TABLE {table} (n integer)"));
        run_ddl(
            &*conn,
            &format!("INSERT INTO {table} SELECT * FROM generate_series(1, {seeded_rows})"),
        );
        // Deliberately no ANALYZE here: this is the whole point of the test.

        let row_count = block_on(conn.count_rows("public", table)).expect("count_rows must run");
        tracing::info!(
            ?row_count,
            "count_rows_falls_back_to_exact_for_an_unanalyzed_table_when_configured executed against the live database"
        );

        assert_eq!(
            row_count,
            zsql_core::RowCount::Exact(seeded_rows),
            "an unanalyzed table must fall back to an exact count"
        );

        run_ddl(&*conn, &format!("DROP TABLE {table}"));
    }

    /// A genuinely empty table that HAS been `ANALYZE`d reports
    /// `reltuples = 0`, distinct from the `-1` never-analyzed sentinel
    /// exercised above, and must be trusted as `Estimated(0)` rather than
    /// falling back to an exact count.
    #[test]
    fn count_rows_returns_an_estimated_zero_for_an_analyzed_empty_table_when_configured() {
        let url = live_database_url();
        let driver = PostgresDriver;
        let cfg = ConnConfig::from_url(&url).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");

        let table = "zsql_test_count_rows_analyzed_empty";
        run_ddl(&*conn, &format!("DROP TABLE IF EXISTS {table}"));
        run_ddl(&*conn, &format!("CREATE TABLE {table} (n integer)"));
        run_ddl(&*conn, &format!("ANALYZE {table}"));

        let row_count = block_on(conn.count_rows("public", table)).expect("count_rows must run");
        tracing::info!(
            ?row_count,
            "count_rows_returns_an_estimated_zero_for_an_analyzed_empty_table_when_configured executed against the live database"
        );

        assert_eq!(
            row_count,
            zsql_core::RowCount::Estimated(0),
            "an analyzed, genuinely empty table must report a trustworthy Estimated(0), \
             not fall back to an exact count"
        );

        run_ddl(&*conn, &format!("DROP TABLE {table}"));
    }

    /// A relation with no matching `pg_class` row at all (never created, or
    /// already dropped) makes `fetch_reltuples` return `None`, which must
    /// still fall through to the exact-count path rather than panicking or
    /// silently reporting a zero count; the exact `COUNT(*)` against a
    /// nonexistent relation then fails, and that failure must surface as a
    /// `CoreError`, not be swallowed.
    #[test]
    fn count_rows_errors_for_a_relation_absent_from_pg_class_when_configured() {
        let conn = live_connection();

        let result = block_on(conn.count_rows("public", "zsql_test_relation_that_does_not_exist"));
        tracing::info!(
            ?result,
            "count_rows_errors_for_a_relation_absent_from_pg_class_when_configured executed against the live database"
        );

        match result {
            Err(zsql_core::CoreError::Query { message, .. }) => {
                assert!(!message.is_empty(), "error message should not be empty");
            }
            Err(other) => panic!("expected CoreError::Query, got {other:?}"),
            Ok(row_count) => {
                panic!("counting a nonexistent relation must fail, got {row_count:?}")
            }
        }
    }

    #[test]
    fn describe_relation_reports_columns_indexes_and_constraints_when_configured() {
        let url = live_database_url();
        let driver = PostgresDriver;
        let cfg = ConnConfig::from_url(&url).unwrap();
        let conn = block_on(driver.connect(&cfg)).expect("connect should succeed");
        let parents = DESCRIBE_RELATION_PARENTS_TABLE;
        let children = DESCRIBE_RELATION_CHILDREN_TABLE;
        seed_describe_relation_tables(&*conn);

        let detail = block_on(conn.describe_relation("public", children))
            .expect("describe_relation must succeed against the live database");
        tracing::info!(
            columns = detail.columns.len(),
            indexes = detail.indexes.len(),
            constraints = detail.constraints.len(),
            "describe_relation_reports_columns_indexes_and_constraints_when_configured \
             executed against the live database"
        );

        let id = detail
            .columns
            .iter()
            .find(|c| c.name == "id")
            .expect("id column is present");
        assert!(id.is_primary_key, "id must be flagged as the primary key");
        assert!(!id.nullable);

        let parent_id = detail
            .columns
            .iter()
            .find(|c| c.name == "parent_id")
            .expect("parent_id column is present");
        let fk = parent_id
            .foreign_key
            .as_ref()
            .expect("parent_id must carry a foreign key");
        assert_eq!(fk.schema, "public");
        assert_eq!(fk.table, parents);
        assert_eq!(fk.columns, vec!["id".to_owned()]);

        let code = detail
            .columns
            .iter()
            .find(|c| c.name == "code")
            .expect("code column is present");
        assert!(
            code.is_unique,
            "code must be flagged unique via its single-column unique index"
        );

        let total = detail
            .columns
            .iter()
            .find(|c| c.name == "total")
            .expect("total column is present");
        assert_eq!(total.default.as_deref(), Some("0"));

        let label = detail
            .columns
            .iter()
            .find(|c| c.name == "label")
            .expect("label column is present");
        assert_eq!(label.default.as_deref(), Some("'pending'::text"));

        let code_index = detail
            .indexes
            .iter()
            .find(|idx| idx.name == "zsql_test_describe_children_code_idx")
            .expect("the named unique index is present");
        assert_eq!(code_index.method, "btree");
        assert!(code_index.unique);

        let check = detail
            .constraints
            .iter()
            .find(|c| c.name == "zsql_test_describe_children_total_check")
            .expect("the check constraint is present");
        assert_eq!(check.kind, zsql_core::ConstraintKind::Check);
        assert!(check.definition.contains("total"));

        let fk_constraint = detail
            .constraints
            .iter()
            .find(|c| c.kind == zsql_core::ConstraintKind::ForeignKey)
            .expect("the foreign key constraint is present");
        assert!(fk_constraint.definition.contains(parents));

        let pk_constraint = detail
            .constraints
            .iter()
            .find(|c| c.kind == zsql_core::ConstraintKind::PrimaryKey)
            .expect("the primary key constraint is present");
        assert!(pk_constraint.definition.contains("id"));

        drop_describe_relation_tables(&*conn);
    }

    #[test]
    fn describe_relation_errors_for_a_relation_that_does_not_exist_when_configured() {
        let conn = live_connection();

        let result = block_on(
            conn.describe_relation("public", "zsql_test_relation_that_does_not_exist_either"),
        );
        tracing::info!(
            ?result,
            "describe_relation_errors_for_a_relation_that_does_not_exist_when_configured \
             executed against the live database"
        );

        match result {
            Err(zsql_core::CoreError::Introspection { message, .. }) => {
                assert!(!message.is_empty(), "error message should not be empty");
            }
            Err(other) => panic!("expected CoreError::Introspection, got {other:?}"),
            Ok(detail) => {
                panic!("describing a nonexistent relation must fail, got {detail:?}")
            }
        }
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

    /// Table name [`seed_sorted_preview_table`] creates.
    const SORTED_PREVIEW_TABLE: &str = "zsql_test_sorted_preview";

    /// Seeds a self-contained table of 25 rows with a distinct `n` value in
    /// each (1..=25), for asserting exact ordered/windowed row content.
    fn seed_sorted_preview_table(conn: &dyn zsql_core::Connection) {
        let table = SORTED_PREVIEW_TABLE;
        run_ddl(conn, &format!("DROP TABLE IF EXISTS {table}"));
        run_ddl(conn, &format!("CREATE TABLE {table} (n integer NOT NULL)"));
        let values: Vec<String> = (1..=25).map(|n| format!("({n})")).collect();
        run_ddl(
            conn,
            &format!("INSERT INTO {table} (n) VALUES {}", values.join(", ")),
        );
    }

    /// Run `sql` to completion, collecting every row's cells into `Vec<Vec<zsql_core::Value>>`
    /// in arrival order.
    fn run_query_collecting_rows(
        conn: &dyn zsql_core::Connection,
        sql: &str,
    ) -> Vec<Vec<zsql_core::Value>> {
        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(sql.to_owned(), tx);
        let mut rows = Vec::new();
        loop {
            match recv(&rx) {
                Ok(zsql_core::QueryEvent::Columns(_)) => {}
                Ok(zsql_core::QueryEvent::Batch(batch)) => {
                    rows.extend(batch.rows.into_iter().map(|row| row.0));
                }
                Ok(zsql_core::QueryEvent::Done { .. }) => break,
                other => panic!("unexpected event mid-stream: {other:?}"),
            }
        }
        rows
    }

    /// Opening a preview, then sorting it and stepping to page 2, must
    /// return exactly the correct ordered, windowed subset of rows -- not
    /// merely "a query ran". Seeds 25 rows (`n` = 1..=25), sorts descending
    /// by `n` at a page size of 10, and asserts page 2 is exactly
    /// `n` = 15..=6 in descending order.
    #[test]
    fn preview_query_windowed_returns_the_correct_sorted_windowed_rows_when_configured() {
        let conn = live_connection();
        seed_sorted_preview_table(&*conn);

        let sql = conn.preview_query_windowed(
            "public",
            SORTED_PREVIEW_TABLE,
            Some(("n", zsql_core::SortDirection::Desc)),
            10,
            10,
        );
        assert!(sql.contains("ORDER BY \"n\" DESC"), "{sql}");
        assert!(sql.contains("OFFSET 10"), "{sql}");

        let rows = run_query_collecting_rows(&*conn, &sql);
        let values: Vec<i64> = rows
            .iter()
            .map(|row| match &row[0] {
                zsql_core::Value::Int(n) => *n,
                other => panic!("expected an Int cell, got {other:?}"),
            })
            .collect();

        assert_eq!(
            values,
            vec![15, 14, 13, 12, 11, 10, 9, 8, 7, 6],
            "page 2 of a 10-row-page, n DESC preview must be exactly n=15..=6 in that order"
        );

        run_ddl(&*conn, &format!("DROP TABLE {SORTED_PREVIEW_TABLE}"));
    }
}
