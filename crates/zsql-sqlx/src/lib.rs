//! Shared utilities for drivers that depend on `sqlx`.

use std::marker::PhantomData;

use futures::StreamExt as _;
use sqlx::{AssertSqlSafe, Database, Executor, Row as _, SqlSafeStr as _, Statement as _};
use zsql_core::{BatchSink, QueryEvent, QueryHandle, RowBatch};

use crate::error::map_sqlx_query_error;

pub mod error;
pub mod pool;

/// Backend-specific hooks [`run_query`] needs from each sqlx-based driver.
///
/// Implemented on a zero-sized marker type per driver crate; every method is
/// associated (no `self`), so the type only selects the implementation.
pub trait SqlxZsqlDriver<DB: Database>: 'static {
    /// Short backend name recorded on tracing spans (e.g. `"postgres"`).
    const NAME: &'static str;

    /// Backend-specific data captured up front that later lets
    /// [`run_query`] cancel the in-flight query from a second connection.
    type Cancel: CancelHandle<DB>;

    /// Per-connection context [`Self::resolve_columns`] needs, captured once
    /// when [`SqlxConnection`] is built and cloned into every query. A
    /// driver that never overrides `resolve_columns` uses `()`.
    type ColumnContext: Clone + Send + Sync + 'static;

    /// Build the `Columns` metadata for a result set's own column list.
    fn column_metas(columns: &[DB::Column]) -> Vec<zsql_core::ColumnMeta>;

    /// Decode a sqlx row into an engine-neutral [`zsql_core::Row`]
    fn decode_row(row: &DB::Row) -> zsql_core::Row;

    /// How many rows were affected by a statement
    fn rows_affected(result: &DB::QueryResult) -> u64;

    /// Get a cancel handle that can be used to cancel an in-flight query from
    /// a separate connection.
    fn cancel_handle(
        conn: &mut DB::Connection,
    ) -> impl Future<Output = Result<Self::Cancel, sqlx::Error>> + Send;

    /// Patch `columns` (already built via [`Self::column_metas`] from this
    /// very same `raw_columns`) in place, after they are collected and
    /// before they are sent as the stream's `Columns` event. Default is a
    /// no-op: only a driver whose backend can report a column's type as
    /// unresolved (e.g. Postgres's dynamically assigned OIDs) needs to
    /// override this.
    fn resolve_columns(
        _ctx: &Self::ColumnContext,
        _raw_columns: &[DB::Column],
        _columns: &mut [zsql_core::ColumnMeta],
    ) -> impl Future<Output = ()> + Send {
        async {}
    }
}

/// Backend-specific data needed to cancel an in-flight query from a second
/// connection (e.g. a postgres backend pid, a mysql connection id).
pub trait CancelHandle<DB: Database>: Send + 'static {
    /// Issue the server-side cancel on a connection drawn from
    /// `cancel_pool`, never the pool running the query itself (which may be
    /// blocked server-side and unable to service another statement).
    fn cancel(
        self,
        cancel_pool: &sqlx::Pool<DB>,
    ) -> impl Future<Output = Result<(), sqlx::Error>> + Send;
}

/// [`CancelHandle`] for backends with no server-side cancel mechanism
/// (sqlite): acquisition always succeeds and cancelling is a no-op, leaving
/// only cooperative cancellation via the `cancel_rx` channel.
pub struct NoServerSideCancel;

impl<DB: Database> CancelHandle<DB> for NoServerSideCancel {
    fn cancel(
        self,
        _cancel_pool: &sqlx::Pool<DB>,
    ) -> impl Future<Output = Result<(), sqlx::Error>> + Send {
        tracing::info!(
            "server-side cancel requested, but backend has no server-side cancel mechanism; query will only be cancelled cooperatively"
        );
        std::future::ready(Ok(()))
    }
}

