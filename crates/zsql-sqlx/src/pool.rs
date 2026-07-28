//! Generic pool construction and liveness checking shared by every
//! sqlx-based driver.
//!
//! A driver connection is backed by up to three independently-bounded pools,
//! never one shared pool:
//!
//! - the **main** pool (built by [`build_pool`]) that
//!   [`crate::SqlxConnection::stream_query`] draws from;
//! - a **cancel** pool (built by [`build_side_pool`]) that a server-side
//!   cancel (e.g. `pg_cancel_backend`, `KILL QUERY`) draws from, so
//!   cancelling a query is never queued behind the very connection running
//!   it -- a cooperatively-cancelled connection does not necessarily free its
//!   permit back to the main pool right away, since returning a pooled
//!   connection first pings it to confirm it is idle, which itself blocks
//!   until the backend actually responds;
//! - a **probe** pool (built by [`build_probe_pool`]) that a liveness probe
//!   draws from, so a probe can never be blocked behind an in-flight query
//!   nor behind (or itself block) a cancel request. This pool disables
//!   sqlx's `test_before_acquire`: sqlx's pool otherwise pings an idle
//!   connection *before* handing it back and, on failure, silently discards
//!   it and hands the caller a freshly opened one instead -- transparent
//!   self-healing that is exactly right for the main pool, but wrong for a
//!   probe, whose entire purpose is to be the thing that notices staleness.
//!   This pool must hand back whatever connection it has, dead or not, and
//!   let the probe's own query surface the failure.
//!
//! A driver with no separate server-side cancel mechanism, or with a
//! single-connection topology that cannot support three independent pools,
//! may reasonably only use [`build_pool`] and skip the other two.

use std::time::Duration;

use sqlx::pool::PoolOptions;
use sqlx::{Database, Executor, Pool, Row as _};
use zsql_core::CoreError;

use crate::error::map_sqlx_connection_error;

/// Bounded pool size for a single desktop client. Small on purpose: this app
/// drives at most a handful of concurrent operations (one running query plus
/// occasional introspection), and a modest ceiling avoids hammering the
/// server from a client that only ever has one user.
pub const MAX_POOL_CONNECTIONS: u32 = 5;

