//! Mapping from `MySQL`/`MariaDB` wire values to the engine-neutral
//! [`zsql_core::Value`]. Dispatch is keyed on each column's own reported
//! type name (e.g. `"BIGINT UNSIGNED"`, `"BOOLEAN"`) rather than its raw wire
//! type code, mirroring `zsql-postgres`'s `values.rs`; `MySQL` folds
//! `TINYINT(1)`/`BOOL`/`BOOLEAN` into the same wire type as every other
//! `TINYINT` width, and the type name is what distinguishes them.

use sqlx::mysql::{MySqlColumn, MySqlRow};
use sqlx::types::chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use sqlx::types::{BigDecimal, Json, JsonRawValue};
use sqlx::{Column as _, Row as _, TypeInfo as _, ValueRef as _};
use zsql_core::{ColumnMeta, Row as CoreRow, Value};

/// The largest `u64` [`Value::Int`] (backed by `i64`) can represent exactly.
/// A `BIGINT UNSIGNED` or `BIT` value above this decodes to [`Value::Numeric`]
/// instead, carrying its exact digits as text so a value in
/// `(i64::MAX, u64::MAX]` round-trips without silently wrapping or
/// truncating.
const MAX_INT_SAFE_UNSIGNED: u64 = i64::MAX as u64;

/// Build the `Columns` metadata for a result set's own column list.
///
/// sqlx's `MySQL` column metadata carries no nullability flag, so every column
/// is conservatively reported as nullable: it is never wrong to say a column
/// *might* be null, only to claim one *can't* be when it can.
pub(crate) fn column_metas(columns: &[MySqlColumn]) -> Vec<ColumnMeta> {
    columns
        .iter()
        .map(|column| ColumnMeta {
            name: column.name().to_owned(),
            type_name: column.type_info().name().to_owned(),
            nullable: true,
        })
        .collect()
}

/// Decode one MySQL/MariaDB row into an engine-neutral [`CoreRow`].
pub(crate) fn decode_row(row: &MySqlRow) -> CoreRow {
    CoreRow((0..row.len()).map(|idx| decode_value(row, idx)).collect())
}

/// Decode a single column of `row` into a [`Value`], dispatching on the
/// column's own reported type name. Falls back to [`raw_fallback`] for any
/// type this module does not explicitly recognize, or if a recognized-type
/// decode unexpectedly fails.
fn decode_value(row: &MySqlRow, idx: usize) -> Value {
    let type_name = row.column(idx).type_info().name().to_owned();
    known_value(row, idx, &type_name).unwrap_or_else(|| raw_fallback(row, idx, &type_name))
}

