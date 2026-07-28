//! Parsing an `mssql://` (or `sqlserver://`) URL into the fields
//! `tiberius::Config` needs. Generic URL structure (scheme, host, port,
//! credentials, database, query parameters) is parsed once by
//! [`zsql_core::ConnectionUrl`]; this module only maps that onto
//! `tiberius`-shaped fields and applies mssql's own query-parameter
//! spellings from [`crate::params`].

use zsql_core::{ConnectionUrl, CoreError};

use crate::params;

/// The default TCP port for a SQL Server instance that was not given an
/// explicit port.
const DEFAULT_PORT: u16 = 1433;

/// The parsed fields of an `mssql://`/`sqlserver://` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MssqlUrl {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) user: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) database: Option<String>,
    /// Whether to require TLS for the whole session (`encrypt=false` to
    /// disable). Defaults to `true`.
    pub(crate) encrypt: bool,
    /// Whether to accept the server's TLS certificate without validating it
    /// against a trust store. Off by default; local/dev servers using a
    /// self-signed certificate need `trustservercertificate=true`.
    pub(crate) trust_server_certificate: bool,
    /// Path to an additional trusted CA certificate file, checked alongside
    /// the system trust store. Lets a server certificate issued by a
    /// private CA (a self-signed dev certificate, for instance) pass full
    /// chain-and-hostname verification without disabling verification
    /// altogether via `trust_server_certificate`.
    pub(crate) ca_cert: Option<String>,
}

/// Parse `url` into its [`MssqlUrl`] fields.
///
/// # Errors
/// Returns [`CoreError::Url`] if `url` is empty, has no `mssql://`/
/// `sqlserver://` scheme, is missing a host, or a boolean query parameter
/// carries an unrecognized value.
pub(crate) fn parse(url: &str) -> Result<MssqlUrl, CoreError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(CoreError::Url("empty URL".to_owned()));
    }
    let Some((scheme, _)) = trimmed.split_once("://") else {
        return Err(CoreError::Url(
            "URL has no scheme (expected mssql:// or sqlserver://)".to_owned(),
        ));
    };
    if !scheme.eq_ignore_ascii_case("mssql") && !scheme.eq_ignore_ascii_case("sqlserver") {
        return Err(CoreError::Url(format!(
            "unrecognized scheme '{scheme}' (expected mssql or sqlserver)"
        )));
    }

    let parsed = ConnectionUrl::parse(trimmed)?;
    let host = parsed
        .host()
        .ok_or_else(|| CoreError::Url("URL is missing a host".to_owned()))?;
    let port = parsed.port().unwrap_or(DEFAULT_PORT);

    let raw_user = parsed.raw_user();
    let user = if raw_user.is_empty() {
        None
    } else {
        Some(percent_decode(raw_user)?)
    };
    let password = parsed.raw_password().map(percent_decode).transpose()?;

    let database = {
        let database = parsed.database();
        let trimmed = database.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    };

    let encrypt = match params::param_ci(&parsed, &[params::ENCRYPT_KEY]) {
        Some(value) => params::parse_bool_param(params::ENCRYPT_KEY, &value)?,
        None => true,
    };
    let trust_server_certificate = match params::param_ci(
        &parsed,
        &[params::TRUST_CERT_KEY, params::TRUST_CERT_ALIAS_KEY],
    ) {
        Some(value) => params::parse_bool_param(params::TRUST_CERT_KEY, &value)?,
        None => false,
    };
    let ca_cert = params::param_ci(&parsed, &[params::CA_CERT_KEY, params::CA_CERT_ALIAS_KEY]);

    Ok(MssqlUrl {
        host,
        port,
        user,
        password,
        database,
        encrypt,
        trust_server_certificate,
        ca_cert,
    })
}

