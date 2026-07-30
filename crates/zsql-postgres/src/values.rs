//! Mapping from Postgres wire values to the engine-neutral [`zsql_core::Value`].

use sqlx::postgres::{PgRow, PgValueFormat, PgValueRef};
use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sqlx::types::{BigDecimal, Json, JsonRawValue, Uuid};
use sqlx::{Column as _, Row as _, TypeInfo as _, ValueRef as _};
use zsql_core::value::UnknownValue;
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

/// Which per-type decode path a column's runtime type name resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeKind {
    Bool,
    BoolArray,
    Int2,
    Int2Array,
    Int4,
    Int4Array,
    Int8,
    Int8Array,
    Float4,
    Float4Array,
    Float8,
    Float8Array,
    Numeric,
    NumericArray,
    Text,
    TextArray,
    Uuid,
    UuidArray,
    Date,
    DateArray,
    Time,
    TimeArray,
    Timestamp,
    TimestampArray,
    Timestamptz,
    TimestamptzArray,
    Json,
    JsonArray,
    Bytea,
    ByteaArray,
}

/// Every Postgres type name this module maps, paired with the decode path it
/// resolves to. `citext` is Postgres's case-insensitive text extension type:
/// its wire representation and decode path are plain text, just like
/// `TEXT`/`VARCHAR`/`CHAR`/`NAME`.
const TYPE_KINDS: &[(&str, DecodeKind)] = &[
    ("BOOL", DecodeKind::Bool),
    ("BOOL[]", DecodeKind::BoolArray),
    ("INT2", DecodeKind::Int2),
    ("INT2[]", DecodeKind::Int2Array),
    ("INT4", DecodeKind::Int4),
    ("INT4[]", DecodeKind::Int4Array),
    ("INT8", DecodeKind::Int8),
    ("INT8[]", DecodeKind::Int8Array),
    ("FLOAT4", DecodeKind::Float4),
    ("FLOAT4[]", DecodeKind::Float4Array),
    ("FLOAT8", DecodeKind::Float8),
    ("FLOAT8[]", DecodeKind::Float8Array),
    ("NUMERIC", DecodeKind::Numeric),
    ("NUMERIC[]", DecodeKind::NumericArray),
    ("TEXT", DecodeKind::Text),
    ("VARCHAR", DecodeKind::Text),
    ("CHAR", DecodeKind::Text),
    ("NAME", DecodeKind::Text),
    ("CITEXT", DecodeKind::Text),
    ("TEXT[]", DecodeKind::TextArray),
    ("VARCHAR[]", DecodeKind::TextArray),
    ("CHAR[]", DecodeKind::TextArray),
    ("NAME[]", DecodeKind::TextArray),
    ("CITEXT[]", DecodeKind::TextArray),
    ("UUID", DecodeKind::Uuid),
    ("UUID[]", DecodeKind::UuidArray),
    ("DATE", DecodeKind::Date),
    ("DATE[]", DecodeKind::DateArray),
    ("TIME", DecodeKind::Time),
    ("TIME[]", DecodeKind::TimeArray),
    ("TIMESTAMP", DecodeKind::Timestamp),
    ("TIMESTAMP[]", DecodeKind::TimestampArray),
    ("TIMESTAMPTZ", DecodeKind::Timestamptz),
    ("TIMESTAMPTZ[]", DecodeKind::TimestamptzArray),
    ("JSON", DecodeKind::Json),
    ("JSONB", DecodeKind::Json),
    ("JSON[]", DecodeKind::JsonArray),
    ("JSONB[]", DecodeKind::JsonArray),
    ("BYTEA", DecodeKind::Bytea),
    ("BYTEA[]", DecodeKind::ByteaArray),
];

/// Classify a Postgres type name (as reported by `type_info().name()`) into
/// the decode path that should handle it, or `None` if this module does not
/// map it. Matching is case-insensitive without allocating: a compile-time-
/// known type is always reported upper-case, but an extension type without a
/// stable OID (such as `citext`) is reported using its catalog name verbatim,
/// whatever case that happens to be, and this runs once per decoded cell.
fn decode_kind(type_name: &str) -> Option<DecodeKind> {
    TYPE_KINDS
        .iter()
        .find(|(name, _)| type_name.eq_ignore_ascii_case(name))
        .map(|(_, kind)| *kind)
}

