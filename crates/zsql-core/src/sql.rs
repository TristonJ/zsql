//! Shared SQL-text helpers safe to reuse across every driver and the UI

use std::fmt::Write as _;

use crate::filter::{FilterState, render_where_body};

/// Arguments for generating preview queries
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewQueryArgs {
    pub limit: u64,
    pub offset: Option<u64>,
    pub sort: Option<(String, SortDirection)>,
    pub filters: FilterState,
}

impl PreviewQueryArgs {
    /// Construct a new `PreviewQueryArgs` with the given limit and no
    /// offset, sort, or filters.
    #[must_use]
    pub fn from_limit(limit: u64) -> Self {
        Self {
            limit,
            offset: None,
            sort: None,
            filters: FilterState::new(),
        }
    }

    /// Set the offset for the preview query.
    #[must_use]
    pub fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Set the sort for the preview query.
    #[must_use]
    pub fn sort(mut self, column: impl AsRef<str>, direction: SortDirection) -> Self {
        self.sort = Some((column.as_ref().to_string(), direction));
        self
    }

    /// Set the filter conditions for the preview query.
    #[must_use]
    pub fn filters(mut self, filters: FilterState) -> Self {
        self.filters = filters;
        self
    }
}

/// Which way a preview's `ORDER BY` sorts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    /// The direction's flip: `Asc` becomes `Desc` and vice versa. What a
    /// second click on an already-sorted column applies.
    #[must_use]
    pub fn flipped(self) -> Self {
        match self {
            SortDirection::Asc => SortDirection::Desc,
            SortDirection::Desc => SortDirection::Asc,
        }
    }

    /// The direction's SQL keyword.
    #[must_use]
    pub fn as_sql(self) -> &'static str {
        match self {
            SortDirection::Asc => "ASC",
            SortDirection::Desc => "DESC",
        }
    }
}

/// Double-quote `ident` for use in generated SQL, escaping any embedded
/// double quote by doubling it. This is the one place identifier quoting is
/// implemented; every driver and the UI reuse it rather than each rolling
/// their own escaping.
#[must_use]
pub fn quote_ident(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len() + 2);
    out.push('"');
    for ch in ident.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// Append `clause` to `sql`: on its own new line when `multiline`, otherwise
/// inlined after a single leading space.
pub fn append_clause_line(sql: &mut String, multiline: bool, clause: &str) {
    sql.push(if multiline { '\n' } else { ' ' });
    sql.push_str(clause);
}

/// Appends the `WHERE` clause including the keyword. A break precedes
/// `WHERE` and each condition after the first starts its own indented line
/// once `state.is_multiline()`. Does nothing when `state` has no conditions.
pub fn append_where_clause(
    sql: &mut String,
    state: &FilterState,
    quote_column: impl Fn(&str) -> String,
    ilike_native: bool,
) {
    let multiline = state.is_multiline();
    if let Some(body) = render_where_body(state, quote_column, ilike_native, multiline) {
        append_clause_line(sql, multiline, &format!("WHERE {body}"));
    }
}

/// The click-to-preview query for `relation` in `schema`. Identifiers are
/// quoted with [`quote_ident`] and `ILIKE` renders natively; a live driver
/// overrides both for its own dialect. The query renders multi-line once
/// `args.filters` says so via [`FilterState::is_multiline`].
#[must_use]
pub fn default_preview_query(schema: &str, relation: &str, args: PreviewQueryArgs) -> String {
    let multiline = args.filters.is_multiline();
    let mut sql = format!(
        "SELECT * FROM {}.{}",
        quote_ident(schema),
        quote_ident(relation)
    );
    append_where_clause(&mut sql, &args.filters, quote_ident, true);
    if let Some((column, direction)) = args.sort {
        append_clause_line(
            &mut sql,
            multiline,
            &format!("ORDER BY {} {}", quote_ident(&column), direction.as_sql()),
        );
    }
    let mut tail = format!("LIMIT {}", args.limit);
    if let Some(offset) = args.offset
        && offset > 0
    {
        let _ = write!(tail, " OFFSET {offset}");
    }
    append_clause_line(&mut sql, multiline, &tail);
    sql
}

