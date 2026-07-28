//! Full per-relation structural detail: `information_schema` ->
//! [`zsql_core::RelationSchema`]. Every query below binds `schema`/`relation`
//! as a parameter -- none of it is ever string-interpolated into SQL text.

use std::collections::{HashMap, HashSet};

use sqlx::Row as _;
use sqlx::mysql::MySqlPool;
use zsql_core::{
    ColumnDetail, ConstraintInfo, ConstraintKind, CoreError, ForeignKeyRef, IndexInfo,
    RelationSchema,
};
use zsql_sqlx::error::map_sqlx_introspect_error;

/// MySQL/MariaDB's fixed name for the index (and constraint) backing a
/// table's primary key. Unlike every other index or constraint name, this
/// one is never chosen by the schema author -- both engines always call it
/// exactly this.
const PRIMARY_KEY_NAME: &str = "PRIMARY";

/// Build a [`RelationSchema`] for `schema.relation`.
///
/// # Errors
/// Returns [`CoreError::Introspection`] if `schema.relation` does not exist
/// or any underlying catalog query fails.
pub(crate) async fn describe_relation(
    pool: &MySqlPool,
    schema: &str,
    relation: &str,
) -> Result<RelationSchema, CoreError> {
    ensure_relation_exists(pool, schema, relation).await?;

    let mut columns = columns(pool, schema, relation).await?;
    let indexes_raw = raw_indexes(pool, schema, relation).await?;
    let foreign_keys_raw = raw_foreign_keys(pool, schema, relation).await?;

    // Owned, not borrowed from `indexes_raw`: `indexes_raw` itself is moved
    // into `indexes` below, and these two sets are still needed afterward
    // to build the primary-key `ConstraintInfo`.
    let primary_key_columns: HashSet<String> = indexes_raw
        .iter()
        .find(|idx| idx.name == PRIMARY_KEY_NAME)
        .map(|idx| idx.columns.iter().cloned().collect())
        .unwrap_or_default();
    let unique_columns: HashSet<String> = indexes_raw
        .iter()
        .filter(|idx| idx.unique && idx.name != PRIMARY_KEY_NAME && idx.columns.len() == 1)
        .filter_map(|idx| idx.columns.first().cloned())
        .collect();
    let foreign_keys_by_column = foreign_keys_by_local_column(&foreign_keys_raw);

    for column in &mut columns {
        column.is_primary_key = primary_key_columns.contains(column.name.as_str());
        column.is_unique = unique_columns.contains(column.name.as_str());
        column.foreign_key = foreign_keys_by_column.get(column.name.as_str()).cloned();
    }

    let indexes = indexes_raw
        .into_iter()
        .map(RawIndex::into_index_info)
        .collect();

    let mut constraints = Vec::new();
    if !primary_key_columns.is_empty() {
        let mut pk_columns: Vec<&str> = primary_key_columns.iter().map(String::as_str).collect();
        pk_columns.sort_unstable();
        constraints.push(ConstraintInfo {
            name: PRIMARY_KEY_NAME.to_owned(),
            kind: ConstraintKind::PrimaryKey,
            definition: format!("PRIMARY KEY ({})", pk_columns.join(", ")),
        });
    }
    constraints.extend(
        foreign_keys_raw
            .into_iter()
            .map(RawForeignKey::into_constraint_info),
    );
    constraints.extend(unique_constraints(pool, schema, relation).await?);
    constraints.extend(check_constraints(pool, schema, relation).await?);
    constraints.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(RelationSchema {
        columns,
        indexes,
        constraints,
    })
}

/// Confirm `schema.relation` exists as a table or view before the rest of
/// `describe_relation` runs, so a nonexistent relation reports a clear
/// "not found" error instead of an empty-but-successful [`RelationSchema`].
async fn ensure_relation_exists(
    pool: &MySqlPool,
    schema: &str,
    relation: &str,
) -> Result<(), CoreError> {
    let row = sqlx::query(
        "SELECT 1 FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND TABLE_TYPE IN ('BASE TABLE', 'VIEW')",
    )
    .bind(schema)
    .bind(relation)
    .fetch_optional(pool)
    .await
    .map_err(map_sqlx_introspect_error)?;

    if row.is_none() {
        return Err(CoreError::introspection(format!(
            "relation not found: {schema}.{relation}"
        )));
    }
    Ok(())
}

