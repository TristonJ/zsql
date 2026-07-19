//! Full per-relation structural detail: `pg_catalog` ->
//! [`zsql_core::RelationSchema`]. Every query below binds `schema`/`relation`
//! (or, once resolved, the relation's own `oid`) as a parameter -- none of
//! it is ever string-interpolated into SQL text.

use std::collections::{HashMap, HashSet};

use sqlx::Row as _;
use sqlx::postgres::PgPool;
use sqlx::postgres::types::Oid;
use zsql_core::{
    ColumnDetail, ConstraintInfo, ConstraintKind, CoreError, ForeignKeyRef, IndexInfo,
    RelationSchema,
};

use crate::error::map_introspect_error;

/// Build a [`RelationSchema`] for `schema.relation`.
///
/// # Errors
/// Returns [`CoreError::Introspection`] if `schema.relation` does not exist
/// or any underlying catalog query fails.
pub(crate) async fn describe_relation(
    pool: &PgPool,
    schema: &str,
    relation: &str,
) -> Result<RelationSchema, CoreError> {
    let oid = relation_oid(pool, schema, relation).await?;

    let mut columns = columns(pool, oid).await?;
    let (primary_key_attnums, unique_attnums) = key_flag_attnums(pool, oid).await?;
    let foreign_keys_by_attnum = foreign_keys(pool, oid).await?;

    for (attnum, column) in &mut columns {
        column.is_primary_key = primary_key_attnums.contains(attnum);
        column.is_unique = unique_attnums.contains(attnum);
        column.foreign_key = foreign_keys_by_attnum.get(attnum).cloned();
    }

    let indexes = indexes(pool, oid).await?;
    let constraints = constraints(pool, oid).await?;

    Ok(RelationSchema {
        columns: columns.into_iter().map(|(_, column)| column).collect(),
        indexes,
        constraints,
    })
}

/// Resolve `schema.relation` to its `pg_class` oid.
///
/// # Errors
/// Returns [`CoreError::Introspection`] if no such relation exists, or the
/// lookup query fails.
async fn relation_oid(pool: &PgPool, schema: &str, relation: &str) -> Result<Oid, CoreError> {
    let row = sqlx::query(
        "SELECT c.oid \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r', 'p', 'v', 'm')",
    )
    .bind(schema)
    .bind(relation)
    .fetch_optional(pool)
    .await
    .map_err(map_introspect_error)?;

    let Some(row) = row else {
        return Err(CoreError::Introspection(format!(
            "relation not found: {schema}.{relation}"
        )));
    };
    row.try_get("oid").map_err(map_introspect_error)
}

/// Every live column of `oid`, in ordinal position, paired with its
/// `attnum` (used by the caller to fold in key/foreign-key flags looked up
/// separately). Defaults are rendered as backend-native SQL text via
/// `pg_get_expr`; `None` for a column with no default.
async fn columns(pool: &PgPool, oid: Oid) -> Result<Vec<(i16, ColumnDetail)>, CoreError> {
    let rows = sqlx::query(
        "SELECT a.attnum, a.attname, \
                pg_catalog.format_type(a.atttypid, a.atttypmod) AS type_name, \
                NOT a.attnotnull AS nullable, \
                pg_catalog.pg_get_expr(ad.adbin, ad.adrelid) AS default_expr \
         FROM pg_catalog.pg_attribute a \
         LEFT JOIN pg_catalog.pg_attrdef ad \
           ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum \
         WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped \
         ORDER BY a.attnum",
    )
    .bind(oid)
    .fetch_all(pool)
    .await
    .map_err(map_introspect_error)?;

    let mut columns = Vec::with_capacity(rows.len());
    for row in &rows {
        let attnum: i16 = row.try_get("attnum").map_err(map_introspect_error)?;
        let name: String = row.try_get("attname").map_err(map_introspect_error)?;
        let type_name: String = row.try_get("type_name").map_err(map_introspect_error)?;
        let nullable: bool = row.try_get("nullable").map_err(map_introspect_error)?;
        let default: Option<String> = row.try_get("default_expr").map_err(map_introspect_error)?;
        columns.push((
            attnum,
            ColumnDetail {
                name,
                type_name,
                nullable,
                default,
                is_primary_key: false,
                is_unique: false,
                foreign_key: None,
            },
        ));
    }
    Ok(columns)
}

