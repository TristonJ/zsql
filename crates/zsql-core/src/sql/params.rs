//! Named `:param` detection, heuristic type inference, and safe client-side
//! substitution over raw SQL text, independent of any driver or UI.

use std::collections::HashMap;

use crate::filter::quote_sql_string;

/// One `:name` token found in SQL text: the parameter it names, the byte
/// offset of its leading colon, and the 1-based line it appears on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamOccurrence {
    pub name: String,
    pub offset: usize,
    pub line: usize,
}

/// A heuristically inferred display type for a parameter. Advisory only:
/// it never changes how a value is escaped or validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    Date,
    Numeric,
    Integer,
    Text,
}

impl ParamType {
    /// The uppercase badge label this type renders as.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ParamType::Date => "DATE",
            ParamType::Numeric => "NUMERIC",
            ParamType::Integer => "INTEGER",
            ParamType::Text => "TEXT",
        }
    }
}

/// One logical `:name` parameter: every occurrence of that name in the SQL
/// text, and its heuristically inferred type (from the first occurrence's
/// surrounding expression).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub name: String,
    pub occurrences: Vec<ParamOccurrence>,
    pub inferred_type: ParamType,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The lexical region the scan is currently inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    Normal,
    SingleQuoted,
    DoubleQuoted,
    LineComment,
    BlockComment,
}

/// Advance past one byte of a quoted region delimited by `quote` (a single
/// or double quote), where a doubled `quote` is an escaped quote inside the
/// region, not its close. Returns the next index and the region's state:
/// still `quote_state` while open, [`ScanState::Normal`] once closed.
fn advance_quoted(
    bytes: &[u8],
    i: usize,
    len: usize,
    quote: u8,
    quote_state: ScanState,
) -> (usize, ScanState) {
    if bytes[i] != quote {
        return (i + 1, quote_state);
    }
    if i + 1 < len && bytes[i + 1] == quote {
        return (i + 2, quote_state);
    }
    (i + 1, ScanState::Normal)
}

/// Advance past one byte of a `/* */` block comment, closing it on `*/`.
fn advance_block_comment(bytes: &[u8], i: usize, len: usize) -> (usize, ScanState) {
    if bytes[i] == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
        (i + 2, ScanState::Normal)
    } else {
        (i + 1, ScanState::BlockComment)
    }
}

/// Advance past one byte of a `--` line comment, closing it after `\n`.
fn advance_line_comment(bytes: &[u8], i: usize) -> (usize, ScanState) {
    if bytes[i] == b'\n' {
        (i + 1, ScanState::Normal)
    } else {
        (i + 1, ScanState::LineComment)
    }
}

/// Advance past one byte outside any quoted region or comment: recognizes
/// where a quoted region or comment starts, a Postgres `::` cast operator,
/// and a `:name` parameter token (returned as an occurrence, using `line`
/// as its 1-based line).
fn advance_normal(
    sql: &str,
    bytes: &[u8],
    i: usize,
    len: usize,
    line: usize,
) -> (usize, ScanState, Option<ParamOccurrence>) {
    let b = bytes[i];
    if b == b'-' && i + 1 < len && bytes[i + 1] == b'-' {
        return (i + 2, ScanState::LineComment, None);
    }
    if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
        return (i + 2, ScanState::BlockComment, None);
    }
    if b == b'\'' {
        return (i + 1, ScanState::SingleQuoted, None);
    }
    if b == b'"' {
        return (i + 1, ScanState::DoubleQuoted, None);
    }
    if b != b':' {
        return (i + 1, ScanState::Normal, None);
    }
    if i + 1 < len && bytes[i + 1] == b':' {
        // Postgres cast operator (`total::numeric`), not a parameter.
        return (i + 2, ScanState::Normal, None);
    }
    if i + 1 < len && is_ident_start(bytes[i + 1]) {
        let mut j = i + 1;
        while j < len && is_ident_continue(bytes[j]) {
            j += 1;
        }
        let occurrence = ParamOccurrence {
            name: sql[i + 1..j].to_owned(),
            offset: i,
            line,
        };
        return (j, ScanState::Normal, Some(occurrence));
    }
    (i + 1, ScanState::Normal, None)
}

