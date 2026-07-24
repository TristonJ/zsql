//! Per-connection TLS-verification intent: whether (and how strictly) the
//! server's TLS certificate is checked, independent of whatever transport
//! (direct, or through a tunnel) actually carries the bytes. Each network
//! driver crate translates this into its own client library's terms.

/// How strictly a network driver should verify the server's TLS certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TlsVerify {
    /// No certificate verification requested (or TLS not in use at all).
    #[default]
    Off,
    /// Verify the certificate chain against a trusted CA, but not the
    /// server's hostname.
    VerifyCa,
    /// Verify the certificate chain and that it was issued for the server's
    /// hostname.
    VerifyFull,
}

impl TlsVerify {
    /// A short, stable label safe to attach to a tracing span or log line --
    /// never derived from anything secret.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::VerifyCa => "verify-ca",
            Self::VerifyFull => "verify-full",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TlsVerify;

    #[test]
    fn default_is_off() {
        assert_eq!(TlsVerify::default(), TlsVerify::Off);
    }

    #[test]
    fn label_names_each_variant() {
        assert_eq!(TlsVerify::Off.label(), "off");
        assert_eq!(TlsVerify::VerifyCa.label(), "verify-ca");
        assert_eq!(TlsVerify::VerifyFull.label(), "verify-full");
    }
}
