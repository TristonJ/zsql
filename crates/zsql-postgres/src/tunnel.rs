//! Translates a connect URL plus an already-open tunnel's local address into
//! the URL this driver actually connects with, per the requested TLS
//! verification. Pure and network-free: every function here only builds a
//! string.
//!
//! sqlx-postgres resolves a URL's `host` and `hostaddr` to a single dial
//! target that it also verifies a `verify-full` certificate's hostname
//! against, with no hook to dial one address while verifying identity
//! against another. A tunneled connection that requested full identity
//! verification is therefore capped to CA-chain verification instead of
//! silently losing verification entirely: the SSH transport itself already
//! authenticates the server being tunneled to.

use std::net::SocketAddr;

use zsql_core::{ConnectionUrl, CoreError, TlsVerify, rewrite_for_tunnel};

/// The `sslmode` value requesting full certificate-chain and hostname
/// verification, matching libpq's own spelling.
const SSLMODE_VERIFY_FULL: &str = "verify-full";
/// The `sslmode` value requesting certificate-chain verification without
/// checking the hostname -- what a tunneled `verify-full` request is capped
/// to.
const SSLMODE_VERIFY_CA: &str = "verify-ca";

/// Build the URL this driver should actually connect with when `original`
/// requested a tunnel whose local address is `tunnel_addr`, and report which
/// [`TlsVerify`] level that URL will end up requesting.
///
/// A requested `verify-full` is capped to `verify-ca`: the certificate chain
/// is still checked, but not the hostname (which cannot be, once the dial
/// target is rewritten to the tunnel's loopback address). Every other mode
/// (absent, `disable`, `allow`, `prefer`, `require`, or an already-`verify-ca`
/// request) uses the plain fallback rewrite of host and port via
/// [`zsql_core::rewrite_for_tunnel`], unchanged beyond that.
///
/// # Errors
/// Returns [`CoreError::Url`] if `original` cannot be parsed, or has no
/// host (a sqlite URL never reaches this driver).
pub fn tunneled_connect_url(
    original: &str,
    tunnel_addr: SocketAddr,
) -> Result<(String, TlsVerify), CoreError> {
    let parsed = ConnectionUrl::parse(original)?;
    let requested = detect_requested_verify(&parsed);

    match requested {
        TlsVerify::VerifyFull => {
            let mut url = parsed;
            url.set_host(&tunnel_addr.ip().to_string())?;
            url.set_port(Some(tunnel_addr.port()))?;
            url.set_query_param("sslmode", SSLMODE_VERIFY_CA);
            Ok((url.to_url_string(), TlsVerify::VerifyCa))
        }
        TlsVerify::VerifyCa | TlsVerify::Off => {
            Ok((rewrite_for_tunnel(original, tunnel_addr)?, requested))
        }
    }
}

/// Read `sslmode` off `url` and translate it to a [`TlsVerify`] intent.
/// Anything other than `verify-full`/`verify-ca` (including no `sslmode` at
/// all) is treated as [`TlsVerify::Off`]: those modes either use no TLS or
/// accept whatever certificate the server presents, so this driver's tunnel
/// translation has nothing extra to preserve for them.
fn detect_requested_verify(url: &ConnectionUrl) -> TlsVerify {
    match url.query_param("sslmode").as_deref() {
        Some(SSLMODE_VERIFY_FULL) => TlsVerify::VerifyFull,
        Some(SSLMODE_VERIFY_CA) => TlsVerify::VerifyCa,
        _ => TlsVerify::Off,
    }
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