/// Every live column of `schema.relation`, in ordinal position. Defaults are
/// rendered as backend-native SQL text via `COLUMN_DEFAULT`; `None` for a
/// column with no default.
async fn columns(
    pool: &MySqlPool,
    schema: &str,
    relation: &str,
) -> Result<Vec<ColumnDetail>, CoreError> {
    let rows = sqlx::query(
        "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT \
         FROM information_schema.COLUMNS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
         ORDER BY ORDINAL_POSITION",
    )
    .bind(schema)
    .bind(relation)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_introspect_error)?;

    let mut columns = Vec::with_capacity(rows.len());
    for row in &rows {
        let name: String = row
            .try_get("COLUMN_NAME")
            .map_err(map_sqlx_introspect_error)?;
        let type_name: String = row
            .try_get("COLUMN_TYPE")
            .map_err(map_sqlx_introspect_error)?;
        let is_nullable: String = row
            .try_get("IS_NULLABLE")
            .map_err(map_sqlx_introspect_error)?;
        let default: Option<String> = row
            .try_get("COLUMN_DEFAULT")
            .map_err(map_sqlx_introspect_error)?;
        columns.push(ColumnDetail {
            name,
            type_name,
            nullable: is_nullable.eq_ignore_ascii_case("YES"),
            default,
            is_primary_key: false,
            is_unique: false,
            foreign_key: None,
        });
    }
    Ok(columns)
}

/// One index's raw shape as read from `information_schema.STATISTICS`,
/// before it is split into an [`IndexInfo`] plus the primary-key/unique
/// column sets [`describe_relation`] derives from it.
struct RawIndex {
    name: String,
    method: String,
    unique: bool,
    columns: Vec<String>,
}

impl RawIndex {
    fn into_index_info(self) -> IndexInfo {
        IndexInfo {
            name: self.name,
            method: self.method,
            unique: self.unique,
            definition: format!("({})", self.columns.join(", ")),
        }
    }
}

/// Every index on `schema.relation`, each with its columns in index-key
/// order. MySQL/MariaDB name a table's primary-key index literally
/// `"PRIMARY"`, so this single query -- grouped in Rust rather than with
/// `GROUP_CONCAT`, to keep column ordering exact and avoid its
/// engine-specific length cap -- is also how [`describe_relation`] learns
/// which columns are primary-key/unique, without a second round trip.
async fn raw_indexes(
    pool: &MySqlPool,
    schema: &str,
    relation: &str,
) -> Result<Vec<RawIndex>, CoreError> {
    let rows = sqlx::query(
        "SELECT INDEX_NAME, NON_UNIQUE, INDEX_TYPE, COLUMN_NAME \
         FROM information_schema.STATISTICS \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? \
         ORDER BY INDEX_NAME, SEQ_IN_INDEX",
    )
    .bind(schema)
    .bind(relation)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_introspect_error)?;

    let mut indexes: Vec<RawIndex> = Vec::new();
    for row in &rows {
        let name: String = row
            .try_get("INDEX_NAME")
            .map_err(map_sqlx_introspect_error)?;
        let non_unique: i64 = row
            .try_get("NON_UNIQUE")
            .map_err(map_sqlx_introspect_error)?;
        let method: String = row
            .try_get("INDEX_TYPE")
            .map_err(map_sqlx_introspect_error)?;
        let column: String = row
            .try_get("COLUMN_NAME")
            .map_err(map_sqlx_introspect_error)?;

        match indexes.last_mut() {
            Some(last) if last.name == name => last.columns.push(column),
            _ => indexes.push(RawIndex {
                name,
                method: method.to_lowercase(),
                unique: non_unique == 0,
                columns: vec![column],
            }),
        }
    }
    Ok(indexes)
}

/// One foreign key's raw shape as read from `information_schema`, before it
/// is split into per-column attachments (on [`ColumnDetail::foreign_key`])
/// plus its own [`ConstraintInfo`] entry.
struct RawForeignKey {
    constraint_name: String,
    local_columns: Vec<String>,
    target: ForeignKeyRef,
}

impl RawForeignKey {
    fn into_constraint_info(self) -> ConstraintInfo {
        ConstraintInfo {
            name: self.constraint_name,
            kind: ConstraintKind::ForeignKey,
            definition: format!(
                "FOREIGN KEY ({}) REFERENCES {}.{}({})",
                self.local_columns.join(", "),
                self.target.schema,
                self.target.table,
                self.target.columns.join(", ")
            ),
        }
    }
}

/// Every foreign key defined on `schema.relation`, one entry per named
/// constraint (a multi-column foreign key stays one entry, with every local
/// and referenced column in the constraint's own order).
async fn raw_foreign_keys(
    pool: &MySqlPool,
    schema: &str,
    relation: &str,
) -> Result<Vec<RawForeignKey>, CoreError> {
    let rows = sqlx::query(
        "SELECT CONSTRAINT_NAME, COLUMN_NAME, REFERENCED_TABLE_SCHEMA, \
                REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
         FROM information_schema.KEY_COLUMN_USAGE \
         WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ? AND REFERENCED_TABLE_NAME IS NOT NULL \
         ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION",
    )
    .bind(schema)
    .bind(relation)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_introspect_error)?;

    let mut foreign_keys: Vec<RawForeignKey> = Vec::new();
    for row in &rows {
        let constraint_name: String = row
            .try_get("CONSTRAINT_NAME")
            .map_err(map_sqlx_introspect_error)?;
        let local_column: String = row
            .try_get("COLUMN_NAME")
            .map_err(map_sqlx_introspect_error)?;
        let ref_schema: String = row
            .try_get("REFERENCED_TABLE_SCHEMA")
            .map_err(map_sqlx_introspect_error)?;
        let ref_table: String = row
            .try_get("REFERENCED_TABLE_NAME")
            .map_err(map_sqlx_introspect_error)?;
        let ref_column: String = row
            .try_get("REFERENCED_COLUMN_NAME")
            .map_err(map_sqlx_introspect_error)?;

        match foreign_keys.last_mut() {
            Some(last) if last.constraint_name == constraint_name => {
                last.local_columns.push(local_column);
                last.target.columns.push(ref_column);
            }
            _ => foreign_keys.push(RawForeignKey {
                constraint_name,
                local_columns: vec![local_column],
                target: ForeignKeyRef {
                    schema: ref_schema,
                    table: ref_table,
                    columns: vec![ref_column],
                },
            }),
        }
    }
    Ok(foreign_keys)
}