/// Attempt to decode `row[idx]` using a type-specific mapping. Returns
/// `None` when the type name is not one this module maps (including array
/// suffixes it does not recognize) or when decoding it unexpectedly errors.
/// Each arm reads the driver-decoded value out of `row` and immediately
/// hands it to a plain-value conversion function ([`scalar_value`] or
/// [`array_value`]), so the actual `Value` construction logic underneath is
/// testable without a live row.
fn known_value(row: &PgRow, idx: usize, type_name: &str) -> Option<Value> {
    match decode_kind(type_name)? {
        DecodeKind::Bool => decode::<bool>(row, idx).map(|v| scalar_value(v, Value::Bool)),
        DecodeKind::BoolArray => {
            decode_array::<bool>(row, idx).map(|v| array_value(v, Value::Bool))
        }
        DecodeKind::Int2 => {
            decode::<i16>(row, idx).map(|v| scalar_value(v, |x| Value::Int(i64::from(x))))
        }
        DecodeKind::Int2Array => {
            decode_array::<i16>(row, idx).map(|v| array_value(v, |x| Value::Int(i64::from(x))))
        }
        DecodeKind::Int4 => {
            decode::<i32>(row, idx).map(|v| scalar_value(v, |x| Value::Int(i64::from(x))))
        }
        DecodeKind::Int4Array => {
            decode_array::<i32>(row, idx).map(|v| array_value(v, |x| Value::Int(i64::from(x))))
        }
        DecodeKind::Int8 => decode::<i64>(row, idx).map(|v| scalar_value(v, Value::Int)),
        DecodeKind::Int8Array => decode_array::<i64>(row, idx).map(|v| array_value(v, Value::Int)),
        DecodeKind::Float4 => {
            decode::<f32>(row, idx).map(|v| scalar_value(v, |x| Value::Float(f64::from(x))))
        }
        DecodeKind::Float4Array => {
            decode_array::<f32>(row, idx).map(|v| array_value(v, |x| Value::Float(f64::from(x))))
        }
        DecodeKind::Float8 => decode::<f64>(row, idx).map(|v| scalar_value(v, Value::Float)),
        DecodeKind::Float8Array => {
            decode_array::<f64>(row, idx).map(|v| array_value(v, Value::Float))
        }
        DecodeKind::Numeric => decode::<BigDecimal>(row, idx)
            .map(|v| scalar_value(v, |d| Value::Numeric(d.to_string()))),
        DecodeKind::NumericArray => decode_array::<BigDecimal>(row, idx)
            .map(|v| array_value(v, |d| Value::Numeric(d.to_string()))),
        DecodeKind::Text => decode::<String>(row, idx).map(|v| scalar_value(v, Value::Text)),
        DecodeKind::TextArray => {
            decode_array::<String>(row, idx).map(|v| array_value(v, Value::Text))
        }
        DecodeKind::Uuid => {
            decode::<Uuid>(row, idx).map(|v| scalar_value(v, |u| Value::Uuid(u.to_string())))
        }
        DecodeKind::UuidArray => {
            decode_array::<Uuid>(row, idx).map(|v| array_value(v, |u| Value::Uuid(u.to_string())))
        }
        DecodeKind::Date => decode::<NaiveDate>(row, idx)
            .map(|v| scalar_value(v, |d| Value::Timestamp(d.to_string()))),
        DecodeKind::DateArray => decode_array::<NaiveDate>(row, idx)
            .map(|v| array_value(v, |d| Value::Timestamp(d.to_string()))),
        DecodeKind::Time => decode::<NaiveTime>(row, idx)
            .map(|v| scalar_value(v, |t| Value::Timestamp(t.to_string()))),
        DecodeKind::TimeArray => decode_array::<NaiveTime>(row, idx)
            .map(|v| array_value(v, |t| Value::Timestamp(t.to_string()))),
        DecodeKind::Timestamp => decode::<NaiveDateTime>(row, idx)
            .map(|v| scalar_value(v, |dt| Value::Timestamp(format_naive_timestamp(dt)))),
        DecodeKind::TimestampArray => decode_array::<NaiveDateTime>(row, idx)
            .map(|v| array_value(v, |dt| Value::Timestamp(format_naive_timestamp(dt)))),
        DecodeKind::Timestamptz => decode::<DateTime<Utc>>(row, idx)
            .map(|v| scalar_value(v, |dt| Value::Timestamp(dt.to_rfc3339()))),
        DecodeKind::TimestamptzArray => decode_array::<DateTime<Utc>>(row, idx)
            .map(|v| array_value(v, |dt| Value::Timestamp(dt.to_rfc3339()))),
        // `String`'s sqlx `Type::compatible` only accepts text-family OIDs
        // (text/varchar/bpchar/name/unknown), not json/jsonb, so
        // `try_get::<String>` on a json/jsonb column always errors and would
        // silently fall through to the raw-text `Unknown` fallback below
        DecodeKind::Json => {
            decode::<Json<Box<JsonRawValue>>>(row, idx).map(|v| scalar_value(v, |j| json_text(&j)))
        }
        DecodeKind::JsonArray => decode_array::<Json<Box<JsonRawValue>>>(row, idx)
            .map(|v| array_value(v, |j| json_text(&j))),
        DecodeKind::Bytea => decode::<Vec<u8>>(row, idx).map(|v| scalar_value(v, Value::Bytes)),
        DecodeKind::ByteaArray => {
            decode_array::<Vec<u8>>(row, idx).map(|v| array_value(v, Value::Bytes))
        }
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
/// whose typed decode unexpectedly failed): carry the server's own display
/// text when the wire format guarantees it is available.
fn raw_fallback(row: &PgRow, idx: usize) -> Value {
    let Ok(raw) = row.try_get_raw(idx) else {
        return Value::Unknown(UnknownValue::None);
    };
    if raw.is_null() {
        return Value::Null;
    }
    Value::Unknown(unknown_value(raw))
}

/// Map a PgValueRef to an UnknownValue - this is assuming that a null check
/// has already been performed.
fn unknown_value(value: PgValueRef<'_>) -> UnknownValue {
    match value.format() {
        PgValueFormat::Text => {
            let text = value.as_str().ok().map(str::to_owned);
            text.map(UnknownValue::Text).unwrap_or(UnknownValue::None)
        }
        PgValueFormat::Binary => {
            let bytes = value.as_bytes().ok().map(|b| b.to_vec());
            bytes.map(UnknownValue::Bytes).unwrap_or(UnknownValue::None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DecodeKind, array_value, decode_kind, format_naive_timestamp, json_text, scalar_value,
    };
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
    fn decode_kind_dispatch_is_case_insensitive_for_a_built_in_type() {
        assert_eq!(decode_kind("BOOL"), Some(DecodeKind::Bool));
        assert_eq!(decode_kind("bool"), Some(DecodeKind::Bool));
        assert_eq!(decode_kind("Bool"), Some(DecodeKind::Bool));
        assert_eq!(decode_kind("bOoL"), Some(DecodeKind::Bool));
    }

    #[test]
    fn decode_kind_dispatch_is_case_insensitive_for_text_and_its_array_form() {
        assert_eq!(decode_kind("text"), Some(DecodeKind::Text));
        assert_eq!(decode_kind("Text"), Some(DecodeKind::Text));
        assert_eq!(decode_kind("text[]"), Some(DecodeKind::TextArray));
        assert_eq!(decode_kind("Text[]"), Some(DecodeKind::TextArray));
    }

    #[test]
    fn decode_kind_maps_citext_to_the_same_arm_as_text_regardless_of_case() {
        assert_eq!(decode_kind("citext"), Some(DecodeKind::Text));
        assert_eq!(decode_kind("CITEXT"), Some(DecodeKind::Text));
        assert_eq!(decode_kind("CiText"), Some(DecodeKind::Text));
        assert_eq!(decode_kind("TEXT"), decode_kind("CITEXT"));
    }

    #[test]
    fn decode_kind_maps_the_citext_array_form_to_the_same_arm_as_the_text_array_regardless_of_case()
    {
        assert_eq!(decode_kind("citext[]"), Some(DecodeKind::TextArray));
        assert_eq!(decode_kind("CITEXT[]"), Some(DecodeKind::TextArray));
        assert_eq!(decode_kind("TEXT[]"), decode_kind("CITEXT[]"));
    }

    #[test]
    fn decode_kind_returns_none_for_an_unmapped_type_name() {
        assert_eq!(decode_kind("HSTORE"), None);
        assert_eq!(decode_kind("POINT"), None);
    }
}
