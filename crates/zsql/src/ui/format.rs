//! Formats `zsql_core::Value` cells for display in the results grid, and
//! classifies each value into a semantic [`ValueKind`] used to pick a
//! per-kind text color/style.

use std::fmt::Write as _;

use zsql_core::{ColumnMeta, Row, Value, value::UnknownValue};
use zsql_ui::theme::Theme;

/// Displayed for a [`Value::Unknown`] that carries no backend text.
const UNKNOWN_PLACEHOLDER: &str = "?";

/// Semantic category of a formatted cell, used to select a per-kind text
/// color/style in the results grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// SQL NULL - the literal text `NULL`, styled distinctly from text.
    Null,
    /// Boolean `true`/`false`.
    Bool,
    /// Any numeric value: integer, float, or exact decimal.
    Number,
    /// Plain text, including UUIDs (rendered as text) and empty strings.
    Text,
    /// JSON/JSONB, rendered as its source text.
    Json,
    /// A timestamp rendered as ISO-8601 text.
    Timestamp,
    /// Raw bytes, rendered as a hex literal.
    Bytes,
    /// Anything that doesn't map to a more specific kind
    Unknown,
}

impl ValueKind {
    pub fn color(self, theme: &Theme) -> u32 {
        let colors = &theme.colors;
        match self {
            ValueKind::Null => colors.value_null,
            ValueKind::Bool => colors.value_bool,
            ValueKind::Number => colors.value_number,
            ValueKind::Text => colors.value_text,
            ValueKind::Json => colors.value_json,
            ValueKind::Timestamp => colors.value_timestamp,
            ValueKind::Bytes => colors.value_bytes,
            ValueKind::Unknown => colors.value_unknown,
        }
    }
}

/// A formatted cell: display text paired with the semantic kind the grid
/// uses to color it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattedValue {
    /// The text to render in the cell.
    pub text: String,
    /// The semantic kind, used to pick a per-kind text color/style.
    pub kind: ValueKind,
}

/// Format a single value for display in the results grid.
#[must_use]
pub fn format_value(value: &Value) -> FormattedValue {
    match value {
        Value::Null => FormattedValue {
            text: "NULL".to_owned(),
            kind: ValueKind::Null,
        },
        Value::Bool(b) => FormattedValue {
            text: b.to_string(),
            kind: ValueKind::Bool,
        },
        Value::Int(i) => FormattedValue {
            text: i.to_string(),
            kind: ValueKind::Number,
        },
        Value::Float(f) => FormattedValue {
            text: format_float(*f),
            kind: ValueKind::Number,
        },
        Value::Numeric(text) => FormattedValue {
            text: text.clone(),
            kind: ValueKind::Number,
        },
        Value::Text(text) | Value::Uuid(text) => FormattedValue {
            text: text.clone(),
            kind: ValueKind::Text,
        },
        Value::Bytes(bytes) => FormattedValue {
            text: format_bytes(bytes),
            kind: ValueKind::Bytes,
        },
        Value::Timestamp(text) => FormattedValue {
            text: text.clone(),
            kind: ValueKind::Timestamp,
        },
        Value::Json(text) => FormattedValue {
            text: text.clone(),
            kind: ValueKind::Json,
        },
        Value::Array(items) => FormattedValue {
            text: format_array(items),
            kind: ValueKind::Unknown,
        },
        Value::Unknown(v) => FormattedValue {
            text: match v {
                UnknownValue::Text(t) => t.clone(),
                UnknownValue::Bytes(bytes) => format_bytes(bytes),
                UnknownValue::None => UNKNOWN_PLACEHOLDER.to_owned(),
            },
            kind: ValueKind::Unknown,
        },
    }
}

/// Format a single value for the clipboard
#[must_use]
pub fn format_value_for_clipboard(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        other => format_value(other).text,
    }
}

/// Render a float without a trailing-zero ambiguity
fn format_float(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 {
        format!("{value:.1}")
    } else {
        value.to_string()
    }
}

