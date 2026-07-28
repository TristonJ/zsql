//! Full per-relation structural detail via `SQLite`'s `pragma_table_info`,
//! `pragma_index_list`/`pragma_index_info`, and `pragma_foreign_key_list` --
//! [`zsql_core::RelationSchema`].
//!
//! `SQLite`'s introspection surface is narrower than Postgres's catalog:
//! there is no `CHECK`-constraint introspection available via any `PRAGMA`
//! (a `CHECK` clause lives only in the table's original `CREATE TABLE` SQL
//! text, which this module does not parse), so [`RelationSchema::constraints`]
//! here never contains a [`zsql_core::ConstraintKind::Check`] entry. `SQLite`
//! also has no index access-method concept -- every index is a B-tree, so
//! every [`zsql_core::IndexInfo::method`] this module reports is the fixed
//! string `"btree"` rather than something read from the database.

use std::collections::HashMap;

use sqlx::Row as _;
use sqlx::sqlite::SqlitePool;
use zsql_core::{
    ColumnDetail, ConstraintInfo, ConstraintKind, CoreError, ForeignKeyRef, IndexInfo,
    RelationSchema,
};
use zsql_sqlx::error::map_sqlx_introspect_error;

/// The access method every `SQLite` index reports, since `SQLite` has no
/// access-method concept of its own (see the module doc comment).
const SQLITE_INDEX_METHOD: &str = "btree";

/// Build a [`RelationSchema`] for `relation`. `schema` is accepted for
/// parity with [`zsql_core::Connection::describe_relation`]'s signature but
/// unused: `SQLite` introspection only ever sees the single `main` schema
/// (see `introspect.rs`'s `MAIN_SCHEMA_NAME`).
///
/// # Errors
/// Returns [`CoreError::Introspection`] if `relation` does not exist or any
/// underlying `PRAGMA` query fails.
pub(crate) async fn describe_relation(
    pool: &SqlitePool,
    schema: &str,
    relation: &str,
) -> Result<RelationSchema, CoreError> {
    let _ = schema;
    ensure_relation_exists(pool, relation).await?;

    let (mut columns, primary_key_columns) = columns(pool, relation).await?;
    let index_rows = index_list(pool, relation).await?;
    let unique_single_columns = unique_single_columns(pool, &index_rows).await?;
    let foreign_keys_by_column = foreign_keys(pool, relation).await?;

    for column in &mut columns {
        column.is_primary_key = primary_key_columns.contains(&column.name);
        column.is_unique = unique_single_columns.contains(&column.name);
        column.foreign_key = foreign_keys_by_column.get(&column.name).cloned();
    }

    let indexes = index_definitions(pool, &index_rows).await?;
    let constraints = constraints(
        relation,
        &primary_key_columns,
        &index_rows,
        &indexes,
        &foreign_keys_by_column,
    );

    Ok(RelationSchema {
        columns,
        indexes,
        constraints,
    })
}

/// Confirm `relation` exists as a table or view before running any
/// `PRAGMA` against it: unlike a catalog query, `pragma_table_info` for a
/// nonexistent name simply returns zero rows rather than erroring, which
/// would otherwise make a typo'd relation name look like a real, empty one.
async fn ensure_relation_exists(pool: &SqlitePool, relation: &str) -> Result<(), CoreError> {
    let row =
        sqlx::query("SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') AND name = ?")
            .bind(relation)
            .fetch_optional(pool)
            .await
            .map_err(map_sqlx_introspect_error)?;
    if row.is_none() {
        return Err(CoreError::introspection(format!(
            "relation not found: {relation}"
        )));
    }
    Ok(())
}

