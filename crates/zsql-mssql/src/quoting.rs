//! MSSQL `[bracket]`-quoted identifier escaping. MSSQL does not use the
//! ANSI double-quote convention `zsql_core::quote_ident` implements, so this
//! driver carries its own quoting helper rather than depending on the
//! zsql binary or reusing the ANSI helper incorrectly.

/// Wrap `ident` in `[brackets]` for use in generated SQL, escaping any
/// embedded `]` by doubling it. This is the one place MSSQL identifier
/// quoting is implemented in this crate; every query that interpolates a
/// schema or relation name into SQL text goes through this first.
#[must_use]
pub fn bracket_quote_ident(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len() + 2);
    out.push('[');
    for ch in ident.chars() {
        if ch == ']' {
            out.push(']');
        }
        out.push(ch);
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::bracket_quote_ident;

    #[test]
    fn wraps_a_plain_name_in_brackets() {
        assert_eq!(bracket_quote_ident("orders"), "[orders]");
    }

    #[test]
    fn wraps_a_name_that_needs_quoting() {
        assert_eq!(bracket_quote_ident("Order Table"), "[Order Table]");
        assert_eq!(bracket_quote_ident("select"), "[select]");
    }

    #[test]
    fn escapes_an_embedded_close_bracket() {
        assert_eq!(bracket_quote_ident("weird]name"), "[weird]]name]");
    }

    #[test]
    fn is_safe_against_an_injection_shaped_name() {
        let quoted = bracket_quote_ident("orders]; DROP TABLE users; --");
        assert_eq!(quoted, "[orders]]; DROP TABLE users; --]");
        // The escaped `]]` never closes the identifier early: exactly one
        // real close bracket remains, at the end.
        assert_eq!(quoted.matches(']').count(), 3);
        assert!(quoted.ends_with(']'));
    }
}
