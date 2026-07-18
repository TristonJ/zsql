//! Pure SQL-text builders for queries the UI constructs from user actions
//! (e.g. clicking a relation in the schema sidebar) rather than accepting
//! arbitrary interpolated strings

/// Double-quote `ident` for use in generated SQL, escaping any embedded
/// double quote by doubling it. Delegates to `zsql_core::quote_ident`, the
/// single implementation every driver and the UI share, rather than keeping
/// a second copy of the escaping logic in the binary crate.
#[must_use]
pub fn quote_ident(ident: &str) -> String {
    zsql_core::quote_ident(ident)
}

/// Build the click-to-preview query for a relation:
/// `SELECT * FROM "<schema>"."<relation>" LIMIT <limit>`
#[must_use]
pub fn preview_sql(schema: &str, relation: &str, limit: u64) -> String {
    format!(
        "SELECT * FROM {}.{} LIMIT {limit}",
        quote_ident(schema),
        quote_ident(relation)
    )
}

#[cfg(test)]
mod tests {
    use super::{preview_sql, quote_ident};

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
    fn preview_sql_quotes_both_identifiers_and_applies_the_limit() {
        assert_eq!(
            preview_sql("public", "orders", 200),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200"
        );
    }

    #[test]
    fn preview_sql_is_safe_against_an_injection_attempting_relation_name() {
        let sql = preview_sql("public", "orders\"; DROP TABLE users; --", 200);
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\"\"; DROP TABLE users; --\" LIMIT 200"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }
}
