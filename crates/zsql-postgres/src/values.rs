//! Mapping from Postgres wire values to the engine-neutral [`zsql_core::Value`].

use sqlx::postgres::{PgRow, PgValueFormat};
use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sqlx::types::{BigDecimal, Json, JsonRawValue, Uuid};
use sqlx::{Column as _, Row as _, TypeInfo as _, ValueRef as _};
use zsql_core::{Row as CoreRow, Value};

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
/// suffixes it does not recognize) or when decoding it unexpectedly errors.
/// Each arm reads the driver-decoded value out of `row` and immediately
/// hands it to a plain-value conversion function ([`scalar_value`] or
/// [`array_value`]), so the actual `Value` construction logic underneath is
/// testable without a live row.
fn known_value(row: &PgRow, idx: usize, type_name: &str) -> Option<Value> {
    match type_name {
        "BOOL" => decode::<bool>(row, idx).map(|v| scalar_value(v, Value::Bool)),
        "BOOL[]" => decode_array::<bool>(row, idx).map(|v| array_value(v, Value::Bool)),
        "INT2" => decode::<i16>(row, idx).map(|v| scalar_value(v, |x| Value::Int(i64::from(x)))),
        "INT2[]" => {
            decode_array::<i16>(row, idx).map(|v| array_value(v, |x| Value::Int(i64::from(x))))
        }
        "INT4" => decode::<i32>(row, idx).map(|v| scalar_value(v, |x| Value::Int(i64::from(x)))),
        "INT4[]" => {
            decode_array::<i32>(row, idx).map(|v| array_value(v, |x| Value::Int(i64::from(x))))
        }
        "INT8" => decode::<i64>(row, idx).map(|v| scalar_value(v, Value::Int)),
        "INT8[]" => decode_array::<i64>(row, idx).map(|v| array_value(v, Value::Int)),
        "FLOAT4" => {
            decode::<f32>(row, idx).map(|v| scalar_value(v, |x| Value::Float(f64::from(x))))
        }
        "FLOAT4[]" => {
            decode_array::<f32>(row, idx).map(|v| array_value(v, |x| Value::Float(f64::from(x))))
        }
        "FLOAT8" => decode::<f64>(row, idx).map(|v| scalar_value(v, Value::Float)),
        "FLOAT8[]" => decode_array::<f64>(row, idx).map(|v| array_value(v, Value::Float)),
        "NUMERIC" => decode::<BigDecimal>(row, idx)
            .map(|v| scalar_value(v, |d| Value::Numeric(d.to_string()))),
        "NUMERIC[]" => decode_array::<BigDecimal>(row, idx)
            .map(|v| array_value(v, |d| Value::Numeric(d.to_string()))),
        "TEXT" | "VARCHAR" | "CHAR" | "NAME" => {
            decode::<String>(row, idx).map(|v| scalar_value(v, Value::Text))
        }
        "TEXT[]" | "VARCHAR[]" | "CHAR[]" | "NAME[]" => {
            decode_array::<String>(row, idx).map(|v| array_value(v, Value::Text))
        }
        "UUID" => decode::<Uuid>(row, idx).map(|v| scalar_value(v, |u| Value::Uuid(u.to_string()))),
        "UUID[]" => {
            decode_array::<Uuid>(row, idx).map(|v| array_value(v, |u| Value::Uuid(u.to_string())))
        }
        "DATE" => decode::<NaiveDate>(row, idx)
            .map(|v| scalar_value(v, |d| Value::Timestamp(d.to_string()))),
        "DATE[]" => decode_array::<NaiveDate>(row, idx)
            .map(|v| array_value(v, |d| Value::Timestamp(d.to_string()))),
        "TIME" => decode::<NaiveTime>(row, idx)
            .map(|v| scalar_value(v, |t| Value::Timestamp(t.to_string()))),
        "TIME[]" => decode_array::<NaiveTime>(row, idx)
            .map(|v| array_value(v, |t| Value::Timestamp(t.to_string()))),
        "TIMESTAMP" => decode::<NaiveDateTime>(row, idx)
            .map(|v| scalar_value(v, |dt| Value::Timestamp(format_naive_timestamp(dt)))),
        "TIMESTAMP[]" => decode_array::<NaiveDateTime>(row, idx)
            .map(|v| array_value(v, |dt| Value::Timestamp(format_naive_timestamp(dt)))),
        "TIMESTAMPTZ" => decode::<DateTime<Utc>>(row, idx)
            .map(|v| scalar_value(v, |dt| Value::Timestamp(dt.to_rfc3339()))),
        "TIMESTAMPTZ[]" => decode_array::<DateTime<Utc>>(row, idx)
            .map(|v| array_value(v, |dt| Value::Timestamp(dt.to_rfc3339()))),
        // `String`'s sqlx `Type::compatible` only accepts text-family OIDs
        // (text/varchar/bpchar/name/unknown), not json/jsonb, so
        // `try_get::<String>` on a json/jsonb column always errors and would
        // silently fall through to the raw-text `Unknown` fallback below
        "JSON" | "JSONB" => {
            decode::<Json<Box<JsonRawValue>>>(row, idx).map(|v| scalar_value(v, |j| json_text(&j)))
        }
        "JSON[]" | "JSONB[]" => decode_array::<Json<Box<JsonRawValue>>>(row, idx)
            .map(|v| array_value(v, |j| json_text(&j))),
        "BYTEA" => decode::<Vec<u8>>(row, idx).map(|v| scalar_value(v, Value::Bytes)),
        "BYTEA[]" => decode_array::<Vec<u8>>(row, idx).map(|v| array_value(v, Value::Bytes)),
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

/// Decode column `idx` as `Option<T>`, treating any decode error as "not
/// this type after all" (`None`) rather than propagating it: the caller
/// falls back to [`raw_fallback`] either way.
// The outer `Option` (decode succeeded at all) and inner `Option` (decoded
// value vs. SQL NULL) are genuinely distinct axes here, not redundant
// nesting.
#[allow(clippy::option_option)]
fn decode<T>(row: &PgRow, idx: usize) -> Option<Option<T>>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get::<Option<T>, _>(idx).ok()
}

/// Decode column `idx` as a 1-D Postgres array, `Option<Vec<Option<T>>>`. Any
/// decode error is treated as "not this type after all" (`None`).
#[allow(clippy::option_option)]
fn decode_array<T>(row: &PgRow, idx: usize) -> Option<Option<Vec<Option<T>>>>
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    Vec<Option<T>>: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get::<Option<Vec<Option<T>>>, _>(idx).ok()
}

