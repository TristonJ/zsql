//! Pure SQL-text builders for queries the UI constructs from user actions
//! (e.g. clicking a relation in the schema sidebar) rather than accepting
//! arbitrary interpolated strings. No gpui, no database: unit-testable in
//! isolation.

/// Double-quote `ident` for use in generated SQL, escaping any embedded
/// double quote by doubling it. This is what makes it safe to interpolate a
/// schema/relation name straight into generated SQL: a name that needs
/// quoting (mixed case, a reserved word, whitespace) is quoted correctly,
/// and a name containing a double quote is escaped rather than letting that
/// quote close the identifier early and expose whatever follows as SQL
/// syntax instead of literal identifier text.
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

/// Build the click-to-preview query for a relation:
/// `SELECT * FROM "<schema>"."<relation>" LIMIT <limit>`. Both identifiers
/// are quoted via [`quote_ident`], so a reserved word or special character
/// in either name cannot break out of the identifier position.
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
        // Mixed case and a reserved word: Postgres would otherwise fold or
        // reject either one unquoted.
        assert_eq!(quote_ident("Order Table"), "\"Order Table\"");
        assert_eq!(quote_ident("select"), "\"select\"");
    }

    #[test]
    fn quote_ident_escapes_embedded_double_quotes() {
        // A double quote inside the identifier must be doubled, not
        // terminate the identifier early -- this is what keeps the quoting
        // safe against a maliciously- or accidentally-named relation.
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
        // The malicious segment stays inside the quoted identifier (as a
        // doubled-quote-escaped literal), so it can never be read as a
        // second statement.
        assert_eq!(
            sql,
            "SELECT * FROM \"public\".\"orders\"\"; DROP TABLE users; --\" LIMIT 200"
        );
        assert_eq!(sql.matches("DROP TABLE").count(), 1);
    }
}