/// Every live column of `relation`, in ordinal position, plus the names of
/// its primary-key columns in primary-key-position order (`pk` column of
/// `pragma_table_info`, 1-based; `0` means "not part of the primary key").
async fn columns(
    pool: &SqlitePool,
    relation: &str,
) -> Result<(Vec<ColumnDetail>, Vec<String>), CoreError> {
    let rows = sqlx::query(
        r#"SELECT name, type, "notnull" AS not_null, dflt_value, pk
           FROM pragma_table_info(?) ORDER BY cid"#,
    )
    .bind(relation)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_introspect_error)?;

    let mut columns = Vec::with_capacity(rows.len());
    // `(pk position, column name)`, sorted by position below so a composite
    // primary key's columns come out in the order the key itself defines,
    // not `pragma_table_info`'s own `cid` (ordinal-position) order.
    let mut pk_positions: Vec<(i64, String)> = Vec::new();
    for row in &rows {
        let name: String = row.try_get("name").map_err(map_sqlx_introspect_error)?;
        let type_name: String = row.try_get("type").map_err(map_sqlx_introspect_error)?;
        let not_null: i64 = row.try_get("not_null").map_err(map_sqlx_introspect_error)?;
        let default: Option<String> = row
            .try_get("dflt_value")
            .map_err(map_sqlx_introspect_error)?;
        let pk: i64 = row.try_get("pk").map_err(map_sqlx_introspect_error)?;
        if pk > 0 {
            pk_positions.push((pk, name.clone()));
        }
        columns.push(ColumnDetail {
            name,
            type_name,
            nullable: not_null == 0,
            default,
            is_primary_key: false,
            is_unique: false,
            foreign_key: None,
        });
    }
    pk_positions.sort_by_key(|(position, _)| *position);
    let primary_key_columns = pk_positions.into_iter().map(|(_, name)| name).collect();
    Ok((columns, primary_key_columns))
}

/// One row of `pragma_index_list`.
struct IndexListRow {
    name: String,
    unique: bool,
    /// `"c"` (explicit `CREATE INDEX`), `"u"` (backs a `UNIQUE` constraint),
    /// or `"pk"` (backs a composite/non-rowid-alias primary key).
    origin: String,
}

/// Every index `pragma_index_list` reports for `relation`, including the
/// implicit indexes backing `UNIQUE`/`PRIMARY KEY` constraints.
async fn index_list(pool: &SqlitePool, relation: &str) -> Result<Vec<IndexListRow>, CoreError> {
    let rows =
        sqlx::query("SELECT name, \"unique\" AS is_unique, origin FROM pragma_index_list(?)")
            .bind(relation)
            .fetch_all(pool)
            .await
            .map_err(map_sqlx_introspect_error)?;

    let mut list = Vec::with_capacity(rows.len());
    for row in &rows {
        let name: String = row.try_get("name").map_err(map_sqlx_introspect_error)?;
        let is_unique: i64 = row
            .try_get("is_unique")
            .map_err(map_sqlx_introspect_error)?;
        let origin: String = row.try_get("origin").map_err(map_sqlx_introspect_error)?;
        list.push(IndexListRow {
            name,
            unique: is_unique != 0,
            origin,
        });
    }
    Ok(list)
}

/// The columns (in index-key order) of one named index, via
/// `pragma_index_info`.
async fn index_columns(pool: &SqlitePool, index_name: &str) -> Result<Vec<String>, CoreError> {
    let rows = sqlx::query("SELECT name FROM pragma_index_info(?) ORDER BY seqno")
        .bind(index_name)
        .fetch_all(pool)
        .await
        .map_err(map_sqlx_introspect_error)?;

    let mut columns = Vec::with_capacity(rows.len());
    for row in &rows {
        let name: String = row.try_get("name").map_err(map_sqlx_introspect_error)?;
        columns.push(name);
    }
    Ok(columns)
}

/// The set of column names that are the sole column of some explicit
/// (non-primary-key-backing) unique index. A composite unique index does
/// not mark any single column unique on its own.
async fn unique_single_columns(
    pool: &SqlitePool,
    index_rows: &[IndexListRow],
) -> Result<std::collections::HashSet<String>, CoreError> {
    let mut unique_columns = std::collections::HashSet::new();
    for index in index_rows {
        if !index.unique || index.origin == "pk" {
            continue;
        }
        let columns = index_columns(pool, &index.name).await?;
        if let [column] = columns.as_slice() {
            unique_columns.insert(column.clone());
        }
    }
    Ok(unique_columns)
}

