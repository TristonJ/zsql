//! Schema introspection: `sys.*` catalog views -> [`zsql_core::SchemaTree`].

use std::collections::HashMap;

use async_net::TcpStream;
use tiberius::Client;
use zsql_core::{Catalog, ColumnMeta, CoreError, Relation, RelationKind, SchemaNs, SchemaTree};

use crate::error::map_introspect_error;

/// Schemas every MSSQL database carries that are not user schemas: the
/// system catalog's own schema, the SQL-standard `INFORMATION_SCHEMA` view
/// layer, the `guest` user's schema, and the fixed database roles that
/// double as schema names. Shared by every query below that walks
/// `sys.schemas`.
const SYSTEM_SCHEMAS: &[&str] = &[
    "sys",
    "INFORMATION_SCHEMA",
    "guest",
    "db_owner",
    "db_accessadmin",
    "db_securityadmin",
    "db_ddladmin",
    "db_backupoperator",
    "db_datareader",
    "db_datawriter",
    "db_denydatareader",
    "db_denydatawriter",
];

/// Render [`SYSTEM_SCHEMAS`] as a SQL `IN (...)` literal list. Built purely
/// from the fixed constant above, never from anything read at runtime.
fn system_schemas_sql_list() -> String {
    SYSTEM_SCHEMAS
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build a full [`SchemaTree`] for the database `client` is connected to.
///
/// MSSQL has no equivalent of Postgres's materialized views, and detecting
/// table partitioning would need a further `sys.partitions` join this
/// introspection does not attempt, so every relation reports as
/// [`RelationKind::Table`] or [`RelationKind::View`].
///
/// # Errors
/// Returns [`CoreError::Introspection`] if any underlying query fails.
pub(crate) async fn introspect(client: &mut Client<TcpStream>) -> Result<SchemaTree, CoreError> {
    let catalog_name = current_database(client).await?;
    let schema_names = schema_names(client).await?;
    let mut relations_by_schema = relations(client).await?;
    let mut columns_by_relation = columns(client).await?;

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

/// Run `sql` (a query built entirely from fixed string fragments, never
/// runtime text) and collect its first result set.
pub(crate) async fn run(
    client: &mut Client<TcpStream>,
    sql: String,
) -> Result<Vec<tiberius::Row>, CoreError> {
    client
        .simple_query(sql)
        .await
        .map_err(map_introspect_error)?
        .into_first_result()
        .await
        .map_err(map_introspect_error)
}

/// A non-null text column, by name, from `row`.
pub(crate) fn text(row: &tiberius::Row, column: &str) -> Result<String, CoreError> {
    row.try_get::<&str, _>(column)
        .map_err(map_introspect_error)?
        .map(str::to_owned)
        .ok_or_else(|| {
            CoreError::introspection(format!("expected column '{column}' to be non-null"))
        })
}

/// A non-null boolean column, by name, from `row`.
fn boolean(row: &tiberius::Row, column: &str) -> Result<bool, CoreError> {
    row.try_get::<bool, _>(column)
        .map_err(map_introspect_error)?
        .ok_or_else(|| {
            CoreError::introspection(format!("expected column '{column}' to be non-null"))
        })
}

/// The name of the database the current connection is attached to.
async fn current_database(client: &mut Client<TcpStream>) -> Result<String, CoreError> {
    let rows = run(client, "SELECT DB_NAME() AS name".to_owned()).await?;
    let row = rows
        .first()
        .ok_or_else(|| CoreError::introspection("DB_NAME() returned no row".to_owned()))?;
    text(row, "name")
}

/// Every non-system schema, sorted by name.
async fn schema_names(client: &mut Client<TcpStream>) -> Result<Vec<String>, CoreError> {
    let sql = format!(
        "SELECT s.name AS name FROM sys.schemas s \
         WHERE s.name NOT IN ({}) ORDER BY s.name",
        system_schemas_sql_list()
    );
    let rows = run(client, sql).await?;
    rows.iter().map(|row| text(row, "name")).collect()
}

/// Every table and view across all non-system schemas, grouped by schema
/// name. Within each schema's list, relations are sorted by name.
async fn relations(
    client: &mut Client<TcpStream>,
) -> Result<HashMap<String, Vec<Relation>>, CoreError> {
    let system_schemas = system_schemas_sql_list();
    let sql = format!(
        "SELECT schema_name, relation_name, kind FROM ( \
             SELECT s.name AS schema_name, t.name AS relation_name, 'table' AS kind \
             FROM sys.tables t JOIN sys.schemas s ON s.schema_id = t.schema_id \
             WHERE s.name NOT IN ({system_schemas}) \
             UNION ALL \
             SELECT s.name, v.name, 'view' \
             FROM sys.views v JOIN sys.schemas s ON s.schema_id = v.schema_id \
             WHERE s.name NOT IN ({system_schemas}) \
         ) relations \
         ORDER BY schema_name, relation_name"
    );
    let rows = run(client, sql).await?;

    let mut grouped: HashMap<String, Vec<Relation>> = HashMap::new();
    for row in &rows {
        let schema = text(row, "schema_name")?;
        let name = text(row, "relation_name")?;
        let kind = text(row, "kind")?;
        let Some(kind) = relation_kind(&kind) else {
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

/// Map this module's own `'table'`/`'view'` tag (see [`relations`]) to the
/// neutral [`RelationKind`].
fn relation_kind(tag: &str) -> Option<RelationKind> {
    match tag {
        "table" => Some(RelationKind::Table),
        "view" => Some(RelationKind::View),
        _ => None,
    }
}

/// Every column of every table/view across all non-system schemas, grouped
/// by `(schema name, relation name)`. Within each group, columns are in
/// ordinal position.
async fn columns(
    client: &mut Client<TcpStream>,
) -> Result<HashMap<(String, String), Vec<ColumnMeta>>, CoreError> {
    let sql = format!(
        "SELECT s.name AS schema_name, o.name AS relation_name, c.name AS column_name, \
                ty.name AS type_name, c.is_nullable AS nullable \
         FROM sys.columns c \
         JOIN sys.objects o ON o.object_id = c.object_id \
         JOIN sys.schemas s ON s.schema_id = o.schema_id \
         JOIN sys.types ty ON ty.user_type_id = c.user_type_id \
         WHERE o.type IN ('U', 'V') AND s.name NOT IN ({}) \
         ORDER BY s.name, o.name, c.column_id",
        system_schemas_sql_list()
    );
    let rows = run(client, sql).await?;

    let mut grouped: HashMap<(String, String), Vec<ColumnMeta>> = HashMap::new();
    for row in &rows {
        let schema = text(row, "schema_name")?;
        let relation = text(row, "relation_name")?;
        let name = text(row, "column_name")?;
        let type_name = text(row, "type_name")?;
        let nullable = boolean(row, "nullable")?;
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
    use super::{SYSTEM_SCHEMAS, relation_kind, system_schemas_sql_list};
    use zsql_core::RelationKind;

    #[test]
    fn maps_the_relation_tags_this_module_produces() {
        assert_eq!(relation_kind("table"), Some(RelationKind::Table));
        assert_eq!(relation_kind("view"), Some(RelationKind::View));
        assert_eq!(
            relation_kind("index"),
            None,
            "unrecognized tags map to nothing"
        );
        assert_eq!(relation_kind(""), None);
    }

    #[test]
    fn system_schemas_list_quotes_every_entry() {
        let sql = system_schemas_sql_list();
        for name in SYSTEM_SCHEMAS {
            assert!(
                sql.contains(&format!("'{name}'")),
                "missing {name} in {sql}"
            );
        }
        assert_eq!(sql.matches(',').count(), SYSTEM_SCHEMAS.len() - 1);
    }
}