/// Every column `attnum` that is (part of) `oid`'s primary key, and every
/// `attnum` that is the sole column of some other unique index (a composite
/// unique index does not mark any single column unique on its own).
async fn key_flag_attnums(
    pool: &PgPool,
    oid: Oid,
) -> Result<(HashSet<i16>, HashSet<i16>), CoreError> {
    // `pg_index.indkey` is Postgres's special `int2vector` pseudo-type, whose
    // 0-based lower bound survives a plain `::int2[]` cast; sqlx's array
    // decoder rejects any array not starting at index 1. `unnest` + `array()`
    // rebuilds a standard 1-based `int2[]` from its elements instead.
    let rows = sqlx::query(
        "SELECT i.indisprimary AS is_primary, i.indisunique AS is_unique, \
                ARRAY(SELECT unnest(i.indkey)) AS indkey \
         FROM pg_catalog.pg_index i \
         WHERE i.indrelid = $1",
    )
    .bind(oid)
    .fetch_all(pool)
    .await
    .map_err(map_introspect_error)?;

    let mut primary_key_attnums = HashSet::new();
    let mut unique_attnums = HashSet::new();
    for row in &rows {
        let is_primary: bool = row.try_get("is_primary").map_err(map_introspect_error)?;
        let is_unique: bool = row.try_get("is_unique").map_err(map_introspect_error)?;
        let indkey: Vec<i16> = row.try_get("indkey").map_err(map_introspect_error)?;
        if is_primary {
            primary_key_attnums.extend(indkey);
        } else if is_unique && let [attnum] = indkey[..] {
            unique_attnums.insert(attnum);
        }
    }
    Ok((primary_key_attnums, unique_attnums))
}

/// Every local column `attnum` of `oid` that participates in a foreign key,
/// mapped to that foreign key's target. A multi-column foreign key attaches
/// the same [`ForeignKeyRef`] (listing every referenced column, in the
/// constraint's own order) to each of its local columns.
async fn foreign_keys(pool: &PgPool, oid: Oid) -> Result<HashMap<i16, ForeignKeyRef>, CoreError> {
    let rows = sqlx::query(
        "SELECT c.conkey AS conkey, c.confkey AS confkey, c.confrelid AS ref_oid, \
                rn.nspname AS ref_schema, rc.relname AS ref_table \
         FROM pg_catalog.pg_constraint c \
         JOIN pg_catalog.pg_class rc ON rc.oid = c.confrelid \
         JOIN pg_catalog.pg_namespace rn ON rn.oid = rc.relnamespace \
         WHERE c.conrelid = $1 AND c.contype = 'f'",
    )
    .bind(oid)
    .fetch_all(pool)
    .await
    .map_err(map_introspect_error)?;

    let mut by_attnum = HashMap::new();
    for row in &rows {
        let local_attnums: Vec<i16> = row.try_get("conkey").map_err(map_introspect_error)?;
        let ref_attnums: Vec<i16> = row.try_get("confkey").map_err(map_introspect_error)?;
        let ref_oid: Oid = row.try_get("ref_oid").map_err(map_introspect_error)?;
        let ref_schema: String = row.try_get("ref_schema").map_err(map_introspect_error)?;
        let ref_table: String = row.try_get("ref_table").map_err(map_introspect_error)?;

        let ref_column_names = attribute_names(pool, ref_oid, &ref_attnums).await?;
        let columns = ref_attnums
            .iter()
            .filter_map(|attnum| ref_column_names.get(attnum).cloned())
            .collect();
        let target = ForeignKeyRef {
            schema: ref_schema,
            table: ref_table,
            columns,
        };
        for attnum in local_attnums {
            by_attnum.insert(attnum, target.clone());
        }
    }
    Ok(by_attnum)
}

