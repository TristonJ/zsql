//! Mapping from Postgres wire values to the engine-neutral [`zsql_core::Value`].

use sqlx::postgres::{PgColumn, PgRow, PgValueFormat};
use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sqlx::types::{BigDecimal, Json, JsonRawValue, Uuid};
use sqlx::{Column as _, Row as _, TypeInfo as _, ValueRef as _};
use zsql_core::{ColumnMeta, Row as CoreRow, Value};

/// Build the `Columns` metadata for a prepared statement's output columns.
///
/// Postgres does not report column nullability from a plain `Describe`
/// (that would require walking `pg_attribute` per column, which this
/// mapping layer does not do), so every column is conservatively reported as
/// nullable: it is never wrong to say a column *might* be null, only to
/// claim one *can't* be when it can.
pub(crate) fn column_metas(columns: &[PgColumn]) -> Vec<ColumnMeta> {
    columns
        .iter()
        .map(|column| ColumnMeta {
            name: column.name().to_owned(),
            type_name: column.type_info().name().to_owned(),
            nullable: true,
        })
        .collect()
}

/// Decode one Postgres row into an engine-neutral [`CoreRow`].
pub(crate) fn decode_row(row: &PgRow) -> CoreRow {
    CoreRow((0..row.len()).map(|idx| decode_value(row, idx)).collect())
}

/// Decode a single column of `row` into a [`Value`], dispatching on the
/// column's own runtime type name. Falls back to a raw text decode for any
/// type this module does not explicitly recognize, or if a recognized-type
/// decode unexpectedly fails
fn decode_value(row: &PgRow, idx: usize) -> Value {
    let type_name = row.column(idx).type_info().name();
    known_value(row, idx, type_name).unwrap_or_else(|| raw_fallback(row, idx))
}

/// Attempt to decode `row[idx]` using a type-specific mapping. Returns
/// `None` when the type name is not one this module maps (including array
/// suffixes it does not recognize) or when decoding it unexpectedly errors
fn known_value(row: &PgRow, idx: usize, type_name: &str) -> Option<Value> {
    match type_name {
        "BOOL" => scalar::<bool, _>(row, idx, Value::Bool),
        "BOOL[]" => array::<bool, _>(row, idx, Value::Bool),
        "INT2" => scalar::<i16, _>(row, idx, |v| Value::Int(i64::from(v))),
        "INT2[]" => array::<i16, _>(row, idx, |v| Value::Int(i64::from(v))),
        "INT4" => scalar::<i32, _>(row, idx, |v| Value::Int(i64::from(v))),
        "INT4[]" => array::<i32, _>(row, idx, |v| Value::Int(i64::from(v))),
        "INT8" => scalar::<i64, _>(row, idx, Value::Int),
        "INT8[]" => array::<i64, _>(row, idx, Value::Int),
        "FLOAT4" => scalar::<f32, _>(row, idx, |v| Value::Float(f64::from(v))),
        "FLOAT4[]" => array::<f32, _>(row, idx, |v| Value::Float(f64::from(v))),
        "FLOAT8" => scalar::<f64, _>(row, idx, Value::Float),
        "FLOAT8[]" => array::<f64, _>(row, idx, Value::Float),
        "NUMERIC" => scalar::<BigDecimal, _>(row, idx, |v| Value::Numeric(v.to_string())),
        "NUMERIC[]" => array::<BigDecimal, _>(row, idx, |v| Value::Numeric(v.to_string())),
        "TEXT" | "VARCHAR" | "CHAR" | "NAME" => scalar::<String, _>(row, idx, Value::Text),
        "TEXT[]" | "VARCHAR[]" | "CHAR[]" | "NAME[]" => array::<String, _>(row, idx, Value::Text),
        "UUID" => scalar::<Uuid, _>(row, idx, |v| Value::Uuid(v.to_string())),
        "UUID[]" => array::<Uuid, _>(row, idx, |v| Value::Uuid(v.to_string())),
        "DATE" => scalar::<NaiveDate, _>(row, idx, |v| Value::Timestamp(v.to_string())),
        "DATE[]" => array::<NaiveDate, _>(row, idx, |v| Value::Timestamp(v.to_string())),
        "TIME" => scalar::<NaiveTime, _>(row, idx, |v| Value::Timestamp(v.to_string())),
        "TIME[]" => array::<NaiveTime, _>(row, idx, |v| Value::Timestamp(v.to_string())),
        "TIMESTAMP" => {
            scalar::<NaiveDateTime, _>(row, idx, |v| Value::Timestamp(format_naive_timestamp(v)))
        }
        "TIMESTAMP[]" => {
            array::<NaiveDateTime, _>(row, idx, |v| Value::Timestamp(format_naive_timestamp(v)))
        }
        "TIMESTAMPTZ" => scalar::<DateTime<Utc>, _>(row, idx, |v| Value::Timestamp(v.to_rfc3339())),
        "TIMESTAMPTZ[]" => {
            array::<DateTime<Utc>, _>(row, idx, |v| Value::Timestamp(v.to_rfc3339()))
        }
        // `String`'s sqlx `Type::compatible` only accepts text-family OIDs
        // (text/varchar/bpchar/name/unknown), not json/jsonb, so
        // `try_get::<String>` on a json/jsonb column always errors and would
        // silently fall through to the raw-text `Unknown` fallback below
        "JSON" | "JSONB" => scalar::<Json<Box<JsonRawValue>>, _>(row, idx, |v| json_text(&v)),
        "JSON[]" | "JSONB[]" => array::<Json<Box<JsonRawValue>>, _>(row, idx, |v| json_text(&v)),
        "BYTEA" => scalar::<Vec<u8>, _>(row, idx, Value::Bytes),
        "BYTEA[]" => array::<Vec<u8>, _>(row, idx, Value::Bytes),
        _ => None,
    }
}

