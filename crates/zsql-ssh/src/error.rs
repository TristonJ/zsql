//! Neutral, UI-safe errors surfaced across the `zsql-ssh` boundary.

use std::path::PathBuf;

use thiserror::Error;

/// Errors an SSH tunnel operation can resolve to. Messages are written for
/// display to a user and never carry russh's internal error types, debug
/// output, or secret material.
#[derive(Debug, Error)]
pub enum SshError {
    /// The TCP connection or SSH handshake to the SSH host failed.
    #[error("could not reach ssh host {host}:{port}: {reason}")]
    Connect {
        host: String,
        port: u16,
        reason: String,
    },

    /// The SSH server rejected the supplied credentials.
    #[error("ssh authentication failed for user {user}")]
    AuthFailed { user: String },

    /// The private key file at `path` could not be opened or parsed.
    #[error("could not read the ssh key file at {}", path.display())]
    KeyUnreadable { path: PathBuf },

    /// The private key file at `path` is passphrase-protected and either no
    /// passphrase was supplied or the one supplied did not decrypt it.
    #[error(
        "the ssh key file at {} is passphrase-protected and could not be decrypted",
        path.display()
    )]
    KeyPassphrase { path: PathBuf },

    /// No ssh-agent could be reached to authenticate with.
    #[error("no ssh-agent is available to authenticate with")]
    AgentUnavailable,

    /// The requested host-key verification policy is not implemented yet.
    #[error("ssh host key policy {policy} is not yet supported")]
    UnsupportedHostKeyPolicy { policy: &'static str },

    /// The server's host key does not match the one previously recorded for
    /// this host, which could indicate a man-in-the-middle attack.
    #[error("ssh host key for {host}:{port} has changed since it was last seen")]
    HostKeyChanged { host: String, port: u16 },

    /// A strict host-key policy has no recorded entry for this host.
    #[error("ssh host key for {host}:{port} is not in the known_hosts file")]
    HostKeyUnknown { host: String, port: u16 },

    /// The `known_hosts` file could not be read or updated.
    #[error("could not access the known_hosts file: {reason}")]
    HostKeyStore { reason: String },

    /// The SSH session ended or misbehaved after the handshake completed.
    #[error("ssh session error: {reason}")]
    Session { reason: String },

    /// The local loopback listener that backs the tunnel could not be
    /// created.
    #[error("could not open the local tunnel listener: {reason}")]
    ListenerBind { reason: String },

    /// The shared background runtime that drives every tunnel is not
    /// available, so the tunnel request could not be serviced.
    #[error("the ssh tunnel background runtime is unavailable")]
    RuntimeUnavailable,
}

#[cfg(test)]
mod tests {
    use super::SshError;

    #[test]
    fn connect_error_message_names_host_port_and_reason() {
        let err = SshError::Connect {
            host: "db.example.com".to_owned(),
            port: 22,
            reason: "connection refused".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "could not reach ssh host db.example.com:22: connection refused"
        );
    }

    #[test]
    fn auth_failed_message_names_user_without_leaking_credentials() {
        let err = SshError::AuthFailed {
            user: "deploy".to_owned(),
        };
        assert_eq!(err.to_string(), "ssh authentication failed for user deploy");
    }

    #[test]
    fn key_unreadable_message_names_the_path_only() {
        let err = SshError::KeyUnreadable {
            path: "/home/alice/.ssh/id_ed25519".into(),
        };
        let rendered = err.to_string();
        assert_eq!(
            rendered,
            "could not read the ssh key file at /home/alice/.ssh/id_ed25519"
        );
    }

    #[test]
    fn key_passphrase_message_names_the_path_without_the_passphrase() {
        let err = SshError::KeyPassphrase {
            path: "/home/alice/.ssh/id_ed25519".into(),
        };
        let rendered = err.to_string();
        assert!(rendered.contains("/home/alice/.ssh/id_ed25519"));
        assert!(!rendered.to_lowercase().contains("hunter"));
    }

    #[test]
    fn agent_unavailable_message_is_static() {
        let err = SshError::AgentUnavailable;
        assert_eq!(
            err.to_string(),
            "no ssh-agent is available to authenticate with"
        );
    }

    #[test]
    fn unsupported_host_key_policy_message_names_the_policy() {
        let err = SshError::UnsupportedHostKeyPolicy {
            policy: "known_hosts",
        };
        assert_eq!(
            err.to_string(),
            "ssh host key policy known_hosts is not yet supported"
        );
    }

    #[test]
    fn host_key_changed_message_names_host_and_port_without_key_bytes() {
        let err = SshError::HostKeyChanged {
            host: "db.example.com".to_owned(),
            port: 22,
        };
        let rendered = err.to_string();
        assert_eq!(
            rendered,
            "ssh host key for db.example.com:22 has changed since it was last seen"
        );
        assert!(!rendered.contains("ssh-ed25519"));
        assert!(!rendered.contains("AAAA"));
    }

    #[test]
    fn host_key_unknown_message_names_host_and_port() {
        let err = SshError::HostKeyUnknown {
            host: "db.example.com".to_owned(),
            port: 2222,
        };
        assert_eq!(
            err.to_string(),
            "ssh host key for db.example.com:2222 is not in the known_hosts file"
        );
    }

    #[test]
    fn host_key_store_message_carries_the_reason() {
        let err = SshError::HostKeyStore {
            reason: "could not record the ssh host key".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "could not access the known_hosts file: could not record the ssh host key"
        );
    }

    #[test]
    fn session_error_message_carries_the_reason() {
        let err = SshError::Session {
            reason: "connection closed by the remote side".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "ssh session error: connection closed by the remote side"
        );
    }

    #[test]
    fn listener_bind_error_message_carries_the_reason() {
        let err = SshError::ListenerBind {
            reason: "address already in use".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "could not open the local tunnel listener: address already in use"
        );
    }

    #[test]
    fn runtime_unavailable_message_is_static() {
        let err = SshError::RuntimeUnavailable;
        assert_eq!(
            err.to_string(),
            "the ssh tunnel background runtime is unavailable"
        );
    }
}
