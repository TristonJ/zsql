//! MySQL/MariaDB `` `backtick` `` identifier escaping. Both engines default
//! to the backtick convention (not the ANSI double-quote convention
//! `zsql_core::quote_ident` implements), so this driver carries its own
//! quoting helper rather than reusing the ANSI one incorrectly.

/// Wrap `ident` in `` `backticks` `` for use in generated SQL, escaping any
/// embedded backtick by doubling it. This is the one place this crate quotes
/// a MySQL/MariaDB identifier; every query that interpolates a schema or
/// relation name into SQL text goes through this first.
#[must_use]
pub(crate) fn backtick_quote_ident(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len() + 2);
    out.push('`');
    for ch in ident.chars() {
        if ch == '`' {
            out.push('`');
        }
        out.push(ch);
    }
    out.push('`');
    out
}

#[cfg(test)]
mod tests {
    use super::backtick_quote_ident;

    #[test]
    fn wraps_a_plain_name_in_backticks() {
        assert_eq!(backtick_quote_ident("orders"), "`orders`");
    }

    #[test]
    fn wraps_a_name_that_needs_quoting() {
        assert_eq!(backtick_quote_ident("Order Table"), "`Order Table`");
        assert_eq!(backtick_quote_ident("select"), "`select`");
    }

    #[test]
    fn escapes_an_embedded_backtick() {
        assert_eq!(backtick_quote_ident("weird`name"), "`weird``name`");
    }

    #[test]
    fn is_safe_against_an_injection_shaped_name() {
        let quoted = backtick_quote_ident("orders`; DROP TABLE users; --");
        assert_eq!(quoted, "`orders``; DROP TABLE users; --`");
        // Opening backtick, the doubled (escaped) embedded backtick, and the
        // closing backtick: four total, none of which closes the
        // identifier early except the real one at the very end.
        assert_eq!(quoted.matches('`').count(), 4);
        assert!(quoted.ends_with('`'));
    }
}
