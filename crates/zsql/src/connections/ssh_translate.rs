//! Translation from a [`StoredConnection`]'s persisted, non-secret SSH
//! settings (plus a separately-resolved secret) into the runtime
//! [`zsql_ssh::SshConfig`] a tunnel is actually opened with.

use super::{ConnectionStoreError, HostKeyPolicy, SshAuthKind, StoredConnection, StoredSsh};

impl StoredConnection {
    /// Build the [`zsql_ssh::SshConfig`] this connection's tunnel should
    /// open with, reading its secret (password or key passphrase, if any)
    /// from the keyring. `None` when no tunnel is configured, or one is
    /// configured but not enabled.
    ///
    /// # Errors
    /// Returns [`ConnectionStoreError::Keyring`] if a password- or
    /// key-passphrase-authenticated tunnel's secret cannot be read from the
    /// keyring.
    pub fn ssh_config(&self) -> Result<Option<zsql_ssh::SshConfig>, ConnectionStoreError> {
        let Some(ssh) = self.ssh.as_ref().filter(|ssh| ssh.enabled) else {
            return Ok(None);
        };
        let secret = match ssh.auth_kind {
            SshAuthKind::Agent => None,
            SshAuthKind::Password => Some(self.get_ssh_secret()?),
            // A key with no passphrase has no keyring entry at all
            // (`ConnectionArgs::into_stored` only writes one when
            // `ssh_secret` is set), so a missing entry here means
            // "unprotected key", not an error.
            SshAuthKind::Key => self.get_ssh_secret().ok(),
        };
        Ok(Some(ssh_config_from_stored(ssh, secret)))
    }
}

