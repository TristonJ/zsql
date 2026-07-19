//! Mapping from tiberius's decoded `ColumnData` to the engine-neutral
//! [`zsql_core::Value`].

use tiberius::time::chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
use tiberius::{Column, ColumnData, ColumnType, FromSql, Row as MssqlRow, Uuid};
use zsql_core::{ColumnMeta, Row as CoreRow, Value};

/// Build the `Columns` metadata for a result set from tiberius's own column
/// list.
///
/// tiberius's `Column` carries no nullability information, so every column
/// is conservatively reported as nullable: it is never wrong to say a column
/// *might* be null, only to claim one *can't* be when it can.
pub(crate) fn column_metas(columns: &[Column]) -> Vec<ColumnMeta> {
    columns
        .iter()
        .map(|column| ColumnMeta {
            name: column.name().to_owned(),
            type_name: type_name(column.column_type()).to_owned(),
            nullable: true,
        })
        .collect()
}

/// A short, stable display name for an MSSQL wire type, used only for
/// [`ColumnMeta::type_name`] (display/formatting), not for decode dispatch.
fn type_name(column_type: ColumnType) -> &'static str {
    match column_type {
        ColumnType::Bit | ColumnType::Bitn => "bit",
        ColumnType::Int1 => "tinyint",
        ColumnType::Int2 => "smallint",
        ColumnType::Int4 | ColumnType::Intn => "int",
        ColumnType::Int8 => "bigint",
        ColumnType::Float4 => "real",
        ColumnType::Float8 | ColumnType::Floatn => "float",
        ColumnType::Decimaln => "decimal",
        ColumnType::Numericn => "numeric",
        ColumnType::Money | ColumnType::Money4 => "money",
        ColumnType::Guid => "uniqueidentifier",
        ColumnType::BigBinary | ColumnType::BigVarBin => "varbinary",
        ColumnType::BigChar => "char",
        ColumnType::BigVarChar => "varchar",
        ColumnType::NChar => "nchar",
        ColumnType::NVarchar => "nvarchar",
        ColumnType::Daten => "date",
        ColumnType::Timen => "time",
        ColumnType::Datetime2 => "datetime2",
        ColumnType::DatetimeOffsetn => "datetimeoffset",
        ColumnType::Datetime | ColumnType::Datetimen => "datetime",
        ColumnType::Datetime4 => "smalldatetime",
        ColumnType::Xml => "xml",
        ColumnType::Text => "text",
        ColumnType::NText => "ntext",
        ColumnType::Image => "image",
        ColumnType::Udt => "udt",
        ColumnType::SSVariant => "sql_variant",
        ColumnType::Null => "null",
    }
}

/// Decode one tiberius row into an engine-neutral [`CoreRow`].
pub(crate) fn decode_row(row: &MssqlRow) -> CoreRow {
    CoreRow(
        row.cells()
            .map(|(column, data)| decode_value(column.column_type(), data))
            .collect(),
    )
}

/// Decode a single cell into a [`Value`], dispatching on the column's wire
/// type. Falls back to a raw fallback for any type this module does not
/// explicitly map.
fn decode_value(column_type: ColumnType, data: &ColumnData<'static>) -> Value {
    known_value(column_type, data).unwrap_or_else(|| raw_fallback(data))
}

