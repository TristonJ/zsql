//! Translates a connect URL plus an already-open tunnel's local address into
//! the URL this driver actually connects with. A thin wrapper over
//! [`zsql_core::tunneled_connect_url_capping_verify_full`] naming this
//! dialect's own `ssl-mode`/`sslmode` spelling, on top of the `mariadb://` ->
//! `mysql://` scheme normalization sqlx's `MySql` backend requires.

use std::net::SocketAddr;

use zsql_core::{CoreError, SslModeSpelling, TlsVerify};

/// sqlx's own `ssl-mode` query parameter (with the legacy `sslmode` spelling
/// also accepted) and its `verify_identity`/`verify_ca` value spellings.
const SSLMODE: SslModeSpelling = SslModeSpelling {
    param_names: &["ssl-mode", "sslmode"],
    verify_full: "verify_identity",
    verify_ca: "verify_ca",
};

/// Build the URL this driver should actually connect with when `original`
/// requested a tunnel whose local address is `tunnel_addr`, and report which
/// [`TlsVerify`] level that URL will end up requesting. See
/// [`zsql_core::tunneled_connect_url_capping_verify_full`] for the capping
/// rationale.
///
/// # Errors
/// Returns [`CoreError::Url`] if `original` cannot be parsed, or has no
/// host.
pub fn tunneled_connect_url(
    original: &str,
    tunnel_addr: SocketAddr,
) -> Result<(String, TlsVerify), CoreError> {
    let normalized = crate::url::normalize_for_sqlx(original)?;
    zsql_core::tunneled_connect_url_capping_verify_full(&normalized, tunnel_addr, &SSLMODE)
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