/// Scan `sql` for every `:name` parameter token: identifier characters
/// only, matching `:start_date` syntax. Skips a token inside a single-
/// quoted string literal, a double-quoted identifier, a `--` line comment,
/// a `/* */` block comment, or a Postgres `::type` cast (so
/// `total::numeric` is never read as parameter `:numeric`).
#[must_use]
#[tracing::instrument(skip(sql))]
pub fn find_param_occurrences(sql: &str) -> Vec<ParamOccurrence> {
    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut occurrences = Vec::new();
    let mut i = 0usize;
    let mut line = 1usize;
    let mut state = ScanState::Normal;

    while i < len {
        if bytes[i] == b'\n' {
            line += 1;
        }
        let (next_i, next_state) = match state {
            ScanState::Normal => {
                let (next_i, next_state, occurrence) = advance_normal(sql, bytes, i, len, line);
                if let Some(occurrence) = occurrence {
                    occurrences.push(occurrence);
                }
                (next_i, next_state)
            }
            ScanState::SingleQuoted => advance_quoted(bytes, i, len, b'\'', state),
            ScanState::DoubleQuoted => advance_quoted(bytes, i, len, b'"', state),
            ScanState::LineComment => advance_line_comment(bytes, i),
            ScanState::BlockComment => advance_block_comment(bytes, i, len),
        };
        i = next_i;
        state = next_state;
    }

    occurrences
}

/// Words whose presence in a column name or parameter name suggests a
/// date/timestamp-shaped value.
const DATE_HINTS: [&str; 3] = ["date", "_at", "timestamp"];
/// Words suggesting a fractional numeric value.
const NUMERIC_HINTS: [&str; 5] = ["total", "amount", "price", "cost", "sum"];
/// Words suggesting a whole-number value.
const INTEGER_HINTS: [&str; 4] = ["limit", "count", "qty", "quantity"];

fn classify_hint(text: &str) -> Option<ParamType> {
    let lower = text.to_ascii_lowercase();
    if DATE_HINTS.iter().any(|hint| lower.contains(hint)) {
        return Some(ParamType::Date);
    }
    if NUMERIC_HINTS.iter().any(|hint| lower.contains(hint)) {
        return Some(ParamType::Numeric);
    }
    if INTEGER_HINTS.iter().any(|hint| lower.contains(hint)) {
        return Some(ParamType::Integer);
    }
    None
}

/// A Postgres cast type name to the badge it implies, or `None` for a cast
/// this heuristic does not recognize.
fn classify_cast(cast: &str) -> Option<ParamType> {
    match cast.to_ascii_lowercase().as_str() {
        "numeric" | "decimal" | "real" | "float" | "float4" | "float8" => Some(ParamType::Numeric),
        "int" | "int2" | "int4" | "int8" | "integer" | "bigint" | "smallint" => {
            Some(ParamType::Integer)
        }
        "date" | "timestamp" | "timestamptz" | "time" | "timetz" => Some(ParamType::Date),
        "text" | "varchar" | "char" | "citext" => Some(ParamType::Text),
        _ => None,
    }
}

/// The identifier (possibly `alias.column`) immediately left of the last
/// comparison operator in `context`, if any: the rightmost-ending match
/// among `>=`, `<=`, `<>`, `!=`, `=`, `<`, `>`, so e.g. `>=` wins over the
/// `>` it contains.
fn column_before_operator(context: &str) -> Option<&str> {
    const OPERATORS: [&str; 7] = [">=", "<=", "<>", "!=", "=", "<", ">"];
    let trimmed = context.trim_end();
    let mut best: Option<(usize, usize)> = None;
    for op in OPERATORS {
        if let Some(start) = trimmed.rfind(op) {
            let end = start + op.len();
            if best.is_none_or(|(_, best_end)| end > best_end) {
                best = Some((start, end));
            }
        }
    }
    let (start, _) = best?;
    let before_op = trimmed[..start].trim_end();
    let word_start = before_op
        .rfind(|c: char| c.is_whitespace() || c == '(' || c == ',')
        .map_or(0, |i| i + 1);
    let word = &before_op[word_start..];
    if word.is_empty() { None } else { Some(word) }
}