/// Attempt to decode `data` using a type-specific mapping. Returns `None`
/// for any `column_type` this module does not map.
fn known_value(column_type: ColumnType, data: &ColumnData<'static>) -> Option<Value> {
    match column_type {
        ColumnType::Bit | ColumnType::Bitn => scalar(data, Value::Bool),
        ColumnType::Int1 => u8_scalar(data),
        ColumnType::Int2 => i16_scalar(data),
        ColumnType::Int4 => i32_scalar(data),
        ColumnType::Int8 => scalar(data, Value::Int),
        ColumnType::Float4 => f32_scalar(data),
        ColumnType::Float8 => scalar(data, Value::Float),
        ColumnType::Decimaln | ColumnType::Numericn => {
            scalar(data, |n: tiberius::numeric::Numeric| {
                Value::Numeric(n.to_string())
            })
        }
        ColumnType::BigChar | ColumnType::BigVarChar | ColumnType::NChar | ColumnType::NVarchar => {
            string_scalar(data)
        }
        ColumnType::Guid => scalar(data, |uuid: Uuid| Value::Uuid(uuid.to_string())),
        ColumnType::BigBinary | ColumnType::BigVarBin => bytes_scalar(data),
        ColumnType::Daten => scalar(data, |d: NaiveDate| Value::Timestamp(d.to_string())),
        ColumnType::Timen => scalar(data, |t: NaiveTime| Value::Timestamp(t.to_string())),
        ColumnType::Datetime2
        | ColumnType::Datetime
        | ColumnType::Datetimen
        | ColumnType::Datetime4 => scalar(data, |dt: NaiveDateTime| {
            Value::Timestamp(format_naive_timestamp(dt))
        }),
        ColumnType::DatetimeOffsetn => scalar(data, |dt: DateTime<FixedOffset>| {
            Value::Timestamp(format!(
                "{}{}",
                format_naive_timestamp(dt.naive_local()),
                dt.offset()
            ))
        }),
        _ => None,
    }
}

/// ISO-8601-ish rendering of a timezone-less timestamp with a `T` separator.
/// tiberius pulls in `chrono` with its `alloc`/`std` features off (this
/// crate has no way to turn them on without adding `chrono` as a direct
/// dependency just to do so), so `chrono::NaiveDateTime::format` is
/// unavailable here; its space-separated `Display` output is reformatted by
/// hand instead.
fn format_naive_timestamp(dt: NaiveDateTime) -> String {
    dt.to_string().replacen(' ', "T", 1)
}

/// Decode `data` via tiberius's own [`FromSql`] for `T`, treating a decode
/// error the same as "not this type after all" (`None`) rather than
/// propagating it: the caller falls back to [`raw_fallback`] either way, so
/// a mismatched `column_type`/`ColumnData` pairing degrades gracefully
/// instead of failing the whole row.
fn scalar<'a, T, F>(data: &'a ColumnData<'static>, to_value: F) -> Option<Value>
where
    T: FromSql<'a>,
    F: FnOnce(T) -> Value,
{
    match T::from_sql(data) {
        Ok(Some(v)) => Some(to_value(v)),
        Ok(None) => Some(Value::Null),
        Err(_) => None,
    }
}

/// Gives the integer-width dispatch in [`known_value`] a uniform shape
/// alongside [`i16_scalar`], [`i32_scalar`].
fn u8_scalar(data: &ColumnData<'static>) -> Option<Value> {
    scalar::<u8, _>(data, |v| Value::Int(i64::from(v)))
}

fn i16_scalar(data: &ColumnData<'static>) -> Option<Value> {
    scalar::<i16, _>(data, |v| Value::Int(i64::from(v)))
}

fn i32_scalar(data: &ColumnData<'static>) -> Option<Value> {
    scalar::<i32, _>(data, |v| Value::Int(i64::from(v)))
}

fn f32_scalar(data: &ColumnData<'static>) -> Option<Value> {
    scalar::<f32, _>(data, |v| Value::Float(f64::from(v)))
}

fn string_scalar(data: &ColumnData<'static>) -> Option<Value> {
    scalar::<&str, _>(data, |s: &str| Value::Text(s.to_owned()))
}

fn bytes_scalar(data: &ColumnData<'static>) -> Option<Value> {
    scalar::<&[u8], _>(data, |b: &[u8]| Value::Bytes(b.to_owned()))
}