/// Render bytes the way Postgres' `bytea` hex format does: `\x` followed by
/// two lowercase hex digits per byte.
fn format_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("\\x");
    for byte in bytes {
        // Writing to a `String` cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Render an array as a brace-delimited, comma-separated list of its
/// formatted elements
fn format_array(items: &[Value]) -> String {
    let rendered: Vec<String> = items.iter().map(|item| format_value(item).text).collect();
    format!("{{{}}}", rendered.join(","))
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard (RFC 4648, padded) base64 encoding of `bytes`. Hand-rolled
/// rather than a dependency: the panel's only user of base64, and small
/// enough to keep self-contained and fully covered by its own tests.
#[must_use]
pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(BASE64_ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(BASE64_ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// `row`'s cells as a single JSON object keyed by `columns`' names, each
/// value serialized via [`value_to_json`]
#[must_use]
pub fn row_as_json(row: &Row, columns: &[ColumnMeta]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (column, value) in columns.iter().zip(row.0.iter()) {
        map.insert(column.name.clone(), value_to_json(value));
    }
    serde_json::Value::Object(map)
}

/// [`row_as_json`], serialized to the compact JSON text
#[must_use]
pub fn row_as_json_string(row: &Row, columns: &[ColumnMeta]) -> String {
    serde_json::to_string(&row_as_json(row, columns)).unwrap_or_default()
}

/// `row`'s cells as a single line CSV string, each value serialized via [`format_value_for_clipboard`]
#[must_use]
pub fn row_as_csv_string(row: &Row) -> String {
    let mut out = String::new();
    for (i, value) in row.0.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let cell_text = format_value_for_clipboard(value);
        // Escape any cell that contains a comma, quote, or newline
        if cell_text.contains(&[',', '"', '\n'][..]) {
            out.push('"');
            for c in cell_text.chars() {
                if c == '"' {
                    out.push('"'); // Escape quotes by doubling them
                }
                out.push(c);
            }
            out.push('"');
        } else {
            out.push_str(&cell_text);
        }
    }
    out
}

/// `value`'s own JSON representation
#[must_use]
pub fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(i) => serde_json::Value::Number((*i).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Numeric(text) | Value::Text(text) | Value::Uuid(text) | Value::Timestamp(text) => {
            serde_json::Value::String(text.clone())
        }
        Value::Bytes(bytes) => serde_json::Value::String(base64_encode(bytes)),
        Value::Json(text) => {
            serde_json::from_str(text).unwrap_or_else(|_| serde_json::Value::String(text.clone()))
        }
        Value::Array(items) => serde_json::Value::Array(items.iter().map(value_to_json).collect()),
        Value::Unknown(v) => match v {
            UnknownValue::Text(text) => serde_json::Value::String(text.clone()),
            UnknownValue::Bytes(bytes) => serde_json::Value::String(base64_encode(bytes)),
            UnknownValue::None => serde_json::Value::String(UNKNOWN_PLACEHOLDER.to_owned()),
        },
    }
}

/// Extract a `host[:port]`-shaped label from a connection URL for display,
/// e.g. `postgres://user:pass@localhost:5432/db` -> `localhost:5432`. Falls
/// back to the scheme-stripped remainder of the URL if no host segment can
/// be isolated (e.g. a `sqlite:` path), so even an unusual URL still renders
/// something instead of an empty label.
#[must_use]
pub fn host_label(url: &str) -> String {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let after_userinfo = after_scheme
        .rsplit_once('@')
        .map_or(after_scheme, |(_, rest)| rest);
    let host = after_userinfo
        .split(['/', '?'])
        .next()
        .unwrap_or(after_userinfo);
    if host.is_empty() {
        after_scheme.to_owned()
    } else {
        host.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use zsql_core::{ColumnMeta, Row, Value, value::UnknownValue};
    use zsql_ui::theme::Theme;

    use super::{ValueKind, base64_encode, format_value, row_as_json, value_to_json};

    fn column(name: &str, type_name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            nullable: true,
        }
    }

    #[test]
    fn null_renders_as_literal_and_its_own_kind() {
        let formatted = format_value(&Value::Null);
        assert_eq!(formatted.text, "NULL");
        assert_eq!(formatted.kind, ValueKind::Null);
    }

    #[test]
    fn empty_text_is_distinct_from_null() {
        let formatted = format_value(&Value::Text(String::new()));
        assert_eq!(formatted.text, "");
        assert_eq!(formatted.kind, ValueKind::Text);
    }

    #[test]
    fn bool_renders_true_false() {
        assert_eq!(format_value(&Value::Bool(true)).text, "true");
        assert_eq!(format_value(&Value::Bool(false)).text, "false");
        assert_eq!(format_value(&Value::Bool(true)).kind, ValueKind::Bool);
    }

    #[test]
    fn int_renders_as_plain_digits() {
        let formatted = format_value(&Value::Int(-42));
        assert_eq!(formatted.text, "-42");
        assert_eq!(formatted.kind, ValueKind::Number);
    }

    #[test]
    fn float_keeps_a_decimal_point_for_whole_numbers() {
        assert_eq!(format_value(&Value::Float(2.0)).text, "2.0");
        assert_eq!(format_value(&Value::Float(2.5)).text, "2.5");
        assert_eq!(format_value(&Value::Float(2.0)).kind, ValueKind::Number);
    }

    #[test]
    fn float_renders_non_finite_values_via_their_default_display() {
        // Postgres' float4/float8 can carry `NaN`/`Infinity`; these bypass the
        // whole-number `.1` suffix path and fall through to `f64::to_string`.
        assert_eq!(format_value(&Value::Float(f64::INFINITY)).text, "inf");
        assert_eq!(format_value(&Value::Float(f64::NEG_INFINITY)).text, "-inf");
        assert_eq!(format_value(&Value::Float(f64::NAN)).text, "NaN");
        assert_eq!(
            format_value(&Value::Float(f64::INFINITY)).kind,
            ValueKind::Number
        );
    }

    #[test]
    fn numeric_passes_through_its_exact_text() {
        let formatted = format_value(&Value::Numeric("12345678901234567890.5".to_owned()));
        assert_eq!(formatted.text, "12345678901234567890.5");
        assert_eq!(formatted.kind, ValueKind::Number);
    }

    #[test]
    fn text_passes_through() {
        let formatted = format_value(&Value::Text("hello".to_owned()));
        assert_eq!(formatted.text, "hello");
        assert_eq!(formatted.kind, ValueKind::Text);
    }

    #[test]
    fn bytes_render_as_hex_bytea_literal() {
        let formatted = format_value(&Value::Bytes(vec![0x01, 0xAB, 0xff]));
        assert_eq!(formatted.text, "\\x01abff");
        assert_eq!(formatted.kind, ValueKind::Bytes);
    }

    #[test]
    fn empty_bytes_render_as_bare_prefix() {
        let formatted = format_value(&Value::Bytes(Vec::new()));
        assert_eq!(formatted.text, "\\x");
        assert_eq!(formatted.kind, ValueKind::Bytes);
    }

    #[test]
    fn uuid_renders_as_text_kind() {
        let formatted = format_value(&Value::Uuid(
            "550e8400-e29b-41d4-a716-446655440000".to_owned(),
        ));
        assert_eq!(formatted.text, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(formatted.kind, ValueKind::Text);
    }

    #[test]
    fn timestamp_passes_through_as_its_own_kind() {
        let formatted = format_value(&Value::Timestamp("2026-07-14T09:12:31+00:00".to_owned()));
        assert_eq!(formatted.text, "2026-07-14T09:12:31+00:00");
        assert_eq!(formatted.kind, ValueKind::Timestamp);
    }

    #[test]
    fn json_passes_through() {
        let formatted = format_value(&Value::Json(r#"{"coupon":"WELCOME"}"#.to_owned()));
        assert_eq!(formatted.text, r#"{"coupon":"WELCOME"}"#);
        assert_eq!(formatted.kind, ValueKind::Json);
    }

    #[test]
    fn array_renders_as_brace_list_and_recurses_into_elements() {
        let formatted = format_value(&Value::Array(vec![
            Value::Int(1),
            Value::Null,
            Value::Text("x".to_owned()),
        ]));
        assert_eq!(formatted.text, "{1,NULL,x}");
        assert_eq!(formatted.kind, ValueKind::Unknown);
    }

    #[test]
    fn unknown_passes_through_the_backends_text_rendering_when_present() {
        let formatted = format_value(&Value::Unknown(UnknownValue::Text("(1,2)".to_owned())));
        assert_eq!(formatted.text, "(1,2)");
        assert_eq!(formatted.kind, ValueKind::Unknown);
    }

    #[test]
    fn unknown_renders_a_placeholder_when_no_text_is_carried() {
        let formatted = format_value(&Value::Unknown(UnknownValue::None));
        assert_eq!(formatted.text, "?");
        assert_eq!(formatted.kind, ValueKind::Unknown);
    }

    #[test]
    fn unknown_bytes_render_as_hex_bytea_literal() {
        let formatted = format_value(&Value::Unknown(UnknownValue::Bytes(vec![0x01, 0xAB, 0xff])));
        assert_eq!(formatted.text, "\\x01abff");
        assert_eq!(formatted.kind, ValueKind::Unknown);
    }

    #[test]
    fn value_kind_color_maps_every_value_kind_to_its_named_color_role() {
        let theme = Theme::default();
        let colors = theme.colors;
        assert_eq!(ValueKind::Null.color(&theme), colors.value_null);
        assert_eq!(ValueKind::Bool.color(&theme), colors.value_bool);
        assert_eq!(ValueKind::Number.color(&theme), colors.value_number);
        assert_eq!(ValueKind::Text.color(&theme), colors.value_text);
        assert_eq!(ValueKind::Json.color(&theme), colors.value_json);
        assert_eq!(ValueKind::Timestamp.color(&theme), colors.value_timestamp);
        assert_eq!(ValueKind::Bytes.color(&theme), colors.value_bytes);
        assert_eq!(ValueKind::Unknown.color(&theme), colors.value_unknown);
    }

    #[test]
    fn base64_encode_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn row_as_json_covers_null_number_text_and_json_cells() {
        let columns = vec![
            column("id", "int8"),
            column("note", "text"),
            column("deleted_at", "timestamptz"),
            column("payload", "jsonb"),
        ];
        let row = Row(vec![
            Value::Int(7),
            Value::Text("hi".to_owned()),
            Value::Null,
            Value::Json(r#"{"a":1}"#.to_owned()),
        ]);
        let json = row_as_json(&row, &columns);
        assert_eq!(json["id"], serde_json::json!(7));
        assert_eq!(json["note"], serde_json::json!("hi"));
        assert_eq!(json["deleted_at"], serde_json::Value::Null);
        assert_eq!(json["payload"], serde_json::json!({"a": 1}));
    }

    #[test]
    fn row_as_json_falls_back_to_a_string_for_a_json_cell_that_fails_to_parse() {
        let columns = vec![column("payload", "jsonb")];
        let row = Row(vec![Value::Json("not json".to_owned())]);
        let json = row_as_json(&row, &columns);
        assert_eq!(json["payload"], serde_json::json!("not json"));
    }

    #[test]
    fn value_to_json_encodes_bytes_as_base64() {
        let json = value_to_json(&Value::Bytes(b"foo".to_vec()));
        assert_eq!(json, serde_json::json!("Zm9v"));
    }

    #[test]
    fn value_to_json_recurses_into_array_elements() {
        let json = value_to_json(&Value::Array(vec![
            Value::Int(1),
            Value::Text("two".to_owned()),
            Value::Null,
        ]));
        assert_eq!(json, serde_json::json!([1, "two", null]));
    }

    #[test]
    fn value_to_json_maps_a_non_finite_float_to_null() {
        assert_eq!(
            value_to_json(&Value::Float(f64::NAN)),
            serde_json::Value::Null
        );
        assert_eq!(
            value_to_json(&Value::Float(f64::INFINITY)),
            serde_json::Value::Null
        );
    }

    #[test]
    fn value_to_json_maps_bool_numeric_timestamp_uuid_and_unknown() {
        assert_eq!(value_to_json(&Value::Bool(true)), serde_json::json!(true));
        assert_eq!(value_to_json(&Value::Bool(false)), serde_json::json!(false));

        // Numeric's exact source digits, longer than any float can hold
        // safely, must survive as a JSON string rather than a lossy number.
        let exact = "123456789012345678901234567890.123456789012345";
        assert_eq!(
            value_to_json(&Value::Numeric(exact.to_owned())),
            serde_json::json!(exact)
        );

        assert_eq!(
            value_to_json(&Value::Timestamp("2026-07-14T09:12:31+00:00".to_owned())),
            serde_json::json!("2026-07-14T09:12:31+00:00")
        );

        assert_eq!(
            value_to_json(&Value::Uuid(
                "550e8400-e29b-41d4-a716-446655440000".to_owned()
            )),
            serde_json::json!("550e8400-e29b-41d4-a716-446655440000")
        );

        assert_eq!(
            value_to_json(&Value::Unknown(UnknownValue::Text("(1,2)".to_owned()))),
            serde_json::json!("(1,2)")
        );
        assert_eq!(
            value_to_json(&Value::Unknown(UnknownValue::None)),
            serde_json::json!("?")
        );
    }

    #[test]
    fn row_as_csv_string_escapes_cells_with_commas_quotes_or_newlines() {
        let row = Row(vec![
            Value::Text("simple".to_owned()),
            Value::Text("with,comma".to_owned()),
            Value::Text("with\"quote".to_owned()),
            Value::Text("with\nnewline".to_owned()),
        ]);
        let csv = super::row_as_csv_string(&row);
        assert_eq!(
            csv,
            "simple,\"with,comma\",\"with\"\"quote\",\"with\nnewline\""
        );
    }
}