/// Heuristically infer `occurrence`'s type from its surrounding SQL text: a
/// `::type` cast immediately after it, a `LIMIT` keyword immediately
/// before it, the column compared against it on the same line, or (as a
/// last resort) hints in the parameter's own name. Falls back to
/// [`ParamType::Text`] when nothing more specific matches; no general SQL
/// type checker is implemented.
#[must_use]
fn infer_param_type(sql: &str, occurrence: &ParamOccurrence) -> ParamType {
    let token_end = occurrence.offset + 1 + occurrence.name.len();
    if let Some(after) = sql.get(token_end..)
        && let Some(rest) = after.strip_prefix("::")
    {
        let cast: String = rest.chars().take_while(char::is_ascii_alphabetic).collect();
        if let Some(inferred) = classify_cast(&cast) {
            return inferred;
        }
    }

    let before = &sql[..occurrence.offset];
    let trimmed_before = before.trim_end();
    let last_word = trimmed_before
        .rsplit(|c: char| !c.is_ascii_alphabetic())
        .next();
    if last_word.is_some_and(|word| word.eq_ignore_ascii_case("limit")) {
        return ParamType::Integer;
    }

    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let context = &sql[line_start..occurrence.offset];
    if let Some(column) = column_before_operator(context)
        && let Some(inferred) = classify_hint(column)
    {
        return inferred;
    }

    classify_hint(&occurrence.name).unwrap_or(ParamType::Text)
}

/// Detect every `:name` parameter in `sql`, collapsing repeated uses of the
/// same name into one logical [`Parameter`] (in first-occurrence order)
/// while keeping every occurrence's own line, so the UI can show each use.
#[must_use]
#[tracing::instrument(skip(sql))]
pub fn detect_parameters(sql: &str) -> Vec<Parameter> {
    let occurrences = find_param_occurrences(sql);
    let mut order: Vec<String> = Vec::new();
    let mut grouped: HashMap<String, Vec<ParamOccurrence>> = HashMap::new();
    for occurrence in occurrences {
        if !grouped.contains_key(&occurrence.name) {
            order.push(occurrence.name.clone());
        }
        grouped
            .entry(occurrence.name.clone())
            .or_default()
            .push(occurrence);
    }

    order
        .into_iter()
        .filter_map(|name| {
            let occurrences = grouped.remove(&name)?;
            let inferred_type = occurrences
                .first()
                .map_or(ParamType::Text, |occ| infer_param_type(sql, occ));
            Some(Parameter {
                name,
                occurrences,
                inferred_type,
            })
        })
        .collect()
}

/// The driver id ([`crate::driver::Driver::id`]) of the one supported
/// backend whose string literals treat a backslash as an escape character
/// by default, rather than a literal character: MySQL/MariaDB (unless a
/// connection sets `NO_BACKSLASH_ESCAPES`). Every other backend this
/// substitution path supports uses standard-conforming string literals,
/// where a backslash carries no special meaning.
const MYSQL_DRIVER_ID: &str = "mysql";

/// Quote `value` as a SQL string literal for `driver_id`'s dialect: always
/// doubles an embedded single quote via [`quote_sql_string`], and for
/// [`MYSQL_DRIVER_ID`] also doubles an embedded backslash first, so a
/// value ending in (or containing) a backslash can never escape the
/// literal's closing quote.
fn quote_param_value(value: &str, driver_id: &str) -> String {
    if driver_id == MYSQL_DRIVER_ID {
        quote_sql_string(&value.replace('\\', "\\\\"))
    } else {
        quote_sql_string(value)
    }
}