/// Fallback decode for a column whose wire type this module does not map:
/// render each decoded [`ColumnData`] variant's own scalar using its
/// natural text form (an integer's digits, a GUID's hyphenated form, binary
/// data as a `0x`-prefixed hex string, and so on), matching
/// [`Value::Unknown`]'s contract of carrying the backend's own text
/// rendering rather than a debug dump of this crate's internal decode
/// state. tiberius exposes no raw/untyped wire form the way a text-protocol
/// cursor would, so this is reconstructed from the typed value tiberius
/// already decoded. The `DateTime`/`SmallDateTime`/`Time`/`Date`/
/// `DateTime2`/`DateTimeOffset` variants have no natural text form of their
/// own (their text rendering lives in [`known_value`]'s chrono conversions)
/// and reach this function only if that conversion unexpectedly errors, so
/// they still fall back to `Debug` output.
fn raw_fallback(data: &ColumnData<'static>) -> Value {
    if is_null(data) {
        return Value::Null;
    }
    match data {
        ColumnData::U8(Some(v)) => Value::Unknown(v.to_string()),
        ColumnData::I16(Some(v)) => Value::Unknown(v.to_string()),
        ColumnData::I32(Some(v)) => Value::Unknown(v.to_string()),
        ColumnData::I64(Some(v)) => Value::Unknown(v.to_string()),
        ColumnData::F32(Some(v)) => Value::Unknown(v.to_string()),
        ColumnData::F64(Some(v)) => Value::Unknown(v.to_string()),
        ColumnData::Bit(Some(v)) => Value::Unknown(v.to_string()),
        ColumnData::String(Some(v)) => Value::Unknown(v.to_string()),
        ColumnData::Guid(Some(v)) => Value::Unknown(v.to_string()),
        ColumnData::Binary(Some(bytes)) => Value::Unknown(hex_encode(bytes)),
        ColumnData::Numeric(Some(v)) => Value::Unknown(v.to_string()),
        ColumnData::Xml(Some(v)) => Value::Unknown(v.to_string()),
        _ => Value::Unknown(format!("{data:?}")),
    }
}

