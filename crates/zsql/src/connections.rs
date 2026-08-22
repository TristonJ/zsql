//! Persisted connection store: user-named database connections saved to
//! disk under [`crate::config::Config::connections_path`]. Connection URLs
//! themselves are stored in the OS keyring.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use zsql_core::ConnectionUrl;

use crate::{drivers::detect_driver_name, ui::format::host_label};

mod secrets;
mod ssh_translate;
mod store;

pub use ssh_translate::ssh_config_from_stored;
pub use store::{ConnectionStore, ConnectionStoreError};

/// A user-named, persisted connection: a display name paired with its id.
/// The connection URL itself is stored using the OS keyring
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredConnection {
    /// An automatically-generated unique ID for this connection, used to identify it
    pub id: uuid::Uuid,
    /// User-given display name.
    pub name: String,
    /// A displayed kind for this connection - persisted so we don't need to access the
    /// keyring to display information about the connection.
    pub display_kind: String,
    /// A displayed host for this connection - persisted so we don't need to access the
    /// keyring to display information about the connection.
    pub display_host: String,
    /// Non-secret SSH tunnel settings. Absent for connections with no
    /// tunnel configured, and for connections saved before SSH tunnel
    /// support existed.
    #[serde(default)]
    pub ssh: Option<StoredSsh>,
    /// The connection URL with its password cleared, kept alongside the
    /// keyring-only secret so a lost keyring entry can still be rebuilt
    /// once a password is re-entered. Absent for connections saved before
    /// this field existed, and never carries a password itself.
    #[serde(default)]
    pub sanitized_url: Option<String>,
}

/// Non-secret SSH tunnel settings for a [`StoredConnection`]. The SSH
/// password or key passphrase, if any, is never held here - see
/// [`StoredConnection::get_ssh_secret`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSsh {
    /// Whether the tunnel is used when connecting.
    pub enabled: bool,
    /// The SSH server's hostname or IP address.
    pub host: String,
    /// The SSH server's port.
    pub port: u16,
    /// The username to authenticate to the SSH server as.
    pub user: String,
    /// How to authenticate to the SSH server.
    pub auth_kind: SshAuthKind,
    /// The private key file, used when `auth_kind` is [`SshAuthKind::Key`].
    pub key_path: Option<PathBuf>,
    /// How the SSH server's host key is verified.
    pub host_key_policy: HostKeyPolicy,
}

/// How zsql authenticates to the SSH server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SshAuthKind {
    /// Delegate to a running `ssh-agent`.
    Agent,
    /// A password, kept in the OS keyring.
    Password,
    /// A private key file, optionally protected by a passphrase kept in the OS keyring.
    Key,
}

/// How an SSH server's host key is verified before a tunnel is used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostKeyPolicy {
    /// Verify against entries in a `known_hosts` file.
    KnownHosts(PathBuf),
    /// Accept and record any host key not already known (trust-on-first-use).
    AcceptNew,
    /// Reserved for an interactive accept/reject prompt.
    Prompt,
}

/// A connection that has not yet been persisted to disk
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConnectionArgs {
    /// User-given display name.
    pub name: String,
    /// The connection URL to be saved in the OS keyring
    pub url: String,
    /// Non-secret SSH tunnel settings, if a tunnel is configured.
    pub ssh: Option<StoredSsh>,
    /// The SSH password or key passphrase to be saved in the OS keyring.
    /// Absent when SSH is disabled, uses agent auth, or an unprotected key.
    pub ssh_secret: Option<String>,
}

impl ConnectionArgs {
    pub fn into_stored(self) -> Result<StoredConnection, ConnectionStoreError> {
        let stored = StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: self.name,
            display_kind: detect_driver_name(&self.url)
                .unwrap_or("Unknown")
                .to_owned(),
            display_host: host_label(&self.url),
            ssh: self.ssh,
            sanitized_url: sanitize_url(&self.url),
        };
        stored.set_url(&self.url)?;
        if let Some(secret) = &self.ssh_secret {
            stored.set_ssh_secret(secret)?;
        }
        Ok(stored)
    }
}

