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
    let relations_by_schema = relations(pool).await?;
    let columns_by_relation = columns(pool).await?;

    let schemas = assemble_schemas(schema_names, relations_by_schema, columns_by_relation);

    Ok(SchemaTree {
        catalogs: vec![Catalog {
            name: CATALOG_NAME.to_owned(),
            schemas,
        }],
    })
}

/// Build every [`SchemaNs`] this database reports: one per name in
/// `schema_names`, each carrying the relations grouped under it (empty if
/// none were found) with that relation's own grouped columns attached (also
/// empty if none were found).
fn assemble_schemas(
    schema_names: Vec<String>,
    mut relations_by_schema: HashMap<String, Vec<Relation>>,
    mut columns_by_relation: HashMap<(String, String), Vec<ColumnMeta>>,
) -> Vec<SchemaNs> {
    schema_names
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
        .collect()
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

    let mut plain_rows = Vec::with_capacity(rows.len());
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
        plain_rows.push((schema, name, table_type));
    }
    Ok(group_relations(plain_rows))
}

/// Group plain `(schema, relation, table_type)` rows into a per-schema
/// relation list, skipping any row whose `table_type` has no
/// [`RelationKind`] mapping.
fn group_relations(rows: Vec<(String, String, String)>) -> HashMap<String, Vec<Relation>> {
    let mut grouped: HashMap<String, Vec<Relation>> = HashMap::new();
    for (schema, name, table_type) in rows {
        let Some(kind) = relation_kind(&table_type) else {
            continue;
        };
        grouped.entry(schema).or_default().push(Relation {
            name,
            kind,
            columns: Vec::new(),
        });
    }
    grouped
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

    let mut plain_rows = Vec::with_capacity(rows.len());
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
        plain_rows.push((
            schema,
            relation,
            ColumnMeta {
                name,
                type_name,
                nullable: is_nullable.eq_ignore_ascii_case("YES"),
            },
        ));
    }
    Ok(group_columns(plain_rows))
}

/// Group plain `(schema, relation, column)` rows into a per-relation column
/// list, keyed by `(schema name, relation name)`.
fn group_columns(
    rows: Vec<(String, String, ColumnMeta)>,
) -> HashMap<(String, String), Vec<ColumnMeta>> {
    let mut grouped: HashMap<(String, String), Vec<ColumnMeta>> = HashMap::new();
    for (schema, relation, column) in rows {
        grouped.entry((schema, relation)).or_default().push(column);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::{assemble_schemas, group_columns, group_relations, relation_kind};
    use std::collections::HashMap;
    use zsql_core::{ColumnMeta, RelationKind};

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

    #[test]
    fn group_relations_groups_multiple_relations_under_the_same_schema() {
        let grouped = group_relations(vec![
            (
                "app".to_owned(),
                "users".to_owned(),
                "BASE TABLE".to_owned(),
            ),
            ("app".to_owned(), "widgets".to_owned(), "VIEW".to_owned()),
        ]);
        let relations = &grouped["app"];
        assert_eq!(relations.len(), 2);
        assert_eq!(relations[0].name, "users");
        assert_eq!(relations[0].kind, RelationKind::Table);
        assert_eq!(relations[1].name, "widgets");
        assert_eq!(relations[1].kind, RelationKind::View);
    }

    #[test]
    fn group_relations_skips_a_relation_with_an_unmapped_table_type() {
        let grouped = group_relations(vec![(
            "app".to_owned(),
            "v_stats".to_owned(),
            "SYSTEM VIEW".to_owned(),
        )]);
        assert!(grouped.is_empty());
    }

    #[test]
    fn group_columns_groups_multiple_columns_under_the_same_relation_key() {
        let column = |name: &str| ColumnMeta {
            name: name.to_owned(),
            type_name: "int".to_owned(),
            nullable: false,
        };
        let grouped = group_columns(vec![
            ("app".to_owned(), "users".to_owned(), column("id")),
            ("app".to_owned(), "users".to_owned(), column("name")),
        ]);
        let columns = &grouped[&("app".to_owned(), "users".to_owned())];
        assert_eq!(columns.len(), 2);
        assert_eq!(columns[0].name, "id");
        assert_eq!(columns[1].name, "name");
    }

    #[test]
    fn assemble_schemas_produces_an_empty_relation_list_for_a_schema_with_no_tables() {
        let schemas = assemble_schemas(
            vec!["empty_schema".to_owned()],
            HashMap::new(),
            HashMap::new(),
        );
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "empty_schema");
        assert!(schemas[0].tables.is_empty());
    }

    #[test]
    fn assemble_schemas_attaches_columns_when_the_grouped_map_has_a_matching_key() {
        let relations = group_relations(vec![(
            "app".to_owned(),
            "users".to_owned(),
            "BASE TABLE".to_owned(),
        )]);
        let columns = group_columns(vec![(
            "app".to_owned(),
            "users".to_owned(),
            ColumnMeta {
                name: "id".to_owned(),
                type_name: "int".to_owned(),
                nullable: false,
            },
        )]);
        let schemas = assemble_schemas(vec!["app".to_owned()], relations, columns);
        assert_eq!(schemas[0].tables[0].columns.len(), 1);
        assert_eq!(schemas[0].tables[0].columns[0].name, "id");
    }

    #[test]
    fn assemble_schemas_leaves_columns_empty_when_the_grouped_map_has_no_matching_key() {
        let relations = group_relations(vec![(
            "app".to_owned(),
            "users".to_owned(),
            "BASE TABLE".to_owned(),
        )]);
        let schemas = assemble_schemas(vec!["app".to_owned()], relations, HashMap::new());
        assert!(schemas[0].tables[0].columns.is_empty());
    }
}
