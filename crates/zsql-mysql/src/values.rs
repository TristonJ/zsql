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
use zsql_core::value::UnknownValue;
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
    known_value(row, idx, &type_name).unwrap_or_else(|| raw_fallback(row, idx))
}

/// Attempt to decode `row[idx]` using a type-specific mapping. Returns
/// `None` when `type_name` is not one this module maps, or when decoding it
/// unexpectedly errors. Each arm reads the driver-native value out of `row`
/// and immediately hands it to a plain-value conversion function, so the
/// actual `Value` construction logic underneath is testable without a live
/// row.
fn known_value(row: &MySqlRow, idx: usize, type_name: &str) -> Option<Value> {
    match type_name {
        // `TINYINT(1)`, `BOOL`, and `BOOLEAN` all report as "BOOLEAN" here
        // (MySQL has no wire-level boolean type of its own); every other
        // `TINYINT` width stays an integer, matched below.
        "BOOLEAN" => decode::<bool>(row, idx).map(bool_value),
        "TINYINT" => decode::<i8>(row, idx).map(|v| int_value(v.map(i64::from))),
        "SMALLINT" => decode::<i16>(row, idx).map(|v| int_value(v.map(i64::from))),
        "MEDIUMINT" | "INT" => decode::<i32>(row, idx).map(|v| int_value(v.map(i64::from))),
        "BIGINT" => decode::<i64>(row, idx).map(int_value),
        "TINYINT UNSIGNED" => decode::<u8>(row, idx).map(|v| int_value(v.map(i64::from))),
        "SMALLINT UNSIGNED" => decode::<u16>(row, idx).map(|v| int_value(v.map(i64::from))),
        "MEDIUMINT UNSIGNED" | "INT UNSIGNED" => {
            decode::<u32>(row, idx).map(|v| int_value(v.map(i64::from)))
        }
        // `BIT` has no dedicated `Value` variant; it joins `BIGINT UNSIGNED`
        // on the same overflow-safe unsigned-integer path, treating the bit
        // field as the unsigned integer it numerically is.
        "BIGINT UNSIGNED" | "BIT" => decode::<u64>(row, idx).map(unsigned_value),
        "FLOAT" => decode::<f32>(row, idx).map(|v| float_value(v.map(f64::from))),
        "DOUBLE" => decode::<f64>(row, idx).map(float_value),
        "DECIMAL" => decode::<BigDecimal>(row, idx).map(decimal_value),
        "CHAR" | "VARCHAR" | "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" => {
            decode::<String>(row, idx).map(text_value)
        }
        "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" => {
            decode::<Vec<u8>>(row, idx).map(bytes_value)
        }
        "DATE" => decode::<NaiveDate>(row, idx).map(date_value),
        "TIME" => decode::<NaiveTime>(row, idx).map(time_value),
        // `TIMESTAMP` is decoded exactly like `DATETIME`: the naive
        // wall-clock value the server sends back, with no time zone
        // conversion applied (MySQL's TIMESTAMP wire encoding is
        // indistinguishable from DATETIME's; only its storage semantics on
        // the server differ).
        "DATETIME" | "TIMESTAMP" => decode::<NaiveDateTime>(row, idx).map(timestamp_value),
        "YEAR" => decode::<u16>(row, idx).map(|v| int_value(v.map(i64::from))),
        // MariaDB has no native JSON wire type of its own -- JSON there is
        // a `LONGTEXT` alias, so a MariaDB JSON column (or a JSON-producing
        // expression) never reaches this arm and instead decodes as
        // `Value::Text` via the `LONGTEXT` arm above. This arm only ever
        // matches on MySQL, whose JSON type is a genuine distinct wire type.
        "JSON" => decode::<Json<Box<JsonRawValue>>>(row, idx).map(json_value),
        _ => None,
    }
}

