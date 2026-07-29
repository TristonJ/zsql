//! Connection URL parsing and field-level editing.
//!
//! A saved connection is persisted as a single URL string; this module lets
//! a caller (typically a UI form) show that URL's host/port/user/password/
//! database/query pieces alongside the URL text itself and keep both in
//! sync. The URL stays the single source of truth: every field setter here
//! mutates only its own component of the underlying representation and
//! reserializes through [`ConnectionUrl::to_url_string`], so a part with no
//! field of its own -- an extra query parameter, an IPv6 host,
//! percent-encoded credentials -- is carried through untouched by an edit to
//! some other field.
//!
//! Two shapes are handled: a network-style URL (`postgres://`, `mssql://`,
//! and anything else with a host), parsed with the `url` crate's
//! WHATWG-compliant percent-encoding; and a `sqlite:`/`file:` URL, which
//! names a filesystem path (or `:memory:`) rather than a network address and
//! so has no host/port/user/password/database fields at all.

use crate::error::CoreError;
use crate::tls_verify::TlsVerify;

/// A parsed connection URL, editable field-by-field and always
/// reserializable back to a URL string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionUrl {
    inner: Inner,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Inner {
    /// A URL with a host: postgres, mssql, or any other scheme shaped like
    /// one. Backed directly by `url::Url` so percent-encoding and query-pair
    /// handling come from a WHATWG-compliant parser rather than a hand
    /// rolled one.
    Network(url::Url),
    /// A `sqlite:`/`file:` URL: a filesystem path (or `:memory:`), not a
    /// network address.
    SqlitePath(SqlitePathUrl),
}

/// The scheme text and path/token of a `sqlite:`/`file:` URL, e.g.
/// `sqlite::memory:` -> (`sqlite`, `:memory:`) or `sqlite:///tmp/b.db` ->
/// (`sqlite`, `/tmp/b.db`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct SqlitePathUrl {
    scheme: String,
    path: String,
}

impl SqlitePathUrl {
    /// Parse `full` (the whole URL text) given its already-identified
    /// `scheme`. A `//` immediately after the scheme's colon is a
    /// conventional marker for an absolute filesystem path and is stripped
    /// down to the leading `/` it introduces (`sqlite:///tmp/b.db` ->
    /// `/tmp/b.db`); anything else -- a relative path or the literal
    /// `:memory:` token -- is kept exactly as written.
    fn parse(full: &str, scheme: &str) -> Self {
        let after_scheme = &full[scheme.len() + 1..];
        let path = after_scheme
            .strip_prefix("//")
            .map_or(after_scheme, |rest| rest)
            .to_owned();
        Self {
            scheme: scheme.to_owned(),
            path,
        }
    }

    /// Reserialize: an absolute path (starting with `/`) gets the `//`
    /// marker back; anything else (a relative path, `:memory:`) is written
    /// directly after the scheme's colon.
    fn to_url_string(&self) -> String {
        if self.path.starts_with('/') {
            format!("{}://{}", self.scheme, self.path)
        } else {
            format!("{}:{}", self.scheme, self.path)
        }
    }
}

/// Whether `scheme` names a filesystem-path connection rather than a
/// network one, matching the sqlite aliases [`crate::registry`] recognizes.
fn is_sqlite_like_scheme(scheme: &str) -> bool {
    scheme.eq_ignore_ascii_case("sqlite") || scheme.eq_ignore_ascii_case("file")
}

/// Percent-decode `text`, falling back to the original text (lossily, via
/// the UTF-8 replacement character) if it is not valid UTF-8 once decoded --
/// this is for display in an editable field, where showing something is
/// better than a parse error over a credential's exotic byte content.
fn percent_decode(text: &str) -> String {
    percent_encoding::percent_decode_str(text)
        .decode_utf8_lossy()
        .into_owned()
}

impl ConnectionUrl {
    /// Parse `url` into its network or sqlite-path shape.
    ///
    /// # Errors
    /// Returns [`CoreError::Url`] if `url` is empty, has no scheme, or (for
    /// a non-sqlite scheme) fails `url`'s WHATWG parse or has no host --
    /// never silently produces a half-populated/zeroed result.
    pub fn parse(url: &str) -> Result<Self, CoreError> {
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return Err(CoreError::Url("empty URL".to_owned()));
        }
        let Some((scheme, _rest)) = trimmed.split_once(':') else {
            return Err(CoreError::Url("URL has no scheme".to_owned()));
        };
        if scheme.is_empty() {
            return Err(CoreError::Url("URL has no scheme".to_owned()));
        }

        if is_sqlite_like_scheme(scheme) {
            return Ok(Self {
                inner: Inner::SqlitePath(SqlitePathUrl::parse(trimmed, scheme)),
            });
        }