/// ISO-8601-ish rendering of a timezone-less timestamp: chrono's own
/// `Display` uses a space between date and time, so this formats explicitly
/// with a `T` separator to match the ISO-8601 text the other temporal types
/// already produce.
fn format_naive_timestamp(dt: NaiveDateTime) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.f").to_string()
}

/// Extract a decoded json/jsonb value's raw text as a [`Value::Json`]
fn json_text(value: &Json<Box<JsonRawValue>>) -> Value {
    Value::Json(value.0.get().to_owned())
}

/// Decode column `idx` as `Option<T>` and wrap it, treating SQL NULL as
/// [`Value::Null`] and any decode error as "not this type after all" (`None`)
fn scalar<T, F>(row: &PgRow, idx: usize, to_value: F) -> Option<Value>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    F: FnOnce(T) -> Value,
{
    match row.try_get::<Option<T>, _>(idx) {
        Ok(Some(v)) => Some(to_value(v)),
        Ok(None) => Some(Value::Null),
        Err(_) => None,
    }
}

/// Decode column `idx` as a 1-D Postgres array, `Option<Vec<Option<T>>>`,
/// mapping element NULLs and the whole-array NULL to [`Value::Null`]. Any
/// decode error is treated as "not this type after all" (`None`)
fn array<T, F>(row: &PgRow, idx: usize, to_value: F) -> Option<Value>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    Vec<Option<T>>: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    F: Fn(T) -> Value,
{
    match row.try_get::<Option<Vec<Option<T>>>, _>(idx) {
        Ok(Some(items)) => Some(Value::Array(
            items
                .into_iter()
                .map(|item| item.map_or(Value::Null, &to_value))
                .collect(),
        )),
        Ok(None) => Some(Value::Null),
        Err(_) => None,
    }
}

/// Fallback decode for a column whose type this module does not map (or
/// whose typed decode unexpectedly failed): read it as raw text
fn raw_fallback(row: &PgRow, idx: usize) -> Value {
    let Ok(raw) = row.try_get_raw(idx) else {
        return Value::Unknown(String::new());
    };
    if raw.is_null() {
        return Value::Null;
    }
    match raw.format() {
        PgValueFormat::Text => raw.as_str().map_or_else(
            |_| Value::Unknown(hex_encode(raw.as_bytes().unwrap_or_default())),
            |s| Value::Unknown(s.to_owned()),
        ),
        PgValueFormat::Binary => Value::Unknown(hex_encode(raw.as_bytes().unwrap_or_default())),
    }
}

/// Render `bytes` as a lowercase hex string, Postgres `bytea`-literal style.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("\\x");
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}