#[cfg(test)]
mod tests {
    use super::{
        PreviewQueryArgs, SortDirection, append_clause_line, default_preview_query, quote_ident,
    };
    use crate::filter::{FilterOperator, FilterState, render_where_body};

    #[test]
    fn quote_ident_wraps_a_plain_name_in_double_quotes() {
        assert_eq!(quote_ident("orders"), "\"orders\"");
    }

    #[test]
    fn quote_ident_wraps_a_name_that_needs_quoting() {
        assert_eq!(quote_ident("Order Table"), "\"Order Table\"");
        assert_eq!(quote_ident("select"), "\"select\"");
    }

    #[test]
    fn quote_ident_escapes_embedded_double_quotes() {
        assert_eq!(quote_ident("weird\"name"), "\"weird\"\"name\"");
        assert_eq!(
            quote_ident("a\"; DROP TABLE users; --"),
            "\"a\"\"; DROP TABLE users; --\""
        );
    }

    #[test]
    fn default_preview_query_quotes_both_identifiers_and_applies_the_limit() {
        assert_eq!(
            default_preview_query("public", "orders", PreviewQueryArgs::from_limit(200)),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200"
        );
    }

    #[test]
    fn default_preview_query_is_safe_against_an_injection_attempting_relation_name() {
        let sql = default_preview_query(
            "public",
            "orders\"; DROP TABLE users; --",
            PreviewQueryArgs::from_limit(200),
        );
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\"\"; DROP TABLE users; --\" LIMIT 200"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }

    #[test]
    fn sort_direction_flip_is_its_own_inverse() {
        assert_eq!(SortDirection::Asc.flipped(), SortDirection::Desc);
        assert_eq!(SortDirection::Desc.flipped(), SortDirection::Asc);
    }

    #[test]
    fn windowed_query_with_no_sort_and_page_one_matches_the_plain_default_form() {
        assert_eq!(
            default_preview_query(
                "public",
                "orders",
                PreviewQueryArgs::from_limit(200).offset(0)
            ),
            default_preview_query("public", "orders", PreviewQueryArgs::from_limit(200))
        );
    }

    #[test]
    fn windowed_query_applies_an_ascending_sort() {
        assert_eq!(
            default_preview_query(
                "public",
                "orders",
                PreviewQueryArgs::from_limit(200)
                    .offset(0)
                    .sort("total_cents", SortDirection::Asc)
            ),
            "SELECT * FROM \"public\".\"orders\" ORDER BY \"total_cents\" ASC LIMIT 200"
        );
    }

    #[test]
    fn windowed_query_applies_a_descending_sort() {
        assert_eq!(
            default_preview_query(
                "public",
                "orders",
                PreviewQueryArgs::from_limit(200)
                    .offset(0)
                    .sort("total_cents", SortDirection::Desc)
            ),
            "SELECT * FROM \"public\".\"orders\" ORDER BY \"total_cents\" DESC LIMIT 200"
        );
    }

    #[test]
    fn windowed_query_omits_offset_on_page_one() {
        let sql = default_preview_query(
            "public",
            "orders",
            PreviewQueryArgs::from_limit(200).offset(0),
        );
        assert!(
            !sql.contains("OFFSET"),
            "a zero offset must not appear in the generated text: {sql}"
        );
    }

    #[test]
    fn windowed_query_applies_offset_math_for_page_two() {
        assert_eq!(
            default_preview_query(
                "public",
                "orders",
                PreviewQueryArgs::from_limit(200).offset(200)
            ),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200 OFFSET 200"
        );
    }

