//! Full per-relation structural detail: `sys.*` catalog views ->
//! [`zsql_core::RelationSchema`]. Every query below binds `schema`/
//! `relation` (or, once resolved, the relation's own `object_id`) as a
//! parameter via `@P1`/`@P2` -- none of it is ever string-interpolated into
//! SQL text.

use std::collections::{HashMap, HashSet};

use async_net::TcpStream;
use tiberius::Client;
use zsql_core::{
    ColumnDetail, ConstraintInfo, ConstraintKind, CoreError, ForeignKeyRef, IndexInfo,
    RelationSchema,
};

use crate::error::map_introspect_error;

/// `sys.key_constraints.type` code for a primary key constraint.
const CONSTRAINT_TYPE_PRIMARY_KEY: &str = "PK";
/// `sys.key_constraints.type` code for a unique constraint.
const CONSTRAINT_TYPE_UNIQUE: &str = "UQ";
/// `sys.objects.type` code for a foreign key constraint.
const CONSTRAINT_TYPE_FOREIGN_KEY: &str = "F";
/// `sys.objects.type` code for a check constraint.
const CONSTRAINT_TYPE_CHECK: &str = "C";

/// Definition-text keyword for a primary key constraint.
const PRIMARY_KEY_KEYWORD: &str = "PRIMARY KEY";
/// Definition-text keyword for a unique constraint.
const UNIQUE_KEYWORD: &str = "UNIQUE";

/// `sys.columns.max_length` types whose length is meaningful and belongs in
/// a readable `type_name` (e.g. `nvarchar(255)`).
const LENGTH_CARRYING_TYPES: &[&str] = &[
    "char",
    "varchar",
    "nchar",
    "nvarchar",
    "binary",
    "varbinary",
];
/// Of [`LENGTH_CARRYING_TYPES`], the ones whose `max_length` is a byte count
/// twice the character count (`n`-prefixed, UTF-16), so it must be halved to
/// report the length a user would actually type in DDL.
const WIDE_CHAR_TYPES: &[&str] = &["nchar", "nvarchar"];
/// Types whose `precision`/`scale` are meaningful and belong in a readable
/// `type_name` (e.g. `decimal(10,2)`).
const PRECISION_SCALE_TYPES: &[&str] = &["decimal", "numeric"];
/// The `sys.columns.max_length` value SQL Server reports for a `(max)`
/// length type (`varchar(max)`, `nvarchar(max)`, `varbinary(max)`).
const MAX_LENGTH_SENTINEL: i16 = -1;

/// Build a [`RelationSchema`] for `schema.relation`.
///
/// # Errors
/// Returns [`CoreError::Introspection`] if `schema.relation` does not exist
/// or any underlying catalog query fails.
pub(crate) async fn describe_relation(
    client: &mut Client<TcpStream>,
    schema: &str,
    relation: &str,
) -> Result<RelationSchema, CoreError> {
    let object_id = relation_object_id(client, schema, relation).await?;

    let mut columns = columns(client, object_id).await?;
    let (primary_key_column_ids, unique_column_ids) =
        key_flag_column_ids(client, object_id).await?;
    let foreign_key_groups = foreign_key_groups(client, object_id).await?;
    let foreign_keys_by_column_id = foreign_keys_by_column_id(&foreign_key_groups);

    for (column_id, column) in &mut columns {
        column.is_primary_key = primary_key_column_ids.contains(column_id);
        column.is_unique = unique_column_ids.contains(column_id);
        column.foreign_key = foreign_keys_by_column_id.get(column_id).cloned();
    }

    let index_key_columns = index_key_columns_by_index_id(client, object_id).await?;
    let indexes = indexes(client, object_id, &index_key_columns).await?;

    let mut constraints = key_constraints(client, object_id, &index_key_columns).await?;
    constraints.extend(foreign_key_constraints(&foreign_key_groups));
    constraints.extend(check_constraints(client, object_id).await?);
    constraints.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(RelationSchema {
        columns: columns.into_iter().map(|(_, column)| column).collect(),
        indexes,
        constraints,
    })
}

