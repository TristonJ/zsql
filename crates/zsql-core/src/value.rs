//! Engine-neutral result values. The UI never sees a backend-specific type.

/// Metadata for one result column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnMeta {
    /// Column name as reported by the backend.
    pub name: String,
    /// Backend type name (e.g. `int8`, `text`), for display and formatting.
    pub type_name: String,
    /// Whether the column may be null.
    pub nullable: bool,
}

/// A single cell value, normalized across backends. Unknown/unsupported types
/// degrade to [`Value::Unknown`] (the backend's text rendering) rather than
/// erroring, so a novel column type never breaks a query.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// SQL NULL.
    Null,
    /// Boolean.
    Bool(bool),
    /// Signed integer (all integer widths widen to `i64`).
    Int(i64),
    /// Floating point.
    Float(f64),
    /// Exact decimal, kept as text to avoid precision loss.
    Numeric(String),
    /// UTF-8 text.
    Text(String),
    /// Raw bytes.
    Bytes(Vec<u8>),
    /// UUID rendered as text.
    Uuid(String),
    /// Timestamp rendered as ISO-8601 text (v0 keeps time as text).
    Timestamp(String),
    /// JSON rendered as text.
    Json(String),
    /// Array of values.
    Array(Vec<Value>),
    /// Fallback: the backend's own text rendering for an unmapped type.
    Unknown(String),
}

/// One result row: a positional list of cells matching the column list.
#[derive(Debug, Clone, PartialEq)]
pub struct Row(pub Vec<Value>);

/// A batch of rows, the unit streamed from driver to UI.
#[derive(Debug, Clone, Default)]
pub struct RowBatch {
    /// Rows in this batch.
    pub rows: Vec<Row>,
}

impl RowBatch {
    /// An empty batch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a row.
    pub fn push(&mut self, row: Row) {
        self.rows.push(row);
    }

    /// Number of rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the batch is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// A fully materialized result set (used for small/eager results and tests;
/// large results stream as [`RowBatch`]es).
#[derive(Debug, Clone, Default)]
pub struct ResultSet {
    /// Column metadata.
    pub columns: Vec<ColumnMeta>,
    /// All rows.
    pub rows: Vec<Row>,
    /// Rows affected, for non-SELECT statements.
    pub affected: Option<u64>,
    /// Server notices/warnings.
    pub notices: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{Row, RowBatch, Value};

    #[test]
    fn row_batch_push_and_len() {
        let mut batch = RowBatch::new();
        assert!(batch.is_empty());
        batch.push(Row(vec![Value::Int(1), Value::Null]));
        batch.push(Row(vec![Value::Text("hi".to_owned()), Value::Bool(true)]));
        assert_eq!(batch.len(), 2);
    }
}
