//! Neutral, UI-safe errors surfaced across the `zsql-ssh` boundary.

use thiserror::Error;

/// Errors an SSH tunnel operation can resolve to. Messages are written for
/// display to a user and never carry russh's internal error types or debug
/// output.
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

    /// The requested authentication method is not implemented yet.
    #[error("{auth} authentication is not yet supported")]
    UnsupportedAuth { auth: &'static str },

    /// The requested host-key verification policy is not implemented yet.
    #[error("ssh host key policy {policy} is not yet supported")]
    UnsupportedHostKeyPolicy { policy: &'static str },

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
    fn unsupported_auth_message_names_the_auth_kind() {
        let err = SshError::UnsupportedAuth { auth: "agent" };
        assert_eq!(err.to_string(), "agent authentication is not yet supported");
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