/// Render `bytes` as a `0x`-prefixed lowercase hex string, matching a
/// `varbinary` literal's own text form in Transact-SQL.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Whether a [`ColumnData`] holds SQL NULL, regardless of which variant it
/// is.
fn is_null(data: &ColumnData<'static>) -> bool {
    match data {
        ColumnData::U8(v) => v.is_none(),
        ColumnData::I16(v) => v.is_none(),
        ColumnData::I32(v) => v.is_none(),
        ColumnData::I64(v) => v.is_none(),
        ColumnData::F32(v) => v.is_none(),
        ColumnData::F64(v) => v.is_none(),
        ColumnData::Bit(v) => v.is_none(),
        ColumnData::String(v) => v.is_none(),
        ColumnData::Guid(v) => v.is_none(),
        ColumnData::Binary(v) => v.is_none(),
        ColumnData::Numeric(v) => v.is_none(),
        ColumnData::Xml(v) => v.is_none(),
        ColumnData::DateTime(v) => v.is_none(),
        ColumnData::SmallDateTime(v) => v.is_none(),
        ColumnData::Time(v) => v.is_none(),
        ColumnData::Date(v) => v.is_none(),
        ColumnData::DateTime2(v) => v.is_none(),
        ColumnData::DateTimeOffset(v) => v.is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_value, format_naive_timestamp, is_null};
    use tiberius::numeric::Numeric;
    use tiberius::time::chrono::NaiveDate;
    use tiberius::{ColumnData, ColumnType, Uuid};
    use zsql_core::Value;

    #[test]
    fn maps_bit_to_bool() {
        assert_eq!(
            decode_value(ColumnType::Bit, &ColumnData::Bit(Some(true))),
            Value::Bool(true)
        );
    }

    #[test]
    fn maps_every_integer_width_to_int() {
        assert_eq!(
            decode_value(ColumnType::Int1, &ColumnData::U8(Some(8))),
            Value::Int(8)
        );
        assert_eq!(
            decode_value(ColumnType::Int2, &ColumnData::I16(Some(16))),
            Value::Int(16)
        );
        assert_eq!(
            decode_value(ColumnType::Int4, &ColumnData::I32(Some(32))),
            Value::Int(32)
        );
        assert_eq!(
            decode_value(ColumnType::Int8, &ColumnData::I64(Some(64))),
            Value::Int(64)
        );
    }

    #[test]
    fn maps_real_and_float_to_float() {
        assert_eq!(
            decode_value(ColumnType::Float4, &ColumnData::F32(Some(1.5))),
            Value::Float(1.5)
        );
        assert_eq!(
            decode_value(ColumnType::Float8, &ColumnData::F64(Some(2.5))),
            Value::Float(2.5)
        );
    }

    #[test]
    fn maps_numeric_to_a_decimal_string() {
        let n = Numeric::new_with_scale(123_456, 3);
        assert_eq!(
            decode_value(ColumnType::Numericn, &ColumnData::Numeric(Some(n))),
            Value::Numeric("123.456".to_owned())
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
    fn maps_datetime2_to_a_timestamp() {
        use tiberius::time::{Date, DateTime2, Time};

        let dt2 = DateTime2::new(
            Date::new(days_since_year_one(2024, 1, 15)),
            Time::new(49_530, 0),
        );
        assert_eq!(
            decode_value(ColumnType::Datetime2, &ColumnData::DateTime2(Some(dt2))),
            Value::Timestamp("2024-01-15T13:45:30".to_owned())
        );
    }

    #[test]
    fn maps_the_legacy_datetime_family_to_a_timestamp() {
        use tiberius::time::{DateTime as MssqlDateTime, SmallDateTime};

        let days_since_1900 = days_since(1900, 1, 1, 2024, 1, 15);

        // `datetime`: second precision, expressed as 1/300ths of a second.
        let seconds_since_midnight = 13 * 3600 + 45 * 60 + 30;
        let dt = MssqlDateTime::new(days_since_1900, seconds_since_midnight * 300);
        for column_type in [ColumnType::Datetime, ColumnType::Datetimen] {
            assert_eq!(
                decode_value(column_type, &ColumnData::DateTime(Some(dt))),
                Value::Timestamp("2024-01-15T13:45:30".to_owned())
            );
        }

        // `smalldatetime`: minute precision only.
        let small = SmallDateTime::new(u16::try_from(days_since_1900).unwrap(), 13 * 60 + 45);
        for column_type in [ColumnType::Datetime4, ColumnType::Datetimen] {
            assert_eq!(
                decode_value(column_type, &ColumnData::SmallDateTime(Some(small))),
                Value::Timestamp("2024-01-15T13:45:00".to_owned())
            );
        }
    }

    #[test]
    fn maps_datetimeoffset_preserving_a_non_utc_offset() {
        use tiberius::time::{Date, DateTime2, DateTimeOffset, Time};

        // The wire format stores UTC date/time plus an eastward offset in
        // minutes; a local wall-clock reading of 13:45:30+05:00 is stored as
        // UTC 08:45:30 with a 300-minute offset.
        let utc_seconds_since_midnight = 8 * 3600 + 45 * 60 + 30;
        let dt2 = DateTime2::new(
            Date::new(days_since_year_one(2024, 1, 15)),
            Time::new(utc_seconds_since_midnight, 0),
        );
        let dto = DateTimeOffset::new(dt2, 5 * 60);
        assert_eq!(
            decode_value(
                ColumnType::DatetimeOffsetn,
                &ColumnData::DateTimeOffset(Some(dto))
            ),
            Value::Timestamp("2024-01-15T13:45:30+05:00".to_owned())
        );
    }

    #[test]
    fn maps_datetimeoffset_at_utc_to_a_plus_zero_offset() {
        use tiberius::time::{Date, DateTime2, DateTimeOffset, Time};

        let dt2 = DateTime2::new(
            Date::new(days_since_year_one(2024, 1, 15)),
            Time::new(49_530, 0),
        );
        let dto = DateTimeOffset::new(dt2, 0);
        assert_eq!(
            decode_value(
                ColumnType::DatetimeOffsetn,
                &ColumnData::DateTimeOffset(Some(dto))
            ),
            Value::Timestamp("2024-01-15T13:45:30+00:00".to_owned())
        );
    }

    /// Days from `0001-01-01` to `y-m-d`, matching tiberius's own `datetime2`
    /// epoch so a test fixture's `Date` lines up with the calendar date it is
    /// meant to represent.
    fn days_since_year_one(y: i32, m: u32, d: u32) -> u32 {
        let target = NaiveDate::from_ymd_opt(y, m, d).unwrap();
        let epoch = NaiveDate::from_ymd_opt(1, 1, 1).unwrap();
        u32::try_from(target.signed_duration_since(epoch).num_days()).unwrap()
    }

    /// Days from `ey-em-ed` to `y-m-d`, matching tiberius's legacy
    /// `datetime`/`smalldatetime` epoch (1900-01-01) so a test fixture's day
    /// count lines up with the calendar date it is meant to represent.
    fn days_since(ey: i32, em: u32, ed: u32, y: i32, m: u32, d: u32) -> i32 {
        let target = NaiveDate::from_ymd_opt(y, m, d).unwrap();
        let epoch = NaiveDate::from_ymd_opt(ey, em, ed).unwrap();
        i32::try_from(target.signed_duration_since(epoch).num_days()).unwrap()
    }

    #[test]
    fn maps_the_text_family_to_text() {
        for column_type in [
            ColumnType::BigChar,
            ColumnType::BigVarChar,
            ColumnType::NChar,
            ColumnType::NVarchar,
        ] {
            let data = ColumnData::String(Some("hi".into()));
            assert_eq!(
                decode_value(column_type, &data),
                Value::Text("hi".to_owned())
            );
        }
    }

    #[test]
    fn maps_guid_to_uuid_text() {
        let uuid = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        assert_eq!(
            decode_value(ColumnType::Guid, &ColumnData::Guid(Some(uuid))),
            Value::Uuid("11111111-1111-1111-1111-111111111111".to_owned())
        );
    }

    #[test]
    fn maps_varbinary_to_bytes() {
        let data = ColumnData::Binary(Some(vec![1, 2, 3].into()));
        assert_eq!(
            decode_value(ColumnType::BigVarBin, &data),
            Value::Bytes(vec![1, 2, 3])
        );
    }

    #[test]
    fn maps_null_to_null_regardless_of_declared_type() {
        assert_eq!(
            decode_value(ColumnType::Int4, &ColumnData::I32(None)),
            Value::Null
        );
        assert_eq!(
            decode_value(ColumnType::NVarchar, &ColumnData::String(None)),
            Value::Null
        );
    }

    #[test]
    fn an_unmapped_type_falls_back_to_unknown_with_its_backend_text_form() {
        // `money` decodes on the wire into `ColumnData::F64`, but this
        // module does not map `ColumnType::Money`/`Money4` -- unlike
        // `Float8`/`Float4`, which are mapped -- so it must degrade to
        // `Value::Unknown` rather than silently being read as a float, and
        // that fallback must carry the value's own text form ("19.99"),
        // not a debug dump of the decode wrapper ("F64(Some(19.99))").
        let data = ColumnData::F64(Some(19.99));
        assert_eq!(
            decode_value(ColumnType::Money, &data),
            Value::Unknown("19.99".to_owned())
        );
    }

    #[test]
    fn an_unmapped_binary_type_falls_back_to_a_hex_string() {
        // `image` decodes on the wire into `ColumnData::Binary`, but this
        // module maps only `BigBinary`/`BigVarBin` (`varbinary`), not
        // `Image`, so it must degrade to `Value::Unknown` carrying a
        // Transact-SQL-style hex literal rather than a debug dump.
        let data = ColumnData::Binary(Some(vec![0xDE, 0xAD, 0xBE, 0xEF].into()));
        assert_eq!(
            decode_value(ColumnType::Image, &data),
            Value::Unknown("0xdeadbeef".to_owned())
        );
    }

    #[test]
    fn an_unmapped_null_still_decodes_to_null_not_unknown() {
        assert_eq!(
            decode_value(ColumnType::Money, &ColumnData::F64(None)),
            Value::Null
        );
    }

    #[test]
    fn is_null_recognizes_every_variant() {
        assert!(is_null(&ColumnData::I32(None)));
        assert!(!is_null(&ColumnData::I32(Some(1))));
    }
}
