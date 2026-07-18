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

#[cfg(test)]
mod tests {
    use super::quote_ident;

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
}
