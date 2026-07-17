//! Formats `zsql_core::Value` cells for display in the results grid, and
//! classifies each value into a semantic [`ValueKind`] used to pick a
//! per-kind text color/style. Pure Rust: no gpui, no database, so this is
//! unit-testable on its own with no window and no connection.

use std::fmt::Write as _;

use zsql_core::Value;

/// Semantic category of a formatted cell, used to select a per-kind text
/// color/style in the results grid. [`ValueKind::Null`] is its own kind
/// (rendered faint and italic) so it never gets confused with an empty
/// [`Value::Text`], which is ordinary text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// SQL NULL — the literal text `NULL`, styled distinctly from text.
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
    /// Anything that doesn't map to a more specific kind: arrays and the
    /// backend's own fallback text rendering for unmapped types.
    Unknown,
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
        // UUIDs have no dedicated kind of their own: they render as plain text.
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
        Value::Unknown(text) => FormattedValue {
            text: text.clone(),
            kind: ValueKind::Unknown,
        },
    }
}

/// Render a float without a trailing-zero ambiguity: whole numbers still get
/// one decimal place so `2.0` reads as a float rather than an integer that
/// silently lost its type.
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
/// formatted elements (Postgres' text array literal shape).
fn format_array(items: &[Value]) -> String {
    let rendered: Vec<String> = items.iter().map(|item| format_value(item).text).collect();
    format!("{{{}}}", rendered.join(","))
}

#[cfg(test)]
mod tests {
    use zsql_core::Value;

    use super::{ValueKind, format_value};

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
    fn unknown_passes_through_the_backends_text_rendering() {
        let formatted = format_value(&Value::Unknown("(1,2)".to_owned()));
        assert_eq!(formatted.text, "(1,2)");
        assert_eq!(formatted.kind, ValueKind::Unknown);
    }
}
