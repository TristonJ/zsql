//! TOML file persistence for the connection list: load/save, with owner-only
//! file permissions on write and rollback of the in-memory list (and the
//! keyring writes a mutation already made) when a write fails partway
//! through.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{ConnectionArgs, StoredConnection};

/// Owner-only file mode (`rw-------`) the connection store is written with.
/// The store's only protection is these filesystem permissions: no
/// encryption or keyring integration is applied to the file itself (the
/// connection URLs it references are what live in the OS keyring).
#[cfg(unix)]
const STORE_FILE_MODE: u32 = 0o600;

/// The on-disk shape of the connection store file: a single named array so
/// the file reads clearly as TOML (`[[connections]]` tables) rather than a
/// bare top-level array.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct ConnectionStoreFile {
    connections: Vec<StoredConnection>,
}

/// Errors loading or saving the connection store.
#[derive(Debug, thiserror::Error)]
pub enum ConnectionStoreError {
    /// The store file exists but could not be read.
    #[error("failed to read connection store: {0}")]
    Read(std::io::Error),
    /// The store file's contents are not valid TOML for this shape.
    #[error("failed to parse connection store: {0}")]
    Parse(#[from] toml::de::Error),
    /// The in-memory store could not be serialized back to TOML.
    #[error("failed to serialize connection store: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// The store file could not be written (or its permissions set).
    #[error("failed to write connection store: {0}")]
    Write(std::io::Error),
    /// There was an error accessing the OS keyring
    #[error("failed to access OS keyring: {0}")]
    Keyring(#[from] crate::keyring::Error),
}

impl ConnectionStoreError {
    /// Whether this error is a keyring failure meaning no credential is
    /// stored for the account accessed, as opposed to some other store or
    /// keyring-access failure. Programmatic classification: never decide
    /// this from the error's [`std::fmt::Display`] text.
    #[must_use]
    pub fn is_keyring_absent(&self) -> bool {
        matches!(self, Self::Keyring(err) if err.is_absent())
    }
}

/// The persisted list of [`StoredConnection`]s, backed by a single TOML
/// file. Every mutation saves immediately: there is no separate "dirty"
/// state to forget to flush.
#[derive(Debug)]
pub struct ConnectionStore {
    path: PathBuf,
    connections: Vec<StoredConnection>,
    pending_connections: Vec<ConnectionArgs>,
}

impl ConnectionStore {
    /// Build an empty store backed by no file on disk. Used when no config
    /// directory can be resolved at all (e.g. `dirs::config_dir()` returns
    /// `None`); `add` will fail to persist in that case, but the app can
    /// still run with an empty list rather than refusing to start.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            path: PathBuf::new(),
            connections: Vec::new(),
            pending_connections: Vec::new(),
        }
    }

    /// Load the store from `path`. A missing file is not an error: it means
    /// no connection has ever been saved yet, so this returns an empty
    /// store, mirroring [`crate::config::Config::load_or_default`]'s
    /// fallback-to-default behavior.
    ///
    /// # Errors
    /// Returns [`ConnectionStoreError::Read`] if the file exists but cannot
    /// be read, or [`ConnectionStoreError::Parse`] if its contents are not
    /// valid for this shape.
    #[tracing::instrument(name = "connection_store_load", skip_all)]
    pub fn load(path: &Path) -> Result<Self, ConnectionStoreError> {
        if !path.exists() {
            tracing::debug!("no connection store file yet; starting with an empty list");
            return Ok(Self {
                path: path.to_owned(),
                connections: Vec::new(),
                pending_connections: Vec::new(),
            });
        }

        let text = fs::read_to_string(path).map_err(ConnectionStoreError::Read)?;
        let file: ConnectionStoreFile = toml::from_str(&text)?;
        tracing::info!(count = file.connections.len(), "connection store loaded");
        Ok(Self {
            path: path.to_owned(),
            connections: file.connections,
            pending_connections: Vec::new(),
        })
    }

    /// Every persisted connection, in the order they were added.
    #[must_use]
    pub fn connections(&self) -> &[StoredConnection] {
        &self.connections
    }