/// The password-cleared form of `url` to persist as
/// [`StoredConnection::sanitized_url`]. `None` when `url` fails to parse,
/// never a partially-scrubbed string.
pub(crate) fn sanitize_url(url: &str) -> Option<String> {
    let mut parsed = ConnectionUrl::parse(url).ok()?;
    parsed.set_password("");
    Some(parsed.to_url_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        ConnectionArgs, HostKeyPolicy, SshAuthKind, StoredConnection, StoredSsh, sanitize_url,
    };

    /// Non-secret SSH settings used by tests that don't care about specific
    /// field values, just that a tunnel is configured.
    fn sample_ssh() -> StoredSsh {
        StoredSsh {
            enabled: true,
            host: "bastion.example.com".to_owned(),
            port: 2222,
            user: "deploy".to_owned(),
            auth_kind: SshAuthKind::Password,
            key_path: None,
            host_key_policy: HostKeyPolicy::AcceptNew,
        }
    }

    #[test]
    fn stored_connection_round_trips_through_toml_with_no_data_loss() {
        let connection = StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: "test".to_owned(),
            display_kind: "postgres".to_owned(),
            display_host: "localhost".to_owned(),
            ssh: None,
            sanitized_url: Some("postgres://user@localhost/app".to_owned()),
        };
        let text = toml::to_string(&connection).expect("must serialize");
        let parsed: StoredConnection = toml::from_str(&text).expect("must parse back");
        assert_eq!(parsed, connection);
    }

    #[test]
    fn stored_connection_with_no_ssh_round_trips_through_toml() {
        let connection = StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: "no tunnel".to_owned(),
            display_kind: "sqlite".to_owned(),
            display_host: "local file".to_owned(),
            ssh: None,
            sanitized_url: None,
        };
        let text = toml::to_string(&connection).expect("must serialize");
        let parsed: StoredConnection = toml::from_str(&text).expect("must parse back");
        assert_eq!(parsed, connection);
    }

    #[test]
    fn stored_connection_with_ssh_round_trips_through_toml_for_every_auth_kind_and_host_key_policy()
    {
        let cases = [
            (SshAuthKind::Agent, HostKeyPolicy::AcceptNew, None),
            (
                SshAuthKind::Password,
                HostKeyPolicy::KnownHosts(PathBuf::from("/home/user/.ssh/known_hosts")),
                None,
            ),
            (
                SshAuthKind::Key,
                HostKeyPolicy::Prompt,
                Some(PathBuf::from("/home/user/.ssh/id_ed25519")),
            ),
        ];

        for (auth_kind, host_key_policy, key_path) in cases {
            let connection = StoredConnection {
                id: uuid::Uuid::new_v4(),
                name: "tunneled".to_owned(),
                display_kind: "postgres".to_owned(),
                display_host: "bastion.example.com".to_owned(),
                ssh: Some(StoredSsh {
                    enabled: true,
                    host: "bastion.example.com".to_owned(),
                    port: 22,
                    user: "deploy".to_owned(),
                    auth_kind,
                    key_path,
                    host_key_policy,
                }),
                sanitized_url: None,
            };
            let text = toml::to_string(&connection).expect("must serialize");
            let parsed: StoredConnection = toml::from_str(&text).expect("must parse back");
            assert_eq!(parsed, connection);
        }
    }

    #[test]
    fn stored_connection_never_serializes_an_ssh_password() {
        let secret = "hunter2-super-secret-password";
        let connection = StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: "tunneled".to_owned(),
            display_kind: "postgres".to_owned(),
            display_host: "bastion.example.com".to_owned(),
            ssh: Some(sample_ssh()),
            sanitized_url: None,
        };
        // The password never has anywhere to go on `StoredConnection` or
        // `StoredSsh` in the first place; this pins that down at the
        // serialized-text level too, so a future field addition can't
        // regress it silently.
        let text = toml::to_string(&connection).expect("must serialize");
        assert!(
            !text.contains(secret),
            "the SSH password must never appear in the serialized TOML, got:\n{text}"
        );
    }

    #[test]
    fn sanitize_url_clears_the_password_but_keeps_every_other_part() {
        let sanitized =
            sanitize_url("postgres://readonly:hunter2@db.internal:5432/analytics?sslmode=require")
                .expect("a well-formed URL must sanitize");
        assert!(
            !sanitized.contains("hunter2"),
            "the sanitized URL must not contain the password, got: {sanitized}"
        );
        assert!(sanitized.contains("readonly"));
        assert!(sanitized.contains("db.internal"));
        assert!(sanitized.contains("analytics"));
    }

    #[test]
    fn sanitize_url_is_none_for_an_unparseable_url() {
        assert_eq!(sanitize_url("not-a-url"), None);
    }

    #[test]
    fn into_stored_populates_sanitized_url_with_the_password_cleared() {
        let args = ConnectionArgs {
            name: "prod".to_owned(),
            url: "postgres://readonly:hunter2@db.internal:5432/analytics".to_owned(),
            ssh: None,
            ssh_secret: None,
        };
        let stored = args.into_stored().expect("into_stored must succeed");
        let sanitized = stored
            .sanitized_url
            .expect("sanitized_url must be populated");
        assert!(!sanitized.contains("hunter2"));
        assert!(sanitized.contains("db.internal"));
    }

    #[test]
    fn a_saved_connections_serialized_toml_never_contains_the_plaintext_password_anywhere() {
        let secret = "hunter2-super-secret-password";
        let args = ConnectionArgs {
            name: "prod".to_owned(),
            url: format!("postgres://readonly:{secret}@db.internal:5432/analytics"),
            ssh: None,
            ssh_secret: None,
        };
        let stored = args.into_stored().expect("into_stored must succeed");
        let text = toml::to_string(&stored).expect("must serialize");
        assert!(
            !text.contains(secret),
            "the password must never appear anywhere in the serialized TOML, \
             including in sanitized_url, got:\n{text}"
        );
    }
}
