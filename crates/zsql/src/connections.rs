//! Persisted connection store: user-named database connection URLs saved to
//! disk under [`crate::config::Config::connections_path`].
//!
//! V0 "secure" storage means the store file is written with owner-only
//! filesystem permissions (`0600` on unix) -- see [`STORE_FILE_MODE`]. It
//! does not integrate with any OS keyring or external secret manager; that
//! integration is deferred. Treat this file as sensitive: a connection URL
//! may embed a plaintext password, exactly as the `DATABASE_URL` env var
//! does today.

use std::fs;
#[cfg(test)]
use std::fs::File;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{drivers::detect_driver_name, ui::format::host_label};

/// Owner-only file mode (`rw-------`) the connection store is written with.
/// The entirety of V0's "secure": filesystem permissions only, no
/// encryption or keyring integration.
#[cfg(unix)]
const STORE_FILE_MODE: u32 = 0o600;

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
}

/// A connection that has not yet been persisted to disk
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionArgs {
    /// User-given display name.
    pub name: String,
    /// The connection URL to be saved in the OS keyring
    pub url: String,
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
        };
        stored.set_url(&self.url)?;
        Ok(stored)
    }
}

impl StoredConnection {
    #[cfg(not(test))]
    pub fn get_url(&self) -> Result<String, ConnectionStoreError> {
        let entry = self.get_keyring_entry()?;
        Ok(entry.get_password()?)
    }

    #[cfg(test)]
    #[allow(clippy::unnecessary_wraps)]
    pub fn get_url(&self) -> Result<String, ConnectionStoreError> {
        use std::io::Read;
        let path = self.get_mock_file_path();
        let mut url = String::new();
        let mut file = File::open(&path).expect("failed to open mock keyring file");
        file.read_to_string(&mut url)
            .expect("failed to read mock keyring file");
        Ok(url)
    }

    #[cfg(test)]
    #[allow(clippy::unnecessary_wraps)]
    pub fn set_url(&self, url: &str) -> Result<(), ConnectionStoreError> {
        use std::io::Write;
        let path = self.get_mock_file_path();
        let mut file = File::create(path).expect("failed to create mock keyring file");
        file.write_all(url.as_bytes())
            .expect("failed to write mock keyring file");
        Ok(())
    }

    #[cfg(test)]
    #[allow(clippy::unnecessary_wraps)]
    pub fn delete_url(&self) -> Result<(), ConnectionStoreError> {
        let file_path = self.get_mock_file_path();
        std::fs::remove_file(file_path).expect("failed to delete mock keyring file");
        Ok(())
    }

    #[cfg(not(test))]
    pub(crate) fn set_url(&self, url: &str) -> Result<(), ConnectionStoreError> {
        let entry = self.get_keyring_entry()?;
        entry.set_password(url)?;
        Ok(())
    }

    #[cfg(not(test))]
    pub(crate) fn delete_url(&self) -> Result<(), ConnectionStoreError> {
        let entry = self.get_keyring_entry()?;
        entry.delete_credential()?;
        Ok(())
    }

    #[cfg(not(test))]
    fn get_keyring_entry(&self) -> Result<keyring::Entry, ConnectionStoreError> {
        let username = format!("zsql-connection-{}", self.id);
        let entry = keyring::Entry::new("zsql", &username)?;
        Ok(entry)
    }

    #[cfg(test)]
    fn get_mock_file_path(&self) -> PathBuf {
        let dir = std::env::temp_dir();
        dir.join(format!("zsql-test-connection-{}.txt", self.id))
    }
}

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
    Keyring(#[from] keyring::Error),
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
        };
        // Attempt to update the keyring first
        conn.set_url(&args.url)?;
        conn.name = args.name;
        // Now try to save - undoing the keyring change if it fails
        if let Err(err) = self.save() {
            let conn = self
                .connections
                .get_mut(index)
                .expect("index must still be valid");
            conn.set_url(&prior_args.url)?;
            conn.name = prior_args.name;
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
    // No portable equivalent is applied here; see this module's doc comment
    // for what "secure" means in V0.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ConnectionArgs, ConnectionStore, StoredConnection};

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
    fn stored_connection_round_trips_through_toml_with_no_data_loss() {
        let connection = StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: "test".to_owned(),
            display_kind: "postgres".to_owned(),
            display_host: "localhost".to_owned(),
        };
        let text = toml::to_string(&connection).expect("must serialize");
        let parsed: StoredConnection = toml::from_str(&text).expect("must parse back");
        assert_eq!(parsed, connection);
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
    fn adding_two_connections_preserves_both_in_order() {
        let temp = TempStorePath::new("two");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");

        let first = ConnectionArgs {
            name: "first".to_owned(),
            url: "postgres://host/a".to_owned(),
        };
        let second = ConnectionArgs {
            name: "second".to_owned(),
            url: "sqlite:///tmp/b.db".to_owned(),
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
        };
        let second = ConnectionArgs {
            name: "second".to_owned(),
            url: "sqlite:///tmp/b.db".to_owned(),
        };
        store.add(first).expect("add first");
        store.add(second.clone()).expect("add second");

        let first_id = store.connections()[0].id;
        let updated_first = ConnectionArgs {
            name: "first renamed".to_owned(),
            url: "postgres://host/other".to_owned(),
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
            },
        );
        assert!(
            result.is_err(),
            "update must fail when the store file cannot be overwritten"
        );
        assert_connection_eq(&store.connections()[0], &sample());

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
        };
        let second = ConnectionArgs {
            name: "second".to_owned(),
            url: "sqlite:///tmp/b.db".to_owned(),
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