    #[test]
    fn windowed_query_applies_offset_math_for_a_later_page() {
        // Page 5 at 200 rows/page: offset = (5 - 1) * 200.
        assert_eq!(
            default_preview_query(
                "public",
                "orders",
                PreviewQueryArgs::from_limit(200).offset(800)
            ),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200 OFFSET 800"
        );
    }

    #[test]
    fn windowed_query_supports_every_configured_page_size() {
        for page_size in [100_u64, 200, 500, 1000] {
            let sql =
                default_preview_query("public", "orders", PreviewQueryArgs::from_limit(page_size));
            assert_eq!(
                sql,
                format!("SELECT * FROM \"public\".\"orders\" LIMIT {page_size}")
            );
        }
    }

    #[test]
    fn windowed_query_combines_sort_and_offset_for_page_two() {
        assert_eq!(
            default_preview_query(
                "public",
                "orders",
                PreviewQueryArgs::from_limit(200)
                    .offset(200)
                    .sort("total_cents", SortDirection::Desc)
            ),
            "SELECT * FROM \"public\".\"orders\" ORDER BY \"total_cents\" DESC LIMIT 200 OFFSET 200"
        );
    }

    #[test]
    fn windowed_query_is_safe_against_an_injection_shaped_sort_column() {
        let sql = default_preview_query(
            "public",
            "orders",
            PreviewQueryArgs::from_limit(200)
                .offset(0)
                .sort("total\"; DROP TABLE users; --", SortDirection::Asc),
        );
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\" ORDER BY \"total\"\"; DROP TABLE users; --\" ASC LIMIT 200"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }

    #[test]
    fn default_preview_query_has_no_where_clause_with_no_filters() {
        let sql = default_preview_query("public", "orders", PreviewQueryArgs::from_limit(200));
        assert!(!sql.contains("WHERE"));
    }