        let parsed = url::Url::parse(trimmed).map_err(|err| CoreError::Url(err.to_string()))?;
        match parsed.host_str() {
            None | Some("") => return Err(CoreError::Url("URL is missing a host".to_owned())),
            Some(_) => {}
        }
        Ok(Self {
            inner: Inner::Network(parsed),
        })
    }

    /// Reserialize back to a URL string. For a [`Inner::Network`] URL this
    /// is `url::Url`'s own canonical serialization (WHATWG percent-encoding
    /// normalized); for a sqlite-path URL it is [`SqlitePathUrl::to_url_string`].
    #[must_use]
    pub fn to_url_string(&self) -> String {
        match &self.inner {
            Inner::Network(url) => url.to_string(),
            Inner::SqlitePath(sqlite) => sqlite.to_url_string(),
        }
    }

    /// Whether this URL is a sqlite-path URL (no host/port/user/password/
    /// database fields) rather than a network one.
    #[must_use]
    pub fn is_sqlite(&self) -> bool {
        matches!(self.inner, Inner::SqlitePath(_))
    }

    /// The host, if this is a network URL. `None` for a sqlite-path URL.
    #[must_use]
    pub fn host(&self) -> Option<String> {
        match &self.inner {
            Inner::Network(url) => url.host_str().map(str::to_owned),
            Inner::SqlitePath(_) => None,
        }
    }

    /// Replace the host. A no-op on a sqlite-path URL.
    ///
    /// # Errors
    /// Returns [`CoreError::Url`] if `host` is not a valid host (e.g.
    /// contains characters no host may contain).
    pub fn set_host(&mut self, host: &str) -> Result<(), CoreError> {
        let Inner::Network(url) = &mut self.inner else {
            return Ok(());
        };
        url.set_host(Some(host))
            .map_err(|_| CoreError::Url(format!("invalid host '{host}'")))
    }

    /// The explicit port, if one is present in the URL. `None` if the URL
    /// has no port (not "the driver's default port" -- this module has no
    /// opinion on driver defaults) or is a sqlite-path URL.
    #[must_use]
    pub fn port(&self) -> Option<u16> {
        match &self.inner {
            Inner::Network(url) => url.port(),
            Inner::SqlitePath(_) => None,
        }
    }

    /// Replace the port, or clear it with `None`. A no-op on a sqlite-path
    /// URL.
    ///
    /// # Errors
    /// Returns [`CoreError::Url`] if this URL cannot carry a port (e.g. has
    /// no host at all).
    pub fn set_port(&mut self, port: Option<u16>) -> Result<(), CoreError> {
        let Inner::Network(url) = &mut self.inner else {
            return Ok(());
        };
        url.set_port(port)
            .map_err(|()| CoreError::Url("this URL cannot carry a port".to_owned()))
    }

    /// The percent-decoded username. Empty if the URL carries none, or if
    /// this is a sqlite-path URL.
    #[must_use]
    pub fn user(&self) -> String {
        match &self.inner {
            Inner::Network(url) => percent_decode(url.username()),
            Inner::SqlitePath(_) => String::new(),
        }
    }

    /// The username exactly as written in the URL: still percent-encoded,
    /// not decoded. Empty if the URL carries none, or if this is a
    /// sqlite-path URL.
    ///
    /// For a caller that needs to validate percent-encoding strictly (e.g.
    /// reject a truncated `%` escape) rather than accept it leniently the
    /// way [`Self::user`]'s display-oriented decode does.
    #[must_use]
    pub fn raw_user(&self) -> &str {
        match &self.inner {
            Inner::Network(url) => url.username(),
            Inner::SqlitePath(_) => "",
        }
    }

    /// Replace the username (plain text; percent-encoding is applied for
    /// you). A no-op on a sqlite-path URL.
    pub fn set_user(&mut self, user: &str) {
        if let Inner::Network(url) = &mut self.inner {
            let _ = url.set_username(user);
        }
    }

    /// The percent-decoded password, if the URL carries one. `None` if
    /// absent, or if this is a sqlite-path URL.
    #[must_use]
    pub fn password(&self) -> Option<String> {
        match &self.inner {
            Inner::Network(url) => url.password().map(percent_decode),
            Inner::SqlitePath(_) => None,
        }
    }

    /// The password exactly as written in the URL: still percent-encoded,
    /// not decoded. `None` if absent, or if this is a sqlite-path URL. See
    /// [`Self::raw_user`] for why this differs from [`Self::password`].
    #[must_use]
    pub fn raw_password(&self) -> Option<&str> {
        match &self.inner {
            Inner::Network(url) => url.password(),
            Inner::SqlitePath(_) => None,
        }
    }

    /// Replace the password (plain text; percent-encoding is applied for
    /// you). An empty `password` clears it entirely rather than setting an
    /// empty-string password. A no-op on a sqlite-path URL.
    pub fn set_password(&mut self, password: &str) {
        let Inner::Network(url) = &mut self.inner else {
            return;
        };
        let value = if password.is_empty() {
            None
        } else {
            Some(password)
        };
        let _ = url.set_password(value);
    }

    /// The database name, taken from the URL's path with its leading `/`
    /// stripped. Empty if the URL has no path, or if this is a sqlite-path
    /// URL.
    #[must_use]
    pub fn database(&self) -> String {
        match &self.inner {
            Inner::Network(url) => url.path().trim_start_matches('/').to_owned(),
            Inner::SqlitePath(_) => String::new(),
        }
    }

    /// Replace the database name (a single path segment; percent-encoding
    /// is applied for you). A no-op on a sqlite-path URL.
    pub fn set_database(&mut self, database: &str) {
        let Inner::Network(url) = &mut self.inner else {
            return;
        };
        if database.is_empty() {
            url.set_path("");
        } else {
            url.set_path(&format!("/{database}"));
        }
    }

    /// The value of query parameter `key`, if present. `None` on a
    /// sqlite-path URL.
    #[must_use]
    pub fn query_param(&self, key: &str) -> Option<String> {
        match &self.inner {
            Inner::Network(url) => url
                .query_pairs()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.into_owned()),
            Inner::SqlitePath(_) => None,
        }
    }

    /// Set query parameter `key` to `value`, replacing its first existing
    /// occurrence in place (a repeated key collapses to one) or appending it
    /// at the end if absent. Every other query parameter -- known to a
    /// caller's field set or not -- is carried through unchanged, in its
    /// original order. A no-op on a sqlite-path URL.
    pub fn set_query_param(&mut self, key: &str, value: &str) {
        let Inner::Network(url) = &mut self.inner else {
            return;
        };
        let mut found = false;
        let mut pairs: Vec<(String, String)> = Vec::new();
        for (k, v) in url.query_pairs() {
            if k == key {
                if !found {
                    pairs.push((key.to_owned(), value.to_owned()));
                    found = true;
                }
            } else {
                pairs.push((k.into_owned(), v.into_owned()));
            }
        }
        if !found {
            pairs.push((key.to_owned(), value.to_owned()));
        }
        if pairs.is_empty() {
            url.set_query(None);
        } else {
            url.query_pairs_mut()
                .clear()
                .extend_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        }
    }

    /// Drop query parameter `key` entirely, rather than leaving it present
    /// with an empty value. Every other query parameter is carried through
    /// unchanged, in its original order. A no-op on a sqlite-path URL or if
    /// `key` is not present.
    pub fn remove_query_param(&mut self, key: &str) {
        let Inner::Network(url) = &mut self.inner else {
            return;
        };
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| k != key)
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        if pairs.is_empty() {
            url.set_query(None);
        } else {
            url.query_pairs_mut()
                .clear()
                .extend_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        }
    }

    /// The query string exactly as written in the URL (after the `?`, before
    /// any `#` fragment): not percent-decoded and its `&`/`=` structure not
    /// interpreted. `None` if the URL has no query string, or if this is a
    /// sqlite-path URL.
    ///
    /// For a caller that must parse a query value strictly (e.g. reject a
    /// value that only resembles a recognized spelling once decoded) rather
    /// than accept [`Self::query_param`]'s lenient percent- and
    /// plus-decoding.
    #[must_use]
    pub fn raw_query(&self) -> Option<&str> {
        match &self.inner {
            Inner::Network(url) => url.query(),
            Inner::SqlitePath(_) => None,
        }
    }

    /// Every query parameter whose key is not (case-insensitively) in
    /// `known_keys`, in their original order -- the parts a driver-specific
    /// field set has no slot for, shown read-only so nothing pasted into the
    /// URL is ever silently hidden. Empty on a sqlite-path URL.
    #[must_use]
    pub fn extra_query_params(&self, known_keys: &[&str]) -> Vec<(String, String)> {
        match &self.inner {
            Inner::Network(url) => url
                .query_pairs()
                .filter(|(k, _)| !known_keys.iter().any(|known| known.eq_ignore_ascii_case(k)))
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect(),
            Inner::SqlitePath(_) => Vec::new(),
        }
    }

    /// The filesystem path (or `:memory:`), if this is a sqlite-path URL.
    /// `None` on a network URL.
    #[must_use]
    pub fn sqlite_path(&self) -> Option<&str> {
        match &self.inner {
            Inner::SqlitePath(sqlite) => Some(&sqlite.path),
            Inner::Network(_) => None,
        }
    }

    /// Replace the sqlite path/token. A no-op on a network URL.
    pub fn set_sqlite_path(&mut self, path: &str) {
        if let Inner::SqlitePath(sqlite) = &mut self.inner {
            path.clone_into(&mut sqlite.path);
        }
    }
}