/// Wrap an already-decoded scalar, mapping SQL NULL to [`Value::Null`] and
/// any other value through `to_value`.
fn scalar_value<T>(v: Option<T>, to_value: impl FnOnce(T) -> Value) -> Value {
    v.map_or(Value::Null, to_value)
}

/// Wrap an already-decoded 1-D array, mapping the whole-array NULL and each
/// element NULL to [`Value::Null`], and every present element through
/// `to_value`.
fn array_value<T>(items: Option<Vec<Option<T>>>, to_value: impl Fn(T) -> Value) -> Value {
    match items {
        Some(items) => Value::Array(
            items
                .into_iter()
                .map(|item| item.map_or(Value::Null, &to_value))
                .collect(),
        ),
        None => Value::Null,
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
    let bytes = raw.as_bytes().unwrap_or_default();
    let text = raw.as_str().ok();
    raw_fallback_value(raw.format(), text, bytes)
}

/// Render an already-null-checked raw wire value's fallback text: the
/// server's own text-protocol rendering when available, otherwise a
/// hex-encoded dump of its raw bytes (always the case for a binary-protocol
/// value, and the degraded case for a text-protocol value whose bytes are
/// not valid UTF-8).
fn raw_fallback_value(format: PgValueFormat, text: Option<&str>, bytes: &[u8]) -> Value {
    match format {
        PgValueFormat::Text => text.map_or_else(
            || Value::Unknown(hex_encode(bytes)),
            |s| Value::Unknown(s.to_owned()),
        ),
        PgValueFormat::Binary => Value::Unknown(hex_encode(bytes)),
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

#[cfg(test)]
mod tests {
    use super::{
        array_value, format_naive_timestamp, hex_encode, json_text, raw_fallback_value,
        scalar_value,
    };
    use sqlx::postgres::PgValueFormat;
    use sqlx::types::chrono::{NaiveDate, NaiveTime};
    use sqlx::types::{BigDecimal, Json, JsonRawValue};
    use zsql_core::Value;

    #[test]
    fn scalar_value_maps_none_to_null() {
        assert_eq!(scalar_value::<i64>(None, Value::Int), Value::Null);
    }

    #[test]
    fn scalar_value_maps_a_bool() {
        assert_eq!(scalar_value(Some(true), Value::Bool), Value::Bool(true));
    }

    #[test]
    fn scalar_value_widens_a_smallint() {
        assert_eq!(
            scalar_value(Some(16i16), |v| Value::Int(i64::from(v))),
            Value::Int(16)
        );
    }

    #[test]
    fn scalar_value_widens_an_int() {
        assert_eq!(
            scalar_value(Some(32i32), |v| Value::Int(i64::from(v))),
            Value::Int(32)
        );
    }

    #[test]
    fn scalar_value_maps_a_bigint_without_conversion() {
        assert_eq!(scalar_value(Some(64i64), Value::Int), Value::Int(64));
    }

    #[test]
    fn scalar_value_formats_a_numeric_string() {
        let d: BigDecimal = "123.450".parse().unwrap();
        assert_eq!(
            scalar_value(Some(d), |v| Value::Numeric(v.to_string())),
            Value::Numeric("123.450".to_owned())
        );
    }

    #[test]
    fn scalar_value_maps_text() {
        assert_eq!(
            scalar_value(Some("hi".to_owned()), Value::Text),
            Value::Text("hi".to_owned())
        );
    }

    #[test]
    fn scalar_value_maps_bytea() {
        assert_eq!(
            scalar_value(Some(vec![1u8, 2, 3]), Value::Bytes),
            Value::Bytes(vec![1, 2, 3])
        );
    }

    #[test]
    fn scalar_value_formats_a_date() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        assert_eq!(
            scalar_value(Some(d), |v| Value::Timestamp(v.to_string())),
            Value::Timestamp("2024-01-15".to_owned())
        );
    }

    #[test]
    fn scalar_value_formats_a_time() {
        let t = NaiveTime::from_hms_opt(13, 45, 30).unwrap();
        assert_eq!(
            scalar_value(Some(t), |v| Value::Timestamp(v.to_string())),
            Value::Timestamp("13:45:30".to_owned())
        );
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
    fn json_text_extracts_the_raw_json_text() {
        let raw = JsonRawValue::from_string("{\"a\":1}".to_owned()).unwrap();
        assert_eq!(json_text(&Json(raw)), Value::Json("{\"a\":1}".to_owned()));
    }

    #[test]
    fn array_value_maps_the_whole_array_null_to_null() {
        assert_eq!(array_value::<i64>(None, Value::Int), Value::Null);
    }

    #[test]
    fn array_value_maps_every_element_through_the_given_conversion() {
        assert_eq!(
            array_value(Some(vec![Some(1i64), Some(2)]), Value::Int),
            Value::Array(vec![Value::Int(1), Value::Int(2)])
        );
    }

    #[test]
    fn array_value_maps_an_element_null_to_null_within_the_array() {
        assert_eq!(
            array_value(Some(vec![Some(1i64), None]), Value::Int),
            Value::Array(vec![Value::Int(1), Value::Null])
        );
    }

    #[test]
    fn raw_fallback_value_uses_the_servers_own_text_rendering() {
        assert_eq!(
            raw_fallback_value(PgValueFormat::Text, Some("19.99"), b""),
            Value::Unknown("19.99".to_owned())
        );
    }

    #[test]
    fn raw_fallback_value_hex_encodes_a_binary_format_value() {
        assert_eq!(
            raw_fallback_value(PgValueFormat::Binary, None, &[0xDE, 0xAD, 0xBE, 0xEF]),
            Value::Unknown("\\xdeadbeef".to_owned())
        );
    }

    #[test]
    fn raw_fallback_value_hex_encodes_a_text_format_value_with_no_text_rendering() {
        assert_eq!(
            raw_fallback_value(PgValueFormat::Text, None, &[1, 2]),
            Value::Unknown(hex_encode(&[1, 2]))
        );
    }

    #[test]
    fn hex_encode_uses_a_backslash_x_prefix() {
        assert_eq!(hex_encode(&[0xAB, 0x01]), "\\xab01");
    }
}