/// Attempt to decode `row[idx]` using a type-specific mapping. Returns
/// `None` when `type_name` is not one this module maps, or when decoding it
/// unexpectedly errors.
fn known_value(row: &MySqlRow, idx: usize, type_name: &str) -> Option<Value> {
    match type_name {
        // `TINYINT(1)`, `BOOL`, and `BOOLEAN` all report as "BOOLEAN" here
        // (MySQL has no wire-level boolean type of its own); every other
        // `TINYINT` width stays an integer, matched below.
        "BOOLEAN" => scalar::<bool, _>(row, idx, Value::Bool),
        "TINYINT" => scalar::<i8, _>(row, idx, |v| Value::Int(i64::from(v))),
        "SMALLINT" => scalar::<i16, _>(row, idx, |v| Value::Int(i64::from(v))),
        "MEDIUMINT" | "INT" => scalar::<i32, _>(row, idx, |v| Value::Int(i64::from(v))),
        "BIGINT" => scalar::<i64, _>(row, idx, Value::Int),
        "TINYINT UNSIGNED" => scalar::<u8, _>(row, idx, |v| Value::Int(i64::from(v))),
        "SMALLINT UNSIGNED" => scalar::<u16, _>(row, idx, |v| Value::Int(i64::from(v))),
        "MEDIUMINT UNSIGNED" | "INT UNSIGNED" => {
            scalar::<u32, _>(row, idx, |v| Value::Int(i64::from(v)))
        }
        // `BIT` has no dedicated `Value` variant; it joins `BIGINT UNSIGNED`
        // on the same overflow-safe unsigned-integer path, treating the bit
        // field as the unsigned integer it numerically is.
        "BIGINT UNSIGNED" | "BIT" => scalar::<u64, _>(row, idx, unsigned_to_value),
        "FLOAT" => scalar::<f32, _>(row, idx, |v| Value::Float(f64::from(v))),
        "DOUBLE" => scalar::<f64, _>(row, idx, Value::Float),
        "DECIMAL" => scalar::<BigDecimal, _>(row, idx, |v| Value::Numeric(v.to_string())),
        "CHAR" | "VARCHAR" | "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" => {
            scalar::<String, _>(row, idx, Value::Text)
        }
        "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" => {
            scalar::<Vec<u8>, _>(row, idx, Value::Bytes)
        }
        "DATE" => scalar::<NaiveDate, _>(row, idx, |v| Value::Timestamp(v.to_string())),
        "TIME" => scalar::<NaiveTime, _>(row, idx, |v| Value::Timestamp(v.to_string())),
        // `TIMESTAMP` is decoded exactly like `DATETIME`: the naive
        // wall-clock value the server sends back, with no time zone
        // conversion applied (MySQL's TIMESTAMP wire encoding is
        // indistinguishable from DATETIME's; only its storage semantics on
        // the server differ).
        "DATETIME" | "TIMESTAMP" => {
            scalar::<NaiveDateTime, _>(row, idx, |v| Value::Timestamp(format_naive_timestamp(v)))
        }
        "YEAR" => scalar::<u16, _>(row, idx, |v| Value::Int(i64::from(v))),
        // MariaDB has no native JSON wire type of its own -- JSON there is
        // a `LONGTEXT` alias, so a MariaDB JSON column (or a JSON-producing
        // expression) never reaches this arm and instead decodes as
        // `Value::Text` via the `LONGTEXT` arm above. This arm only ever
        // matches on MySQL, whose JSON type is a genuine distinct wire type.
        "JSON" => {
            scalar::<Json<Box<JsonRawValue>>, _>(row, idx, |v| Value::Json(v.0.get().to_owned()))
        }
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

/// Widen an unsigned 64-bit value to [`Value::Int`] when it fits `i64`'s
/// range; a value above `i64::MAX` (only reachable via `BIGINT UNSIGNED` or
/// a 64-bit `BIT`) decodes to [`Value::Numeric`] instead, so its exact
/// digits survive as text rather than wrapping or being silently truncated.
fn unsigned_to_value(v: u64) -> Value {
    if v <= MAX_INT_SAFE_UNSIGNED {
        Value::Int(i64::try_from(v).unwrap_or(i64::MAX))
    } else {
        Value::Numeric(v.to_string())
    }
}

/// Decode column `idx` as `Option<T>` via sqlx's unchecked getter (this
/// module's own `type_name` dispatch has already established which Rust
/// type applies; sqlx's static `Type::compatible` check would otherwise
/// reject some of these pairings, e.g. `NaiveDateTime` against a
/// `TIMESTAMP`-typed column, which are byte-for-byte identical to `DATETIME`
/// on the wire). SQL NULL maps to [`Value::Null`]; any decode error is
/// treated as "not this type after all" (`None`), falling back to
/// [`raw_fallback`].
fn scalar<'r, T, F>(row: &'r MySqlRow, idx: usize, to_value: F) -> Option<Value>
where
    T: sqlx::Decode<'r, sqlx::MySql>,
    F: FnOnce(T) -> Value,
{
    match row.try_get_unchecked::<Option<T>, _>(idx) {
        Ok(Some(v)) => Some(to_value(v)),
        Ok(None) => Some(Value::Null),
        Err(_) => None,
    }
}

/// Fallback decode for a column whose type this module does not map: `NULL`
/// still decodes to [`Value::Null`] regardless of declared type; otherwise
/// [`Value::Unknown`] carries the column's own type name (sqlx's `MySQL`
/// value accessors are private to its crate, so -- unlike the Postgres and
/// `MSSQL` drivers -- there is no raw text/bytes form of the value available
/// to carry here).
fn raw_fallback(row: &MySqlRow, idx: usize, type_name: &str) -> Value {
    match row.try_get_raw(idx) {
        Ok(raw) if raw.is_null() => Value::Null,
        _ => Value::Unknown(type_name.to_owned()),
    }
}
