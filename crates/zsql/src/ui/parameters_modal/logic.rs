//! Pure, `gpui`-free helpers for the "Run with parameters" modal: turning a
//! detected [`Parameter`] list plus its source SQL into the per-row context
//! the view renders (name, type badge, line, and the highlighted query-line
//! snippet).

use zsql_core::sql::params::{ParamKind, ParamOccurrence, ParamType, Parameter};

/// One parameter row's display context: its name, kind (deciding the
/// row's own label, `:name`, `@name`, or `?1`), the identifier its value
/// and remembered history are keyed by (see [`Parameter::storage_key`]),
/// inferred type, the 1-based line its first occurrence appears on, that
/// line's text, and the real token's byte range within `line_text` for
/// highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowContext {
    pub name: String,
    pub kind: ParamKind,
    pub key: String,
    pub param_type: ParamType,
    pub line: usize,
    pub line_text: String,
    pub token_start: usize,
    pub token_end: usize,
}

/// `sql`'s line containing `occurrence`, plus its real token's (start, end)
/// byte range within that line.
fn line_snippet(sql: &str, occurrence: &ParamOccurrence) -> (String, usize, usize) {
    let offset = occurrence.offset;
    let line_start = sql[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line_end = sql[offset..].find('\n').map_or(sql.len(), |i| offset + i);
    let line_text = sql[line_start..line_end].to_owned();
    let token_start = offset - line_start;
    let token_end = token_start + occurrence.token_len;
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
            let (line_text, token_start, token_end) = line_snippet(sql, first);
            Some(RowContext {
                name: parameter.name.clone(),
                kind: parameter.kind,
                key: parameter.storage_key(),
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
    use zsql_core::sql::params::{ParamKind, ParamType, detect_parameters};

    use super::build_row_contexts;

    const POSTGRES: &str = "postgres";
    const MYSQL: &str = "mysql";
    const MSSQL: &str = "mssql";

    #[test]
    fn a_single_line_query_produces_a_row_context_matching_its_own_text() {
        let sql = "SELECT * FROM orders WHERE status = :status";
        let parameters = detect_parameters(sql, POSTGRES);
        let rows = build_row_contexts(sql, &parameters);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "status");
        assert_eq!(rows[0].kind, ParamKind::Colon);
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
        let parameters = detect_parameters(sql, POSTGRES);
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
        let parameters = detect_parameters(sql, POSTGRES);
        let rows = build_row_contexts(sql, &parameters);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            &rows[0].line_text[rows[0].token_start..rows[0].token_end],
            ":status"
        );
        assert!(rows[0].token_start < "WHERE status = ".len() + 1);
    }

    #[test]
    fn a_positional_row_labels_as_a_bare_question_mark_number_and_highlights_the_bare_token() {
        let sql = "WHERE status = ? OR prior_status = ?";
        let parameters = detect_parameters(sql, MYSQL);
        let rows = build_row_contexts(sql, &parameters);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "?1");
        assert_eq!(rows[0].kind, ParamKind::Positional);
        assert_eq!(rows[0].key, "?1");
        assert_eq!(
            &rows[0].line_text[rows[0].token_start..rows[0].token_end],
            "?"
        );
        assert_eq!(rows[1].name, "?2");
        assert_eq!(rows[1].key, "?2");
        assert_eq!(
            &rows[1].line_text[rows[1].token_start..rows[1].token_end],
            "?"
        );
    }

    #[test]
    fn an_at_name_row_on_mssql_highlights_the_real_at_name_token_and_is_keyed_with_its_at_sign() {
        let sql = "SELECT * FROM orders WHERE start_date >= @start_date";
        let parameters = detect_parameters(sql, MSSQL);
        let rows = build_row_contexts(sql, &parameters);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "start_date");
        assert_eq!(rows[0].kind, ParamKind::At);
        assert_eq!(rows[0].key, "@start_date");
        assert_eq!(
            &rows[0].line_text[rows[0].token_start..rows[0].token_end],
            "@start_date"
        );
    }

    #[test]
    fn a_colon_and_at_row_sharing_the_same_identifier_carry_different_storage_keys() {
        let sql = "WHERE status = :status OR legacy_status = @status";
        let parameters = detect_parameters(sql, MSSQL);
        let rows = build_row_contexts(sql, &parameters);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].key, "status");
        assert_eq!(rows[1].key, "@status");
    }
}
