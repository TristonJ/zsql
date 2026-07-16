//! Postgres backend for zsql, built on sqlx with the **smol** runtime so its
//! futures await directly on gpui's executor — no tokio runtime, no bridge thread.
//!
//! M0 provides only the [`spike_select_one`] proof-of-life plus a skeleton
//! [`PostgresDriver`]. The real connection/streaming/introspection lands in M1.

use async_trait::async_trait;
use zsql_core::{ConnConfig, Connection, CoreError, Driver};

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

    fn parse_dsn(&self, dsn: &str) -> Result<ConnConfig, CoreError> {
        ConnConfig::from_dsn(dsn)
    }

    async fn connect(&self, _cfg: &ConnConfig) -> Result<Box<dyn Connection>, CoreError> {
        // Pool build + PgConnection wrapper land in M1.
        Err(CoreError::Connection(
            "connect() not implemented until M1".into(),
        ))
    }
}

/// M0 spike: prove sqlx (runtime-smol) connects and runs a trivial query while
/// driven by a non-tokio executor. Returns the value of `SELECT 1`.
///
/// # Errors
/// Returns an error if the connection or query fails.
#[tracing::instrument(skip_all)]
pub async fn spike_select_one(url: &str) -> anyhow::Result<i64> {
    use sqlx::Row;

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(url)
        .await?;
    let row = sqlx::query("SELECT 1 AS one").fetch_one(&pool).await?;
    let one: i32 = row.try_get("one")?;
    pool.close().await;
    Ok(i64::from(one))
}
