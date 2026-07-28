//! OS keyring access for a connection's secrets: its database URL and, if
//! it tunnels over SSH, the tunnel's password or key passphrase. Both are
//! kept out of the TOML store file entirely.

use super::{ConnectionStoreError, StoredConnection};

/// Keyring account prefix for a connection's database URL, followed by the
/// connection id.
const CONNECTION_KEYRING_ACCOUNT_PREFIX: &str = "zsql-connection-";

/// Keyring account prefix for a connection's SSH tunnel secret (password or
/// key passphrase), followed by the connection id. Kept distinct from
/// [`CONNECTION_KEYRING_ACCOUNT_PREFIX`] so the database URL and the SSH
/// secret are independent keyring entries.
const SSH_KEYRING_ACCOUNT_PREFIX: &str = "zsql-ssh-";

impl StoredConnection {
    pub fn get_url(&self) -> Result<String, ConnectionStoreError> {
        let entry = keyring_entry(CONNECTION_KEYRING_ACCOUNT_PREFIX, self.id)?;
        Ok(entry.get_password()?)
    }

    pub(crate) fn set_url(&self, url: &str) -> Result<(), ConnectionStoreError> {
        let entry = keyring_entry(CONNECTION_KEYRING_ACCOUNT_PREFIX, self.id)?;
        entry.set_password(url)?;
        Ok(())
    }

    pub(crate) fn delete_url(&self) -> Result<(), ConnectionStoreError> {
        let entry = keyring_entry(CONNECTION_KEYRING_ACCOUNT_PREFIX, self.id)?;
        entry.delete()?;
        Ok(())
    }

    /// The SSH tunnel password or key passphrase, if one is stored for this
    /// connection.
    ///
    /// # Errors
    /// Returns [`ConnectionStoreError::Keyring`] if the OS keyring cannot
    /// be accessed, or if no secret is currently stored for this connection.
    pub fn get_ssh_secret(&self) -> Result<String, ConnectionStoreError> {
        let entry = keyring_entry(SSH_KEYRING_ACCOUNT_PREFIX, self.id)?;
        Ok(entry.get_password()?)
    }

    pub(crate) fn set_ssh_secret(&self, secret: &str) -> Result<(), ConnectionStoreError> {
        let entry = keyring_entry(SSH_KEYRING_ACCOUNT_PREFIX, self.id)?;
        entry.set_password(secret)?;
        Ok(())
    }

    pub(crate) fn delete_ssh_secret(&self) -> Result<(), ConnectionStoreError> {
        let entry = keyring_entry(SSH_KEYRING_ACCOUNT_PREFIX, self.id)?;
        entry.delete()?;
        Ok(())
    }
}

fn keyring_entry(
    account_prefix: &str,
    id: uuid::Uuid,
) -> Result<crate::keyring::Entry, ConnectionStoreError> {
    let account = format!("{account_prefix}{id}");
    Ok(crate::keyring::Entry::new(&account)?)
}

#[cfg(test)]
mod tests {
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
    fn the_url_and_ssh_secret_keyring_accounts_are_independent() {
        let connection = StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: "tunneled".to_owned(),
            display_kind: "postgres".to_owned(),
            display_host: "bastion.example.com".to_owned(),
            ssh: Some(sample_ssh()),
        };
        connection
            .set_url("postgres://host/db")
            .expect("set_url must succeed");
        connection
            .set_ssh_secret("ssh-secret-value")
            .expect("set_ssh_secret must succeed");

        assert_eq!(
            connection.get_url().expect("get_url must succeed"),
            "postgres://host/db"
        );
        assert_eq!(
            connection
                .get_ssh_secret()
                .expect("get_ssh_secret must succeed"),
            "ssh-secret-value"
        );
    }
}