    /// Append `connection` and persist the updated list immediately. On a
    /// write failure the connection is rolled back out of memory so a
    /// retried `add` cannot leave the on-disk store holding a duplicate.
    ///
    /// # Errors
    /// Returns [`ConnectionStoreError`] if the store cannot be written.
    #[tracing::instrument(name = "connection_store_add", skip_all, fields(name = %connection.name))]
    pub fn add(&mut self, connection: ConnectionArgs) -> Result<(), ConnectionStoreError> {
        self.pending_connections.push(connection);
        if let Err(err) = self.save() {
            self.connections.pop();
            return Err(err);
        }
        Ok(())
    }

    /// Replace the connection with `id` with `args` in place and persist the updated
    /// list immediately. A no-op if the `id` is not found.
    ///
    /// # Errors
    /// Returns [`ConnectionStoreError`] if the store cannot be written.
    #[tracing::instrument(name = "connection_store_update", skip_all, fields(index))]
    pub fn update(
        &mut self,
        id: uuid::Uuid,
        args: ConnectionArgs,
    ) -> Result<(), ConnectionStoreError> {
        let Some(index) = self.connections.iter().position(|conn| conn.id == id) else {
            tracing::warn!(id = %id, "update requested for a non-existent connection id");
            return Ok(());
        };
        let Some(conn) = self.connections.get_mut(index) else {
            tracing::warn!(index, "update requested for an out-of-range index");
            return Ok(());
        };
        let prior_args = ConnectionArgs {
            name: conn.name.clone(),
            url: conn.get_url()?,
            ssh: conn.ssh.clone(),
            ssh_secret: if conn.ssh.is_some() {
                conn.get_ssh_secret().ok()
            } else {
                None
            },
        };
        let prior_sanitized_url = conn.sanitized_url.clone();
        // Attempt to update the keyring first
        conn.set_url(&args.url)?;
        match &args.ssh_secret {
            Some(secret) => conn.set_ssh_secret(secret)?,
            None => conn.delete_ssh_secret()?,
        }
        conn.ssh = args.ssh;
        conn.name = args.name;
        conn.sanitized_url = super::sanitize_url(&args.url);
        // Now try to save - undoing the keyring change if it fails
        if let Err(err) = self.save() {
            let conn = self
                .connections
                .get_mut(index)
                .expect("index must still be valid");
            conn.set_url(&prior_args.url)?;
            match &prior_args.ssh_secret {
                Some(secret) => conn.set_ssh_secret(secret)?,
                None => conn.delete_ssh_secret()?,
            }
            conn.ssh = prior_args.ssh;
            conn.name = prior_args.name;
            conn.sanitized_url = prior_sanitized_url;
            return Err(err);
        }
        Ok(())
    }

    /// Remove the connection with `id` and persist the updated list
    /// immediately
    ///
    /// # Errors
    /// Returns [`ConnectionStoreError`] if the store cannot be written.
    #[tracing::instrument(name = "connection_store_remove", skip_all, fields(index))]
    pub fn remove(&mut self, id: uuid::Uuid) -> Result<(), ConnectionStoreError> {
        let Some(index) = self.connections.iter().position(|conn| conn.id == id) else {
            tracing::warn!(id = %id, "remove requested for a non-existent connection id");
            return Ok(());
        };
        // Do the reverse of save/update, first removing from the disk, then keyring
        let removed = self.connections.remove(index);
        if let Err(err) = self.save() {
            self.connections.insert(index, removed);
            return Err(err);
        }
        if let Err(err) = removed.delete_url() {
            tracing::error!(id = %removed.id, "failed to delete connection URL from keyring: {err}");
            return Err(err);
        }
        if let Err(err) = removed.delete_ssh_secret() {
            tracing::error!(id = %removed.id, "failed to delete SSH secret from keyring: {err}");
            return Err(err);
        }
        Ok(())
    }

