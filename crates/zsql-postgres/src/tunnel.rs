//! Translates a connect URL plus an already-open tunnel's local address into
//! the URL this driver actually connects with, per the requested TLS
//! verification. A thin wrapper over
//! [`zsql_core::tunneled_connect_url_capping_verify_full`] naming this
//! dialect's own `sslmode` spelling.

use std::net::SocketAddr;

use zsql_core::{CoreError, SslModeSpelling, TlsVerify, tunneled_connect_url_capping_verify_full};

/// libpq's own `sslmode` query parameter and its `verify-full`/`verify-ca`
/// value spellings.
const SSLMODE: SslModeSpelling = SslModeSpelling {
    param_names: &["sslmode"],
    verify_full: "verify-full",
    verify_ca: "verify-ca",
};

/// Build the URL this driver should actually connect with when `original`
/// requested a tunnel whose local address is `tunnel_addr`, and report which
/// [`TlsVerify`] level that URL will end up requesting. See
/// [`zsql_core::tunneled_connect_url_capping_verify_full`] for the capping
/// rationale.
///
/// # Errors
/// Returns [`CoreError::Url`] if `original` cannot be parsed, or has no
/// host (a sqlite URL never reaches this driver).
pub fn tunneled_connect_url(
    original: &str,
    tunnel_addr: SocketAddr,
) -> Result<(String, TlsVerify), CoreError> {
    tunneled_connect_url_capping_verify_full(original, tunnel_addr, &SSLMODE)
}

#[cfg(test)]
mod tests {
    use zsql_core::ConnectionUrl;

    use super::{TlsVerify, tunneled_connect_url};

    #[test]
    fn no_sslmode_falls_back_to_the_plain_host_port_rewrite() {
        let addr = "127.0.0.1:54321".parse().unwrap();
        let (url, verify) =
            tunneled_connect_url("postgres://app:pw@db.internal:5432/app", addr).unwrap();
        assert_eq!(verify, TlsVerify::Off);
        assert_eq!(url, "postgres://app:pw@127.0.0.1:54321/app");
    }

    #[test]
    fn sslmode_disable_falls_back_to_the_plain_rewrite() {
        let addr = "127.0.0.1:1".parse().unwrap();
        let (_url, verify) =
            tunneled_connect_url("postgres://db.internal/app?sslmode=disable", addr).unwrap();
        assert_eq!(verify, TlsVerify::Off);
    }

    #[test]
    fn verify_full_is_capped_to_verify_ca_and_rewrites_host_and_port() {
        let addr = "127.0.0.1:54321".parse().unwrap();
        let (url, verify) = tunneled_connect_url(
            "postgres://app:pw@db.internal:5432/app?sslmode=verify-full",
            addr,
        )
        .unwrap();
        assert_eq!(
            verify,
            TlsVerify::VerifyCa,
            "verify-full must be capped to verify-ca, not silently dropped"
        );

        let parsed = ConnectionUrl::parse(&url).unwrap();
        assert_eq!(
            parsed.host().as_deref(),
            Some("127.0.0.1"),
            "the dial target must be the tunnel's loopback address"
        );
        assert_eq!(
            parsed.port(),
            Some(54321),
            "port must be the tunnel's local port"
        );
        assert_eq!(parsed.query_param("sslmode").as_deref(), Some("verify-ca"));
    }

    #[test]
    fn verify_ca_is_preserved_and_functional_through_the_tunnel() {
        let addr = "127.0.0.1:9999".parse().unwrap();
        let (url, verify) =
            tunneled_connect_url("postgres://app@db.internal/app?sslmode=verify-ca", addr).unwrap();
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
    fn verify_full_preserves_extra_query_parameters() {
        let addr = "127.0.0.1:54321".parse().unwrap();
        let (url, _verify) = tunneled_connect_url(
            "postgres://db.internal/app?sslmode=verify-full&application_name=zsql",
            addr,
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
        assert!(tunneled_connect_url("not-a-url", addr).is_err());
    }
}
