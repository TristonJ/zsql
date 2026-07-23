//! Configuration types for an SSH tunnel. Nothing in this module depends on
//! tokio or russh.

use std::path::PathBuf;
use std::time::Duration;

/// The standard SSH port, and [`SshConfig`]'s default. Named so it is never
/// re-typed as a bare literal at more than one call site.
pub(crate) const DEFAULT_SSH_PORT: u16 = 22;

/// How often an open tunnel sends a keepalive probe on its SSH session by
/// default, chosen to comfortably beat common NAT/firewall idle timeouts
/// (typically 60s or more) without adding meaningful traffic.
pub(crate) const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// How to reach an SSH server, authenticate to it, and verify its identity.
#[derive(Debug, Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: SshAuth,
    pub host_key: HostKeyPolicy,
    /// How often to send an SSH keepalive on this session.
    /// `Duration::ZERO` disables the keepalive probe entirely.
    pub keepalive: Duration,
}

impl SshConfig {
    /// Builds a config for `host`/`user` with the documented defaults:
    /// port [`DEFAULT_SSH_PORT`], [`HostKeyPolicy::AcceptNew`], and
    /// [`DEFAULT_KEEPALIVE_INTERVAL`].
    pub fn new(host: impl Into<String>, user: impl Into<String>, auth: SshAuth) -> Self {
        Self {
            host: host.into(),
            port: DEFAULT_SSH_PORT,
            user: user.into(),
            auth,
            host_key: HostKeyPolicy::AcceptNew,
            keepalive: DEFAULT_KEEPALIVE_INTERVAL,
        }
    }
}

/// How to authenticate an SSH session.
#[derive(Clone)]
pub enum SshAuth {
    /// A plaintext password, sent over the encrypted SSH transport.
    Password(String),
    /// Delegate signing to a running `SSH_AUTH_SOCK` agent.
    Agent,
    /// A private key file on disk, optionally passphrase-protected.
    Key {
        path: PathBuf,
        passphrase: Option<String>,
    },
}

// Manual so a debug-print of an `SshAuth` (or a `SshConfig` that derives
// `Debug` through it) can never surface the password or passphrase. The key
// path and whether a passphrase is set stay visible; the secret bytes do not.
impl std::fmt::Debug for SshAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Password(_) => f.write_str("Password(<redacted>)"),
            Self::Agent => f.write_str("Agent"),
            Self::Key { path, passphrase } => f
                .debug_struct("Key")
                .field("path", path)
                .field("passphrase", &passphrase.as_ref().map(|_| "<redacted>"))
                .finish(),
        }
    }
}

/// How to verify the SSH server's host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyPolicy {
    /// Accept any host key the server presents, without verifying it
    /// against anything previously seen for this host.
    AcceptNew,
    /// Verify against a `known_hosts` file at the given path.
    KnownHosts(PathBuf),
    /// Reserved for an interactive trust-on-first-use confirmation. Not
    /// wired to any verification behavior yet.
    Prompt,
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_KEEPALIVE_INTERVAL, DEFAULT_SSH_PORT, HostKeyPolicy, SshAuth, SshConfig};

    #[test]
    fn new_defaults_port_to_the_standard_ssh_port() {
        let cfg = SshConfig::new(
            "db.example.com",
            "alice",
            SshAuth::Password("hunter2".into()),
        );
        assert_eq!(cfg.port, DEFAULT_SSH_PORT);
        assert_eq!(cfg.port, 22);
    }

    #[test]
    fn new_defaults_host_key_policy_to_accept_new() {
        let cfg = SshConfig::new("db.example.com", "alice", SshAuth::Agent);
        assert_eq!(cfg.host_key, HostKeyPolicy::AcceptNew);
    }

    #[test]
    fn new_defaults_keepalive_to_the_standard_interval() {
        let cfg = SshConfig::new("db.example.com", "alice", SshAuth::Agent);
        assert_eq!(cfg.keepalive, DEFAULT_KEEPALIVE_INTERVAL);
    }

    #[test]
    fn new_preserves_host_user_and_auth() {
        let cfg = SshConfig::new(
            "db.example.com",
            "alice",
            SshAuth::Password("hunter2".into()),
        );
        assert_eq!(cfg.host, "db.example.com");
        assert_eq!(cfg.user, "alice");
        assert!(matches!(cfg.auth, SshAuth::Password(ref p) if p == "hunter2"));
    }

    #[test]
    fn fields_are_directly_overridable_after_construction() {
        let mut cfg = SshConfig::new("db.example.com", "alice", SshAuth::Agent);
        cfg.port = 2222;
        cfg.host_key = HostKeyPolicy::KnownHosts("/home/alice/.ssh/known_hosts".into());
        assert_eq!(cfg.port, 2222);
        assert_eq!(
            cfg.host_key,
            HostKeyPolicy::KnownHosts("/home/alice/.ssh/known_hosts".into())
        );
    }

    #[test]
    fn debug_redacts_the_password_and_passphrase() {
        let password = SshAuth::Password("hunter2".into());
        let rendered = format!("{password:?}");
        assert!(!rendered.contains("hunter2"), "password leaked: {rendered}");
        assert!(rendered.contains("redacted"));

        let key = SshAuth::Key {
            path: "/home/alice/.ssh/id_ed25519".into(),
            passphrase: Some("s3cr3t".into()),
        };
        let rendered = format!("{key:?}");
        assert!(
            !rendered.contains("s3cr3t"),
            "passphrase leaked: {rendered}"
        );
        assert!(rendered.contains("id_ed25519"), "path should stay visible");

        // A whole SshConfig debug-prints through SshAuth, so it must be clean too.
        let cfg = SshConfig::new(
            "db.example.com",
            "alice",
            SshAuth::Password("hunter2".into()),
        );
        assert!(!format!("{cfg:?}").contains("hunter2"));
    }
}