/// Every foreign key `pragma_foreign_key_list` reports for `relation`,
/// mapped from each local column name to its target. `SQLite` reports one
/// row per column of a (possibly composite) foreign key, sharing an `id`;
/// rows are grouped by `id` here so a composite foreign key's
/// [`ForeignKeyRef::columns`] lists every referenced column together.
async fn foreign_keys(
    pool: &SqlitePool,
    relation: &str,
) -> Result<HashMap<String, ForeignKeyRef>, CoreError> {
    let rows = sqlx::query(
        r#"SELECT id, "table", "from", "to" FROM pragma_foreign_key_list(?) ORDER BY id, seq"#,
    )
    .bind(relation)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_introspect_error)?;

    // Grouped by `id`: `(referenced table, local columns, referenced columns)`.
    let mut groups: HashMap<i64, (String, Vec<String>, Vec<String>)> = HashMap::new();
    for row in &rows {
        let id: i64 = row.try_get("id").map_err(map_sqlx_introspect_error)?;
        let table: String = row.try_get("table").map_err(map_sqlx_introspect_error)?;
        let from: String = row.try_get("from").map_err(map_sqlx_introspect_error)?;
        let to: String = row.try_get("to").map_err(map_sqlx_introspect_error)?;
        let group = groups
            .entry(id)
            .or_insert_with(|| (table, Vec::new(), Vec::new()));
        group.1.push(from);
        group.2.push(to);
    }

    let mut by_column = HashMap::new();
    for (table, local_columns, ref_columns) in groups.into_values() {
        let target = ForeignKeyRef {
            // `SQLite` has one schema namespace (see the module doc
            // comment); the referenced table is always reported unqualified.
            schema: crate::introspect::MAIN_SCHEMA_NAME.to_owned(),
            table,
            columns: ref_columns,
        };
        for column in local_columns {
            by_column.insert(column, target.clone());
        }
    }
    Ok(by_column)
}

/// [`zsql_core::IndexInfo`] for every explicit (non-primary-key-backing)
/// index in `index_rows`; the implicit index backing a primary key is
/// represented instead as a `PRIMARY KEY` [`ConstraintInfo`] (see
/// [`constraints`]), matching how Postgres's own `describe_relation`
/// separates the two.
async fn index_definitions(
    pool: &SqlitePool,
    index_rows: &[IndexListRow],
) -> Result<Vec<IndexInfo>, CoreError> {
    let mut indexes = Vec::new();
    for index in index_rows {
        if index.origin == "pk" {
            continue;
        }
        let columns = index_columns(pool, &index.name).await?;
        let definition = format!("({})", columns.join(", "));
        indexes.push(IndexInfo {
            name: index.name.clone(),
            method: SQLITE_INDEX_METHOD.to_owned(),
            unique: index.unique,
            definition,
        });
    }
    indexes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(indexes)
}

/// Synthesize `PRIMARY KEY`, `UNIQUE`, and `FOREIGN KEY` constraint entries
/// from what has already been fetched (no further queries): `SQLite` names
/// primary/foreign keys only implicitly, so their [`ConstraintInfo::name`]
/// is synthesized from `relation`, not read from the database. No `CHECK`
/// entry is ever produced (see the module doc comment). Only an index whose
/// `origin` is `"u"` (the implicit index backing a table-level `UNIQUE`
/// constraint) becomes a `UNIQUE` constraint here; an explicit
/// `CREATE UNIQUE INDEX` (`origin` `"c"`) is not a constraint and is left to
/// `indexes` alone, so it is never listed in both tables.
fn constraints(
    relation: &str,
    primary_key_columns: &[String],
    index_rows: &[IndexListRow],
    indexes: &[IndexInfo],
    foreign_keys_by_column: &HashMap<String, ForeignKeyRef>,
) -> Vec<ConstraintInfo> {
    let mut constraints = Vec::new();

    if !primary_key_columns.is_empty() {
        constraints.push(ConstraintInfo {
            name: format!("{relation}_pk"),
            kind: ConstraintKind::PrimaryKey,
            definition: format!("PRIMARY KEY ({})", primary_key_columns.join(", ")),
        });
    }

    for index in index_rows {
        if !index.unique || index.origin != "u" {
            continue;
        }
        // `indexes` already carries this index's column list as
        // `(col1, col2)` (see `index_definitions`); reuse it rather than
        // re-querying `pragma_index_info`.
        let columns = indexes
            .iter()
            .find(|info| info.name == index.name)
            .map_or_else(String::new, |info| info.definition.clone());
        constraints.push(ConstraintInfo {
            name: index.name.clone(),
            kind: ConstraintKind::Unique,
            definition: format!("UNIQUE {columns}"),
        });
    }

    // Foreign keys are already grouped by target in `foreign_keys_by_column`
    // (one shared `ForeignKeyRef` per local column of a composite key), so
    // re-derive one constraint per distinct target rather than per column.
    let mut seen_targets: Vec<&ForeignKeyRef> = Vec::new();
    let mut local_columns_by_target: Vec<(&ForeignKeyRef, Vec<&str>)> = Vec::new();
    for (column, target) in foreign_keys_by_column {
        if let Some(entry) = local_columns_by_target
            .iter_mut()
            .find(|(existing, _)| *existing == target)
        {
            entry.1.push(column);
        } else {
            seen_targets.push(target);
            local_columns_by_target.push((target, vec![column]));
        }
    }
    for (target, mut local_columns) in local_columns_by_target {
        local_columns.sort_unstable();
        constraints.push(ConstraintInfo {
            name: format!("{relation}_{}_fkey", local_columns.join("_")),
            kind: ConstraintKind::ForeignKey,
            definition: format!(
                "FOREIGN KEY ({}) REFERENCES {}({})",
                local_columns.join(", "),
                target.table,
                target.columns.join(", ")
            ),
        });
    }

    constraints.sort_by(|a, b| a.name.cmp(&b.name));
    constraints
}