/// The pool trio backing one sqlx-based driver connection, plus the shared
/// query-streaming entry point. Drivers embed this and delegate
/// [`zsql_core::Connection::stream_query`] to [`Self::stream_query`].
///
/// `C` is the driver's [`SqlxZsqlDriver::ColumnContext`], captured once here
/// at construction and cloned into every query; it defaults to `()` since
/// most drivers never override [`SqlxZsqlDriver::resolve_columns`].
pub struct SqlxConnection<DB: Database, D, C = ()> {
    pool: sqlx::Pool<DB>,
    /// Separate, independently-bounded pool used only for server-side
    /// cancellation
    cancel_pool: sqlx::Pool<DB>,
    /// Separate, independently-bounded pool used only for the liveness
    /// probe, so a probe can never be blocked behind an in-flight query or a
    /// cancel request, nor block either of those in turn.
    probe_pool: sqlx::Pool<DB>,
    /// Rows grouped into one [`QueryEvent::Batch`] at a time by
    /// [`run_query`], set at construction from the app's configured
    /// `query.batch_size`.
    batch_size: usize,
    column_ctx: C,

    driver: PhantomData<D>,
}

impl<DB, D> SqlxConnection<DB, D, D::ColumnContext>
where
    DB: Database,
    D: SqlxZsqlDriver<DB>,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
{
    #[must_use]
    pub fn new(
        pool: sqlx::Pool<DB>,
        cancel_pool: sqlx::Pool<DB>,
        probe_pool: sqlx::Pool<DB>,
        batch_size: usize,
        column_ctx: D::ColumnContext,
    ) -> Self {
        Self {
            pool,
            cancel_pool,
            probe_pool,
            batch_size,
            column_ctx,
            driver: PhantomData,
        }
    }

    #[must_use]
    pub fn pool(&self) -> &sqlx::Pool<DB> {
        &self.pool
    }

    #[must_use]
    pub fn probe_pool(&self) -> &sqlx::Pool<DB> {
        &self.probe_pool
    }

    #[must_use]
    pub fn stream_query(&self, sql: String, sink: BatchSink) -> QueryHandle {
        let (cancel_tx, cancel_rx) = flume::unbounded();
        let pool = self.pool.clone();
        let cancel_pool = self.cancel_pool.clone();
        let batch_size = self.batch_size;
        let column_ctx = self.column_ctx.clone();
        // Run on the smol-based executor sqlx's `runtime-smol` feature uses.
        async_global_executor::spawn(run_query::<DB, D>(
            pool,
            cancel_pool,
            column_ctx,
            sql,
            sink,
            cancel_rx,
            batch_size,
        ))
        .detach();
        QueryHandle::new(cancel_tx)
    }
}

impl<DB: Database, D, C> SqlxConnection<DB, D, C> {
    /// Close all three pools this connection owns (main, cancel, probe),
    /// releasing their background workers rather than leaving that to each
    /// pool's own `Drop`.
    #[tracing::instrument(name = "sqlx_connection_close", skip_all)]
    pub async fn close(&self) {
        self.pool.close().await;
        self.cancel_pool.close().await;
        self.probe_pool.close().await;
        tracing::info!("sqlx connection pools closed");
    }
}

