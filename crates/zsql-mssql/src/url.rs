//! Parsing an `mssql://` (or `sqlserver://`) URL into the fields
//! `tiberius::Config` needs. `tiberius` has no URL-URL parser of its own
//! (only ADO.NET and JDBC connection-string formats), so this module owns a
//! small, dependency-free parser instead of pulling in a general-purpose URL
//! crate for one format.

use zsql_core::CoreError;

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
}

/// Parse `url` into its [`MssqlUrl`] fields.
///
/// # Errors
/// Returns [`CoreError::Url`] if `url` is empty, has no `mssql://`/
/// `sqlserver://` scheme, or is missing a host.
pub(crate) fn parse(url: &str) -> Result<MssqlUrl, CoreError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(CoreError::Url("empty URL".to_owned()));
    }

    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(CoreError::Url(
            "URL has no scheme (expected mssql:// or sqlserver://)".to_owned(),
        ));
    };
    if !scheme.eq_ignore_ascii_case("mssql") && !scheme.eq_ignore_ascii_case("sqlserver") {
        return Err(CoreError::Url(format!(
            "unrecognized scheme '{scheme}' (expected mssql or sqlserver)"
        )));
    }

    let (authority_and_path, query) = match rest.split_once('?') {
        Some((left, right)) => (left, Some(right)),
        None => (rest, None),
    };
    let (authority, path) = match authority_and_path.split_once('/') {
        Some((left, right)) => (left, Some(right)),
        None => (authority_and_path, None),
    };

    let (userinfo, host_port) = match authority.rsplit_once('@') {
        Some((left, right)) => (Some(left), right),
        None => (None, authority),
    };
    // Userinfo is split into user/password on the first literal `:`, so a
    // password containing a literal `:`, `/`, `?`, or `@` -- any of which
    // would otherwise be misread as a URL delimiter -- must be
    // percent-encoded by whoever writes the URL and is decoded back here.
    let (user, password) = match userinfo {
        Some(userinfo) => match userinfo.split_once(':') {
            Some((user, password)) => {
                (Some(percent_decode(user)?), Some(percent_decode(password)?))
            }
            None => (Some(percent_decode(userinfo)?), None),
        },
        None => (None, None),
    };

    let (host, port) = match host_port.split_once(':') {
        Some((host, port_text)) => {
            let port: u16 = port_text
                .parse()
                .map_err(|_| CoreError::Url(format!("invalid port '{port_text}'")))?;
            (host, port)
        }
        None => (host_port, DEFAULT_PORT),
    };
    if host.is_empty() {
        return Err(CoreError::Url("URL is missing a host".to_owned()));
    }

    let database = path
        .map(str::trim)
        .filter(|database| !database.is_empty())
        .map(str::to_owned);

    let mut encrypt = true;
    let mut trust_server_certificate = false;
    for pair in query.into_iter().flat_map(|query| query.split('&')) {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key.to_ascii_lowercase().as_str() {
            "encrypt" => encrypt = parse_bool_param(key, value)?,
            "trustservercertificate" | "trust_server_certificate" => {
                trust_server_certificate = parse_bool_param(key, value)?;
            }
            _ => {}
        }
    }

    Ok(MssqlUrl {
        host: host.to_owned(),
        port,
        user,
        password,
        database,
        encrypt,
        trust_server_certificate,
    })
}

/// Percent-decode `text` (`%XX` -> the byte `0xXX`), the mechanism a URL
/// author uses to embed a delimiter character (`:`, `/`, `?`, `@`) literally
/// inside a username or password instead of having it misread as URL
/// syntax.
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

/// Parse a query-parameter value as a boolean, accepting the common
/// spellings a hand-written URL is likely to use.
fn parse_bool_param(key: &str, value: &str) -> Result<bool, CoreError> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        other => Err(CoreError::Url(format!(
            "invalid value '{other}' for URL parameter '{key}'"
        ))),
    }
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
    fn a_user_with_no_password_is_allowed() {
        let url = parse("mssql://sa@localhost/db").unwrap();
        assert_eq!(url.user.as_deref(), Some("sa"));
        assert_eq!(url.password, None);
    }
}
