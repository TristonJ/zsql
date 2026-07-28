//! The accepted spellings for mssql's boolean-valued and CA-certificate
//! connection-URL query parameters, plus the case-insensitive lookup both
//! [`crate::url::parse`] and the connections form's TLS control read from --
//! a single place either side needs to agree on for the same URL to behave
//! identically in the live driver and the read-only form preview.

use zsql_core::{ConnectionUrl, CoreError};

/// The query-parameter key for "require TLS for the whole session".
pub const ENCRYPT_KEY: &str = "encrypt";

/// The primary spelling for "accept the server's TLS certificate without
/// validating it against a trust store".
pub const TRUST_CERT_KEY: &str = "trustServerCertificate";

/// A `snake_case` alias for [`TRUST_CERT_KEY`], accepted on read so a
/// hand-edited URL using either spelling behaves the same.
pub const TRUST_CERT_ALIAS_KEY: &str = "trust_server_certificate";

/// The primary spelling for an additional trusted CA certificate path.
pub const CA_CERT_KEY: &str = "sslrootcert";

/// A `snake_case` alias for [`CA_CERT_KEY`].
pub const CA_CERT_ALIAS_KEY: &str = "ssl_root_cert";

/// Value spellings a boolean parameter here accepts as `true`.
const TRUTHY: &[&str] = &["true", "1", "yes"];

/// Value spellings a boolean parameter here accepts as `false`.
const FALSY: &[&str] = &["false", "0", "no"];

/// Whether `value` is one of [`TRUTHY`]'s spellings, case-insensitively.
/// Anything else (a [`FALSY`] spelling or garbage) is `false` -- for a
/// best-effort reader (the connections form) that would rather fall back to
/// a default than reject a value outright.
#[must_use]
pub fn is_truthy(value: &str) -> bool {
    TRUTHY.contains(&value.to_ascii_lowercase().as_str())
}

/// Parse `value` as a boolean, accepting [`TRUTHY`]/[`FALSY`]'s spellings
/// case-insensitively and rejecting anything else -- for a caller (the live
/// URL parser) that must reject a garbage value rather than guess.
///
/// # Errors
/// Returns [`CoreError::Url`] if `value` is not one of the recognized
/// spellings.
pub fn parse_bool_param(key: &str, value: &str) -> Result<bool, CoreError> {
    let lower = value.to_ascii_lowercase();
    if TRUTHY.contains(&lower.as_str()) {
        Ok(true)
    } else if FALSY.contains(&lower.as_str()) {
        Ok(false)
    } else {
        Err(CoreError::Url(format!(
            "invalid value '{value}' for URL parameter '{key}'"
        )))
    }
}

/// The value of whichever of `keys` last appears in `parsed`'s query string,
/// matching each key case-insensitively -- so a repeated key or alias
/// resolves the same way a single left-to-right scan of the query string
/// would (the last occurrence wins), and a key spelled in a different case
/// is still recognized.
///
/// Reads the query string's raw, undecoded text rather than
/// [`ConnectionUrl::extra_query_params`]'s percent-/plus-decoded pairs: a
/// value here is compared against a small fixed set of ASCII spellings (see
/// [`is_truthy`]/[`parse_bool_param`]) or taken verbatim as a filesystem
/// path, so decoding would only let an encoded value masquerade as one of
/// those spellings, or silently alter a path's literal `+` or `%XX` bytes.
#[must_use]
pub fn param_ci(parsed: &ConnectionUrl, keys: &[&str]) -> Option<String> {
    raw_pairs(parsed)
        .into_iter()
        .filter(|(k, _)| keys.iter().any(|key| key.eq_ignore_ascii_case(k)))
        .map(|(_, v)| v.to_owned())
        .next_back()
}

/// `parsed`'s query string split into `(key, value)` pairs on `&` then the
/// first `=`, exactly as written -- no percent- or plus-decoding, and a
/// valueless key (`key` with no `=`) maps to an empty value. Empty if
/// `parsed` has no query string.
fn raw_pairs(parsed: &ConnectionUrl) -> Vec<(&str, &str)> {
    parsed
        .raw_query()
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
        .collect()
}

#[cfg(test)]
mod tests {
    use zsql_core::ConnectionUrl;

    use super::{
        ENCRYPT_KEY, TRUST_CERT_ALIAS_KEY, TRUST_CERT_KEY, is_truthy, param_ci, parse_bool_param,
    };

    #[test]
    fn is_truthy_accepts_every_documented_spelling_case_insensitively() {
        for value in ["true", "TRUE", "1", "yes", "Yes"] {
            assert!(is_truthy(value), "{value} should be truthy");
        }
    }

    #[test]
    fn is_truthy_rejects_falsy_and_garbage_alike() {
        for value in ["false", "0", "no", "sideways", ""] {
            assert!(!is_truthy(value), "{value} should not be truthy");
        }
    }

    #[test]
    fn parse_bool_param_accepts_every_documented_spelling() {
        for (value, expected) in [
            ("true", true),
            ("1", true),
            ("yes", true),
            ("false", false),
            ("0", false),
            ("no", false),
        ] {
            assert_eq!(parse_bool_param("encrypt", value).unwrap(), expected);
        }
    }

    #[test]
    fn parse_bool_param_rejects_a_garbage_value() {
        assert!(parse_bool_param("encrypt", "sideways").is_err());
    }

    #[test]
    fn param_ci_matches_a_key_regardless_of_case() {
        let url = ConnectionUrl::parse("mssql://host/db?TrustServerCertificate=true").unwrap();
        assert_eq!(
            param_ci(&url, &[TRUST_CERT_KEY, TRUST_CERT_ALIAS_KEY]).as_deref(),
            Some("true")
        );
    }

    #[test]
    fn param_ci_prefers_the_last_matching_occurrence() {
        let url = ConnectionUrl::parse("mssql://host/db?encrypt=true&encrypt=false").unwrap();
        assert_eq!(param_ci(&url, &[ENCRYPT_KEY]).as_deref(), Some("false"));
    }

    #[test]
    fn param_ci_returns_none_when_no_key_matches() {
        let url = ConnectionUrl::parse("mssql://host/db").unwrap();
        assert_eq!(param_ci(&url, &[ENCRYPT_KEY]), None);
    }

    #[test]
    fn param_ci_returns_the_raw_value_without_percent_or_plus_decoding() {
        let url = ConnectionUrl::parse("mssql://host/db?sslrootcert=/etc/my+ca.crt").unwrap();
        assert_eq!(
            param_ci(&url, &["sslrootcert"]).as_deref(),
            Some("/etc/my+ca.crt"),
            "a literal '+' in a path must not be read back as a space"
        );

        let url = ConnectionUrl::parse("mssql://host/db?encrypt=%74rue").unwrap();
        assert_eq!(
            param_ci(&url, &[ENCRYPT_KEY]).as_deref(),
            Some("%74rue"),
            "a percent-encoded value must not be read back as its decoded spelling"
        );
    }
}
