//! Enumerating the databases selectable on a Postgres server, for
//! [`zsql_core::Connection::list_databases`].

use sqlx::postgres::PgPool;
use zsql_core::CoreError;
use zsql_sqlx::error::map_sqlx_query_error;

/// Every non-template, connectable database on the server, sorted by name.
/// `pg_database` is a shared catalog visible regardless of which database
/// the connection is attached to, so this reflects the whole server, not
/// just the connected database.
const LIST_DATABASES_SQL: &str =
    "SELECT datname FROM pg_database WHERE NOT datistemplate AND datallowconn ORDER BY 1";

/// Fetch [`LIST_DATABASES_SQL`]'s result against `pool`.
///
/// # Errors
/// Returns [`CoreError::Query`] if the underlying query fails.
pub(crate) async fn list_databases(pool: &PgPool) -> Result<Vec<String>, CoreError> {
    sqlx::query_scalar(LIST_DATABASES_SQL)
        .fetch_all(pool)
        .await
        .map_err(map_sqlx_query_error)
}

#[cfg(test)]
mod tests {
    use super::LIST_DATABASES_SQL;

    #[test]
    fn the_query_excludes_template_and_non_connectable_databases() {
        assert!(LIST_DATABASES_SQL.contains("NOT datistemplate"));
        assert!(LIST_DATABASES_SQL.contains("datallowconn"));
    }

    #[test]
    fn the_query_orders_by_name() {
        assert!(LIST_DATABASES_SQL.to_uppercase().contains("ORDER BY 1"));
    }

    #[test]
    fn the_query_selects_from_pg_database_with_no_runtime_interpolation() {
        // The query is a fixed `const`, never built from runtime text, so a
        // database name can never be interpolated into it.
        assert!(LIST_DATABASES_SQL.starts_with("SELECT datname FROM pg_database"));
    }
}
