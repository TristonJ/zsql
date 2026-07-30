//! Shared SQL-text helpers safe to reuse across every driver and the UI

use std::fmt::Write as _;

use crate::filter::{FilterState, render_where_conditions};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// The click-to-preview query for `relation` in `schema`. `args.filters`
/// renders as a `WHERE` clause via [`render_where_conditions`], quoting
/// column identifiers with [`quote_ident`] and mapping `ILIKE` natively --
/// the same default this crate's own [`crate::driver::Connection::preview_query`]
/// falls back to; a live driver overrides both quoting and the `ILIKE`
/// mapping for its own dialect.
#[must_use]
pub fn default_preview_query(schema: &str, relation: &str, args: PreviewQueryArgs) -> String {
    let mut sql = format!(
        "SELECT * FROM {}.{}",
        quote_ident(schema),
        quote_ident(relation)
    );
    if let Some(where_clause) = render_where_conditions(&args.filters, quote_ident, true) {
        let _ = write!(sql, " WHERE {where_clause}");
    }
    if let Some((column, direction)) = args.sort {
        let _ = write!(
            sql,
            " ORDER BY {} {}",
            quote_ident(&column),
            direction.as_sql()
        );
    }
    let _ = write!(sql, " LIMIT {}", args.limit);
    if let Some(offset) = args.offset
        && offset > 0
    {
        let _ = write!(sql, " OFFSET {offset}");
    }
    sql
}

#[cfg(test)]
mod tests {
    use super::{PreviewQueryArgs, SortDirection, default_preview_query, quote_ident};
    use crate::filter::{FilterOperator, FilterState};

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
}
