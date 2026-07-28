//! Schema introspection: `pg_catalog` -> [`zsql_core::SchemaTree`].

use std::collections::HashMap;

use sqlx::postgres::{PgConnection, PgPool};
use sqlx::{AssertSqlSafe, Row as _};
use zsql_core::{Catalog, ColumnMeta, CoreError, Relation, RelationKind, SchemaNs, SchemaTree};
use zsql_sqlx::error::map_sqlx_introspect_error;

/// Shared `WHERE` fragment excluding system schemas from every query below:
/// the system catalog itself, the SQL-standard `information_schema` view
/// layer, and the out-of-line storage schema for oversized column values are
/// excluded by fixed name; per-session temporary-table schemas
/// (`pg_temp_<backend-id>` / `pg_toast_temp_<backend-id>`) vary by session, so
/// they are excluded by prefix instead of by fixed name. Every query joins in
/// `pg_namespace` aliased as `n`, so this fragment always refers to `n.nspname`.
const NAMESPACE_FILTER: &str = "n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast') \
    AND n.nspname !~ '^pg_temp' AND n.nspname !~ '^pg_toast_temp'";

/// Build a full [`SchemaTree`] for the database `pool` is connected to.
///
/// # Errors
/// Returns [`CoreError::Introspection`] if opening the snapshot transaction,
/// any of the underlying queries, or committing that transaction fails.
pub(crate) async fn introspect(pool: &PgPool) -> Result<SchemaTree, CoreError> {
    let mut tx = pool.begin().await.map_err(map_sqlx_introspect_error)?;
    // Plain `BEGIN` defaults to READ WRITE READ COMMITTED, which lets each
    // statement in the transaction see a fresh (and potentially different)
    // snapshot of the catalog. REPEATABLE READ pins one snapshot for every
    // statement below
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_introspect_error)?;

    let catalog_name = current_database(&mut tx).await?;
    let schema_names = schema_names(&mut tx).await?;
    let relations_by_schema = relations(&mut tx).await?;
    let columns_by_relation = columns(&mut tx).await?;

    // The transaction only ever read, so commit vs. rollback would leave the
    // database in the same state either way
    tx.rollback().await.map_err(map_sqlx_introspect_error)?;

    let schemas = assemble_schemas(schema_names, relations_by_schema, columns_by_relation);

    Ok(SchemaTree {
        catalogs: vec![Catalog {
            name: catalog_name,
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

/// The name of the database the current connection is attached to
async fn current_database(conn: &mut PgConnection) -> Result<String, CoreError> {
    sqlx::query_scalar("SELECT current_database()")
        .fetch_one(conn)
        .await
        .map_err(map_sqlx_introspect_error)
}

/// Every non-system schema, sorted by name
async fn schema_names(conn: &mut PgConnection) -> Result<Vec<String>, CoreError> {
    let sql = format!(
        "SELECT n.nspname \
         FROM pg_catalog.pg_namespace n \
         WHERE {NAMESPACE_FILTER} \
         ORDER BY n.nspname"
    );
    // `sql` is assembled purely from compile-time-constant string fragments
    // (this function's own literal plus `NAMESPACE_FILTER`) via `format!`,
    // never from anything read at runtime
    sqlx::query_scalar(AssertSqlSafe(sql))
        .fetch_all(conn)
        .await
        .map_err(map_sqlx_introspect_error)
}

/// Every table, view, and materialized view across all non-system schemas,
/// grouped by schema name. Within each schema's list, relations are sorted
/// by name
async fn relations(conn: &mut PgConnection) -> Result<HashMap<String, Vec<Relation>>, CoreError> {
    // `relkind` is Postgres's internal one-byte `"char"` type, which sqlx has
    // no built-in text decode for
    let sql = format!(
        "SELECT n.nspname, c.relname, c.relkind::text AS relkind \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relkind IN ('r', 'p', 'v', 'm') AND {NAMESPACE_FILTER} \
         ORDER BY n.nspname, c.relname"
    );
    let rows = sqlx::query(AssertSqlSafe(sql))
        .fetch_all(conn)
        .await
        .map_err(map_sqlx_introspect_error)?;

    let mut plain_rows = Vec::with_capacity(rows.len());
    for row in &rows {
        let schema: String = row.try_get("nspname").map_err(map_sqlx_introspect_error)?;
        let name: String = row.try_get("relname").map_err(map_sqlx_introspect_error)?;
        let relkind: String = row.try_get("relkind").map_err(map_sqlx_introspect_error)?;
        plain_rows.push((schema, name, relkind));
    }
    Ok(group_relations(plain_rows))
}

/// Group plain `(schema, relation, relkind)` rows into a per-schema
/// relation list, skipping any row whose `relkind` has no [`RelationKind`]
/// mapping.
fn group_relations(rows: Vec<(String, String, String)>) -> HashMap<String, Vec<Relation>> {
    let mut grouped: HashMap<String, Vec<Relation>> = HashMap::new();
    for (schema, name, relkind) in rows {
        let Some(kind) = relation_kind(&relkind) else {
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

/// Map a `pg_class.relkind` code to the neutral [`RelationKind`].
fn relation_kind(relkind: &str) -> Option<RelationKind> {
    match relkind {
        "r" => Some(RelationKind::Table),
        "p" => Some(RelationKind::Partitioned),
        "v" => Some(RelationKind::View),
        "m" => Some(RelationKind::MatView),
        _ => None,
    }
}

/// Every column of every table/view/matview across all non-system schemas,
/// grouped by `(schema name, relation name)`. Within each group, columns are
/// in ordinal position
async fn columns(
    conn: &mut PgConnection,
) -> Result<HashMap<(String, String), Vec<ColumnMeta>>, CoreError> {
    let sql = format!(
        "SELECT n.nspname, c.relname, a.attname, \
                pg_catalog.format_type(a.atttypid, a.atttypmod) AS type_name, \
                NOT a.attnotnull AS nullable \
         FROM pg_catalog.pg_attribute a \
         JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relkind IN ('r', 'p', 'v', 'm') \
           AND a.attnum > 0 AND NOT a.attisdropped \
           AND {NAMESPACE_FILTER} \
         ORDER BY n.nspname, c.relname, a.attnum"
    );
    //`sql` is built only from compile-time constant fragments
    let rows = sqlx::query(AssertSqlSafe(sql))
        .fetch_all(conn)
        .await
        .map_err(map_sqlx_introspect_error)?;

    let mut plain_rows = Vec::with_capacity(rows.len());
    for row in &rows {
        let schema: String = row.try_get("nspname").map_err(map_sqlx_introspect_error)?;
        let relation: String = row.try_get("relname").map_err(map_sqlx_introspect_error)?;
        let name: String = row.try_get("attname").map_err(map_sqlx_introspect_error)?;
        let type_name: String = row
            .try_get("type_name")
            .map_err(map_sqlx_introspect_error)?;
        let nullable: bool = row.try_get("nullable").map_err(map_sqlx_introspect_error)?;
        plain_rows.push((
            schema,
            relation,
            ColumnMeta {
                name,
                type_name,
                nullable,
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
    fn maps_every_relkind_code() {
        assert_eq!(relation_kind("r"), Some(RelationKind::Table));
        assert_eq!(relation_kind("p"), Some(RelationKind::Partitioned));
        assert_eq!(relation_kind("v"), Some(RelationKind::View));
        assert_eq!(relation_kind("m"), Some(RelationKind::MatView));
        assert_eq!(relation_kind("i"), None, "index relkind is not a relation");
        assert_eq!(
            relation_kind("S"),
            None,
            "sequence relkind is not a relation"
        );
        assert_eq!(relation_kind(""), None, "empty code maps to nothing");
    }

    #[test]
    fn group_relations_groups_multiple_relations_under_the_same_schema() {
        let grouped = group_relations(vec![
            ("public".to_owned(), "users".to_owned(), "r".to_owned()),
            ("public".to_owned(), "widgets".to_owned(), "v".to_owned()),
        ]);
        let relations = &grouped["public"];
        assert_eq!(relations.len(), 2);
        assert_eq!(relations[0].name, "users");
        assert_eq!(relations[0].kind, RelationKind::Table);
        assert_eq!(relations[1].name, "widgets");
        assert_eq!(relations[1].kind, RelationKind::View);
    }

    #[test]
    fn group_relations_skips_a_relation_with_an_unmapped_relkind() {
        let grouped = group_relations(vec![(
            "public".to_owned(),
            "users_id_seq".to_owned(),
            "S".to_owned(),
        )]);
        assert!(grouped.is_empty());
    }

    #[test]
    fn group_columns_groups_multiple_columns_under_the_same_relation_key() {
        let column = |name: &str| ColumnMeta {
            name: name.to_owned(),
            type_name: "int4".to_owned(),
            nullable: false,
        };
        let grouped = group_columns(vec![
            ("public".to_owned(), "users".to_owned(), column("id")),
            ("public".to_owned(), "users".to_owned(), column("name")),
        ]);
        let columns = &grouped[&("public".to_owned(), "users".to_owned())];
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
            "public".to_owned(),
            "users".to_owned(),
            "r".to_owned(),
        )]);
        let columns = group_columns(vec![(
            "public".to_owned(),
            "users".to_owned(),
            ColumnMeta {
                name: "id".to_owned(),
                type_name: "int4".to_owned(),
                nullable: false,
            },
        )]);
        let schemas = assemble_schemas(vec!["public".to_owned()], relations, columns);
        assert_eq!(schemas[0].tables[0].columns.len(), 1);
        assert_eq!(schemas[0].tables[0].columns[0].name, "id");
    }

    #[test]
    fn assemble_schemas_leaves_columns_empty_when_the_grouped_map_has_no_matching_key() {
        let relations = group_relations(vec![(
            "public".to_owned(),
            "users".to_owned(),
            "r".to_owned(),
        )]);
        let schemas = assemble_schemas(vec!["public".to_owned()], relations, HashMap::new());
        assert!(schemas[0].tables[0].columns.is_empty());
    }
}
