//! Pure, `gpui`-free helpers for the "Run with parameters" modal: turning a
//! detected [`Parameter`] list plus its source SQL into the per-row context
//! the view renders (name, type badge, line, and the highlighted query-line
//! snippet).

use zsql_core::sql::params::{ParamType, Parameter};

/// One parameter row's display context: its name, inferred type, the
/// 1-based line its first occurrence appears on, that line's text, and the
/// `:name` token's byte range within `line_text` for highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowContext {
    pub name: String,
    pub param_type: ParamType,
    pub line: usize,
    pub line_text: String,
    pub token_start: usize,
    pub token_end: usize,
}

/// `sql`'s line containing byte offset `offset`, plus the `:name` token's
/// (start, end) byte range within that line, where the token is `1 +
/// name_len` bytes long (the leading colon plus the identifier).
fn line_snippet(sql: &str, offset: usize, name_len: usize) -> (String, usize, usize) {
    let line_start = sql[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line_end = sql[offset..].find('\n').map_or(sql.len(), |i| offset + i);
    let line_text = sql[line_start..line_end].to_owned();
    let token_start = offset - line_start;
    let token_end = token_start + 1 + name_len;
    (line_text, token_start, token_end)
}

/// Build one [`RowContext`] per parameter, in `parameters`' own order,
/// anchored to each parameter's first occurrence.
#[must_use]
pub fn build_row_contexts(sql: &str, parameters: &[Parameter]) -> Vec<RowContext> {
    parameters
        .iter()
        .filter_map(|parameter| {
            let first = parameter.occurrences.first()?;
            let (line_text, token_start, token_end) =
                line_snippet(sql, first.offset, parameter.name.len());
            Some(RowContext {
                name: parameter.name.clone(),
                param_type: parameter.inferred_type,
                line: first.line,
                line_text,
                token_start,
                token_end,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use zsql_core::sql::params::{ParamType, detect_parameters};

    use super::build_row_contexts;

    #[test]
    fn a_single_line_query_produces_a_row_context_matching_its_own_text() {
        let sql = "SELECT * FROM orders WHERE status = :status";
        let parameters = detect_parameters(sql);
        let rows = build_row_contexts(sql, &parameters);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "status");
        assert_eq!(rows[0].param_type, ParamType::Text);
        assert_eq!(rows[0].line, 1);
        assert_eq!(rows[0].line_text, sql);
        assert_eq!(
            &rows[0].line_text[rows[0].token_start..rows[0].token_end],
            ":status"
        );
    }

    #[test]
    fn a_multiline_query_anchors_each_row_to_its_own_line() {
        let sql = "SELECT *\nFROM orders\nWHERE created_at >= :start_date\n  AND status = :status";
        let parameters = detect_parameters(sql);
        let rows = build_row_contexts(sql, &parameters);
        assert_eq!(rows.len(), 2);

        assert_eq!(rows[0].line, 3);
        assert_eq!(rows[0].line_text, "WHERE created_at >= :start_date");
        assert_eq!(
            &rows[0].line_text[rows[0].token_start..rows[0].token_end],
            ":start_date"
        );

        assert_eq!(rows[1].line, 4);
        assert_eq!(rows[1].line_text, "  AND status = :status");
        assert_eq!(
            &rows[1].line_text[rows[1].token_start..rows[1].token_end],
            ":status"
        );
    }

    #[test]
    fn a_repeated_parameter_anchors_to_its_first_occurrence_only() {
        let sql = "WHERE status = :status OR prior_status = :status";
        let parameters = detect_parameters(sql);
        let rows = build_row_contexts(sql, &parameters);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            &rows[0].line_text[rows[0].token_start..rows[0].token_end],
            ":status"
        );
        assert!(rows[0].token_start < "WHERE status = ".len() + 1);
    }
}