#[cfg(test)]
mod tests {
    use super::{ConstraintKind, ForeignKeyRef, IndexInfo, IndexListRow, constraints};

    #[test]
    fn constraints_synthesizes_a_primary_key_entry_when_columns_are_present() {
        let built = constraints(
            "orders",
            &["id".to_owned()],
            &[],
            &[],
            &std::collections::HashMap::new(),
        );
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].kind, ConstraintKind::PrimaryKey);
        assert_eq!(built[0].definition, "PRIMARY KEY (id)");
    }

    #[test]
    fn constraints_is_empty_for_a_relation_with_no_keys() {
        assert!(constraints("orders", &[], &[], &[], &std::collections::HashMap::new()).is_empty());
    }

    #[test]
    fn constraints_never_produces_a_check_entry() {
        let index_rows = [IndexListRow {
            name: "orders_code_idx".to_owned(),
            unique: true,
            origin: "u".to_owned(),
        }];
        let indexes = [IndexInfo {
            name: "orders_code_idx".to_owned(),
            method: "btree".to_owned(),
            unique: true,
            definition: "(code)".to_owned(),
        }];
        let mut fks = std::collections::HashMap::new();
        fks.insert(
            "user_id".to_owned(),
            ForeignKeyRef {
                schema: "main".to_owned(),
                table: "users".to_owned(),
                columns: vec!["id".to_owned()],
            },
        );
        let built = constraints("orders", &["id".to_owned()], &index_rows, &indexes, &fks);
        assert!(
            built.iter().all(|c| c.kind != ConstraintKind::Check),
            "sqlite describe_relation must never report a Check constraint"
        );
        assert_eq!(built.len(), 3, "primary key, unique index, and foreign key");
    }

    #[test]
    fn constraints_synthesizes_unique_only_for_a_constraint_backed_index_not_an_explicit_one() {
        let index_rows = [
            IndexListRow {
                name: "orders_pk_backed_unique".to_owned(),
                unique: true,
                origin: "u".to_owned(),
            },
            IndexListRow {
                name: "orders_explicit_unique_idx".to_owned(),
                unique: true,
                origin: "c".to_owned(),
            },
        ];
        let indexes = [
            IndexInfo {
                name: "orders_pk_backed_unique".to_owned(),
                method: "btree".to_owned(),
                unique: true,
                definition: "(code)".to_owned(),
            },
            IndexInfo {
                name: "orders_explicit_unique_idx".to_owned(),
                method: "btree".to_owned(),
                unique: true,
                definition: "(email)".to_owned(),
            },
        ];

        let built = constraints(
            "orders",
            &[],
            &index_rows,
            &indexes,
            &std::collections::HashMap::new(),
        );

        assert_eq!(
            built.len(),
            1,
            "an explicit CREATE UNIQUE INDEX must not also become a constraint"
        );
        assert_eq!(built[0].name, "orders_pk_backed_unique");
        assert_eq!(built[0].kind, ConstraintKind::Unique);
        assert_eq!(
            built[0].definition, "UNIQUE (code)",
            "the synthesized UNIQUE definition must be valid constraint DDL, not an index label"
        );
    }
}