/// Percent-decode `text` (`%XX` -> the byte `0xXX`), the mechanism a URL
/// author uses to embed a delimiter character (`:`, `/`, `?`, `@`) literally
/// inside a username or password instead of having it misread as URL
/// syntax. Unlike [`ConnectionUrl::user`]/[`ConnectionUrl::password`]'s
/// lossy display-oriented decode, this rejects a malformed escape outright:
/// a live connect must not silently guess what an ambiguous credential
/// meant.
///
/// # Errors
/// Returns [`CoreError::Url`] if a `%` is not followed by two hex digits, or
/// if the decoded bytes are not valid UTF-8.
fn percent_decode(text: &str) -> Result<String, CoreError> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text
                .get(i + 1..i + 3)
                .filter(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or_else(|| {
                    CoreError::Url("invalid percent-encoding in URL userinfo".to_owned())
                })?;
            // The slice was just validated as two ASCII hex digits, so this
            // radix-16 parse cannot fail.
            decoded.push(u8::from_str_radix(hex, 16).unwrap_or(0));
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded)
        .map_err(|_| CoreError::Url("URL userinfo is not valid UTF-8 after decoding".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn rejects_an_empty_url() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }

    #[test]
    fn rejects_a_url_with_no_scheme() {
        assert!(parse("localhost/db").is_err());
    }

    #[test]
    fn rejects_an_unrecognized_scheme() {
        assert!(parse("postgres://localhost/db").is_err());
    }

    #[test]
    fn rejects_a_url_with_no_host() {
        assert!(parse("mssql://").is_err());
        assert!(parse("mssql:///db").is_err());
    }

    #[test]
    fn rejects_an_invalid_port() {
        assert!(parse("mssql://host:not-a-port/db").is_err());
    }

    #[test]
    fn parses_a_minimal_url_with_just_a_host() {
        let url = parse("mssql://localhost").unwrap();
        assert_eq!(url.host, "localhost");
        assert_eq!(url.port, 1433);
        assert_eq!(url.user, None);
        assert_eq!(url.password, None);
        assert_eq!(url.database, None);
        assert!(url.encrypt);
        assert!(!url.trust_server_certificate);
    }

    #[test]
    fn parses_a_full_url() {
        // The password's `@` is percent-encoded (`%40`) so it is not
        // misread as the userinfo/host separator, then decoded back to a
        // literal `@` once the real separator has been found.
        let url = parse("mssql://sa:Str0ngP%40ss@db.example.com:14330/zsql").unwrap();
        assert_eq!(url.host, "db.example.com");
        assert_eq!(url.port, 14330);
        assert_eq!(url.user.as_deref(), Some("sa"));
        assert_eq!(url.password.as_deref(), Some("Str0ngP@ss"));
        assert_eq!(url.database.as_deref(), Some("zsql"));
    }

    #[test]
    fn a_password_with_a_percent_encoded_path_separator_decodes_correctly() {
        // A raw `/` in a password would otherwise be misread as the
        // authority/path boundary (e.g. host would parse as `sa:p`,
        // rejected as an invalid port); percent-encoding it as `%2F` avoids
        // that ambiguity and is decoded back to `/` here.
        let url = parse("mssql://sa:p%2Fw@localhost/db").unwrap();
        assert_eq!(url.host, "localhost");
        assert_eq!(url.user.as_deref(), Some("sa"));
        assert_eq!(url.password.as_deref(), Some("p/w"));
        assert_eq!(url.database.as_deref(), Some("db"));
    }

    #[test]
    fn a_username_with_a_percent_encoded_colon_decodes_correctly() {
        let url = parse("mssql://us%3Aer:pw@localhost").unwrap();
        assert_eq!(url.user.as_deref(), Some("us:er"));
        assert_eq!(url.password.as_deref(), Some("pw"));
    }

    #[test]
    fn rejects_a_truncated_percent_escape() {
        assert!(parse("mssql://sa:p%2@localhost/db").is_err());
        assert!(parse("mssql://sa:p%@localhost/db").is_err());
    }

    #[test]
    fn rejects_a_non_hex_percent_escape() {
        assert!(parse("mssql://sa:p%zz@localhost/db").is_err());
    }

    #[test]
    fn the_sqlserver_scheme_is_accepted_as_an_alias() {
        let url = parse("sqlserver://localhost/db").unwrap();
        assert_eq!(url.host, "localhost");
        assert_eq!(url.database.as_deref(), Some("db"));
    }

    #[test]
    fn scheme_matching_is_case_insensitive() {
        assert!(parse("MSSQL://localhost").is_ok());
        assert!(parse("SqlServer://localhost").is_ok());
    }

    #[test]
    fn parses_query_parameters() {
        let url = parse("mssql://localhost/db?encrypt=false&trustServerCertificate=true").unwrap();
        assert!(!url.encrypt);
        assert!(url.trust_server_certificate);
    }

    #[test]
    fn rejects_an_invalid_boolean_query_parameter() {
        assert!(parse("mssql://localhost/db?encrypt=sideways").is_err());
    }

    #[test]
    fn parses_a_ca_cert_path() {
        let url = parse("mssql://localhost/db?sslrootcert=/etc/zsql/ca.crt").unwrap();
        assert_eq!(url.ca_cert.as_deref(), Some("/etc/zsql/ca.crt"));
    }

    #[test]
    fn ca_cert_defaults_to_absent() {
        let url = parse("mssql://localhost/db").unwrap();
        assert_eq!(url.ca_cert, None);
    }

    #[test]
    fn a_user_with_no_password_is_allowed() {
        let url = parse("mssql://sa@localhost/db").unwrap();
        assert_eq!(url.user.as_deref(), Some("sa"));
        assert_eq!(url.password, None);
    }

    #[test]
    fn a_ca_cert_path_with_a_literal_plus_is_not_read_back_as_a_space() {
        let url = parse("mssql://localhost/db?sslrootcert=/etc/zsql/my+ca.crt").unwrap();
        assert_eq!(url.ca_cert.as_deref(), Some("/etc/zsql/my+ca.crt"));
    }

    #[test]
    fn a_percent_encoded_boolean_value_is_rejected_rather_than_decoded() {
        // "%74rue" percent-decodes to "true", but a query-parameter value is
        // matched against the boolean spellings as written, not decoded
        // first -- so this is rejected rather than silently accepted.
        assert!(parse("mssql://localhost/db?encrypt=%74rue").is_err());
    }
}