/// Rewrite `url`'s host and port to `tunnel_addr`, leaving every other part
/// (credentials, database, query parameters) untouched. This is the
/// non-verifying fallback for routing a network connection through a local
/// tunnel: the real host is discarded entirely, so a driver dialing the
/// rewritten URL cannot verify a TLS certificate against it.
///
/// # Errors
/// Returns [`CoreError::Url`] if `url` cannot be parsed, or is a sqlite-path
/// URL (which has no host/port to rewrite).
pub fn rewrite_for_tunnel(
    url: &str,
    tunnel_addr: std::net::SocketAddr,
) -> Result<String, CoreError> {
    let mut parsed = ConnectionUrl::parse(url)?;
    if parsed.is_sqlite() {
        return Err(CoreError::Url(
            "a sqlite URL has no host to rewrite for a tunnel".to_owned(),
        ));
    }
    parsed.set_host(&tunnel_addr.ip().to_string())?;
    parsed.set_port(Some(tunnel_addr.port()))?;
    Ok(parsed.to_url_string())
}

/// The TLS-verification query parameter a network driver's connect URL uses,
/// and the value spellings it recognizes. Different drivers accept
/// different names and value spellings for the same underlying two intents
/// (verify the certificate chain and the hostname, or the chain only), so
/// [`tunneled_connect_url_capping_verify_full`] takes this rather than
/// hard-coding either.
#[derive(Debug, Clone, Copy)]
pub struct SslModeSpelling {
    /// Query parameter names accepted for this intent, in priority order: the
    /// first one present on the URL wins. A driver that accepts more than one
    /// spelling (e.g. a current name plus a legacy alias) lists both.
    pub param_names: &'static [&'static str],
    /// The value meaning "verify the certificate chain and the hostname".
    pub verify_full: &'static str,
    /// The value meaning "verify the certificate chain only".
    pub verify_ca: &'static str,
}