/// Flatten [`raw_foreign_keys`]'s per-constraint list into a per-local-column
/// map, the shape [`describe_relation`] needs to attach a foreign key to
/// each [`ColumnDetail`] that carries one.
fn foreign_keys_by_local_column(foreign_keys: &[RawForeignKey]) -> HashMap<&str, ForeignKeyRef> {
    let mut by_column = HashMap::new();
    for fk in foreign_keys {
        for local_column in &fk.local_columns {
            by_column.insert(local_column.as_str(), fk.target.clone());
        }
    }
    by_column
}

/// Every named `UNIQUE` table constraint on `schema.relation`, in
/// declaration-name order.
async fn unique_constraints(
    pool: &MySqlPool,
    schema: &str,
    relation: &str,
) -> Result<Vec<ConstraintInfo>, CoreError> {
    let rows = sqlx::query(
        "SELECT tc.CONSTRAINT_NAME, kcu.COLUMN_NAME \
         FROM information_schema.TABLE_CONSTRAINTS tc \
         JOIN information_schema.KEY_COLUMN_USAGE kcu \
           ON kcu.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA \
          AND kcu.CONSTRAINT_NAME = tc.CONSTRAINT_NAME \
          AND kcu.TABLE_SCHEMA = tc.TABLE_SCHEMA \
          AND kcu.TABLE_NAME = tc.TABLE_NAME \
         WHERE tc.TABLE_SCHEMA = ? AND tc.TABLE_NAME = ? AND tc.CONSTRAINT_TYPE = 'UNIQUE' \
         ORDER BY tc.CONSTRAINT_NAME, kcu.ORDINAL_POSITION",
    )
    .bind(schema)
    .bind(relation)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_introspect_error)?;

    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for row in &rows {
        let name: String = row
            .try_get("CONSTRAINT_NAME")
            .map_err(map_sqlx_introspect_error)?;
        let column: String = row
            .try_get("COLUMN_NAME")
            .map_err(map_sqlx_introspect_error)?;
        match grouped.last_mut() {
            Some((last_name, columns)) if *last_name == name => columns.push(column),
            _ => grouped.push((name, vec![column])),
        }
    }

    Ok(grouped
        .into_iter()
        .map(|(name, columns)| ConstraintInfo {
            name,
            kind: ConstraintKind::Unique,
            definition: format!("UNIQUE ({})", columns.join(", ")),
        })
        .collect())
}

/// Every named `CHECK` table constraint on `schema.relation`. Joined through
/// `TABLE_CONSTRAINTS` (present, with the same shape, on both `MySQL` and
/// `MariaDB`) rather than selecting directly from `CHECK_CONSTRAINTS`, whose
/// own column set differs slightly between the two engines.
async fn check_constraints(
    pool: &MySqlPool,
    schema: &str,
    relation: &str,
) -> Result<Vec<ConstraintInfo>, CoreError> {
    let rows = sqlx::query(
        "SELECT tc.CONSTRAINT_NAME, cc.CHECK_CLAUSE \
         FROM information_schema.TABLE_CONSTRAINTS tc \
         JOIN information_schema.CHECK_CONSTRAINTS cc \
           ON cc.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA \
          AND cc.CONSTRAINT_NAME = tc.CONSTRAINT_NAME \
         WHERE tc.TABLE_SCHEMA = ? AND tc.TABLE_NAME = ? AND tc.CONSTRAINT_TYPE = 'CHECK' \
         ORDER BY tc.CONSTRAINT_NAME",
    )
    .bind(schema)
    .bind(relation)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_introspect_error)?;

    let mut constraints = Vec::with_capacity(rows.len());
    for row in &rows {
        let name: String = row
            .try_get("CONSTRAINT_NAME")
            .map_err(map_sqlx_introspect_error)?;
        let check_clause: String = row
            .try_get("CHECK_CLAUSE")
            .map_err(map_sqlx_introspect_error)?;
        constraints.push(ConstraintInfo {
            name,
            kind: ConstraintKind::Check,
            definition: format!("CHECK ({check_clause})"),
        });
    }
    Ok(constraints)
}
