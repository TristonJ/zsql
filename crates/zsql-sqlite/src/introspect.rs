//! Schema introspection: `sqlite_master` + `pragma_table_info` -> [`zsql_core::SchemaTree`].

use sqlx::Row as _;
use sqlx::sqlite::SqlitePool;
use zsql_core::{Catalog, ColumnMeta, CoreError, Relation, RelationKind, SchemaNs, SchemaTree};
use zsql_sqlx::error::map_sqlx_introspect_error;

/// `SQLite`'s fixed name for a connection's primary (always-present) database,
/// as opposed to `temp` or any database added later via `ATTACH`. Introspection
/// does not attach extra databases, so this is the only schema namespace seen.
pub(crate) const MAIN_SCHEMA_NAME: &str = "main";

/// Displayed in place of a file path for an in-memory connection, whose
/// `PRAGMA database_list` file column is an empty string.
const IN_MEMORY_CATALOG_NAME: &str = ":memory:";

/// Build a full [`SchemaTree`] for the database `pool` is connected to.
///
/// # Errors
/// Returns [`CoreError::Introspection`] if any underlying query fails.
pub(crate) async fn introspect(pool: &SqlitePool) -> Result<SchemaTree, CoreError> {
    let catalog_name = main_database_file(pool).await?;
    let mut tables = relations(pool).await?;
    for table in &mut tables {
        table.columns = columns(pool, &table.name).await?;
    }

    Ok(SchemaTree {
        catalogs: vec![Catalog {
            name: catalog_name,
            schemas: vec![SchemaNs {
                name: MAIN_SCHEMA_NAME.to_owned(),
                tables,
            }],
        }],
    })
}

/// The file backing the `main` database, or [`IN_MEMORY_CATALOG_NAME`] if
/// this connection has none.
async fn main_database_file(pool: &SqlitePool) -> Result<String, CoreError> {
    let row = sqlx::query("SELECT file FROM pragma_database_list WHERE name = 'main'")
        .fetch_one(pool)
        .await
        .map_err(map_sqlx_introspect_error)?;
    let file: String = row.try_get("file").map_err(map_sqlx_introspect_error)?;
    Ok(if file.is_empty() {
        IN_MEMORY_CATALOG_NAME.to_owned()
    } else {
        file
    })
}

/// Every table and view in `main`, excluding `SQLite`'s own internal
/// bookkeeping tables (`sqlite_sequence` and similar), sorted by name.
async fn relations(pool: &SqlitePool) -> Result<Vec<Relation>, CoreError> {
    let rows = sqlx::query(
        "SELECT name, type FROM sqlite_master \
         WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
         ORDER BY name",
    )
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_introspect_error)?;

    let mut relations = Vec::with_capacity(rows.len());
    for row in &rows {
        let name: String = row.try_get("name").map_err(map_sqlx_introspect_error)?;
        let sql_type: String = row.try_get("type").map_err(map_sqlx_introspect_error)?;
        let Some(kind) = relation_kind(&sql_type) else {
            continue;
        };
        relations.push(Relation {
            name,
            kind,
            columns: Vec::new(),
        });
    }
    Ok(relations)
}

/// Map a `sqlite_master.type` value to the neutral [`RelationKind`].
///
/// `SQLite` has no materialized-view or partitioned-table concept, so
/// [`RelationKind::MatView`] and [`RelationKind::Partitioned`] are never
/// produced here.
fn relation_kind(sql_type: &str) -> Option<RelationKind> {
    match sql_type {
        "table" => Some(RelationKind::Table),
        "view" => Some(RelationKind::View),
        _ => None,
    }
}

/// Every column of `table_name`, in ordinal position (`pragma_table_info`'s
/// `cid`, not alphabetical).
async fn columns(pool: &SqlitePool, table_name: &str) -> Result<Vec<ColumnMeta>, CoreError> {
    let rows = sqlx::query(
        r#"SELECT name, type, "notnull" AS not_null FROM pragma_table_info(?) ORDER BY cid"#,
    )
    .bind(table_name)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_introspect_error)?;

    let mut columns = Vec::with_capacity(rows.len());
    for row in &rows {
        let name: String = row.try_get("name").map_err(map_sqlx_introspect_error)?;
        let type_name: String = row.try_get("type").map_err(map_sqlx_introspect_error)?;
        let not_null: i64 = row.try_get("not_null").map_err(map_sqlx_introspect_error)?;
        columns.push(ColumnMeta {
            name,
            type_name,
            nullable: not_null == 0,
        });
    }
    Ok(columns)
}

#[cfg(test)]
mod tests {
    use super::relation_kind;
    use zsql_core::RelationKind;

    #[test]
    fn maps_every_sqlite_master_type_this_module_cares_about() {
        assert_eq!(relation_kind("table"), Some(RelationKind::Table));
        assert_eq!(relation_kind("view"), Some(RelationKind::View));
        assert_eq!(relation_kind("index"), None, "an index is not a relation");
        assert_eq!(
            relation_kind("trigger"),
            None,
            "a trigger is not a relation"
        );
        assert_eq!(relation_kind(""), None, "empty type maps to nothing");
    }
}