/// Resolve every `attnum` in `attnums` to its column name on relation `oid`.
async fn attribute_names(
    pool: &PgPool,
    oid: Oid,
    attnums: &[i16],
) -> Result<HashMap<i16, String>, CoreError> {
    let rows = sqlx::query(
        "SELECT a.attnum, a.attname \
         FROM pg_catalog.pg_attribute a \
         WHERE a.attrelid = $1 AND a.attnum = ANY($2)",
    )
    .bind(oid)
    .bind(attnums)
    .fetch_all(pool)
    .await
    .map_err(map_introspect_error)?;

    let mut names = HashMap::with_capacity(rows.len());
    for row in &rows {
        let attnum: i16 = row.try_get("attnum").map_err(map_introspect_error)?;
        let name: String = row.try_get("attname").map_err(map_introspect_error)?;
        names.insert(attnum, name);
    }
    Ok(names)
}

/// Every index on `oid`: name, access method, uniqueness, and its full
/// `CREATE INDEX` definition text.
async fn indexes(pool: &PgPool, oid: Oid) -> Result<Vec<IndexInfo>, CoreError> {
    let rows = sqlx::query(
        "SELECT ic.relname AS name, am.amname AS method, i.indisunique AS is_unique, \
                pg_catalog.pg_get_indexdef(i.indexrelid) AS definition \
         FROM pg_catalog.pg_index i \
         JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid \
         JOIN pg_catalog.pg_am am ON am.oid = ic.relam \
         WHERE i.indrelid = $1 \
         ORDER BY ic.relname",
    )
    .bind(oid)
    .fetch_all(pool)
    .await
    .map_err(map_introspect_error)?;

    let mut indexes = Vec::with_capacity(rows.len());
    for row in &rows {
        let name: String = row.try_get("name").map_err(map_introspect_error)?;
        let method: String = row.try_get("method").map_err(map_introspect_error)?;
        let unique: bool = row.try_get("is_unique").map_err(map_introspect_error)?;
        let definition: String = row.try_get("definition").map_err(map_introspect_error)?;
        indexes.push(IndexInfo {
            name,
            method,
            unique,
            definition,
        });
    }
    Ok(indexes)
}

/// Every constraint on `oid` whose kind maps to [`ConstraintKind`] (primary
/// key, foreign key, unique, or check); any other constraint kind Postgres
/// may report (e.g. an exclusion constraint) is skipped.
async fn constraints(pool: &PgPool, oid: Oid) -> Result<Vec<ConstraintInfo>, CoreError> {
    let rows = sqlx::query(
        "SELECT conname, contype::text AS contype, \
                pg_catalog.pg_get_constraintdef(oid) AS definition \
         FROM pg_catalog.pg_constraint \
         WHERE conrelid = $1 \
         ORDER BY conname",
    )
    .bind(oid)
    .fetch_all(pool)
    .await
    .map_err(map_introspect_error)?;

    let mut constraints = Vec::with_capacity(rows.len());
    for row in &rows {
        let contype: String = row.try_get("contype").map_err(map_introspect_error)?;
        let Some(kind) = constraint_kind(&contype) else {
            continue;
        };
        let name: String = row.try_get("conname").map_err(map_introspect_error)?;
        let definition: String = row.try_get("definition").map_err(map_introspect_error)?;
        constraints.push(ConstraintInfo {
            name,
            kind,
            definition,
        });
    }
    Ok(constraints)
}

/// Map a `pg_constraint.contype` code to the neutral [`ConstraintKind`].
fn constraint_kind(contype: &str) -> Option<ConstraintKind> {
    match contype {
        "p" => Some(ConstraintKind::PrimaryKey),
        "f" => Some(ConstraintKind::ForeignKey),
        "u" => Some(ConstraintKind::Unique),
        "c" => Some(ConstraintKind::Check),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::constraint_kind;
    use zsql_core::ConstraintKind;

    #[test]
    fn maps_every_constraint_kind_code_this_module_cares_about() {
        assert_eq!(constraint_kind("p"), Some(ConstraintKind::PrimaryKey));
        assert_eq!(constraint_kind("f"), Some(ConstraintKind::ForeignKey));
        assert_eq!(constraint_kind("u"), Some(ConstraintKind::Unique));
        assert_eq!(constraint_kind("c"), Some(ConstraintKind::Check));
        assert_eq!(
            constraint_kind("x"),
            None,
            "an exclusion constraint has no ConstraintKind mapping"
        );
        assert_eq!(constraint_kind(""), None, "empty code maps to nothing");
    }
}