/// Decode column `idx` as `Option<T>` via sqlx's unchecked getter (this
/// module's own `type_name` dispatch has already established which Rust
/// type applies; sqlx's static `Type::compatible` check would otherwise
/// reject some of these pairings, e.g. `NaiveDateTime` against a
/// `TIMESTAMP`-typed column, which are byte-for-byte identical to `DATETIME`
/// on the wire). Any decode error is treated as "not this type after all"
/// (`None`), falling back to [`raw_fallback`].
// The outer `Option` (decode succeeded at all) and inner `Option` (decoded
// value vs. SQL NULL) are genuinely distinct axes here, not redundant
// nesting: callers pattern-match `None` (fall back) separately from
// `Some(None)` (a real, typed NULL).
#[allow(clippy::option_option)]
fn decode<'r, T>(row: &'r MySqlRow, idx: usize) -> Option<Option<T>>
where
    T: sqlx::Decode<'r, sqlx::MySql> + sqlx::Type<sqlx::MySql>,
{
    row.try_get_unchecked::<Option<T>, _>(idx).ok()
}

fn bool_value(v: Option<bool>) -> Value {
    v.map_or(Value::Null, Value::Bool)
}

fn int_value(v: Option<i64>) -> Value {
    v.map_or(Value::Null, Value::Int)
}

fn float_value(v: Option<f64>) -> Value {
    v.map_or(Value::Null, Value::Float)
}

fn decimal_value(v: Option<BigDecimal>) -> Value {
    v.map_or(Value::Null, |d| Value::Numeric(d.to_string()))
}

fn text_value(v: Option<String>) -> Value {
    v.map_or(Value::Null, Value::Text)
}

fn bytes_value(v: Option<Vec<u8>>) -> Value {
    v.map_or(Value::Null, Value::Bytes)
}

fn date_value(v: Option<NaiveDate>) -> Value {
    v.map_or(Value::Null, |d| Value::Timestamp(d.to_string()))
}

fn time_value(v: Option<NaiveTime>) -> Value {
    v.map_or(Value::Null, |t| Value::Timestamp(t.to_string()))
}

fn timestamp_value(v: Option<NaiveDateTime>) -> Value {
    v.map_or(Value::Null, |dt| {
        Value::Timestamp(format_naive_timestamp(dt))
    })
}

fn json_value(v: Option<Json<Box<JsonRawValue>>>) -> Value {
    v.map_or(Value::Null, |j| Value::Json(j.0.get().to_owned()))
}

fn unsigned_value(v: Option<u64>) -> Value {
    v.map_or(Value::Null, unsigned_to_value)
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

/// ISO-8601-ish rendering of a timezone-less timestamp: chrono's own
/// `Display` uses a space between date and time, so this formats explicitly
/// with a `T` separator to match the ISO-8601 text the other temporal types
/// already produce.
fn format_naive_timestamp(dt: NaiveDateTime) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.f").to_string()
}

/// Fallback decode for a column whose type this module does not map: `NULL`
/// still decodes to [`Value::Null`] regardless of declared type; otherwise
/// [`Value::Unknown`]. Unlike the Postgres and `MSSQL` drivers -- there is
/// no raw text/bytes form of the value available to carry here).
fn raw_fallback(row: &MySqlRow, idx: usize) -> Value {
    let is_null = row.try_get_raw(idx).is_ok_and(|raw| raw.is_null());
    unknown_fallback(is_null)
}

