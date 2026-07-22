//! `mariadb://` scheme normalization. sqlx's `MySqlConnectOptions` parses a
//! connection URL generically (it never inspects the scheme itself), but
//! this driver's own contract is to accept both `mysql://` and `mariadb://`
//! -- so `mariadb://` is rewritten to `mysql://` before any URL reaches
//! sqlx, keeping the normalization explicit and independent of sqlx's
//! internal parsing behavior.

use zsql_core::CoreError;

const MARIADB_SCHEME_PREFIX: &str = "mariadb://";
const MYSQL_SCHEME_PREFIX: &str = "mysql://";

/// Rewrite a `mariadb://` URL to the equivalent `mysql://` URL sqlx's
/// `MySqlConnectOptions` understands; a `mysql://` URL (or anything else)
/// passes through unchanged.
///
/// # Errors
/// Returns [`CoreError::Url`] if `url` is empty or blank.
pub(crate) fn normalize_for_sqlx(url: &str) -> Result<String, CoreError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(CoreError::Url("empty URL".to_owned()));
    }
    match strip_prefix_case_insensitive(trimmed, MARIADB_SCHEME_PREFIX) {
        Some(rest) => Ok(format!("{MYSQL_SCHEME_PREFIX}{rest}")),
        None => Ok(trimmed.to_owned()),
    }
}

/// Case-insensitive `str::strip_prefix`, since a URL scheme is conventionally
/// lowercase but callers may not write it that way.
fn strip_prefix_case_insensitive<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() >= prefix.len()
        && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
    {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_for_sqlx;

    #[test]
    fn rejects_an_empty_url() {
        assert!(normalize_for_sqlx("").is_err());
        assert!(normalize_for_sqlx("   ").is_err());
    }

    #[test]
    fn rewrites_a_mariadb_url_to_mysql() {
        assert_eq!(
            normalize_for_sqlx("mariadb://root:zsql@localhost:3307/zsql").unwrap(),
            "mysql://root:zsql@localhost:3307/zsql"
        );
    }

    #[test]
    fn leaves_a_mysql_url_unchanged() {
        assert_eq!(
            normalize_for_sqlx("mysql://root:zsql@localhost:3306/zsql").unwrap(),
            "mysql://root:zsql@localhost:3306/zsql"
        );
    }

    #[test]
    fn rewrites_a_mariadb_scheme_regardless_of_case() {
        assert_eq!(
            normalize_for_sqlx("MariaDB://root:zsql@localhost/zsql").unwrap(),
            "mysql://root:zsql@localhost/zsql"
        );
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            normalize_for_sqlx("  mariadb://localhost/zsql  ").unwrap(),
            "mysql://localhost/zsql"
        );
    }

    #[test]
    fn an_unrelated_scheme_still_passes_through() {
        // Not this driver's job to reject a foreign scheme -- that is
        // `zsql_core::select_driver`'s job, upstream of this URL ever
        // reaching this crate.
        assert_eq!(
            normalize_for_sqlx("postgres://localhost/db").unwrap(),
            "postgres://localhost/db"
        );
    }
}