    #[test]
    fn default_preview_query_renders_a_where_clause_from_filters() {
        let mut filters = FilterState::new();
        filters.add_condition("status", "text", FilterOperator::Eq, "paid");
        let sql = default_preview_query(
            "public",
            "orders",
            PreviewQueryArgs::from_limit(200).filters(filters),
        );
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\" WHERE \"status\" = 'paid' LIMIT 200"
        );
    }

    #[test]
    fn default_preview_query_places_where_before_order_by_and_limit() {
        let mut filters = FilterState::new();
        filters.add_condition("status", "text", FilterOperator::Eq, "paid");
        let sql = default_preview_query(
            "public",
            "orders",
            PreviewQueryArgs::from_limit(200)
                .filters(filters)
                .sort("total_cents", SortDirection::Desc)
                .offset(200),
        );
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\" WHERE \"status\" = 'paid' \
             ORDER BY \"total_cents\" DESC LIMIT 200 OFFSET 200"
        );
    }

    #[test]
    fn windowed_query_is_safe_against_a_keyword_shaped_sort_column() {
        let sql = default_preview_query(
            "public",
            "orders",
            PreviewQueryArgs::from_limit(200)
                .offset(0)
                .sort("order", SortDirection::Asc),
        );
        assert!(
            sql.contains("ORDER BY \"order\" ASC"),
            "a column literally named `order` must still be quoted: {sql}"
        );
    }

    // -- multi-line WHERE clauses ---------------------------------------------

    fn two_conditions() -> FilterState {
        let mut filters = FilterState::new();
        filters.add_condition("status", "text", FilterOperator::Eq, "paid");
        filters.add_condition("region", "text", FilterOperator::Eq, "west");
        filters
    }

    #[test]
    fn default_preview_query_with_two_conditions_renders_a_multiline_where_clause() {
        let sql = default_preview_query(
            "public",
            "orders",
            PreviewQueryArgs::from_limit(200).filters(two_conditions()),
        );
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\"\nWHERE \"status\" = 'paid'\n  AND \"region\" = 'west'\nLIMIT 200"
        );
    }

    #[test]
    fn default_preview_query_with_three_mixed_conditions_renders_each_on_its_own_line() {
        let mut filters = FilterState::new();
        filters.add_condition("status", "text", FilterOperator::Eq, "paid");
        filters.add_condition("status", "text", FilterOperator::Eq, "pending");
        filters.toggle_connector(0);
        filters.add_condition("placed_at", "timestamptz", FilterOperator::Gt, "now()");
        let sql = default_preview_query(
            "public",
            "orders",
            PreviewQueryArgs::from_limit(200).filters(filters),
        );
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\"\n\
             WHERE \"status\" = 'paid'\n  \
             OR \"status\" = 'pending'\n  \
             AND \"placed_at\" > now()\n\
             LIMIT 200"
        );
    }

    #[test]
    fn default_preview_query_multiline_where_places_order_by_on_its_own_line() {
        let sql = default_preview_query(
            "public",
            "orders",
            PreviewQueryArgs::from_limit(200)
                .filters(two_conditions())
                .sort("total_cents", SortDirection::Desc),
        );
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\"\n\
             WHERE \"status\" = 'paid'\n  \
             AND \"region\" = 'west'\n\
             ORDER BY \"total_cents\" DESC\n\
             LIMIT 200"
        );
    }

    #[test]
    fn default_preview_query_multiline_where_places_limit_offset_on_its_own_line() {
        let sql = default_preview_query(
            "public",
            "orders",
            PreviewQueryArgs::from_limit(200)
                .filters(two_conditions())
                .offset(200),
        );
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\"\n\
             WHERE \"status\" = 'paid'\n  \
             AND \"region\" = 'west'\n\
             LIMIT 200 OFFSET 200"
        );
    }

    #[test]
    fn default_preview_query_multiline_combines_sort_and_offset() {
        let sql = default_preview_query(
            "public",
            "orders",
            PreviewQueryArgs::from_limit(200)
                .filters(two_conditions())
                .sort("total_cents", SortDirection::Desc)
                .offset(200),
        );
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\"\n\
             WHERE \"status\" = 'paid'\n  \
             AND \"region\" = 'west'\n\
             ORDER BY \"total_cents\" DESC\n\
             LIMIT 200 OFFSET 200"
        );
    }

    #[test]
    fn default_preview_query_multiline_output_has_no_trailing_whitespace_or_newline() {
        let sql = default_preview_query(
            "public",
            "orders",
            PreviewQueryArgs::from_limit(200)
                .filters(two_conditions())
                .sort("total_cents", SortDirection::Desc)
                .offset(200),
        );
        assert!(!sql.ends_with('\n'), "no trailing newline: {sql:?}");
        for line in sql.lines() {
            assert_eq!(
                line,
                line.trim_end(),
                "no trailing whitespace on any line: {sql:?}"
            );
        }
    }

    #[test]
    fn multiline_where_clause_collapses_to_the_same_tokens_as_single_line_rendering() {
        let filters = two_conditions();
        let single_line_body =
            render_where_body(&filters, quote_ident, true, false).expect("has conditions");
        let expected_single_line =
            format!("SELECT * FROM \"public\".\"orders\" WHERE {single_line_body} LIMIT 200");

        let multiline_sql = default_preview_query(
            "public",
            "orders",
            PreviewQueryArgs::from_limit(200).filters(filters),
        );
        let collapsed = multiline_sql
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(collapsed, expected_single_line);
    }

    #[test]
    fn append_clause_line_inlines_after_a_single_space_when_not_multiline() {
        let mut sql = "SELECT 1".to_owned();
        append_clause_line(&mut sql, false, "LIMIT 200");
        assert_eq!(sql, "SELECT 1 LIMIT 200");
    }

    #[test]
    fn append_clause_line_starts_a_new_line_when_multiline() {
        let mut sql = "SELECT 1".to_owned();
        append_clause_line(&mut sql, true, "LIMIT 200");
        assert_eq!(sql, "SELECT 1\nLIMIT 200");
    }
}
