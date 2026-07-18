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
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Owner-only file mode (`rw-------`) the connection store is written with.
/// The entirety of V0's "secure": filesystem permissions only, no
/// encryption or keyring integration.
#[cfg(unix)]
const STORE_FILE_MODE: u32 = 0o600;

/// A user-named, persisted connection: a display name paired with its
/// connection URL. The driver that will handle it is never stored here --
/// it is derived from the URL's scheme on demand via
/// [`zsql_core::select_driver`], so it can never go stale relative to the
/// registered drivers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredConnection {
    /// User-given display name.
    pub name: String,
    /// The connection URL, e.g. `postgres://user@host/db` or
    /// `sqlite:///path/to.db`.
    pub url: String,
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
}

/// The persisted list of [`StoredConnection`]s, backed by a single TOML
/// file. Every mutation saves immediately: there is no separate "dirty"
/// state to forget to flush.
#[derive(Debug)]
pub struct ConnectionStore {
    path: PathBuf,
    connections: Vec<StoredConnection>,
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
            });
        }

        let text = fs::read_to_string(path).map_err(ConnectionStoreError::Read)?;
        let file: ConnectionStoreFile = toml::from_str(&text)?;
        tracing::info!(count = file.connections.len(), "connection store loaded");
        Ok(Self {
            path: path.to_owned(),
            connections: file.connections,
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
    pub fn add(&mut self, connection: StoredConnection) -> Result<(), ConnectionStoreError> {
        self.connections.push(connection);
        if let Err(err) = self.save() {
            self.connections.pop();
            return Err(err);
        }
        Ok(())
    }

    /// Remove the connection at `index` and persist the updated list
    /// immediately. Mirrors [`Self::add`]'s rollback-on-save-failure
    /// discipline: on a write failure the removed entry is reinserted at its
    /// original position and an `Err` is returned, so a failed remove can
    /// never leave the on-disk store out of sync with what's in memory. An
    /// out-of-range `index` is a no-op that returns `Ok(())`, mirroring how
    /// the connection manager already treats an out-of-range connect index.
    ///
    /// # Errors
    /// Returns [`ConnectionStoreError`] if the store cannot be written.
    #[tracing::instrument(name = "connection_store_remove", skip_all, fields(index))]
    pub fn remove(&mut self, index: usize) -> Result<(), ConnectionStoreError> {
        if index >= self.connections.len() {
            tracing::warn!(index, "remove requested for an out-of-range index");
            return Ok(());
        }
        let removed = self.connections.remove(index);
        if let Err(err) = self.save() {
            self.connections.insert(index, removed);
            return Err(err);
        }
        Ok(())
    }

    /// Write the current list to `path`, creating its parent directory if
    /// needed, then set owner-only permissions on the resulting file.
    #[tracing::instrument(name = "connection_store_save", skip_all)]
    fn save(&self) -> Result<(), ConnectionStoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(ConnectionStoreError::Write)?;
        }
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
    use super::{ConnectionStore, StoredConnection};

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

    fn sample() -> StoredConnection {
        StoredConnection {
            name: "local pg".to_owned(),
            url: "postgres://user:pass@localhost:5432/app".to_owned(),
        }
    }

    #[test]
    fn stored_connection_round_trips_through_toml_with_no_data_loss() {
        let connection = sample();
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
        assert_eq!(reloaded.connections(), &[sample()]);
    }

    #[test]
    fn adding_two_connections_preserves_both_in_order() {
        let temp = TempStorePath::new("two");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");

        let first = StoredConnection {
            name: "first".to_owned(),
            url: "postgres://host/a".to_owned(),
        };
        let second = StoredConnection {
            name: "second".to_owned(),
            url: "sqlite:///tmp/b.db".to_owned(),
        };
        store.add(first.clone()).expect("add first");
        store.add(second.clone()).expect("add second");

        let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
        assert_eq!(reloaded.connections(), &[first, second]);
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
    fn removing_a_connection_then_reloading_from_disk_reflects_the_removal() {
        let temp = TempStorePath::new("remove");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");

        let first = StoredConnection {
            name: "first".to_owned(),
            url: "postgres://host/a".to_owned(),
        };
        let second = StoredConnection {
            name: "second".to_owned(),
            url: "sqlite:///tmp/b.db".to_owned(),
        };
        store.add(first.clone()).expect("add first");
        store.add(second.clone()).expect("add second");

        store.remove(0).expect("remove must succeed");
        assert_eq!(
            store.connections(),
            std::slice::from_ref(&second),
            "removing index 0 must leave only the second connection in memory"
        );

        let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
        assert_eq!(
            reloaded.connections(),
            &[second],
            "the removal must be persisted to disk"
        );
    }

    #[test]
    fn removing_an_out_of_range_index_is_a_no_op() {
        let temp = TempStorePath::new("remove-out-of-range");
        let mut store = ConnectionStore::load(&temp.0).expect("initial load must succeed");
        store.add(sample()).expect("add must succeed");

        store
            .remove(5)
            .expect("an out-of-range remove must not error");

        assert_eq!(
            store.connections(),
            &[sample()],
            "an out-of-range remove must not change the list"
        );
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

        let result = store.remove(0);
        assert!(
            result.is_err(),
            "remove must fail when the store file cannot be overwritten"
        );
        assert_eq!(
            store.connections(),
            &[sample()],
            "a failed save must restore the removed entry in memory: {:?}",
            store.connections()
        );

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
