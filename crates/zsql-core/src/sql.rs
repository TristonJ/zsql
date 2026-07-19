//! Shared SQL-text helpers safe to reuse across every driver and the UI

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

/// The click-to-preview query for `relation` in `schema`, capped at `limit`
/// rows, in the dialect [`crate::driver::Connection::preview_query`]'s
/// default implementation uses.
#[must_use]
pub fn default_preview_query(schema: &str, relation: &str, limit: u64) -> String {
    format!(
        "SELECT * FROM {}.{} LIMIT {limit}",
        quote_ident(schema),
        quote_ident(relation)
    )
}

#[cfg(test)]
mod tests {
    use super::{default_preview_query, quote_ident};

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
            default_preview_query("public", "orders", 200),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200"
        );
    }

    #[test]
    fn default_preview_query_is_safe_against_an_injection_attempting_relation_name() {
        let sql = default_preview_query("public", "orders\"; DROP TABLE users; --", 200);
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\"\"; DROP TABLE users; --\" LIMIT 200"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }
}