/// Replace every occurrence of every parameter in `parameters` with its
/// value from `values`, each rendered as an escaped SQL string literal for
/// `driver_id`'s dialect (see [`quote_param_value`]). A name absent from
/// `values` substitutes as an empty string literal.
///
/// This always renders a client-side string literal, never a real driver
/// bind parameter: `sql::Connection::stream_query` takes a plain `String`,
/// and no driver crate threads typed bind parameters through. The tradeoff
/// is that every value is escaped, not type-checked, by the database; the
/// inferred type badges shown alongside a parameter's input are advisory
/// display only and never change how its value is escaped here.
///
/// Escaping for every non-MySQL dialect assumes standard-conforming
/// string literals, where a backslash is a literal character; a server
/// configured otherwise (e.g. `standard_conforming_strings = off`) is out
/// of scope.
#[must_use]
#[tracing::instrument(skip(sql, parameters, values))]
// Every caller passes the standard hasher; generalizing over `BuildHasher`
// would add a type parameter with no real flexibility gained here.
#[allow(clippy::implicit_hasher)]
pub fn substitute_params(
    sql: &str,
    parameters: &[Parameter],
    values: &HashMap<String, String>,
    driver_id: &str,
) -> String {
    let mut occurrences: Vec<&ParamOccurrence> = parameters
        .iter()
        .flat_map(|parameter| parameter.occurrences.iter())
        .collect();
    occurrences.sort_by_key(|occurrence| std::cmp::Reverse(occurrence.offset));

    let mut result = sql.to_owned();
    for occurrence in occurrences {
        let end = occurrence.offset + 1 + occurrence.name.len();
        let value = values.get(&occurrence.name).map_or("", String::as_str);
        let literal = quote_param_value(value, driver_id);
        result.replace_range(occurrence.offset..end, &literal);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{ParamType, detect_parameters, find_param_occurrences, substitute_params};

    #[test]
    fn a_query_with_no_parameters_finds_nothing() {
        let occurrences = find_param_occurrences("SELECT * FROM orders");
        assert!(occurrences.is_empty());
    }

    #[test]
    fn finds_a_single_parameter_with_its_offset_and_line() {
        let sql = "SELECT * FROM orders WHERE status = :status";
        let occurrences = find_param_occurrences(sql);
        assert_eq!(occurrences.len(), 1);
        assert_eq!(occurrences[0].name, "status");
        assert_eq!(occurrences[0].line, 1);
        assert_eq!(
            &sql[occurrences[0].offset..occurrences[0].offset + 7],
            ":status"
        );
    }

    #[test]
    fn finds_multiple_distinct_parameters_across_lines_with_correct_line_numbers() {
        let sql =
            "SELECT *\nFROM orders\nWHERE created_at >= :start_date\n  AND created_at < :end_date";
        let occurrences = find_param_occurrences(sql);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["start_date", "end_date"]);
        assert_eq!(occurrences[0].line, 3);
        assert_eq!(occurrences[1].line, 4);
    }

    #[test]
    fn skips_a_colon_shaped_token_inside_a_single_quoted_string_literal() {
        let sql = "SELECT 'not :a_param here' AS label WHERE x = :real_param";
        let occurrences = find_param_occurrences(sql);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["real_param"]);
    }

    #[test]
    fn a_string_literal_with_a_doubled_quote_still_closes_correctly() {
        let sql = "SELECT 'it''s :not_a_param' WHERE x = :real_param";
        let occurrences = find_param_occurrences(sql);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["real_param"]);
    }

    #[test]
    fn skips_a_colon_shaped_token_inside_a_line_comment() {
        let sql = "SELECT 1 -- :not_a_param\nWHERE x = :real_param";
        let occurrences = find_param_occurrences(sql);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["real_param"]);
    }

    #[test]
    fn skips_a_colon_shaped_token_inside_a_block_comment() {
        let sql = "SELECT 1 /* :not_a_param\nspans lines */ WHERE x = :real_param";
        let occurrences = find_param_occurrences(sql);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["real_param"]);
    }

    #[test]
    fn skips_a_colon_shaped_token_inside_a_double_quoted_identifier() {
        let sql = "SELECT \"a:b\" FROM t WHERE x = :real_param";
        let occurrences = find_param_occurrences(sql);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["real_param"]);
    }

    #[test]
    fn a_double_quoted_identifier_with_a_doubled_quote_still_closes_correctly() {
        let sql = "SELECT \"weird\"\"ident:x\" FROM t WHERE x = :real_param";
        let occurrences = find_param_occurrences(sql);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["real_param"]);
    }

    #[test]
    fn a_postgres_cast_is_never_read_as_a_parameter() {
        let sql = "SELECT total::numeric FROM orders WHERE x = :real_param";
        let occurrences = find_param_occurrences(sql);
        let names: Vec<&str> = occurrences.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["real_param"]);
    }

    #[test]
    fn detect_parameters_collapses_repeated_uses_of_the_same_name_into_one_parameter() {
        let sql = "SELECT * FROM orders WHERE status = :status OR prior_status = :status";
        let parameters = detect_parameters(sql);
        assert_eq!(parameters.len(), 1);
        assert_eq!(parameters[0].name, "status");
        assert_eq!(parameters[0].occurrences.len(), 2);
    }

    #[test]
    fn detect_parameters_preserves_first_occurrence_order_for_distinct_names() {
        let sql = "WHERE created_at >= :start_date AND created_at < :end_date AND status = :status";
        let parameters = detect_parameters(sql);
        let names: Vec<&str> = parameters.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["start_date", "end_date", "status"]);
    }

    #[test]
    fn infers_date_from_a_created_at_style_column_comparison() {
        let sql = "SELECT * FROM orders WHERE o.created_at >= :start_date";
        let parameters = detect_parameters(sql);
        assert_eq!(parameters[0].inferred_type, ParamType::Date);
    }

    #[test]
    fn infers_numeric_from_a_total_style_column_comparison() {
        let sql = "SELECT * FROM orders WHERE o.total >= :min_total";
        let parameters = detect_parameters(sql);
        assert_eq!(parameters[0].inferred_type, ParamType::Numeric);
    }

    #[test]
    fn infers_integer_from_a_limit_clause() {
        let sql = "SELECT * FROM orders ORDER BY created_at DESC LIMIT :row_limit";
        let parameters = detect_parameters(sql);
        assert_eq!(parameters[0].inferred_type, ParamType::Integer);
    }

    #[test]
    fn infers_numeric_from_an_explicit_cast() {
        let sql = "SELECT * FROM orders WHERE o.total >= :amount::numeric";
        let parameters = detect_parameters(sql);
        assert_eq!(parameters[0].inferred_type, ParamType::Numeric);
    }

    #[test]
    fn infers_date_from_the_parameter_name_alone_when_no_context_matches() {
        let sql = "SELECT * FROM orders WHERE x = :start_date";
        let parameters = detect_parameters(sql);
        assert_eq!(parameters[0].inferred_type, ParamType::Date);
    }

    #[test]
    fn infers_integer_from_the_parameter_name_alone_when_no_context_matches() {
        let sql = "SELECT * FROM orders WHERE x = :retry_count";
        let parameters = detect_parameters(sql);
        assert_eq!(parameters[0].inferred_type, ParamType::Integer);
    }

    #[test]
    fn infers_integer_from_an_explicit_int_cast() {
        let sql = "SELECT * FROM t WHERE x = :v::int";
        let parameters = detect_parameters(sql);
        assert_eq!(parameters[0].inferred_type, ParamType::Integer);
    }

    #[test]
    fn infers_date_from_an_explicit_date_cast() {
        let sql = "SELECT * FROM t WHERE x = :v::date";
        let parameters = detect_parameters(sql);
        assert_eq!(parameters[0].inferred_type, ParamType::Date);
    }

    #[test]
    fn infers_text_from_an_explicit_text_cast() {
        let sql = "SELECT * FROM t WHERE x = :v::text";
        let parameters = detect_parameters(sql);
        assert_eq!(parameters[0].inferred_type, ParamType::Text);
    }

    #[test]
    fn an_unrecognized_cast_falls_through_to_the_other_inference_heuristics() {
        let sql = "SELECT * FROM orders WHERE o.created_at >= :cutoff::uuid";
        let parameters = detect_parameters(sql);
        assert_eq!(parameters[0].inferred_type, ParamType::Date);
    }

    #[test]
    fn an_unrecognized_cast_with_no_other_signal_falls_back_to_text() {
        let sql = "SELECT * FROM t WHERE x = :v::uuid";
        let parameters = detect_parameters(sql);
        assert_eq!(parameters[0].inferred_type, ParamType::Text);
    }

    #[test]
    fn falls_back_to_text_when_nothing_more_specific_is_inferred() {
        let sql = "SELECT * FROM orders WHERE o.status = :status";
        let parameters = detect_parameters(sql);
        assert_eq!(parameters[0].inferred_type, ParamType::Text);
    }

    const POSTGRES: &str = "postgres";
    const MYSQL: &str = "mysql";

    #[test]
    fn substitute_params_replaces_every_occurrence_of_a_repeated_name() {
        let sql = "WHERE status = :status OR prior_status = :status";
        let parameters = detect_parameters(sql);
        let mut values = HashMap::new();
        values.insert("status".to_owned(), "shipped".to_owned());
        let result = substitute_params(sql, &parameters, &values, POSTGRES);
        assert_eq!(
            result,
            "WHERE status = 'shipped' OR prior_status = 'shipped'"
        );
    }

    #[test]
    fn substitute_params_preserves_surrounding_sql_across_multiple_distinct_parameters() {
        let sql = "SELECT * FROM orders WHERE created_at >= :start_date AND status = :status LIMIT :row_limit";
        let parameters = detect_parameters(sql);
        let mut values = HashMap::new();
        values.insert("start_date".to_owned(), "2026-01-01".to_owned());
        values.insert("status".to_owned(), "shipped".to_owned());
        values.insert("row_limit".to_owned(), "50".to_owned());
        let result = substitute_params(sql, &parameters, &values, POSTGRES);
        assert_eq!(
            result,
            "SELECT * FROM orders WHERE created_at >= '2026-01-01' AND status = 'shipped' LIMIT '50'"
        );
    }

    #[test]
    fn substitute_params_with_an_empty_values_map_substitutes_an_empty_string_literal() {
        let sql = "WHERE status = :status";
        let parameters = detect_parameters(sql);
        let values = HashMap::new();
        let result = substitute_params(sql, &parameters, &values, POSTGRES);
        assert_eq!(result, "WHERE status = ''");
    }

    #[test]
    fn substitute_params_escapes_an_embedded_single_quote_so_it_cannot_break_out_of_the_literal() {
        let sql = "WHERE name = :name";
        let parameters = detect_parameters(sql);
        let mut values = HashMap::new();
        values.insert("name".to_owned(), "O'Brien".to_owned());
        let result = substitute_params(sql, &parameters, &values, POSTGRES);
        assert_eq!(result, "WHERE name = 'O''Brien'");
    }

    #[test]
    fn substitute_params_is_safe_against_a_single_quote_semicolon_drop_table_shaped_payload() {
        let sql = "WHERE name = :name";
        let parameters = detect_parameters(sql);
        let mut values = HashMap::new();
        values.insert("name".to_owned(), "x'; DROP TABLE users; --".to_owned());
        let result = substitute_params(sql, &parameters, &values, POSTGRES);
        assert_eq!(result, "WHERE name = 'x''; DROP TABLE users; --'");
        assert_eq!(result.matches("DROP TABLE").count(), 1);
        // The payload lands entirely inside one quoted literal: no
        // unescaped quote reopens or closes the string early.
        assert_eq!(result.matches('\'').count(), 4);
    }

    #[test]
    fn substitute_params_on_mysql_doubles_a_trailing_backslash_so_it_cannot_escape_the_closing_quote()
     {
        let sql = "WHERE name = :name";
        let parameters = detect_parameters(sql);
        let mut values = HashMap::new();
        values.insert("name".to_owned(), r"x\".to_owned());
        let result = substitute_params(sql, &parameters, &values, MYSQL);
        assert_eq!(
            result, r"WHERE name = 'x\\'",
            "a trailing backslash must be doubled, not left free to escape the closing quote"
        );
    }

    #[test]
    fn substitute_params_on_mysql_is_safe_against_a_backslash_quote_injection_payload() {
        let sql = "WHERE name = :name";
        let parameters = detect_parameters(sql);
        let mut values = HashMap::new();
        // Under MySQL's default backslash-escaped string literals, an
        // undoubled backslash here would make the following quote an
        // escaped literal quote rather than the string's close, letting
        // ` OR 1=1 -- ` run as live SQL outside the literal.
        values.insert("name".to_owned(), r"x\' OR 1=1 -- ".to_owned());
        let result = substitute_params(sql, &parameters, &values, MYSQL);
        assert_eq!(result, r"WHERE name = 'x\\'' OR 1=1 -- '");
    }

    #[test]
    fn substitute_params_on_a_non_mysql_dialect_never_doubles_a_backslash() {
        let sql = "WHERE name = :name";
        let parameters = detect_parameters(sql);
        let mut values = HashMap::new();
        values.insert("name".to_owned(), r"C:\data".to_owned());
        let result = substitute_params(sql, &parameters, &values, POSTGRES);
        assert_eq!(
            result, r"WHERE name = 'C:\data'",
            "a standard-conforming backend must see the backslash unchanged"
        );
    }
}
