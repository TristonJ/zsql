//! Schema introspection: `information_schema` -> [`zsql_core::SchemaTree`].
//!
//! `MySQL`/`MariaDB` have no catalog level distinct from a database: a
//! connection sees every database it has privileges on as a peer, with no
//! further nesting. `information_schema.TABLES.TABLE_CATALOG` is always the
//! fixed literal `"def"` (`MySQL`'s own vestigial catalog placeholder, never
//! a real, selectable object), so this module reports one synthetic
//! [`Catalog`] named `"def"` and maps every non-system database to a
//! [`SchemaNs`] within it -- the same shape `zsql-mssql` uses for a single
//! connected database's schemas, just one level up.

use std::collections::HashMap;

use sqlx::mysql::MySqlPool;
use sqlx::{AssertSqlSafe, Row as _};
use zsql_core::{Catalog, ColumnMeta, CoreError, Relation, RelationKind, SchemaNs, SchemaTree};
use zsql_sqlx::error::map_sqlx_introspect_error;

/// The fixed catalog name every `MySQL`/`MariaDB` connection reports via
/// `information_schema.TABLES.TABLE_CATALOG`.
const CATALOG_NAME: &str = "def";

/// Every database name introspection never surfaces: the information/
/// performance schema views, and each engine's own system database (`mysql`
/// for `MySQL`/`MariaDB`, `sys` for `MySQL`'s `sys` schema helper views).
const SYSTEM_SCHEMAS_SQL_LIST: &str = "'information_schema', 'performance_schema', 'mysql', 'sys'";

/// Build a full [`SchemaTree`] for every non-system database `pool` can see.
///
/// # Errors
/// Returns [`CoreError::Introspection`] if any underlying query fails.
pub(crate) async fn introspect(pool: &MySqlPool) -> Result<SchemaTree, CoreError> {
    let schema_names = schema_names(pool).await?;
    let mut relations_by_schema = relations(pool).await?;
    let mut columns_by_relation = columns(pool).await?;

    let schemas = schema_names
        .into_iter()
        .map(|schema_name| {
            let mut schema_relations = relations_by_schema.remove(&schema_name).unwrap_or_default();
            for relation in &mut schema_relations {
                let key = (schema_name.clone(), relation.name.clone());
                relation.columns = columns_by_relation.remove(&key).unwrap_or_default();
            }
            SchemaNs {
                name: schema_name,
                tables: schema_relations,
            }
        })
        .collect();

    Ok(SchemaTree {
        catalogs: vec![Catalog {
            name: CATALOG_NAME.to_owned(),
            schemas,
        }],
    })
}

/// Every non-system database, sorted by name.
async fn schema_names(pool: &MySqlPool) -> Result<Vec<String>, CoreError> {
    let sql = format!(
        "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA \
         WHERE SCHEMA_NAME NOT IN ({SYSTEM_SCHEMAS_SQL_LIST}) \
         ORDER BY SCHEMA_NAME"
    );
    // `sql` is assembled purely from compile-time-constant string fragments
    // (this function's own literal plus `SYSTEM_SCHEMAS_SQL_LIST`), never
    // from anything read at runtime.
    sqlx::query_scalar(AssertSqlSafe(sql))
        .fetch_all(pool)
        .await
        .map_err(map_sqlx_introspect_error)
}

/// Every table and view across all non-system databases, grouped by database
/// name. Within each group, relations are sorted by name.
async fn relations(pool: &MySqlPool) -> Result<HashMap<String, Vec<Relation>>, CoreError> {
    let sql = format!(
        "SELECT TABLE_SCHEMA, TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA NOT IN ({SYSTEM_SCHEMAS_SQL_LIST}) \
         ORDER BY TABLE_SCHEMA, TABLE_NAME"
    );
    let rows = sqlx::query(AssertSqlSafe(sql))
        .fetch_all(pool)
        .await
        .map_err(map_sqlx_introspect_error)?;

    let mut grouped: HashMap<String, Vec<Relation>> = HashMap::new();
    for row in &rows {
        let schema: String = row
            .try_get("TABLE_SCHEMA")
            .map_err(map_sqlx_introspect_error)?;
        let name: String = row
            .try_get("TABLE_NAME")
            .map_err(map_sqlx_introspect_error)?;
        let table_type: String = row
            .try_get("TABLE_TYPE")
            .map_err(map_sqlx_introspect_error)?;
        let Some(kind) = relation_kind(&table_type) else {
            continue;
        };
        grouped.entry(schema).or_default().push(Relation {
            name,
            kind,
            columns: Vec::new(),
        });
    }
    Ok(grouped)
}

/// Map an `information_schema.TABLES.TABLE_TYPE` value to the neutral
/// [`RelationKind`]. MySQL/MariaDB never populate `TABLES` with anything a
/// materialized view or a partition would need of their own (a partitioned
/// table's partitions are metadata under `information_schema.PARTITIONS`,
/// not separate `TABLES` rows), so only `Table`/`View` are ever produced;
/// `SYSTEM VIEW` (used only within the system schemas this module already
/// excludes) maps to nothing.
fn relation_kind(table_type: &str) -> Option<RelationKind> {
    match table_type {
        "BASE TABLE" => Some(RelationKind::Table),
        "VIEW" => Some(RelationKind::View),
        _ => None,
    }
}

/// Every column of every table/view across all non-system databases, grouped
/// by `(database name, relation name)`. Within each group, columns are in
/// ordinal position.
async fn columns(
    pool: &MySqlPool,
) -> Result<HashMap<(String, String), Vec<ColumnMeta>>, CoreError> {
    let sql = format!(
        "SELECT TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE \
         FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA NOT IN ({SYSTEM_SCHEMAS_SQL_LIST}) \
         ORDER BY TABLE_SCHEMA, TABLE_NAME, ORDINAL_POSITION"
    );
    let rows = sqlx::query(AssertSqlSafe(sql))
        .fetch_all(pool)
        .await
        .map_err(map_sqlx_introspect_error)?;

    let mut grouped: HashMap<(String, String), Vec<ColumnMeta>> = HashMap::new();
    for row in &rows {
        let schema: String = row
            .try_get("TABLE_SCHEMA")
            .map_err(map_sqlx_introspect_error)?;
        let relation: String = row
            .try_get("TABLE_NAME")
            .map_err(map_sqlx_introspect_error)?;
        let name: String = row
            .try_get("COLUMN_NAME")
            .map_err(map_sqlx_introspect_error)?;
        let type_name: String = row
            .try_get("COLUMN_TYPE")
            .map_err(map_sqlx_introspect_error)?;
        let is_nullable: String = row
            .try_get("IS_NULLABLE")
            .map_err(map_sqlx_introspect_error)?;
        grouped
            .entry((schema, relation))
            .or_default()
            .push(ColumnMeta {
                name,
                type_name,
                nullable: is_nullable.eq_ignore_ascii_case("YES"),
            });
    }
    Ok(grouped)
}

#[cfg(test)]
mod tests {
    use super::relation_kind;
    use zsql_core::RelationKind;

    #[test]
    fn maps_every_table_type_this_module_cares_about() {
        assert_eq!(relation_kind("BASE TABLE"), Some(RelationKind::Table));
        assert_eq!(relation_kind("VIEW"), Some(RelationKind::View));
        assert_eq!(
            relation_kind("SYSTEM VIEW"),
            None,
            "a system view only ever appears within an excluded system schema"
        );
        assert_eq!(relation_kind(""), None, "empty table type maps to nothing");
    }
}
