//! Engine-neutral result values

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
/// degrade to [`Value::Unknown`]
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

/// A batch of rows
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

impl ResultSet {
    /// Whether this result has exactly one column whose non-null cells are
    /// all [`Value::Text`], with at least one non-null cell present. Nulls
    /// are tolerated (mixed in among text cells), but a column that is
    /// entirely null does not count -- there is no text to read. Uses
    /// `Value`'s own engine-neutral variant rather than a column's
    /// backend-reported `type_name`, whose spelling (`nvarchar` vs `text`
    /// vs `TEXT`) varies per driver.
    #[must_use]
    pub fn has_single_text_column(&self) -> bool {
        if self.columns.len() != 1 {
            return false;
        }
        let mut saw_text = false;
        for row in &self.rows {
            match row.0.first() {
                Some(Value::Text(_)) => saw_text = true,
                Some(Value::Null) | None => {}
                Some(_) => return false,
            }
        }
        saw_text
    }

    /// Whether this result reads as a document rather than a table: a
    /// single text-typed column (see
    /// [`ResultSet::has_single_text_column`]) and either more than one row,
    /// or exactly one row whose text contains a newline. A single-row value
    /// with no newline is left as an ordinary one-cell result rather than
    /// promoted to a document view.
    #[must_use]
    pub fn is_document_shaped(&self) -> bool {
        if !self.has_single_text_column() {
            return false;
        }
        match self.rows.len() {
            0 => false,
            1 => matches!(&self.rows[0].0[0], Value::Text(text) if text.contains('\n')),
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ColumnMeta, ResultSet, Row, RowBatch, Value};

    fn text_column() -> ColumnMeta {
        ColumnMeta {
            name: "Text".to_owned(),
            type_name: "nvarchar".to_owned(),
            nullable: true,
        }
    }

    fn int_column() -> ColumnMeta {
        ColumnMeta {
            name: "n".to_owned(),
            type_name: "int8".to_owned(),
            nullable: false,
        }
    }

    fn result_of(columns: Vec<ColumnMeta>, rows: Vec<Row>) -> ResultSet {
        ResultSet {
            columns,
            rows,
            affected: None,
            notices: Vec::new(),
        }
    }

    // -- has_single_text_column -------------------------------------------

    #[test]
    fn has_single_text_column_is_true_for_a_single_all_text_column() {
        let result = result_of(
            vec![text_column()],
            vec![
                Row(vec![Value::Text("a".to_owned())]),
                Row(vec![Value::Text("b".to_owned())]),
            ],
        );
        assert!(result.has_single_text_column());
    }

    #[test]
    fn has_single_text_column_tolerates_nulls_mixed_with_text() {
        let result = result_of(
            vec![text_column()],
            vec![
                Row(vec![Value::Null]),
                Row(vec![Value::Text("a".to_owned())]),
            ],
        );
        assert!(
            result.has_single_text_column(),
            "a null cell alongside a text cell must not disqualify the column"
        );
    }

    #[test]
    fn has_single_text_column_is_false_when_every_cell_is_null() {
        let result = result_of(
            vec![text_column()],
            vec![Row(vec![Value::Null]), Row(vec![Value::Null])],
        );
        assert!(
            !result.has_single_text_column(),
            "an all-null column has no text to read, so it must not vacuously count as text-typed"
        );
    }

    #[test]
    fn has_single_text_column_is_false_for_a_non_text_column() {
        let result = result_of(vec![int_column()], vec![Row(vec![Value::Int(1)])]);
        assert!(!result.has_single_text_column());
    }

    #[test]
    fn has_single_text_column_is_false_for_two_columns_even_if_both_are_text() {
        let result = result_of(
            vec![text_column(), text_column()],
            vec![Row(vec![
                Value::Text("a".to_owned()),
                Value::Text("b".to_owned()),
            ])],
        );
        assert!(!result.has_single_text_column());
    }

    #[test]
    fn has_single_text_column_is_false_for_zero_columns() {
        let result = result_of(Vec::new(), Vec::new());
        assert!(!result.has_single_text_column());
    }

    #[test]
    fn has_single_text_column_is_true_with_no_newline_when_multiple_rows_are_present() {
        // The row-count/newline condition governs is_document_shaped's
        // default, not whether the switch is enabled at all: a multi-row
        // text column with no newlines anywhere is still text-typed.
        let result = result_of(
            vec![text_column()],
            vec![
                Row(vec![Value::Text("a".to_owned())]),
                Row(vec![Value::Text("b".to_owned())]),
            ],
        );
        assert!(result.has_single_text_column());
    }

    // -- is_document_shaped -------------------------------------------------

    #[test]
    fn is_document_shaped_is_true_for_multiple_rows_in_a_single_text_column() {
        let result = result_of(
            vec![text_column()],
            vec![
                Row(vec![Value::Text("line one".to_owned())]),
                Row(vec![Value::Text("line two".to_owned())]),
            ],
        );
        assert!(result.is_document_shaped());
    }

    #[test]
    fn is_document_shaped_is_true_for_a_single_row_containing_a_newline() {
        let result = result_of(
            vec![text_column()],
            vec![Row(vec![Value::Text("line one\nline two".to_owned())])],
        );
        assert!(result.is_document_shaped());
    }

    #[test]
    fn is_document_shaped_is_false_for_a_single_row_with_no_newline() {
        let result = result_of(
            vec![text_column()],
            vec![Row(vec![Value::Text("just one line".to_owned())])],
        );
        assert!(!result.is_document_shaped());
    }

    #[test]
    fn is_document_shaped_is_false_for_zero_rows() {
        let result = result_of(vec![text_column()], Vec::new());
        assert!(!result.is_document_shaped());
    }

    #[test]
    fn is_document_shaped_is_false_for_zero_columns() {
        let result = result_of(Vec::new(), Vec::new());
        assert!(!result.is_document_shaped());
    }

    #[test]
    fn is_document_shaped_is_false_for_multiple_columns() {
        let result = result_of(
            vec![text_column(), text_column()],
            vec![Row(vec![
                Value::Text("a\nb".to_owned()),
                Value::Text("c\nd".to_owned()),
            ])],
        );
        assert!(!result.is_document_shaped());
    }

    #[test]
    fn is_document_shaped_is_false_for_a_non_text_single_column() {
        let result = result_of(
            vec![int_column()],
            vec![Row(vec![Value::Int(1)]), Row(vec![Value::Int(2)])],
        );
        assert!(!result.is_document_shaped());
    }

    #[test]
    fn is_document_shaped_is_false_when_the_single_text_column_is_entirely_null() {
        let result = result_of(
            vec![text_column()],
            vec![Row(vec![Value::Null]), Row(vec![Value::Null])],
        );
        assert!(
            !result.is_document_shaped(),
            "an all-null column must not vacuously read as document-shaped"
        );
    }

    #[test]
    fn row_batch_push_and_len() {
        let mut batch = RowBatch::new();
        assert!(batch.is_empty());
        batch.push(Row(vec![Value::Int(1), Value::Null]));
        batch.push(Row(vec![Value::Text("hi".to_owned()), Value::Bool(true)]));
        assert_eq!(batch.len(), 2);
    }
}
