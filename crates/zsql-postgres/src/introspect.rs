//! Schema introspection: `pg_catalog` -> [`zsql_core::SchemaTree`].
//!
//! A Postgres connection only ever sees one database (catalog): the one it is
//! connected to. The whole tree — that catalog, its non-system schemas, each
//! schema's tables/views/materialized views, and each relation's columns — is
//! built from four bounded, set-based queries (current database name,
//! schemas, relations, columns). None of them is run once per relation or
//! once per column, so the round-trip count stays constant regardless of how
//! many tables or columns the database has.
//!
//! All four queries run inside a single `REPEATABLE READ READ ONLY`
//! transaction, so they observe one consistent snapshot of the catalog:
//! without that, each query could land on a different pooled connection, and
//! concurrent DDL landing between them could drop a relation whose schema
//! vanished mid-way, or list a relation whose column set was only half
//! populated.

use std::collections::HashMap;

use sqlx::postgres::{PgConnection, PgPool};
use sqlx::{AssertSqlSafe, Row as _};
use zsql_core::{Catalog, ColumnMeta, CoreError, Relation, RelationKind, SchemaNs, SchemaTree};

use crate::error::map_introspect_error;

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
    let mut tx = pool.begin().await.map_err(map_introspect_error)?;
    // Plain `BEGIN` defaults to READ WRITE READ COMMITTED, which lets each
    // statement in the transaction see a fresh (and potentially different)
    // snapshot of the catalog. REPEATABLE READ pins one snapshot for every
    // statement below; READ ONLY documents (and lets the server enforce)
    // that introspection never writes.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await
        .map_err(map_introspect_error)?;

    let catalog_name = current_database(&mut tx).await?;
    let schema_names = schema_names(&mut tx).await?;
    let mut relations_by_schema = relations(&mut tx).await?;
    let mut columns_by_relation = columns(&mut tx).await?;

    // The transaction only ever read, so commit vs. rollback would leave the
    // database in the same state either way; committing is the conventional
    // way to close out a transaction that finished without error.
    tx.commit().await.map_err(map_introspect_error)?;

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
            name: catalog_name,
            schemas,
        }],
    })
}

/// The name of the database the current connection is attached to. A
/// Postgres connection can never see another database's catalog, so this is
/// always exactly the tree's single [`Catalog`] name.
async fn current_database(conn: &mut PgConnection) -> Result<String, CoreError> {
    sqlx::query_scalar("SELECT current_database()")
        .fetch_one(conn)
        .await
        .map_err(map_introspect_error)
}

/// Every non-system schema, sorted by name (so the sidebar and tests get a
/// stable order without re-sorting downstream).
async fn schema_names(conn: &mut PgConnection) -> Result<Vec<String>, CoreError> {
    let sql = format!(
        "SELECT n.nspname \
         FROM pg_catalog.pg_namespace n \
         WHERE {NAMESPACE_FILTER} \
         ORDER BY n.nspname"
    );
    // `sql` is assembled purely from compile-time-constant string fragments
    // (this function's own literal plus `NAMESPACE_FILTER`) via `format!`,
    // never from anything read at runtime, so there is no injectable input
    // for `AssertSqlSafe` to be asserting past.
    sqlx::query_scalar(AssertSqlSafe(sql))
        .fetch_all(conn)
        .await
        .map_err(map_introspect_error)
}

/// Every table, view, and materialized view across all non-system schemas,
/// grouped by schema name. Within each schema's list, relations are in the
/// order the query returned them: sorted by name, since the query orders by
/// `(nspname, relname)`.
async fn relations(conn: &mut PgConnection) -> Result<HashMap<String, Vec<Relation>>, CoreError> {
    // `relkind` is Postgres's internal one-byte `"char"` type, which sqlx has
    // no built-in text decode for; casting to `text` in the query keeps the
    // decode on the Rust side to a plain `String`, matching every other
    // column read here.
    let sql = format!(
        "SELECT n.nspname, c.relname, c.relkind::text AS relkind \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.relkind IN ('r', 'p', 'v', 'm') AND {NAMESPACE_FILTER} \
         ORDER BY n.nspname, c.relname"
    );
    // See `schema_names` above: `sql` is built only from compile-time
    // constant fragments, never from runtime input.
    let rows = sqlx::query(AssertSqlSafe(sql))
        .fetch_all(conn)
        .await
        .map_err(map_introspect_error)?;

    let mut grouped: HashMap<String, Vec<Relation>> = HashMap::new();
    for row in &rows {
        let schema: String = row.try_get("nspname").map_err(map_introspect_error)?;
        let name: String = row.try_get("relname").map_err(map_introspect_error)?;
        let relkind: String = row.try_get("relkind").map_err(map_introspect_error)?;
        // The query's own `relkind IN (...)` filter already restricts rows to
        // the four kinds `relation_kind` maps; a row this defensive `None`
        // branch would skip should therefore never actually occur, but
        // skipping (instead of panicking or guessing a kind) keeps this code
        // correct even if that filter and this match ever drift apart.
        let Some(kind) = relation_kind(&relkind) else {
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

/// Map a `pg_class.relkind` code to the neutral [`RelationKind`]. Ordinary
/// (`r`) and partitioned (`p`) tables both surface as `Table` — a partitioned
/// table's own `pg_class` row is a table from the UI's point of view; its
/// partitions are separate rows, enumerated like any other table if not
/// excluded by schema.
fn relation_kind(relkind: &str) -> Option<RelationKind> {
    match relkind {
        "r" | "p" => Some(RelationKind::Table),
        "v" => Some(RelationKind::View),
        "m" => Some(RelationKind::MatView),
        _ => None,
    }
}

/// Every column of every table/view/matview across all non-system schemas,
/// grouped by `(schema name, relation name)`. Within each group, columns are
/// in the order the query returned them: ordinal position, since the query
/// orders by `(nspname, relname, attnum)` — never alphabetical.
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
    // See `schema_names` above: `sql` is built only from compile-time
    // constant fragments, never from runtime input.
    let rows = sqlx::query(AssertSqlSafe(sql))
        .fetch_all(conn)
        .await
        .map_err(map_introspect_error)?;

    let mut grouped: HashMap<(String, String), Vec<ColumnMeta>> = HashMap::new();
    for row in &rows {
        let schema: String = row.try_get("nspname").map_err(map_introspect_error)?;
        let relation: String = row.try_get("relname").map_err(map_introspect_error)?;
        let name: String = row.try_get("attname").map_err(map_introspect_error)?;
        let type_name: String = row.try_get("type_name").map_err(map_introspect_error)?;
        let nullable: bool = row.try_get("nullable").map_err(map_introspect_error)?;
        grouped
            .entry((schema, relation))
            .or_default()
            .push(ColumnMeta {
                name,
                type_name,
                nullable,
            });
    }
    Ok(grouped)
}

#[cfg(test)]
mod tests {
    use super::relation_kind;
    use zsql_core::RelationKind;

    /// Pins the `pg_class.relkind` -> [`RelationKind`] mapping without a
    /// database: an ordinary table and a partitioned table both surface as
    /// `Table`, a view as `View`, a materialized view as `MatView`, and any
    /// other code (e.g. `i` for index, `S` for sequence) as `None` so
    /// `relations`/`columns` skip it instead of guessing at a kind.
    #[test]
    fn maps_every_relkind_code() {
        assert_eq!(relation_kind("r"), Some(RelationKind::Table));
        assert_eq!(relation_kind("p"), Some(RelationKind::Table));
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
}