impl SslModeSpelling {
    /// Read whichever of [`Self::param_names`] is present first off `url`
    /// and translate its value to a [`TlsVerify`] intent. Value comparison is
    /// case-insensitive. Anything other than [`Self::verify_full`]/
    /// [`Self::verify_ca`] (including no matching parameter at all) is
    /// [`TlsVerify::Off`]: those modes either use no TLS or accept whatever
    /// certificate the server presents, so there is nothing extra to
    /// preserve for them here.
    #[must_use]
    fn requested_verify(&self, url: &ConnectionUrl) -> TlsVerify {
        let value = self
            .param_names
            .iter()
            .find_map(|name| url.query_param(name))
            .map(|value| value.to_ascii_lowercase());
        match value.as_deref() {
            Some(v) if v == self.verify_full => TlsVerify::VerifyFull,
            Some(v) if v == self.verify_ca => TlsVerify::VerifyCa,
            _ => TlsVerify::Off,
        }
    }
}

/// Build the URL a network driver should actually connect with when
/// `original` requested a tunnel whose local address is `tunnel_addr`, and
/// report which [`TlsVerify`] level that URL will end up requesting.
///
/// Several sqlx-based client libraries resolve a connect URL's host to a
/// single dial target that is also the identity a `verify-full`-style
/// certificate check runs against, with no hook to dial one address while
/// verifying identity against another. A tunneled connection that requested
/// full identity verification is therefore capped to certificate-chain-only
/// verification instead of silently losing verification entirely: the SSH
/// transport itself already authenticates the server being tunneled to.
/// Every other request (absent, disabled, or already chain-only) uses the
/// plain fallback rewrite of host and port via [`rewrite_for_tunnel`],
/// unchanged beyond that.
///
/// # Errors
/// Returns [`CoreError::Url`] if `original` cannot be parsed, or has no host.
pub fn tunneled_connect_url_capping_verify_full(
    original: &str,
    tunnel_addr: std::net::SocketAddr,
    sslmode: &SslModeSpelling,
) -> Result<(String, TlsVerify), CoreError> {
    let parsed = ConnectionUrl::parse(original)?;
    let requested = sslmode.requested_verify(&parsed);

    match requested {
        TlsVerify::VerifyFull => {
            let mut url = parsed;
            url.set_host(&tunnel_addr.ip().to_string())?;
            url.set_port(Some(tunnel_addr.port()))?;
            url.set_query_param(sslmode.param_names[0], sslmode.verify_ca);
            Ok((url.to_url_string(), TlsVerify::VerifyCa))
        }
        TlsVerify::TrustCert | TlsVerify::VerifyCa | TlsVerify::Off => {
            Ok((rewrite_for_tunnel(original, tunnel_addr)?, requested))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConnectionUrl, SslModeSpelling, rewrite_for_tunnel,
        tunneled_connect_url_capping_verify_full,
    };
    use crate::tls_verify::TlsVerify;

    // -- parse failures ----------------------------------------------------

    #[test]
    fn an_empty_url_is_a_typed_error() {
        assert!(ConnectionUrl::parse("").is_err());
        assert!(ConnectionUrl::parse("   ").is_err());
    }

    #[test]
    fn a_url_with_no_scheme_is_a_typed_error() {
        assert!(ConnectionUrl::parse("not-a-url").is_err());
    }

    #[test]
    fn a_url_with_an_empty_scheme_before_the_first_colon_is_a_typed_error() {
        assert!(ConnectionUrl::parse(":memory:").is_err());
    }

    #[test]
    fn a_network_url_with_no_host_is_a_typed_error_not_a_blank_result() {
        // "postgres://app@" has userinfo but nothing after the '@': an
        // in-progress paste, not a connectable URL.
        assert!(ConnectionUrl::parse("postgres://app@").is_err());
    }

    #[test]
    fn an_unparseable_authority_is_a_typed_error() {
        assert!(ConnectionUrl::parse("postgres://[not-valid-ipv6/db").is_err());
    }

    // -- postgres round trip ------------------------------------------------

    #[test]
    fn a_full_postgres_url_parses_to_the_right_parts_and_rebuilds_identically() {
        let original = "postgres://app:s3cr3t@staging.internal:5432/app?sslmode=require";
        let parsed = ConnectionUrl::parse(original).expect("must parse");

        assert!(!parsed.is_sqlite());
        assert_eq!(parsed.host().as_deref(), Some("staging.internal"));
        assert_eq!(parsed.port(), Some(5432));
        assert_eq!(parsed.user(), "app");
        assert_eq!(parsed.password().as_deref(), Some("s3cr3t"));
        assert_eq!(parsed.database(), "app");
        assert_eq!(parsed.query_param("sslmode").as_deref(), Some("require"));
        assert_eq!(parsed.to_url_string(), original);
    }

    #[test]
    fn a_url_with_extra_query_params_round_trips_without_losing_them() {
        let original =
            "postgres://app@host:5432/app?sslmode=require&application_name=zsql&connect_timeout=5";
        let parsed = ConnectionUrl::parse(original).expect("must parse");
        assert_eq!(parsed.to_url_string(), original);

        let extras = parsed.extra_query_params(&["sslmode"]);
        assert_eq!(
            extras,
            vec![
                ("application_name".to_owned(), "zsql".to_owned()),
                ("connect_timeout".to_owned(), "5".to_owned()),
            ]
        );
    }

    #[test]
    fn editing_an_unrelated_field_preserves_extra_query_params_verbatim() {
        let mut parsed = ConnectionUrl::parse(
            "postgres://app@host:5432/app?sslmode=require&application_name=zsql",
        )
        .expect("must parse");

        parsed.set_port(Some(6543)).expect("set_port must succeed");

        assert_eq!(parsed.query_param("sslmode").as_deref(), Some("require"));
        assert_eq!(
            parsed.query_param("application_name").as_deref(),
            Some("zsql")
        );
        assert_eq!(
            parsed.to_url_string(),
            "postgres://app@host:6543/app?sslmode=require&application_name=zsql"
        );
    }

    #[test]
    fn set_query_param_replaces_an_existing_key_in_place_rather_than_moving_it_to_the_end() {
        let mut parsed =
            ConnectionUrl::parse("postgres://host/db?a=1&sslmode=disable&b=2").expect("must parse");
        parsed.set_query_param("sslmode", "require");
        assert_eq!(
            parsed.to_url_string(),
            "postgres://host/db?a=1&sslmode=require&b=2"
        );
    }

    #[test]
    fn remove_query_param_drops_the_key_entirely_rather_than_leaving_it_empty() {
        let mut parsed =
            ConnectionUrl::parse("postgres://host/db?a=1&sslmode=require&b=2").expect("must parse");
        parsed.remove_query_param("sslmode");
        assert_eq!(parsed.query_param("sslmode"), None);
        assert_eq!(parsed.to_url_string(), "postgres://host/db?a=1&b=2");
    }

    #[test]
    fn remove_query_param_clears_the_query_string_when_it_was_the_only_param() {
        let mut parsed =
            ConnectionUrl::parse("postgres://host/db?sslmode=require").expect("must parse");
        parsed.remove_query_param("sslmode");
        assert_eq!(parsed.to_url_string(), "postgres://host/db");
    }

    #[test]
    fn an_ipv6_host_round_trips_and_is_preserved_by_an_unrelated_field_edit() {
        let mut parsed = ConnectionUrl::parse("postgres://app@[::1]:5432/app").expect("must parse");
        assert_eq!(parsed.host().as_deref(), Some("[::1]"));

        parsed.set_database("other");
        assert_eq!(parsed.host().as_deref(), Some("[::1]"));
        assert_eq!(parsed.to_url_string(), "postgres://app@[::1]:5432/other");
    }

    #[test]
    fn reserved_characters_in_credentials_round_trip_and_survive_an_unrelated_edit() {
        let mut parsed =
            ConnectionUrl::parse("postgres://us%3Aer:p%2Fw%40rd@host/db").expect("must parse");
        assert_eq!(parsed.user(), "us:er");
        assert_eq!(parsed.password().as_deref(), Some("p/w@rd"));

        parsed.set_port(Some(5433)).expect("set_port must succeed");
        assert_eq!(
            parsed.user(),
            "us:er",
            "an unrelated edit must not touch userinfo"
        );
        assert_eq!(parsed.password().as_deref(), Some("p/w@rd"));
    }

    #[test]
    fn raw_user_and_raw_password_stay_percent_encoded() {
        let parsed =
            ConnectionUrl::parse("postgres://us%3Aer:p%2Fw%40rd@host/db").expect("must parse");
        assert_eq!(parsed.raw_user(), "us%3Aer");
        assert_eq!(parsed.raw_password(), Some("p%2Fw%40rd"));
    }

    #[test]
    fn raw_user_and_raw_password_pass_through_a_malformed_percent_escape_unchanged() {
        // The `url` crate does not validate `%` escapes on parse, so a
        // truncated or non-hex escape survives into the raw form exactly as
        // written -- a caller wanting to reject it must do so itself.
        let parsed = ConnectionUrl::parse("mssql://sa:p%zz@host/db").expect("must parse");
        assert_eq!(parsed.raw_password(), Some("p%zz"));
    }

    #[test]
    fn raw_user_and_raw_password_are_empty_and_none_with_no_userinfo() {
        let parsed = ConnectionUrl::parse("postgres://host/db").expect("must parse");
        assert_eq!(parsed.raw_user(), "");
        assert_eq!(parsed.raw_password(), None);
    }

    #[test]
    fn raw_query_is_not_percent_or_plus_decoded() {
        let parsed =
            ConnectionUrl::parse("postgres://host/db?path=/a+b&pct=%74rue").expect("must parse");
        assert_eq!(parsed.raw_query(), Some("path=/a+b&pct=%74rue"));
    }

    #[test]
    fn raw_query_is_none_with_no_query_string() {
        let parsed = ConnectionUrl::parse("postgres://host/db").expect("must parse");
        assert_eq!(parsed.raw_query(), None);
    }

    #[test]
    fn raw_query_is_none_on_a_sqlite_path_url() {
        let parsed = ConnectionUrl::parse("sqlite::memory:").expect("must parse");
        assert_eq!(parsed.raw_query(), None);
    }

    // -- editing a single field touches only that field ---------------------

    #[test]
    fn editing_the_port_leaves_user_password_database_and_other_params_intact() {
        let mut parsed =
            ConnectionUrl::parse("postgres://app:s3cr3t@host:5432/app?sslmode=require")
                .expect("must parse");

        parsed.set_port(Some(5433)).expect("set_port must succeed");

        assert_eq!(parsed.port(), Some(5433));
        assert_eq!(parsed.host().as_deref(), Some("host"));
        assert_eq!(parsed.user(), "app");
        assert_eq!(parsed.password().as_deref(), Some("s3cr3t"));
        assert_eq!(parsed.database(), "app");
        assert_eq!(parsed.query_param("sslmode").as_deref(), Some("require"));
    }

    #[test]
    fn editing_the_user_leaves_every_other_field_intact() {
        let mut parsed =
            ConnectionUrl::parse("postgres://app:s3cr3t@host:5432/app?sslmode=require")
                .expect("must parse");

        parsed.set_user("other");

        assert_eq!(parsed.user(), "other");
        assert_eq!(parsed.host().as_deref(), Some("host"));
        assert_eq!(parsed.port(), Some(5432));
        assert_eq!(parsed.password().as_deref(), Some("s3cr3t"));
        assert_eq!(parsed.database(), "app");
        assert_eq!(parsed.query_param("sslmode").as_deref(), Some("require"));
    }

    // -- mssql ----------------------------------------------------------------

    #[test]
    fn an_mssql_url_with_trust_server_certificate_parses_and_round_trips() {
        let original =
            "mssql://sa:Str0ngP%40ss@db.example.com:1433/zsql?trustServerCertificate=true";
        let parsed = ConnectionUrl::parse(original).expect("must parse");

        assert_eq!(parsed.host().as_deref(), Some("db.example.com"));
        assert_eq!(parsed.port(), Some(1433));
        assert_eq!(parsed.user(), "sa");
        assert_eq!(parsed.password().as_deref(), Some("Str0ngP@ss"));
        assert_eq!(parsed.database(), "zsql");
        assert_eq!(
            parsed.query_param("trustServerCertificate").as_deref(),
            Some("true")
        );
        assert_eq!(parsed.to_url_string(), original);
    }

    #[test]
    fn sqlserver_scheme_is_accepted_as_a_network_url_alias() {
        let parsed = ConnectionUrl::parse("sqlserver://localhost/db").expect("must parse");
        assert_eq!(parsed.host().as_deref(), Some("localhost"));
        assert_eq!(parsed.database(), "db");
    }

    // -- sqlite ---------------------------------------------------------------

    #[test]
    fn a_sqlite_memory_url_round_trips_and_has_no_network_fields() {
        let parsed = ConnectionUrl::parse("sqlite::memory:").expect("must parse");
        assert!(parsed.is_sqlite());
        assert_eq!(parsed.sqlite_path(), Some(":memory:"));
        assert_eq!(parsed.host(), None);
        assert_eq!(parsed.to_url_string(), "sqlite::memory:");
    }

    #[test]
    fn a_sqlite_absolute_path_url_round_trips() {
        let parsed = ConnectionUrl::parse("sqlite:///tmp/b.db").expect("must parse");
        assert!(parsed.is_sqlite());
        assert_eq!(parsed.sqlite_path(), Some("/tmp/b.db"));
        assert_eq!(parsed.to_url_string(), "sqlite:///tmp/b.db");
    }

    #[test]
    fn editing_the_sqlite_path_changes_only_the_path() {
        let mut parsed = ConnectionUrl::parse("sqlite:///tmp/b.db").expect("must parse");
        parsed.set_sqlite_path("/tmp/other.db");
        assert_eq!(parsed.to_url_string(), "sqlite:///tmp/other.db");
    }

    #[test]
    fn a_relative_sqlite_path_round_trips() {
        let parsed = ConnectionUrl::parse("sqlite:data/scratch.db").expect("must parse");
        assert_eq!(parsed.sqlite_path(), Some("data/scratch.db"));
        assert_eq!(parsed.to_url_string(), "sqlite:data/scratch.db");
    }

    #[test]
    fn the_file_scheme_is_treated_as_a_sqlite_path_url() {
        let parsed = ConnectionUrl::parse("file:./local.db").expect("must parse");
        assert!(parsed.is_sqlite());
        assert_eq!(parsed.sqlite_path(), Some("./local.db"));
        assert_eq!(parsed.to_url_string(), "file:./local.db");
    }

    // -- byte-identical: fields-built vs. directly-parsed ----------------------

    #[test]
    fn a_connection_built_entirely_via_field_setters_serializes_identically_to_the_same_one_pasted()
    {
        let pasted = "postgres://app:s3cr3t@staging.internal:5432/app?sslmode=require";
        let via_paste = ConnectionUrl::parse(pasted).expect("must parse");

        let mut via_fields = ConnectionUrl::parse("postgres://placeholder").expect("must parse");
        via_fields.set_host("staging.internal").expect("set_host");
        via_fields.set_port(Some(5432)).expect("set_port");
        via_fields.set_user("app");
        via_fields.set_password("s3cr3t");
        via_fields.set_database("app");
        via_fields.set_query_param("sslmode", "require");

        assert_eq!(via_fields.to_url_string(), via_paste.to_url_string());
    }

    #[test]
    fn set_query_param_preserves_extra_params_through_tls_field_edits() {
        let mut parsed =
            ConnectionUrl::parse("postgres://host/db?extra=hello%20world&sslmode=disable")
                .expect("must parse");
        // Verify the extra param is present before editing TLS.
        assert_eq!(
            parsed.query_param("extra"),
            Some("hello world".to_string()),
            "extra param must be readable before edit"
        );

        // Edit a TLS field, which re-serializes the query string.
        parsed.set_query_param("sslmode", "require");

        // The extra param must survive the TLS edit, even if encoding normalizes.
        let url_str = parsed.to_url_string();
        assert!(
            url_str.contains("extra="),
            "extra param must be preserved in URL string after TLS edit"
        );
        assert_eq!(
            parsed.query_param("extra"),
            Some("hello world".to_string()),
            "extra param must remain readable after TLS edit"
        );
    }

    // -- rewrite_for_tunnel -----------------------------------------------

    #[test]
    fn rewrite_for_tunnel_replaces_only_the_host_and_port() {
        let tunnel_addr = "127.0.0.1:54321".parse().unwrap();
        let rewritten = rewrite_for_tunnel(
            "postgres://app:s3cr3t@staging.internal:5432/app",
            tunnel_addr,
        )
        .expect("rewrite must succeed");
        assert_eq!(rewritten, "postgres://app:s3cr3t@127.0.0.1:54321/app");
    }

    #[test]
    fn rewrite_for_tunnel_preserves_a_percent_encoded_password() {
        let tunnel_addr = "127.0.0.1:9999".parse().unwrap();
        let rewritten =
            rewrite_for_tunnel("postgres://app:p%2Fw%40rd@db.example.com/app", tunnel_addr)
                .expect("rewrite must succeed");
        let parsed = ConnectionUrl::parse(&rewritten).expect("rewritten URL must parse");
        assert_eq!(parsed.host().as_deref(), Some("127.0.0.1"));
        assert_eq!(parsed.port(), Some(9999));
        assert_eq!(
            parsed.password().as_deref(),
            Some("p/w@rd"),
            "the original percent-encoded password must survive the rewrite"
        );
    }

    #[test]
    fn rewrite_for_tunnel_discards_an_ipv6_remote_host_in_favor_of_the_tunnel_address() {
        let tunnel_addr = "127.0.0.1:7777".parse().unwrap();
        let rewritten = rewrite_for_tunnel("postgres://app@[2001:db8::1]:5432/app", tunnel_addr)
            .expect("rewrite must succeed");
        let parsed = ConnectionUrl::parse(&rewritten).expect("rewritten URL must parse");
        assert_eq!(parsed.host().as_deref(), Some("127.0.0.1"));
        assert_eq!(parsed.port(), Some(7777));
        assert_eq!(parsed.user(), "app");
    }

    #[test]
    fn rewrite_for_tunnel_rejects_a_sqlite_url() {
        let tunnel_addr = "127.0.0.1:1".parse().unwrap();
        assert!(rewrite_for_tunnel("sqlite::memory:", tunnel_addr).is_err());
    }

    // -- tunneled_connect_url_capping_verify_full ---------------------------

    /// A postgres-shaped spelling: single `sslmode` parameter,
    /// `verify-full`/`verify-ca` values.
    const POSTGRES_SSLMODE: SslModeSpelling = SslModeSpelling {
        param_names: &["sslmode"],
        verify_full: "verify-full",
        verify_ca: "verify-ca",
    };

    /// A mysql-shaped spelling: two accepted parameter names (a primary and a
    /// legacy alias), `verify_identity`/`verify_ca` values.
    const MYSQL_SSLMODE: SslModeSpelling = SslModeSpelling {
        param_names: &["ssl-mode", "sslmode"],
        verify_full: "verify_identity",
        verify_ca: "verify_ca",
    };

    #[test]
    fn no_sslmode_falls_back_to_the_plain_host_port_rewrite() {
        let addr = "127.0.0.1:54321".parse().unwrap();
        let (url, verify) = tunneled_connect_url_capping_verify_full(
            "postgres://app:pw@db.internal:5432/app",
            addr,
            &POSTGRES_SSLMODE,
        )
        .unwrap();
        assert_eq!(verify, TlsVerify::Off);
        assert_eq!(url, "postgres://app:pw@127.0.0.1:54321/app");
    }

    #[test]
    fn verify_full_is_capped_to_verify_ca_and_rewrites_host_and_port() {
        let addr = "127.0.0.1:54321".parse().unwrap();
        let (url, verify) = tunneled_connect_url_capping_verify_full(
            "postgres://app:pw@db.internal:5432/app?sslmode=verify-full",
            addr,
            &POSTGRES_SSLMODE,
        )
        .unwrap();
        assert_eq!(
            verify,
            TlsVerify::VerifyCa,
            "verify-full must be capped to verify-ca, not silently dropped"
        );
        let parsed = ConnectionUrl::parse(&url).unwrap();
        assert_eq!(parsed.host().as_deref(), Some("127.0.0.1"));
        assert_eq!(parsed.port(), Some(54321));
        assert_eq!(parsed.query_param("sslmode").as_deref(), Some("verify-ca"));
    }

    #[test]
    fn verify_ca_is_preserved_and_functional_through_the_tunnel() {
        let addr = "127.0.0.1:9999".parse().unwrap();
        let (url, verify) = tunneled_connect_url_capping_verify_full(
            "postgres://app@db.internal/app?sslmode=verify-ca",
            addr,
            &POSTGRES_SSLMODE,
        )
        .unwrap();
        assert_eq!(verify, TlsVerify::VerifyCa);
        let parsed = ConnectionUrl::parse(&url).unwrap();
        assert_eq!(parsed.host().as_deref(), Some("127.0.0.1"));
        assert_eq!(parsed.port(), Some(9999));
        assert_eq!(
            parsed.query_param("sslmode").as_deref(),
            Some("verify-ca"),
            "an already-verify-ca request must not be altered"
        );
    }

    #[test]
    fn extra_query_parameters_are_preserved_through_a_capped_rewrite() {
        let addr = "127.0.0.1:54321".parse().unwrap();
        let (url, _verify) = tunneled_connect_url_capping_verify_full(
            "postgres://db.internal/app?sslmode=verify-full&application_name=zsql",
            addr,
            &POSTGRES_SSLMODE,
        )
        .unwrap();
        let parsed = ConnectionUrl::parse(&url).unwrap();
        assert_eq!(
            parsed.query_param("application_name").as_deref(),
            Some("zsql")
        );
    }

    #[test]
    fn an_invalid_url_is_a_typed_error() {
        let addr = "127.0.0.1:1".parse().unwrap();
        assert!(
            tunneled_connect_url_capping_verify_full("not-a-url", addr, &POSTGRES_SSLMODE).is_err()
        );
    }

    #[test]
    fn a_second_accepted_parameter_name_is_read_when_the_primary_is_absent() {
        let addr = "127.0.0.1:54321".parse().unwrap();
        let (url, verify) = tunneled_connect_url_capping_verify_full(
            "mysql://root:pw@db.internal:3306/app?sslmode=verify_identity",
            addr,
            &MYSQL_SSLMODE,
        )
        .unwrap();
        assert_eq!(verify, TlsVerify::VerifyCa);
        let parsed = ConnectionUrl::parse(&url).unwrap();
        assert_eq!(
            parsed.query_param("ssl-mode").as_deref(),
            Some("verify_ca"),
            "the capped value is written back under the primary parameter name"
        );
    }

    #[test]
    fn value_comparison_is_case_insensitive() {
        let addr = "127.0.0.1:54321".parse().unwrap();
        let (_url, verify) = tunneled_connect_url_capping_verify_full(
            "mysql://root@db.internal/app?ssl-mode=VERIFY_IDENTITY",
            addr,
            &MYSQL_SSLMODE,
        )
        .unwrap();
        assert_eq!(verify, TlsVerify::VerifyCa);
    }
}