/// How long to wait for the initial connection (and later, a free pooled
/// connection) before giving up.
pub const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounded size of the dedicated pool a server-side cancel draws from (see
/// the module doc comment). Deliberately small and separate from
/// [`MAX_POOL_CONNECTIONS`]: a cancel is a single scalar statement, never
/// more than a couple of which are ever in flight at once for a single-user
/// desktop client.
pub const CANCEL_POOL_CONNECTIONS: u32 = 2;

/// Bounded size of the dedicated pool a liveness probe draws from (see the
/// module doc comment). Sized at 2, not 1: a session only ever runs one probe
/// at a time, but a second connection lets one probe's acquire succeed
/// promptly even if the prior probe's connection has not yet been returned
/// to the pool.
pub const PROBE_POOL_CONNECTIONS: u32 = 2;

/// Build a bounded connection pool for `url` and verify it is reachable with
/// a trivial liveness query before returning it.
///
/// # Errors
/// Returns [`CoreError::Connection`] if the pool cannot be built or the
/// liveness query fails.
pub async fn build_pool<DB>(
    url: &str,
    max_connections: u32,
    acquire_timeout: Duration,
) -> Result<Pool<DB>, CoreError>
where
    DB: Database,
    DB::Arguments: sqlx::IntoArguments<DB>,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
    for<'r> i32: sqlx::Decode<'r, DB> + sqlx::Type<DB>,
    for<'r> &'r str: sqlx::ColumnIndex<DB::Row>,
{
    let pool = PoolOptions::<DB>::new()
        .max_connections(max_connections)
        .acquire_timeout(acquire_timeout)
        .connect(url)
        .await
        .map_err(map_sqlx_connection_error)?;
    liveness_check(&pool).await?;
    Ok(pool)
}

/// Build a small side pool of `max_connections`, used for an operation that
/// must never share a connection with the main query pool (see the module
/// doc comment). Connects lazily: parsing/validating `url` cannot fail
/// asynchronously here, so this is synchronous, and no network round trip
/// happens against this pool until its first query is actually issued.
///
/// # Errors
/// Returns [`CoreError::Connection`] if `url` cannot be parsed.
pub fn build_side_pool<DB>(
    url: &str,
    max_connections: u32,
    acquire_timeout: Duration,
) -> Result<Pool<DB>, CoreError>
where
    DB: Database,
{
    PoolOptions::<DB>::new()
        .max_connections(max_connections)
        .acquire_timeout(acquire_timeout)
        .connect_lazy(url)
        .map_err(map_sqlx_connection_error)
}

/// Build the small dedicated pool a liveness probe draws from, with sqlx's
/// default `test_before_acquire` disabled (see the module doc comment).
///
/// # Errors
/// Returns [`CoreError::Connection`] if `url` cannot be parsed.
pub fn build_probe_pool<DB>(
    url: &str,
    max_connections: u32,
    acquire_timeout: Duration,
) -> Result<Pool<DB>, CoreError>
where
    DB: Database,
{
    PoolOptions::<DB>::new()
        .max_connections(max_connections)
        .acquire_timeout(acquire_timeout)
        .test_before_acquire(false)
        .connect_lazy(url)
        .map_err(map_sqlx_connection_error)
}

/// Run a trivial `SELECT 1` against `pool` to confirm the connection is
/// actually usable, not just accepted. Returns the decoded value.
///
/// # Errors
/// Returns [`CoreError::Connection`] if the query fails.
pub async fn liveness_check<DB>(pool: &Pool<DB>) -> Result<i64, CoreError>
where
    DB: Database,
    DB::Arguments: sqlx::IntoArguments<DB>,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
    for<'r> i32: sqlx::Decode<'r, DB> + sqlx::Type<DB>,
    for<'r> &'r str: sqlx::ColumnIndex<DB::Row>,
{
    let row = sqlx::query("SELECT 1 AS one")
        .fetch_one(pool)
        .await
        .map_err(map_sqlx_connection_error)?;
    let one: i32 = row.try_get("one").map_err(map_sqlx_connection_error)?;
    Ok(i64::from(one))
}

/// A liveness check for a pool that may only ever hold a single connection:
/// if every connection is currently checked out, there is no way to acquire
/// a second one to run the probe on, so this skips the round trip and
/// reports alive rather than blocking (or failing) behind whatever is
/// already using the pool's one connection.
///
/// # Errors
/// Returns [`CoreError::Connection`] if a connection was acquired but the
/// query failed.
pub async fn liveness_check_or_skip_if_busy<DB>(pool: &Pool<DB>) -> Result<i64, CoreError>
where
    DB: Database,
    DB::Arguments: sqlx::IntoArguments<DB>,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
    for<'r> i32: sqlx::Decode<'r, DB> + sqlx::Type<DB>,
    for<'r> &'r str: sqlx::ColumnIndex<DB::Row>,
{
    let Some(mut connection) = pool.try_acquire() else {
        tracing::info!("pool is busy, skipping liveness check");
        return Ok(1);
    };
    let row = sqlx::query("SELECT 1 AS one")
        .fetch_one(&mut *connection)
        .await
        .map_err(map_sqlx_connection_error)?;
    let one: i32 = row.try_get("one").map_err(map_sqlx_connection_error)?;
    Ok(i64::from(one))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    #[test]
    fn test_liveness_check_or_skip_if_busy_skips_when_pool_exhausted() {
        fn block_on<F: std::future::Future>(fut: F) -> F::Output {
            futures::executor::block_on(fut)
        }

        let pool = block_on(async {
            // Build a max_connections(1) sqlite in-memory pool.
            SqlitePoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_secs(1))
                .connect("sqlite::memory:")
                .await
                .expect("failed to create pool")
        });

        // Hold the single connection via try_acquire().
        let _connection = pool
            .try_acquire()
            .expect("expected to acquire the single connection");

        // Call liveness_check_or_skip_if_busy with the pool exhausted.
        // It should return Ok(1) without blocking or erroring.
        let result = block_on(liveness_check_or_skip_if_busy(&pool));
        match result {
            Ok(val) => assert_eq!(val, 1),
            Err(e) => panic!("expected Ok(1) but got error: {e:?}"),
        }
    }
}