    /// Write the current list to `path`, creating its parent directory if
    /// needed, then set owner-only permissions on the resulting file.
    #[tracing::instrument(name = "connection_store_save", skip_all)]
    fn save(&mut self) -> Result<(), ConnectionStoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(ConnectionStoreError::Write)?;
        }
        // Persist the pending connections to the OS keyring
        let added = self
            .pending_connections
            .drain(..)
            .map(ConnectionArgs::into_stored)
            .collect::<Result<Vec<_>, _>>()?;
        self.connections.extend(added);
        let file = ConnectionStoreFile {
            connections: self.connections.clone(),
        };
        let text = toml::to_string_pretty(&file)?;
        fs::write(&self.path, text).map_err(ConnectionStoreError::Write)?;
        set_owner_only_permissions(&self.path)?;
        tracing::info!(count = self.connections.len(), "connection store saved");
        Ok(())
    }
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path) -> Result<(), ConnectionStoreError> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(STORE_FILE_MODE))
        .map_err(ConnectionStoreError::Write)
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &Path) -> Result<(), ConnectionStoreError> {
    // No portable equivalent is applied here; see `STORE_FILE_MODE`'s doc
    // comment for what protection the store file actually has.
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::{
        ConnectionArgs, HostKeyPolicy, SshAuthKind, StoredConnection, StoredSsh, sanitize_url,
    };
    use super::ConnectionStore;

    /// A temp file path this test owns exclusively, removed on drop so
    /// tests never leak files into the real temp dir.
    struct TempStorePath(std::path::PathBuf);

    impl TempStorePath {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "zsql-connections-test-{label}-{}-{n}.toml",
                std::process::id()
            ));
            Self(path)
        }
    }

    impl Drop for TempStorePath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn sample() -> ConnectionArgs {
        ConnectionArgs {
            name: "local pg".to_owned(),
            url: "postgres://user:pass@localhost:5432/app".to_owned(),
            ssh: None,
            ssh_secret: None,
        }
    }

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

    fn assert_connection_eq(actual: &StoredConnection, expected: &ConnectionArgs) {
        assert_eq!(actual.name, expected.name);
        assert_eq!(
            actual.get_url().expect("must get URL"),
            expected.url,
            "the connection URL must match"
        );
    }

    #[test]
    fn loading_a_missing_file_returns_an_empty_list_not_an_error() {
        let temp = TempStorePath::new("missing");
        let store = ConnectionStore::load(&temp.0).expect("a missing file must not error");
        assert!(store.connections().is_empty());
    }

    #[test]
    fn loading_a_corrupt_file_returns_a_typed_error_not_a_panic() {
        let temp = TempStorePath::new("corrupt");
        std::fs::write(&temp.0, "this is not valid toml {{{").expect("setup write failed");

        let result = ConnectionStore::load(&temp.0);
        assert!(
            matches!(result, Err(super::ConnectionStoreError::Parse(_))),
            "expected a typed parse error, got {result:?}"
        );
    }

    #[test]
    fn loading_a_store_file_written_before_ssh_support_defaults_ssh_to_none() {
        let temp = TempStorePath::new("pre-ssh-store");
        let id = uuid::Uuid::new_v4();
        let pre_ssh_toml = format!(
            "[[connections]]\n\
             id = \"{id}\"\n\
             name = \"legacy\"\n\
             display_kind = \"postgres\"\n\
             display_host = \"localhost\"\n"
        );
        std::fs::write(&temp.0, pre_ssh_toml).expect("setup write failed");

        let store =
            ConnectionStore::load(&temp.0).expect("a store with no ssh key must still parse");
        assert_eq!(store.connections().len(), 1);
        assert_eq!(store.connections()[0].ssh, None);
    }

    #[test]
    fn loading_a_store_file_written_before_sanitized_url_existed_defaults_it_to_none() {
        let temp = TempStorePath::new("pre-sanitized-url-store");
        let id = uuid::Uuid::new_v4();
        let pre_field_toml = format!(
            "[[connections]]\n\
             id = \"{id}\"\n\
             name = \"legacy\"\n\
             display_kind = \"postgres\"\n\
             display_host = \"localhost\"\n"
        );
        std::fs::write(&temp.0, pre_field_toml).expect("setup write failed");

        let store = ConnectionStore::load(&temp.0)
            .expect("a store with no sanitized_url key must still parse");
        assert_eq!(store.connections().len(), 1);
        assert_eq!(store.connections()[0].sanitized_url, None);
    }

    #[test]
    fn adding_a_connection_then_reloading_from_disk_returns_it() {
        let temp = TempStorePath::new("round-trip");

        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");
        store.add(sample()).expect("add must succeed");

        // A fresh `load` call, simulating a new process reading the file
        // this one just wrote.
        let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
        assert_eq!(
            reloaded.connections().len(),
            1,
            "the reloaded store must have one connection"
        );
        assert_connection_eq(&reloaded.connections()[0], &sample());
    }

    #[test]
    fn adding_a_connection_with_ssh_then_reloading_from_disk_returns_it() {
        let temp = TempStorePath::new("ssh-round-trip");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");

        let args = ConnectionArgs {
            name: "tunneled".to_owned(),
            url: "postgres://host/db".to_owned(),
            ssh: Some(StoredSsh {
                enabled: true,
                host: "bastion.example.com".to_owned(),
                port: 2222,
                user: "deploy".to_owned(),
                auth_kind: SshAuthKind::Key,
                key_path: Some(PathBuf::from("/home/user/.ssh/id_ed25519")),
                host_key_policy: HostKeyPolicy::KnownHosts(PathBuf::from(
                    "/home/user/.ssh/known_hosts",
                )),
            }),
            ssh_secret: Some("key-passphrase".to_owned()),
        };
        store.add(args.clone()).expect("add must succeed");

        let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
        assert_eq!(
            reloaded.connections().len(),
            1,
            "the reloaded store must have one connection"
        );
        let stored = &reloaded.connections()[0];
        assert_eq!(stored.name, args.name);
        assert_eq!(stored.get_url().expect("get_url must succeed"), args.url);
        assert_eq!(stored.ssh, args.ssh, "every SSH field must round-trip");
        assert_eq!(
            stored
                .get_ssh_secret()
                .expect("get_ssh_secret must succeed"),
            "key-passphrase"
        );
    }

    #[test]
    fn adding_a_connection_with_ssh_password_keeps_it_out_of_the_store_file() {
        let temp = TempStorePath::new("ssh-password-not-on-disk");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");

        let secret = "correct horse battery staple";
        let args = ConnectionArgs {
            name: "tunneled".to_owned(),
            url: "postgres://host/db".to_owned(),
            ssh: Some(StoredSsh {
                enabled: true,
                host: "bastion.example.com".to_owned(),
                port: 2222,
                user: "deploy".to_owned(),
                auth_kind: SshAuthKind::Password,
                key_path: None,
                host_key_policy: HostKeyPolicy::AcceptNew,
            }),
            ssh_secret: Some(secret.to_owned()),
        };
        store.add(args).expect("add must succeed");

        let file_bytes = std::fs::read(&temp.0).expect("store file must exist");
        assert!(
            !file_bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "the SSH password must never be written to the connection store file"
        );

        // The non-secret SSH settings, by contrast, are expected in plain
        // text on disk.
        let text = String::from_utf8(file_bytes).expect("store file must be utf8");
        assert!(
            text.contains("bastion.example.com"),
            "the non-secret SSH host must be readable on disk"
        );
        assert!(
            text.contains("deploy"),
            "the non-secret SSH user must be readable on disk"
        );

        // The password is retrievable only via the keyring, never via the
        // in-memory `StoredSsh` fields.
        let stored = &store.connections()[0];
        assert_eq!(
            stored
                .get_ssh_secret()
                .expect("get_ssh_secret must succeed"),
            secret
        );
    }

    #[test]
    fn adding_a_connection_with_agent_auth_and_no_secret_creates_no_ssh_keyring_entry() {
        let temp = TempStorePath::new("ssh-agent-no-secret");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");

        store
            .add(ConnectionArgs {
                name: "agent auth".to_owned(),
                url: "postgres://host/db".to_owned(),
                ssh: Some(StoredSsh {
                    auth_kind: SshAuthKind::Agent,
                    ..sample_ssh()
                }),
                ssh_secret: None,
            })
            .expect("add must succeed");

        let result = store.connections()[0].get_ssh_secret();
        assert!(
            result.is_err(),
            "agent auth with no secret must not create a zsql-ssh-{{id}} keyring entry, got {result:?}"
        );
    }

    #[test]
    fn adding_two_connections_preserves_both_in_order() {
        let temp = TempStorePath::new("two");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");

        let first = ConnectionArgs {
            name: "first".to_owned(),
            url: "postgres://host/a".to_owned(),
            ssh: None,
            ssh_secret: None,
        };
        let second = ConnectionArgs {
            name: "second".to_owned(),
            url: "sqlite:///tmp/b.db".to_owned(),
            ssh: None,
            ssh_secret: None,
        };
        store.add(first.clone()).expect("add first");
        store.add(second.clone()).expect("add second");

        let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
        assert_connection_eq(&reloaded.connections()[0], &first);
        assert_connection_eq(&reloaded.connections()[1], &second);
    }

    #[test]
    fn an_in_memory_store_is_empty_and_cannot_persist() {
        let mut store = ConnectionStore::in_memory();
        assert!(
            store.connections().is_empty(),
            "a fresh in-memory store must start empty"
        );

        let result = store.add(sample());
        assert!(
            result.is_err(),
            "an in-memory store backed by no file must fail to persist an add"
        );
        assert!(
            store.connections().is_empty(),
            "a failed add must not leave the connection in memory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn saving_creates_the_file_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = TempStorePath::new("perms");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");
        store.add(sample()).expect("add must succeed");

        let mode = std::fs::metadata(&temp.0)
            .expect("saved file must exist")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "connection store file must be owner-read-write-only, got mode {mode:o}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn add_does_not_leave_the_new_connection_in_memory_when_save_fails() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = std::env::temp_dir().join(format!(
            "zsql-connections-test-unwritable-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("setup: create base dir");
        // Owner-read-execute only: the directory exists (so create_dir_all
        // on the store's own parent is a no-op) but writing a new file
        // inside it must fail with permission denied.
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o500))
            .expect("setup: restrict base dir permissions");

        let path = base.join("connections.toml");
        let mut store = ConnectionStore::load(&path).expect("initial load must succeed");

        let result = store.add(sample());
        assert!(
            result.is_err(),
            "add must fail when the store file cannot be written"
        );
        assert!(
            store.connections().is_empty(),
            "a failed save must not leave the connection in memory: {:?}",
            store.connections()
        );

        // Restore permissions so the temp dir can be cleaned up.
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700))
            .expect("teardown: restore base dir permissions");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn updating_a_connection_replaces_it_in_place_without_changing_the_list_length() {
        let temp = TempStorePath::new("update");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");

        let first = ConnectionArgs {
            name: "first".to_owned(),
            url: "postgres://host/a".to_owned(),
            ssh: None,
            ssh_secret: None,
        };
        let second = ConnectionArgs {
            name: "second".to_owned(),
            url: "sqlite:///tmp/b.db".to_owned(),
            ssh: None,
            ssh_secret: None,
        };
        store.add(first).expect("add first");
        store.add(second.clone()).expect("add second");

        let first_id = store.connections()[0].id;
        let updated_first = ConnectionArgs {
            name: "first renamed".to_owned(),
            url: "postgres://host/other".to_owned(),
            ssh: None,
            ssh_secret: None,
        };
        store
            .update(first_id, updated_first.clone())
            .expect("update must succeed");

        assert_connection_eq(&store.connections()[0], &updated_first);
        assert_connection_eq(&store.connections()[1], &second);

        let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
        assert_connection_eq(&reloaded.connections()[0], &updated_first);
        assert_connection_eq(&reloaded.connections()[1], &second);
    }

    #[test]
    fn updating_a_connection_refreshes_its_sanitized_url() {
        let temp = TempStorePath::new("update-sanitized-url");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");
        store
            .add(ConnectionArgs {
                name: "first".to_owned(),
                url: "postgres://user:hunter2@host/a".to_owned(),
                ssh: None,
                ssh_secret: None,
            })
            .expect("add must succeed");
        let id = store.connections()[0].id;

        store
            .update(
                id,
                ConnectionArgs {
                    name: "first".to_owned(),
                    url: "postgres://otheruser:hunter3@otherhost/b".to_owned(),
                    ssh: None,
                    ssh_secret: None,
                },
            )
            .expect("update must succeed");

        let sanitized = store.connections()[0]
            .sanitized_url
            .clone()
            .expect("sanitized_url must be populated after an update");
        assert!(sanitized.contains("otheruser"));
        assert!(sanitized.contains("otherhost"));
        assert!(!sanitized.contains("hunter3"));
    }

    #[test]
    fn updating_a_connection_changes_its_ssh_settings_and_secret() {
        let temp = TempStorePath::new("update-ssh");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");

        store
            .add(ConnectionArgs {
                name: "first".to_owned(),
                url: "postgres://host/a".to_owned(),
                ssh: None,
                ssh_secret: None,
            })
            .expect("add must succeed");

        let id = store.connections()[0].id;
        let updated = ConnectionArgs {
            name: "first with tunnel".to_owned(),
            url: "postgres://host/a".to_owned(),
            ssh: Some(sample_ssh()),
            ssh_secret: Some("new-secret".to_owned()),
        };
        store
            .update(id, updated.clone())
            .expect("update must succeed");

        assert_eq!(store.connections()[0].ssh, updated.ssh);
        assert_eq!(
            store.connections()[0]
                .get_ssh_secret()
                .expect("get_ssh_secret must succeed"),
            "new-secret"
        );

        let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
        assert_eq!(reloaded.connections()[0].ssh, updated.ssh);
        assert_eq!(
            reloaded.connections()[0]
                .get_ssh_secret()
                .expect("get_ssh_secret must succeed"),
            "new-secret"
        );
    }

    #[test]
    fn updating_a_connection_to_agent_auth_clears_the_stale_ssh_secret() {
        let temp = TempStorePath::new("update-ssh-to-agent");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");

        store
            .add(ConnectionArgs {
                name: "first".to_owned(),
                url: "postgres://host/a".to_owned(),
                ssh: Some(sample_ssh()),
                ssh_secret: Some("stale-password".to_owned()),
            })
            .expect("add must succeed");
        let id = store.connections()[0].id;

        store
            .update(
                id,
                ConnectionArgs {
                    name: "first with agent auth".to_owned(),
                    url: "postgres://host/a".to_owned(),
                    ssh: Some(StoredSsh {
                        auth_kind: SshAuthKind::Agent,
                        ..sample_ssh()
                    }),
                    ssh_secret: None,
                },
            )
            .expect("update must succeed");

        let result = store.connections()[0].get_ssh_secret();
        assert!(
            result.is_err(),
            "switching to agent auth must clear the stale SSH secret from the keyring, got {result:?}"
        );
    }

    #[test]
    fn updating_an_non_existing_id_is_a_no_op() {
        let temp = TempStorePath::new("update-out-of-range");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");
        store.add(sample()).expect("add must succeed");

        store
            .update(
                uuid::Uuid::new_v4(),
                ConnectionArgs {
                    name: "nope".to_owned(),
                    url: "postgres://host/db".to_owned(),
                    ssh: None,
                    ssh_secret: None,
                },
            )
            .expect("an out-of-range update must not error");

        assert_eq!(
            store.connections().len(),
            1,
            "an out-of-range update must not change the list"
        );
        assert_connection_eq(&store.connections()[0], &sample());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_save_on_update_restores_the_original_entry_in_memory() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = std::env::temp_dir().join(format!(
            "zsql-connections-test-update-unwritable-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("setup: create base dir");

        let path = base.join("connections.toml");
        let mut store = ConnectionStore::load(&path).expect("initial load must succeed");
        store
            .add(sample())
            .expect("add must succeed: dir is writable at this point");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
            .expect("setup: make store file read-only");

        let result = store.update(
            store.connections()[0].id,
            ConnectionArgs {
                name: "renamed".to_owned(),
                url: "postgres://host/renamed".to_owned(),
                ssh: None,
                ssh_secret: None,
            },
        );
        assert!(
            result.is_err(),
            "update must fail when the store file cannot be overwritten"
        );
        assert_connection_eq(&store.connections()[0], &sample());
        assert_eq!(
            store.connections()[0].sanitized_url,
            sanitize_url(&sample().url),
            "a rolled-back update must also restore the original sanitized_url"
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("teardown: restore file permissions");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_save_on_update_restores_the_original_ssh_secret() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = std::env::temp_dir().join(format!(
            "zsql-connections-test-update-ssh-unwritable-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("setup: create base dir");

        let path = base.join("connections.toml");
        let mut store = ConnectionStore::load(&path).expect("initial load must succeed");
        store
            .add(ConnectionArgs {
                name: "first".to_owned(),
                url: "postgres://host/a".to_owned(),
                ssh: Some(sample_ssh()),
                ssh_secret: Some("original-secret".to_owned()),
            })
            .expect("add must succeed: dir is writable at this point");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
            .expect("setup: make store file read-only");

        let result = store.update(
            store.connections()[0].id,
            ConnectionArgs {
                name: "renamed".to_owned(),
                url: "postgres://host/a".to_owned(),
                ssh: Some(sample_ssh()),
                ssh_secret: Some("attempted-new-secret".to_owned()),
            },
        );
        assert!(
            result.is_err(),
            "update must fail when the store file cannot be overwritten"
        );
        assert_eq!(
            store.connections()[0]
                .get_ssh_secret()
                .expect("get_ssh_secret must succeed"),
            "original-secret",
            "a failed save must roll the SSH secret back to its prior value"
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("teardown: restore file permissions");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_save_on_update_deletes_a_newly_set_ssh_secret_when_the_prior_connection_had_none() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = std::env::temp_dir().join(format!(
            "zsql-connections-test-update-ssh-add-unwritable-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("setup: create base dir");

        let path = base.join("connections.toml");
        let mut store = ConnectionStore::load(&path).expect("initial load must succeed");
        store
            .add(sample())
            .expect("add must succeed: dir is writable at this point");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
            .expect("setup: make store file read-only");

        let id = store.connections()[0].id;
        let result = store.update(
            id,
            ConnectionArgs {
                name: "with tunnel".to_owned(),
                url: sample().url,
                ssh: Some(sample_ssh()),
                ssh_secret: Some("just-set-secret".to_owned()),
            },
        );
        assert!(
            result.is_err(),
            "update must fail when the store file cannot be overwritten"
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("teardown: restore file permissions so the secret can be read back");

        let fresh = StoredConnection {
            id,
            name: String::new(),
            display_kind: String::new(),
            display_host: String::new(),
            ssh: None,
            sanitized_url: None,
        };
        let secret_result = fresh.get_ssh_secret();
        assert!(
            secret_result.is_err(),
            "a failed save must delete the just-set SSH secret since the prior connection had none, got {secret_result:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_save_on_update_restores_a_deleted_ssh_secret_when_switching_to_agent_auth() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = std::env::temp_dir().join(format!(
            "zsql-connections-test-update-ssh-drop-unwritable-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("setup: create base dir");

        let path = base.join("connections.toml");
        let mut store = ConnectionStore::load(&path).expect("initial load must succeed");
        store
            .add(ConnectionArgs {
                name: "first".to_owned(),
                url: "postgres://host/a".to_owned(),
                ssh: Some(sample_ssh()),
                ssh_secret: Some("original-secret".to_owned()),
            })
            .expect("add must succeed: dir is writable at this point");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
            .expect("setup: make store file read-only");

        // Switch the connection to agent auth, which carries no secret: the
        // forward path deletes the stored secret, then the save fails, so the
        // rollback must restore the just-deleted secret rather than leave it gone.
        let result = store.update(
            store.connections()[0].id,
            ConnectionArgs {
                name: "now on agent".to_owned(),
                url: "postgres://host/a".to_owned(),
                ssh: Some(StoredSsh {
                    auth_kind: SshAuthKind::Agent,
                    ..sample_ssh()
                }),
                ssh_secret: None,
            },
        );
        assert!(
            result.is_err(),
            "update must fail when the store file cannot be overwritten"
        );
        assert_eq!(
            store.connections()[0]
                .get_ssh_secret()
                .expect("get_ssh_secret must succeed"),
            "original-secret",
            "a failed save must restore the SSH secret the forward path deleted"
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("teardown: restore file permissions");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn removing_a_connection_then_reloading_from_disk_reflects_the_removal() {
        let temp = TempStorePath::new("remove");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");

        let first = ConnectionArgs {
            name: "first".to_owned(),
            url: "postgres://host/a".to_owned(),
            ssh: None,
            ssh_secret: None,
        };
        let second = ConnectionArgs {
            name: "second".to_owned(),
            url: "sqlite:///tmp/b.db".to_owned(),
            ssh: None,
            ssh_secret: None,
        };
        store.add(first.clone()).expect("add first");
        store.add(second.clone()).expect("add second");

        store
            .remove(store.connections()[0].id)
            .expect("remove must succeed");
        assert_connection_eq(&store.connections()[0], &second);

        let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
        assert_eq!(
            reloaded.connections().len(),
            1,
            "the reloaded store must have one connection"
        );
        assert_connection_eq(&reloaded.connections()[0], &second);
    }

    #[test]
    fn removing_a_connection_with_an_ssh_secret_deletes_it_from_the_keyring() {
        let temp = TempStorePath::new("remove-ssh-secret");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");

        store
            .add(ConnectionArgs {
                name: "tunneled".to_owned(),
                url: "postgres://host/db".to_owned(),
                ssh: Some(sample_ssh()),
                ssh_secret: Some("hunter2".to_owned()),
            })
            .expect("add must succeed");
        let id = store.connections()[0].id;

        store.remove(id).expect("remove must succeed");

        // A fresh `StoredConnection` for the same id, representing a new
        // `Entry` construction rather than reusing any cached handle.
        let fresh = StoredConnection {
            id,
            name: String::new(),
            display_kind: String::new(),
            display_host: String::new(),
            ssh: None,
            sanitized_url: None,
        };
        let result = fresh.get_ssh_secret();
        assert!(
            result.is_err(),
            "the SSH secret must be gone from the keyring after removal, got {result:?}"
        );
    }

    #[test]
    fn removing_a_non_existing_id_is_a_no_op() {
        let temp = TempStorePath::new("remove-out-of-range");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");
        store.add(sample()).expect("add must succeed");

        store
            .remove(uuid::Uuid::new_v4())
            .expect("an out-of-range remove must not error");

        assert_connection_eq(&store.connections()[0], &sample());
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_save_on_remove_restores_the_entry_in_memory() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = std::env::temp_dir().join(format!(
            "zsql-connections-test-remove-unwritable-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("setup: create base dir");

        let path = base.join("connections.toml");
        let mut store = ConnectionStore::load(&path).expect("initial load must succeed");
        store
            .add(sample())
            .expect("add must succeed: dir is writable at this point");

        // Now make the saved file itself read-only, so the directory stays
        // writable (creation would still succeed) but overwriting the
        // existing file's contents on the remove's save fails.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))
            .expect("setup: make store file read-only");

        let result = store.remove(store.connections()[0].id);
        assert!(
            result.is_err(),
            "remove must fail when the store file cannot be overwritten"
        );
        assert_connection_eq(&store.connections()[0], &sample());

        // Restore permissions so the temp dir can be cleaned up.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("teardown: restore file permissions");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn saving_creates_missing_parent_directories() {
        let base = std::env::temp_dir().join(format!(
            "zsql-connections-test-nested-parent-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let path = base.join("nested").join("connections.toml");

        let mut store = ConnectionStore::load(&path).expect("initial load must succeed");
        store
            .add(sample())
            .expect("add must create parent dirs and save");

        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