/// Stream a query's results into `sink`. `sql` may hold several statements;
/// each result-producing statement emits its own [`QueryEvent::Columns`]
/// followed by that set's [`QueryEvent::Batch`]es, and the whole stream ends
/// with exactly one [`QueryEvent::Done`] - or, on any failure, a single `Err`
/// in place of `Done`. Every statement still executes (so all side effects
/// happen); a fresh `Columns` event marks each set boundary so the consumer
/// can keep only the last set rather than concatenating mismatched rows.
///
/// Runs on a single connection acquired from `pool` for the lifetime of this
/// call (not the pool directly), so the backend's cancel handle can be
/// captured via [`SqlxZsqlDriver::cancel_handle`] *before* the row-streaming
/// loop below ever starts checking `cancel_rx`.
#[tracing::instrument(
    name = "stream_query",
    skip_all,
    fields(driver = D::NAME, pool_size = pool.size(), batch_size)
)]
#[allow(clippy::too_many_lines)] // one streaming state machine; splitting scatters the per-statement latches
pub async fn run_query<DB, D>(
    pool: sqlx::Pool<DB>,
    cancel_pool: sqlx::Pool<DB>,
    column_ctx: D::ColumnContext,
    sql: String,
    sink: BatchSink,
    cancel_rx: flume::Receiver<()>,
    batch_size: usize,
) where
    DB: Database,
    D: SqlxZsqlDriver<DB>,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
{
    // The SQL text itself carries no connection secrets (those live only in
    // the URL, never logged here), so it is fine to record at debug level.
    tracing::debug!(sql = %sql, "streaming query");

    let mut conn = match pool.acquire().await {
        Ok(conn) => conn,
        Err(err) => {
            let _ = sink.send_async(Err(map_sqlx_query_error(err))).await;
            return;
        }
    };

    let cancel_handle = match D::cancel_handle(&mut conn).await {
        Ok(h) => {
            tracing::debug!("dedicated connection backend cancel handle acquired");
            Some(h)
        }
        Err(err) => {
            // Cooperative cancellation still works, so this is not fatal to the query.
            tracing::warn!(
                error = %err,
                "failed to acquire dedicated connection backend cancel handle; server-side cancel unavailable for this query"
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
                if let Some(h) = cancel_handle {
                    spawn_server_side_cancel(&cancel_pool, h);
                }
                return;
            }
            futures::future::Either::Right((None, _)) => break,
            futures::future::Either::Right((Some(Ok(sqlx::Either::Right(row))), _)) => {
                if !columns_sent {
                    let mut columns = D::column_metas(row.columns());
                    D::resolve_columns(&column_ctx, row.columns(), &mut columns).await;
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
                batch.push(D::decode_row(&row));
                if batch.len() >= batch_size {
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
                affected += D::rows_affected(&result);
                columns_sent = false;
            }
            futures::future::Either::Right((Some(Err(err)), _)) => {
                let _ = sink.send_async(Err(map_sqlx_query_error(err))).await;
                return;
            }
        }
    }

    // A statement with no output columns (DDL, or DML without `RETURNING`)
    // reports its row count as `affected` in `Done`. A statement that does
    // produce columns (SELECT, or DML with `RETURNING`) instead lets the
    // caller derive a count from the rows it already streamed, and reports
    // `affected: None`
    let reports_affected = if any_columns_sent {
        false
    } else {
        let columns = match pool.prepare(AssertSqlSafe(sql).into_sql_str()).await {
            Ok(statement) => {
                let mut columns = D::column_metas(statement.columns());
                D::resolve_columns(&column_ctx, statement.columns(), &mut columns).await;
                columns
            }
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

/// Spawn [`CancelHandle::cancel`] as a detached background task so neither
/// the query task (which may itself be about to return) nor the caller of
/// `cancel()` has to wait for the cancel round-trip to complete.
fn spawn_server_side_cancel<DB: Database, C: CancelHandle<DB>>(
    cancel_pool: &sqlx::Pool<DB>,
    handle: C,
) {
    let cancel_pool = cancel_pool.clone();
    async_global_executor::spawn(async move {
        match handle.cancel(&cancel_pool).await {
            Ok(()) => {
                tracing::info!("server-side cancel issued");
            }
            Err(err) => {
                tracing::warn!(error = %err, "server-side cancel request failed");
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use std::marker::PhantomData;
    use std::time::Duration;

    use sqlx::sqlite::{Sqlite, SqlitePoolOptions};

    use super::SqlxConnection;

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        futures::executor::block_on(fut)
    }

    async fn sqlite_pool() -> sqlx::Pool<Sqlite> {
        SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(1))
            .connect("sqlite::memory:")
            .await
            .expect("in-memory connect should succeed")
    }

    #[test]
    fn close_closes_all_three_owned_pools() {
        let (pool, cancel_pool, probe_pool) = block_on(async {
            (
                sqlite_pool().await,
                sqlite_pool().await,
                sqlite_pool().await,
            )
        });
        let pool_handle = pool.clone();
        let cancel_handle = cancel_pool.clone();
        let probe_handle = probe_pool.clone();

        // `D` is only ever a `PhantomData` marker for `close`, which touches
        // no `SqlxZsqlDriver` method, so a plain `()` stands in for it.
        let connection: SqlxConnection<Sqlite, ()> = SqlxConnection {
            pool,
            cancel_pool,
            probe_pool,
            batch_size: zsql_core::DEFAULT_QUERY_BATCH_SIZE,
            column_ctx: (),
            driver: PhantomData,
        };

        block_on(connection.close());

        assert!(pool_handle.is_closed(), "main pool must be closed");
        assert!(cancel_handle.is_closed(), "cancel pool must be closed");
        assert!(probe_handle.is_closed(), "probe pool must be closed");
    }
}