/// Resolve `schema.relation` to its `sys.objects.object_id`, accepting only
/// a table or view (matching what this module's queries below assume).
///
/// # Errors
/// Returns [`CoreError::Introspection`] if no such relation exists, or the
/// lookup query fails.
async fn relation_object_id(
    client: &mut Client<TcpStream>,
    schema: &str,
    relation: &str,
) -> Result<i32, CoreError> {
    let sql = "SELECT o.object_id AS object_id \
               FROM sys.objects o \
               JOIN sys.schemas s ON s.schema_id = o.schema_id \
               WHERE s.name = @P1 AND o.name = @P2 AND o.type IN ('U', 'V')";
    let row = client
        .query(sql, &[&schema, &relation])
        .await
        .map_err(map_introspect_error)?
        .into_row()
        .await
        .map_err(map_introspect_error)?;
    let Some(row) = row else {
        return Err(CoreError::introspection(format!(
            "relation not found: {schema}.{relation}"
        )));
    };
    int32(&row, "object_id")
}

/// Every live column of `object_id`, in ordinal position, paired with its
/// `column_id` (used by the caller to fold in key/foreign-key flags looked
/// up separately). Defaults are rendered as SQL Server's own default
/// constraint text; `None` for a column with no default.
async fn columns(
    client: &mut Client<TcpStream>,
    object_id: i32,
) -> Result<Vec<(i32, ColumnDetail)>, CoreError> {
    let sql = "SELECT c.column_id AS column_id, c.name AS column_name, \
                      c.is_nullable AS nullable, ty.name AS type_name, \
                      c.max_length AS max_length, c.precision AS precision, c.scale AS scale, \
                      dc.definition AS default_definition \
               FROM sys.columns c \
               JOIN sys.types ty ON ty.user_type_id = c.user_type_id \
               LEFT JOIN sys.default_constraints dc \
                 ON dc.parent_object_id = c.object_id AND dc.parent_column_id = c.column_id \
               WHERE c.object_id = @P1 \
               ORDER BY c.column_id";
    let rows = client
        .query(sql, &[&object_id])
        .await
        .map_err(map_introspect_error)?
        .into_first_result()
        .await
        .map_err(map_introspect_error)?;

    let mut columns = Vec::with_capacity(rows.len());
    for row in &rows {
        let column_id = int32(row, "column_id")?;
        let name = text(row, "column_name")?;
        let nullable = boolean(row, "nullable")?;
        let base_type = text(row, "type_name")?;
        let max_length = int16(row, "max_length")?;
        let precision = tiny_uint(row, "precision")?;
        let scale = tiny_uint(row, "scale")?;
        let default = opt_text(row, "default_definition")?;
        columns.push((
            column_id,
            ColumnDetail {
                name,
                type_name: format_type_name(&base_type, max_length, precision, scale),
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

/// Render a readable `type_name`, folding in length or precision/scale for
/// the types that carry one (see [`LENGTH_CARRYING_TYPES`] and
/// [`PRECISION_SCALE_TYPES`]); every other type is reported bare.
fn format_type_name(base_type: &str, max_length: i16, precision: u8, scale: u8) -> String {
    if LENGTH_CARRYING_TYPES.contains(&base_type) {
        if max_length == MAX_LENGTH_SENTINEL {
            return format!("{base_type}(max)");
        }
        let length = if WIDE_CHAR_TYPES.contains(&base_type) {
            i32::from(max_length) / 2
        } else {
            i32::from(max_length)
        };
        return format!("{base_type}({length})");
    }
    if PRECISION_SCALE_TYPES.contains(&base_type) {
        return format!("{base_type}({precision},{scale})");
    }
    base_type.to_owned()
}

/// Every column `column_id` that is (part of) `object_id`'s primary key,
/// and every `column_id` that is the sole column of some other unique index
/// (a composite unique index does not mark any single column unique on its
/// own).
async fn key_flag_column_ids(
    client: &mut Client<TcpStream>,
    object_id: i32,
) -> Result<(HashSet<i32>, HashSet<i32>), CoreError> {
    let sql = "SELECT i.index_id AS index_id, i.is_primary_key AS is_primary_key, \
                      i.is_unique AS is_unique, ic.column_id AS column_id \
               FROM sys.indexes i \
               JOIN sys.index_columns ic \
                 ON ic.object_id = i.object_id AND ic.index_id = i.index_id \
               WHERE i.object_id = @P1 AND ic.is_included_column = 0 \
                 AND (i.is_primary_key = 1 OR i.is_unique = 1) \
               ORDER BY i.index_id, ic.key_ordinal";
    let rows = client
        .query(sql, &[&object_id])
        .await
        .map_err(map_introspect_error)?
        .into_first_result()
        .await
        .map_err(map_introspect_error)?;

    let mut by_index: HashMap<i32, (bool, bool, Vec<i32>)> = HashMap::new();
    for row in &rows {
        let index_id = int32(row, "index_id")?;
        let is_primary = boolean(row, "is_primary_key")?;
        let is_unique = boolean(row, "is_unique")?;
        let column_id = int32(row, "column_id")?;
        let entry = by_index
            .entry(index_id)
            .or_insert_with(|| (is_primary, is_unique, Vec::new()));
        entry.2.push(column_id);
    }

    let mut primary_key_column_ids = HashSet::new();
    let mut unique_column_ids = HashSet::new();
    for (is_primary, is_unique, column_ids) in by_index.into_values() {
        if is_primary {
            primary_key_column_ids.extend(column_ids);
        } else if is_unique && let [column_id] = column_ids[..] {
            unique_column_ids.insert(column_id);
        }
    }
    Ok((primary_key_column_ids, unique_column_ids))
}

/// One foreign key on a relation, gathered from every row
/// `sys.foreign_key_columns` reports for it (one row per participating
/// column, sharing the same constraint name).
struct ForeignKeyGroup {
    name: String,
    local_column_ids: Vec<i32>,
    local_column_names: Vec<String>,
    target: ForeignKeyRef,
}

/// Every foreign key defined on `object_id`, each with its full local
/// column list and referenced-target detail.
async fn foreign_key_groups(
    client: &mut Client<TcpStream>,
    object_id: i32,
) -> Result<Vec<ForeignKeyGroup>, CoreError> {
    let sql = "SELECT fk.name AS fk_name, fkc.parent_column_id AS local_column_id, \
                      pc.name AS local_column_name, rs.name AS ref_schema, \
                      ro.name AS ref_table, rc.name AS ref_column_name \
               FROM sys.foreign_keys fk \
               JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id \
               JOIN sys.columns pc \
                 ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id \
               JOIN sys.objects ro ON ro.object_id = fk.referenced_object_id \
               JOIN sys.schemas rs ON rs.schema_id = ro.schema_id \
               JOIN sys.columns rc \
                 ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id \
               WHERE fk.parent_object_id = @P1 \
               ORDER BY fk.name, fkc.constraint_column_id";
    let rows = client
        .query(sql, &[&object_id])
        .await
        .map_err(map_introspect_error)?
        .into_first_result()
        .await
        .map_err(map_introspect_error)?;

    let mut groups: Vec<ForeignKeyGroup> = Vec::new();
    for row in &rows {
        let name = text(row, "fk_name")?;
        let local_column_id = int32(row, "local_column_id")?;
        let local_column_name = text(row, "local_column_name")?;
        let ref_column_name = text(row, "ref_column_name")?;

        if let Some(group) = groups.iter_mut().find(|group| group.name == name) {
            group.local_column_ids.push(local_column_id);
            group.local_column_names.push(local_column_name);
            group.target.columns.push(ref_column_name);
        } else {
            let ref_schema = text(row, "ref_schema")?;
            let ref_table = text(row, "ref_table")?;
            groups.push(ForeignKeyGroup {
                name,
                local_column_ids: vec![local_column_id],
                local_column_names: vec![local_column_name],
                target: ForeignKeyRef {
                    schema: ref_schema,
                    table: ref_table,
                    columns: vec![ref_column_name],
                },
            });
        }
    }
    Ok(groups)
}

/// Map each foreign key group's local columns to its shared target, so the
/// caller can look up a [`ForeignKeyRef`] by `column_id`.
fn foreign_keys_by_column_id(groups: &[ForeignKeyGroup]) -> HashMap<i32, ForeignKeyRef> {
    let mut by_column_id = HashMap::new();
    for group in groups {
        for &column_id in &group.local_column_ids {
            by_column_id.insert(column_id, group.target.clone());
        }
    }
    by_column_id
}

/// [`ConstraintInfo`] for every foreign key group, in the order given.
fn foreign_key_constraints(groups: &[ForeignKeyGroup]) -> Vec<ConstraintInfo> {
    groups
        .iter()
        .map(|group| ConstraintInfo {
            name: group.name.clone(),
            kind: ConstraintKind::ForeignKey,
            definition: format!(
                "FOREIGN KEY ({}) REFERENCES {}.{}({})",
                group.local_column_names.join(", "),
                group.target.schema,
                group.target.table,
                group.target.columns.join(", ")
            ),
        })
        .collect()
}

/// The key (non-included) columns of every index on `object_id`, in
/// key-ordinal order, keyed by `index_id`.
async fn index_key_columns_by_index_id(
    client: &mut Client<TcpStream>,
    object_id: i32,
) -> Result<HashMap<i32, Vec<String>>, CoreError> {
    let sql = "SELECT ic.index_id AS index_id, c.name AS column_name \
               FROM sys.index_columns ic \
               JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
               WHERE ic.object_id = @P1 AND ic.is_included_column = 0 \
               ORDER BY ic.index_id, ic.key_ordinal";
    let rows = client
        .query(sql, &[&object_id])
        .await
        .map_err(map_introspect_error)?
        .into_first_result()
        .await
        .map_err(map_introspect_error)?;

    let mut by_index_id: HashMap<i32, Vec<String>> = HashMap::new();
    for row in &rows {
        let index_id = int32(row, "index_id")?;
        let column_name = text(row, "column_name")?;
        by_index_id.entry(index_id).or_default().push(column_name);
    }
    Ok(by_index_id)
}

/// Every real index on `object_id` (the implicit heap, `index_id` `0`, is
/// excluded): name, access method, uniqueness, and a human-readable
/// definition.
async fn indexes(
    client: &mut Client<TcpStream>,
    object_id: i32,
    index_key_columns: &HashMap<i32, Vec<String>>,
) -> Result<Vec<IndexInfo>, CoreError> {
    let sql = "SELECT i.index_id AS index_id, i.name AS name, i.type_desc AS type_desc, \
                      i.is_unique AS is_unique, i.filter_definition AS filter_definition \
               FROM sys.indexes i \
               WHERE i.object_id = @P1 AND i.index_id > 0 \
               ORDER BY i.name";
    let rows = client
        .query(sql, &[&object_id])
        .await
        .map_err(map_introspect_error)?
        .into_first_result()
        .await
        .map_err(map_introspect_error)?;

    let mut indexes = Vec::with_capacity(rows.len());
    for row in &rows {
        let index_id = int32(row, "index_id")?;
        let name = text(row, "name")?;
        let type_desc = text(row, "type_desc")?;
        let unique = boolean(row, "is_unique")?;
        let filter_definition = opt_text(row, "filter_definition")?;
        let columns = index_key_columns
            .get(&index_id)
            .cloned()
            .unwrap_or_default();
        indexes.push(IndexInfo {
            name,
            method: index_method(&type_desc),
            unique,
            definition: index_definition(&columns, filter_definition.as_deref()),
        });
    }
    Ok(indexes)
}

/// Map a `sys.indexes.type_desc` value to a short readable `method` string:
/// lowercased, with spaces replaced by underscores.
fn index_method(type_desc: &str) -> String {
    type_desc.to_lowercase().replace(' ', "_")
}

/// Render an index's key column list, plus its filter predicate when it has
/// one, as human-readable text.
fn index_definition(columns: &[String], filter_definition: Option<&str>) -> String {
    let base = format!("({})", columns.join(", "));
    match filter_definition {
        Some(filter) => format!("{base} WHERE {filter}"),
        None => base,
    }
}

/// [`ConstraintInfo`] for `object_id`'s primary key and unique constraints
/// (`sys.key_constraints`, whose `type` is always `PK` or `UQ`).
async fn key_constraints(
    client: &mut Client<TcpStream>,
    object_id: i32,
    index_key_columns: &HashMap<i32, Vec<String>>,
) -> Result<Vec<ConstraintInfo>, CoreError> {
    let sql = "SELECT kc.name AS name, kc.type AS type_code, kc.unique_index_id AS index_id \
               FROM sys.key_constraints kc \
               WHERE kc.parent_object_id = @P1 \
               ORDER BY kc.name";
    let rows = client
        .query(sql, &[&object_id])
        .await
        .map_err(map_introspect_error)?
        .into_first_result()
        .await
        .map_err(map_introspect_error)?;

    let mut constraints = Vec::with_capacity(rows.len());
    for row in &rows {
        let type_code = text(row, "type_code")?;
        let Some(kind) = constraint_kind(type_code.trim()) else {
            continue;
        };
        let keyword = match kind {
            ConstraintKind::PrimaryKey => PRIMARY_KEY_KEYWORD,
            ConstraintKind::Unique => UNIQUE_KEYWORD,
            // `sys.key_constraints.type` only ever reports PK or UQ.
            ConstraintKind::ForeignKey | ConstraintKind::Check => continue,
        };
        let index_id = int32(row, "index_id")?;
        let columns = index_key_columns
            .get(&index_id)
            .cloned()
            .unwrap_or_default();
        constraints.push(ConstraintInfo {
            name: text(row, "name")?,
            kind,
            definition: format!("{keyword} ({})", columns.join(", ")),
        });
    }
    Ok(constraints)
}

/// [`ConstraintInfo`] for every `CHECK` constraint on `object_id`, with its
/// definition reported verbatim from `sys.check_constraints.definition`.
async fn check_constraints(
    client: &mut Client<TcpStream>,
    object_id: i32,
) -> Result<Vec<ConstraintInfo>, CoreError> {
    let sql = "SELECT cc.name AS name, cc.definition AS definition \
               FROM sys.check_constraints cc \
               WHERE cc.parent_object_id = @P1 \
               ORDER BY cc.name";
    let rows = client
        .query(sql, &[&object_id])
        .await
        .map_err(map_introspect_error)?
        .into_first_result()
        .await
        .map_err(map_introspect_error)?;

    let mut constraints = Vec::with_capacity(rows.len());
    for row in &rows {
        constraints.push(ConstraintInfo {
            name: text(row, "name")?,
            kind: ConstraintKind::Check,
            definition: text(row, "definition")?,
        });
    }
    Ok(constraints)
}

/// Map a constraint type code (`sys.key_constraints.type`, or the analogous
/// `sys.objects.type` codes for a foreign key or check constraint) to the
/// neutral [`ConstraintKind`].
fn constraint_kind(type_code: &str) -> Option<ConstraintKind> {
    match type_code {
        CONSTRAINT_TYPE_PRIMARY_KEY => Some(ConstraintKind::PrimaryKey),
        CONSTRAINT_TYPE_UNIQUE => Some(ConstraintKind::Unique),
        CONSTRAINT_TYPE_FOREIGN_KEY => Some(ConstraintKind::ForeignKey),
        CONSTRAINT_TYPE_CHECK => Some(ConstraintKind::Check),
        _ => None,
    }
}

/// A non-null text column, by name, from `row`.
fn text(row: &tiberius::Row, column: &str) -> Result<String, CoreError> {
    row.try_get::<&str, _>(column)
        .map_err(map_introspect_error)?
        .map(str::to_owned)
        .ok_or_else(|| {
            CoreError::introspection(format!("expected column '{column}' to be non-null"))
        })
}

/// A possibly-null text column, by name, from `row`.
fn opt_text(row: &tiberius::Row, column: &str) -> Result<Option<String>, CoreError> {
    Ok(row
        .try_get::<&str, _>(column)
        .map_err(map_introspect_error)?
        .map(str::to_owned))
}

/// A non-null boolean column, by name, from `row`.
fn boolean(row: &tiberius::Row, column: &str) -> Result<bool, CoreError> {
    row.try_get::<bool, _>(column)
        .map_err(map_introspect_error)?
        .ok_or_else(|| {
            CoreError::introspection(format!("expected column '{column}' to be non-null"))
        })
}

/// A non-null `int` column, by name, from `row`.
fn int32(row: &tiberius::Row, column: &str) -> Result<i32, CoreError> {
    row.try_get::<i32, _>(column)
        .map_err(map_introspect_error)?
        .ok_or_else(|| {
            CoreError::introspection(format!("expected column '{column}' to be non-null"))
        })
}

/// A non-null `smallint` column, by name, from `row`.
fn int16(row: &tiberius::Row, column: &str) -> Result<i16, CoreError> {
    row.try_get::<i16, _>(column)
        .map_err(map_introspect_error)?
        .ok_or_else(|| {
            CoreError::introspection(format!("expected column '{column}' to be non-null"))
        })
}

/// A non-null `tinyint` column, by name, from `row`.
fn tiny_uint(row: &tiberius::Row, column: &str) -> Result<u8, CoreError> {
    row.try_get::<u8, _>(column)
        .map_err(map_introspect_error)?
        .ok_or_else(|| {
            CoreError::introspection(format!("expected column '{column}' to be non-null"))
        })
}

#[cfg(test)]
mod tests {
    use super::{constraint_kind, format_type_name, index_definition, index_method};
    use zsql_core::ConstraintKind;

    #[test]
    fn format_type_name_reports_a_narrow_character_length() {
        assert_eq!(format_type_name("varchar", 255, 0, 0), "varchar(255)");
    }

    #[test]
    fn format_type_name_halves_a_wide_character_byte_length() {
        assert_eq!(format_type_name("nvarchar", 510, 0, 0), "nvarchar(255)");
    }

    #[test]
    fn format_type_name_reports_max_for_the_length_sentinel() {
        assert_eq!(format_type_name("nvarchar", -1, 0, 0), "nvarchar(max)");
        assert_eq!(format_type_name("varbinary", -1, 0, 0), "varbinary(max)");
    }

    #[test]
    fn format_type_name_reports_decimal_precision_and_scale() {
        assert_eq!(format_type_name("decimal", 0, 10, 2), "decimal(10,2)");
        assert_eq!(format_type_name("numeric", 0, 5, 0), "numeric(5,0)");
    }

    #[test]
    fn format_type_name_leaves_a_type_with_no_carried_length_bare() {
        assert_eq!(format_type_name("int", 4, 10, 0), "int");
        assert_eq!(format_type_name("bit", 1, 0, 0), "bit");
        assert_eq!(format_type_name("datetime2", 8, 27, 7), "datetime2");
    }

    #[test]
    fn index_method_lowercases_and_underscores_the_type_desc() {
        assert_eq!(index_method("CLUSTERED"), "clustered");
        assert_eq!(index_method("NONCLUSTERED"), "nonclustered");
        assert_eq!(
            index_method("NONCLUSTERED COLUMNSTORE"),
            "nonclustered_columnstore"
        );
        assert_eq!(index_method("SOMETHING NEW"), "something_new");
    }

    #[test]
    fn constraint_kind_maps_every_code_this_module_cares_about() {
        assert_eq!(constraint_kind("PK"), Some(ConstraintKind::PrimaryKey));
        assert_eq!(constraint_kind("UQ"), Some(ConstraintKind::Unique));
        assert_eq!(constraint_kind("F"), Some(ConstraintKind::ForeignKey));
        assert_eq!(constraint_kind("C"), Some(ConstraintKind::Check));
        assert_eq!(
            constraint_kind("D"),
            None,
            "a default constraint has no ConstraintKind mapping"
        );
        assert_eq!(constraint_kind(""), None, "empty code maps to nothing");
    }

    #[test]
    fn index_definition_lists_the_key_columns() {
        assert_eq!(
            index_definition(&["a".to_owned(), "b".to_owned()], None),
            "(a, b)"
        );
    }

    #[test]
    fn index_definition_appends_a_filter_predicate_when_present() {
        assert_eq!(
            index_definition(&["a".to_owned()], Some("([a] IS NOT NULL)")),
            "(a) WHERE ([a] IS NOT NULL)"
        );
    }
}