fn unknown_fallback(is_null: bool) -> Value {
    if is_null {
        Value::Null
    } else {
        Value::Unknown(UnknownValue::None)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bool_value, bytes_value, date_value, decimal_value, float_value, format_naive_timestamp,
        int_value, json_value, text_value, time_value, timestamp_value, unknown_fallback,
        unsigned_to_value, unsigned_value,
    };
    use sqlx::types::chrono::{NaiveDate, NaiveTime};
    use sqlx::types::{BigDecimal, Json, JsonRawValue};
    use zsql_core::Value;
    use zsql_core::value::UnknownValue;

    #[test]
    fn bool_value_maps_a_boolean() {
        assert_eq!(bool_value(Some(true)), Value::Bool(true));
    }

    #[test]
    fn bool_value_maps_none_to_null() {
        assert_eq!(bool_value(None), Value::Null);
    }

    #[test]
    fn int_value_maps_a_tinyint_width_value() {
        assert_eq!(int_value(Some(i64::from(8i8))), Value::Int(8));
    }

    #[test]
    fn int_value_maps_a_bigint_width_value() {
        assert_eq!(int_value(Some(64i64)), Value::Int(64));
    }

    #[test]
    fn int_value_maps_none_to_null() {
        assert_eq!(int_value(None), Value::Null);
    }

    #[test]
    fn unsigned_to_value_stays_an_int_within_i64_range() {
        assert_eq!(unsigned_to_value(42), Value::Int(42));
        assert_eq!(
            unsigned_to_value(i64::MAX as u64),
            Value::Int(i64::MAX),
            "the exact i64::MAX boundary must still decode as Int, not Numeric"
        );
    }

    #[test]
    fn unsigned_to_value_falls_back_to_numeric_above_i64_max() {
        let above_max = i64::MAX as u64 + 1;
        assert_eq!(
            unsigned_to_value(above_max),
            Value::Numeric(above_max.to_string())
        );
    }

    #[test]
    fn unsigned_value_maps_none_to_null() {
        assert_eq!(unsigned_value(None), Value::Null);
    }

    #[test]
    fn float_value_maps_a_double() {
        assert_eq!(float_value(Some(2.5)), Value::Float(2.5));
    }

    #[test]
    fn float_value_maps_none_to_null() {
        assert_eq!(float_value(None), Value::Null);
    }

    #[test]
    fn decimal_value_formats_a_decimal_string() {
        let d: BigDecimal = "123.450".parse().unwrap();
        assert_eq!(decimal_value(Some(d)), Value::Numeric("123.450".to_owned()));
    }

    #[test]
    fn decimal_value_maps_none_to_null() {
        assert_eq!(decimal_value(None), Value::Null);
    }

    #[test]
    fn text_value_maps_a_string() {
        assert_eq!(
            text_value(Some("hi".to_owned())),
            Value::Text("hi".to_owned())
        );
    }

    #[test]
    fn text_value_maps_none_to_null() {
        assert_eq!(text_value(None), Value::Null);
    }

    #[test]
    fn bytes_value_maps_binary_data() {
        assert_eq!(
            bytes_value(Some(vec![1, 2, 3])),
            Value::Bytes(vec![1, 2, 3])
        );
    }

    #[test]
    fn bytes_value_maps_none_to_null() {
        assert_eq!(bytes_value(None), Value::Null);
    }

    #[test]
    fn date_value_formats_a_calendar_date() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        assert_eq!(
            date_value(Some(d)),
            Value::Timestamp("2024-01-15".to_owned())
        );
    }

    #[test]
    fn time_value_formats_a_clock_time() {
        let t = NaiveTime::from_hms_opt(13, 45, 30).unwrap();
        assert_eq!(time_value(Some(t)), Value::Timestamp("13:45:30".to_owned()));
    }

    #[test]
    fn format_naive_timestamp_uses_a_t_separator() {
        let dt = NaiveDate::from_ymd_opt(2024, 1, 15)
            .unwrap()
            .and_hms_opt(13, 45, 30)
            .unwrap();
        assert_eq!(format_naive_timestamp(dt), "2024-01-15T13:45:30");
    }

    #[test]
    fn timestamp_value_formats_a_datetime() {
        let dt = NaiveDate::from_ymd_opt(2024, 1, 15)
            .unwrap()
            .and_hms_opt(13, 45, 30)
            .unwrap();
        assert_eq!(
            timestamp_value(Some(dt)),
            Value::Timestamp("2024-01-15T13:45:30".to_owned())
        );
    }

    #[test]
    fn timestamp_value_maps_none_to_null() {
        assert_eq!(timestamp_value(None), Value::Null);
    }

    #[test]
    fn json_value_extracts_the_raw_json_text() {
        let raw = JsonRawValue::from_string("{\"a\":1}".to_owned()).unwrap();
        assert_eq!(
            json_value(Some(Json(raw))),
            Value::Json("{\"a\":1}".to_owned())
        );
    }

    #[test]
    fn json_value_maps_none_to_null() {
        assert_eq!(json_value(None), Value::Null);
    }

    #[test]
    fn unknown_fallback_reports_unknown_for_a_non_null_value() {
        assert_eq!(unknown_fallback(false), Value::Unknown(UnknownValue::None));
    }

    #[test]
    fn unknown_fallback_reports_null() {
        assert_eq!(unknown_fallback(true), Value::Null);
    }
}