/// Build the [`zsql_ssh::SshConfig`] `ssh` describes, given its secret
/// (password or key passphrase) already resolved. `secret` is ignored for
/// [`SshAuthKind::Agent`], and treated as "no passphrase" for
/// [`SshAuthKind::Key`] when absent.
///
/// Shared between [`StoredConnection::ssh_config`] (which resolves `secret`
/// from the keyring) and the connection form (which reads it straight from
/// its own unsaved SSH fields).
#[must_use]
pub fn ssh_config_from_stored(ssh: &StoredSsh, secret: Option<String>) -> zsql_ssh::SshConfig {
    let auth = match ssh.auth_kind {
        SshAuthKind::Agent => zsql_ssh::SshAuth::Agent,
        SshAuthKind::Password => zsql_ssh::SshAuth::Password(secret.unwrap_or_default()),
        SshAuthKind::Key => zsql_ssh::SshAuth::Key {
            path: ssh.key_path.clone().unwrap_or_default(),
            passphrase: secret,
        },
    };
    let mut cfg = zsql_ssh::SshConfig::new(ssh.host.clone(), ssh.user.clone(), auth);
    cfg.port = ssh.port;
    cfg.host_key = match &ssh.host_key_policy {
        HostKeyPolicy::KnownHosts(path) => zsql_ssh::HostKeyPolicy::KnownHosts(path.clone()),
        HostKeyPolicy::AcceptNew => zsql_ssh::HostKeyPolicy::AcceptNew,
        HostKeyPolicy::Prompt => zsql_ssh::HostKeyPolicy::Prompt,
    };
    cfg
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::{HostKeyPolicy, SshAuthKind, StoredConnection, StoredSsh};

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
    fn ssh_config_is_none_when_no_ssh_is_configured() {
        let connection = StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: "plain".to_owned(),
            display_kind: "postgres".to_owned(),
            display_host: "localhost".to_owned(),
            ssh: None,
            sanitized_url: None,
        };
        assert!(connection.ssh_config().unwrap().is_none());
    }

    #[test]
    fn ssh_config_is_none_when_ssh_is_configured_but_disabled() {
        let connection = StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: "disabled tunnel".to_owned(),
            display_kind: "postgres".to_owned(),
            display_host: "localhost".to_owned(),
            ssh: Some(StoredSsh {
                enabled: false,
                ..sample_ssh()
            }),
            sanitized_url: None,
        };
        assert!(connection.ssh_config().unwrap().is_none());
    }

    #[test]
    fn ssh_config_builds_agent_auth_with_no_keyring_access() {
        let connection = StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: "agent".to_owned(),
            display_kind: "postgres".to_owned(),
            display_host: "localhost".to_owned(),
            ssh: Some(StoredSsh {
                auth_kind: SshAuthKind::Agent,
                ..sample_ssh()
            }),
            sanitized_url: None,
        };
        let cfg = connection
            .ssh_config()
            .expect("agent auth needs no keyring access")
            .expect("ssh is enabled");
        assert!(matches!(cfg.auth, zsql_ssh::SshAuth::Agent));
        assert_eq!(cfg.host, "bastion.example.com");
        assert_eq!(cfg.port, 2222);
        assert_eq!(cfg.user, "deploy");
    }

    #[test]
    fn ssh_config_builds_password_auth_from_the_keyring_secret() {
        let connection = StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: "password".to_owned(),
            display_kind: "postgres".to_owned(),
            display_host: "localhost".to_owned(),
            ssh: Some(StoredSsh {
                auth_kind: SshAuthKind::Password,
                ..sample_ssh()
            }),
            sanitized_url: None,
        };
        connection
            .set_ssh_secret("tunnel-password")
            .expect("set_ssh_secret must succeed");

        let cfg = connection
            .ssh_config()
            .expect("password auth must succeed")
            .expect("ssh is enabled");
        assert!(matches!(
            cfg.auth,
            zsql_ssh::SshAuth::Password(ref pw) if pw == "tunnel-password"
        ));
    }

    #[test]
    fn ssh_config_reports_an_error_when_password_auth_has_no_keyring_secret() {
        let connection = StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: "password, missing secret".to_owned(),
            display_kind: "postgres".to_owned(),
            display_host: "localhost".to_owned(),
            ssh: Some(StoredSsh {
                auth_kind: SshAuthKind::Password,
                ..sample_ssh()
            }),
            sanitized_url: None,
        };
        assert!(connection.ssh_config().is_err());
    }

    #[test]
    fn ssh_config_builds_key_auth_with_a_keyring_passphrase() {
        let connection = StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: "key with passphrase".to_owned(),
            display_kind: "postgres".to_owned(),
            display_host: "localhost".to_owned(),
            ssh: Some(StoredSsh {
                auth_kind: SshAuthKind::Key,
                key_path: Some(PathBuf::from("/home/user/.ssh/id_ed25519")),
                ..sample_ssh()
            }),
            sanitized_url: None,
        };
        connection
            .set_ssh_secret("key-passphrase")
            .expect("set_ssh_secret must succeed");

        let cfg = connection
            .ssh_config()
            .expect("key auth must succeed")
            .expect("ssh is enabled");
        match cfg.auth {
            zsql_ssh::SshAuth::Key { path, passphrase } => {
                assert_eq!(path, PathBuf::from("/home/user/.ssh/id_ed25519"));
                assert_eq!(passphrase.as_deref(), Some("key-passphrase"));
            }
            other => panic!("expected SshAuth::Key, got {other:?}"),
        }
    }

    #[test]
    fn ssh_config_builds_key_auth_with_no_passphrase_when_the_keyring_has_none() {
        let connection = StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: "unprotected key".to_owned(),
            display_kind: "postgres".to_owned(),
            display_host: "localhost".to_owned(),
            ssh: Some(StoredSsh {
                auth_kind: SshAuthKind::Key,
                key_path: Some(PathBuf::from("/home/user/.ssh/id_ed25519")),
                ..sample_ssh()
            }),
            sanitized_url: None,
        };

        let cfg = connection
            .ssh_config()
            .expect("an unprotected key must not require a keyring secret")
            .expect("ssh is enabled");
        match cfg.auth {
            zsql_ssh::SshAuth::Key { passphrase, .. } => {
                assert_eq!(passphrase, None);
            }
            other => panic!("expected SshAuth::Key, got {other:?}"),
        }
    }

    #[test]
    fn ssh_config_translates_the_host_key_policy() {
        let connection = StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: "known hosts".to_owned(),
            display_kind: "postgres".to_owned(),
            display_host: "localhost".to_owned(),
            ssh: Some(StoredSsh {
                auth_kind: SshAuthKind::Agent,
                host_key_policy: HostKeyPolicy::KnownHosts(PathBuf::from(
                    "/home/user/.ssh/known_hosts",
                )),
                ..sample_ssh()
            }),
            sanitized_url: None,
        };
        let cfg = connection
            .ssh_config()
            .expect("agent auth must succeed")
            .expect("ssh is enabled");
        assert_eq!(
            cfg.host_key,
            zsql_ssh::HostKeyPolicy::KnownHosts(PathBuf::from("/home/user/.ssh/known_hosts"))
        );
    }

    // -- ssh_config_from_stored (no keyring involved) ------------------------

    #[test]
    fn ssh_config_from_stored_builds_agent_auth_with_no_secret() {
        let ssh = StoredSsh {
            auth_kind: SshAuthKind::Agent,
            ..sample_ssh()
        };
        let cfg = super::ssh_config_from_stored(&ssh, None);
        assert!(matches!(cfg.auth, zsql_ssh::SshAuth::Agent));
        assert_eq!(cfg.host, ssh.host);
        assert_eq!(cfg.port, ssh.port);
        assert_eq!(cfg.user, ssh.user);
    }

    #[test]
    fn ssh_config_from_stored_builds_password_auth_from_the_given_secret() {
        let ssh = StoredSsh {
            auth_kind: SshAuthKind::Password,
            ..sample_ssh()
        };
        let cfg = super::ssh_config_from_stored(&ssh, Some("form-password".to_owned()));
        assert!(matches!(
            cfg.auth,
            zsql_ssh::SshAuth::Password(ref pw) if pw == "form-password"
        ));
    }

    #[test]
    fn ssh_config_from_stored_builds_key_auth_with_the_given_passphrase() {
        let ssh = StoredSsh {
            auth_kind: SshAuthKind::Key,
            key_path: Some(PathBuf::from("/home/user/.ssh/id_ed25519")),
            ..sample_ssh()
        };
        let cfg = super::ssh_config_from_stored(&ssh, Some("form-passphrase".to_owned()));
        match cfg.auth {
            zsql_ssh::SshAuth::Key { path, passphrase } => {
                assert_eq!(path, PathBuf::from("/home/user/.ssh/id_ed25519"));
                assert_eq!(passphrase.as_deref(), Some("form-passphrase"));
            }
            other => panic!("expected SshAuth::Key, got {other:?}"),
        }
    }
}
