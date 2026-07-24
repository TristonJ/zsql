//! Translates a connect URL plus an already-open tunnel's local address into
//! the URL this driver actually connects with. Pure and network-free: every
//! function here only builds a string.
//!
//! sqlx's `MySqlConnectOptions` has a single `host` field used both to dial
//! and to verify a certificate's identity against, with no hook to install a
//! custom TLS verifier -- so, unlike Postgres, there is no way to dial a
//! tunnel's loopback address while still checking the certificate against
//! the real remote hostname. A tunneled connection that requested full
//! identity verification is capped to CA-chain verification instead of
//! silently losing verification entirely: the SSH transport itself already
//! authenticates the server being tunneled to.

use std::net::SocketAddr;

use zsql_core::{ConnectionUrl, CoreError, TlsVerify, rewrite_for_tunnel};

/// The `ssl-mode` value requesting full certificate-chain and hostname
/// verification, matching sqlx's own spelling.
const SSLMODE_VERIFY_IDENTITY: &str = "verify_identity";
/// The `ssl-mode` value requesting certificate-chain verification without
/// checking the hostname -- what a tunneled `verify_identity` request is
/// capped to.
const SSLMODE_VERIFY_CA: &str = "verify_ca";

/// Build the URL this driver should actually connect with when `original`
/// requested a tunnel whose local address is `tunnel_addr`, and report which
/// [`TlsVerify`] level that URL will end up requesting.
///
/// A requested `verify_identity` is capped to `verify_ca`: the certificate
/// chain is still checked, but not the hostname (which cannot be, once the
/// dial target is rewritten to the tunnel's loopback address). Every other
/// mode (absent, `disabled`, `preferred`, `required`, or an already-`verify_ca`
/// request) uses the plain fallback rewrite, unchanged beyond host and port.
///
/// # Errors
/// Returns [`CoreError::Url`] if `original` cannot be parsed, or has no
/// host.
pub fn tunneled_connect_url(
    original: &str,
    tunnel_addr: SocketAddr,
) -> Result<(String, TlsVerify), CoreError> {
    let normalized = crate::url::normalize_for_sqlx(original)?;
    let requested = detect_requested_verify(&ConnectionUrl::parse(&normalized)?);

    match requested {
        TlsVerify::VerifyFull => {
            let mut url = ConnectionUrl::parse(&normalized)?;
            url.set_host(&tunnel_addr.ip().to_string())?;
            url.set_port(Some(tunnel_addr.port()))?;
            url.set_query_param("ssl-mode", SSLMODE_VERIFY_CA);
            Ok((url.to_url_string(), TlsVerify::VerifyCa))
        }
        TlsVerify::VerifyCa | TlsVerify::Off => {
            Ok((rewrite_for_tunnel(&normalized, tunnel_addr)?, requested))
        }
    }
}

/// Read the `ssl-mode`/`sslmode` query parameter off `url` and translate it
/// to a [`TlsVerify`] intent. Anything other than `verify_identity`/
/// `verify_ca` (including no parameter at all) is [`TlsVerify::Off`]: those
/// modes either use no TLS or accept whatever certificate the server
/// presents, so tunneling has nothing extra to preserve for them.
fn detect_requested_verify(url: &ConnectionUrl) -> TlsVerify {
    let mode = url
        .query_param("ssl-mode")
        .or_else(|| url.query_param("sslmode"))
        .map(|value| value.to_ascii_lowercase());
    match mode.as_deref() {
        Some(SSLMODE_VERIFY_IDENTITY) => TlsVerify::VerifyFull,
        Some(SSLMODE_VERIFY_CA) => TlsVerify::VerifyCa,
        _ => TlsVerify::Off,
    }
}

#[cfg(test)]
mod tests {
    use zsql_core::ConnectionUrl;

    use super::{TlsVerify, tunneled_connect_url};

    #[test]
    fn no_ssl_mode_falls_back_to_the_plain_host_port_rewrite() {
        let addr = "127.0.0.1:54321".parse().unwrap();
        let (url, verify) =
            tunneled_connect_url("mysql://root:pw@db.internal:3306/app", addr).unwrap();
        assert_eq!(verify, TlsVerify::Off);
        assert_eq!(url, "mysql://root:pw@127.0.0.1:54321/app");
    }

    #[test]
    fn a_mariadb_scheme_is_normalized_before_rewriting() {
        let addr = "127.0.0.1:1".parse().unwrap();
        let (url, _verify) = tunneled_connect_url("mariadb://root@db.internal/app", addr).unwrap();
        assert!(url.starts_with("mysql://"));
    }

    #[test]
    fn verify_identity_is_capped_to_verify_ca_and_rewrites_host_and_port() {
        let addr = "127.0.0.1:54321".parse().unwrap();
        let (url, verify) = tunneled_connect_url(
            "mysql://root:pw@db.internal:3306/app?ssl-mode=verify_identity",
            addr,
        )
        .unwrap();
        assert_eq!(
            verify,
            TlsVerify::VerifyCa,
            "verify_identity must be capped to verify_ca, not silently dropped"
        );

        let parsed = ConnectionUrl::parse(&url).unwrap();
        assert_eq!(parsed.host().as_deref(), Some("127.0.0.1"));
        assert_eq!(parsed.port(), Some(54321));
        assert_eq!(parsed.query_param("ssl-mode").as_deref(), Some("verify_ca"));
    }

    #[test]
    fn verify_ca_is_preserved_and_functional_through_the_tunnel() {
        let addr = "127.0.0.1:9999".parse().unwrap();
        let (url, verify) =
            tunneled_connect_url("mysql://root@db.internal/app?ssl-mode=verify_ca", addr).unwrap();
        assert_eq!(verify, TlsVerify::VerifyCa);

        let parsed = ConnectionUrl::parse(&url).unwrap();
        assert_eq!(parsed.host().as_deref(), Some("127.0.0.1"));
        assert_eq!(parsed.port(), Some(9999));
        assert_eq!(
            parsed.query_param("ssl-mode").as_deref(),
            Some("verify_ca"),
            "an already-verify_ca request must not be altered"
        );
    }

    #[test]
    fn required_mode_falls_back_to_the_plain_rewrite() {
        let addr = "127.0.0.1:1".parse().unwrap();
        let (_url, verify) =
            tunneled_connect_url("mysql://db.internal/app?ssl-mode=required", addr).unwrap();
        assert_eq!(verify, TlsVerify::Off);
    }

    #[test]
    fn an_invalid_url_is_a_typed_error() {
        let addr = "127.0.0.1:1".parse().unwrap();
        assert!(tunneled_connect_url("not-a-url", addr).is_err());
    }
}
