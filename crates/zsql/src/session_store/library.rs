//! The shared library: a flat pool of `.sql` files under
//! [`crate::config::Config::library_dir`], visible from every connection.
//! Unlike a session directory, the library holds no `tabs.toml` or other
//! metadata, just one file per script.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::SCRIPT_FILE_EXTENSION;
use super::SessionStoreError;
use super::backing::LibraryName;
use super::session_dir::IoGuard;
use super::session_dir::atomic_write;

/// Owns the shared library root's I/O
pub struct LibraryDir {
    root: PathBuf,
}

/// One library script as returned by [`LibraryDir::list`]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryEntry {
    pub name: String,
    pub modified: SystemTime,
}

impl LibraryDir {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    #[must_use]
    pub fn at(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// Every `.sql` file directly under the root, in no particular order.
    /// A missing (never-yet-created) root returns an empty list. A file
    /// whose stem is not a valid [`LibraryName`] is skipped.
    ///
    /// # Errors
    /// Returns [`SessionStoreError::Read`] if the root exists but cannot be
    /// read, or if an entry's metadata cannot be read.
    #[tracing::instrument(name = "library_list", skip(self))]
    pub fn list(&self) -> Result<Vec<LibraryEntry>, SessionStoreError> {
        let _guard = IoGuard::acquire();
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(SessionStoreError::Read(err)),
        };

        let mut scripts = Vec::new();
        for entry in entries {
            let entry = entry.map_err(SessionStoreError::Read)?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("sql") {
                continue;
            }
            let metadata = entry.metadata().map_err(SessionStoreError::Read)?;
            if !metadata.is_file() {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if LibraryName::new(name).is_err() {
                tracing::debug!(
                    name,
                    "skipping a library file whose stem is not a valid name"
                );
                continue;
            }
            let modified = metadata.modified().map_err(SessionStoreError::Read)?;
            scripts.push(LibraryEntry {
                name: name.to_owned(),
                modified,
            });
        }
        tracing::debug!(count = scripts.len(), "listed library scripts");
        Ok(scripts)
    }

    /// The on-disk path the library script named `name` lives at.
    #[must_use]
    pub fn script_path(&self, name: &LibraryName) -> PathBuf {
        self.root.join(format!("{name}{SCRIPT_FILE_EXTENSION}"))
    }

    /// Write `text` as the library script named `name`, creating the root
    /// directory if needed.
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if the root cannot be created or the
    /// file cannot be written.
    #[tracing::instrument(name = "library_save", skip(self, text), fields(name = %name))]
    pub fn save(&self, name: &LibraryName, text: &str) -> Result<(), SessionStoreError> {
        let _guard = IoGuard::acquire();
        fs::create_dir_all(&self.root).map_err(SessionStoreError::Write)?;
        atomic_write(&self.script_path(name), text)?;
        tracing::info!(%name, "library script saved");
        Ok(())
    }

    /// Read the library script named `name`'s content, or `None` if it
    /// does not exist.
    ///
    /// # Errors
    /// Returns [`SessionStoreError::Read`] if the file exists but cannot be
    /// read.
    #[tracing::instrument(name = "library_load", skip(self), fields(name = %name))]
    pub fn load(&self, name: &LibraryName) -> Result<Option<String>, SessionStoreError> {
        let _guard = IoGuard::acquire();
        let path = self.script_path(name);
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(
            fs::read_to_string(&path).map_err(SessionStoreError::Read)?,
        ))
    }

    /// A library name derived from `title`, counter-suffixed (`name-2`,
    /// `name-3`, ...) until it collides with nothing already in the library.
    ///
    /// # Errors
    /// Returns [`SessionStoreError::Read`] if the library cannot be listed,
    /// or [`SessionStoreError::UnsafeRef`] if `title` cannot be made into a
    /// valid [`LibraryName`].
    pub fn unique_name(&self, title: &str) -> Result<LibraryName, SessionStoreError> {
        let used: std::collections::HashSet<String> = self
            .list()?
            .into_iter()
            .map(|entry| format!("{}{SCRIPT_FILE_EXTENSION}", entry.name))
            .collect();
        let file_name = crate::session_store::unique_script_file_name(title, &used);
        let bare = file_name
            .strip_suffix(SCRIPT_FILE_EXTENSION)
            .unwrap_or(&file_name);
        LibraryName::new(bare).map_err(|_| SessionStoreError::UnsafeRef(bare.to_owned()))
    }

    /// Atomically rename the library script `old_name` to `new_name`.
    ///
    /// # Errors
    /// Returns [`SessionStoreError::Duplicate`] if `new_name` already
    /// exists at the time the rename is about to run, or
    /// [`SessionStoreError::Write`] if the rename itself fails, e.g.
    /// `old_name` does not exist.
    #[tracing::instrument(name = "library_rename", skip(self), fields(old = %old_name, new = %new_name))]
    pub fn rename(
        &self,
        old_name: &LibraryName,
        new_name: &LibraryName,
    ) -> Result<(), SessionStoreError> {
        let _guard = IoGuard::acquire();
        let new_path = self.script_path(new_name);
        if old_name != new_name && new_path.is_file() {
            return Err(SessionStoreError::Duplicate(new_name.as_str().to_owned()));
        }
        fs::rename(self.script_path(old_name), &new_path).map_err(SessionStoreError::Write)?;
        tracing::info!(%old_name, %new_name, "library script renamed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::SessionStoreError;
    use super::LibraryDir;
    use crate::session_store::LibraryName;

    fn lib(name: &str) -> LibraryName {
        LibraryName::new(name).expect("must be a valid library name")
    }

    /// A temp directory this test owns exclusively, removed on drop so
    /// tests never leak directories into the real temp dir.
    struct TempLibraryDir(std::path::PathBuf);

    impl TempLibraryDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "zsql-library-test-{label}-{}-{n}",
                std::process::id()
            ));
            Self(path)
        }

        fn dir(&self) -> LibraryDir {
            LibraryDir::at(&self.0)
        }
    }

    impl Drop for TempLibraryDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn saving_then_loading_reproduces_the_scripts_text_exactly() {
        let temp = TempLibraryDir::new("round-trip");
        let dir = temp.dir();
        dir.save(&lib("revenue-report"), "select * from revenue;")
            .expect("save must succeed");

        let loaded = dir.load(&lib("revenue-report")).expect("load must succeed");

        assert_eq!(loaded, Some("select * from revenue;".to_owned()));
    }

    #[test]
    fn loading_a_script_that_was_never_saved_returns_none_not_an_error() {
        let temp = TempLibraryDir::new("missing");
        let loaded = temp
            .dir()
            .load(&lib("nonexistent"))
            .expect("load must succeed");
        assert_eq!(loaded, None);
    }

    #[test]
    fn script_path_is_a_file_only_after_the_script_has_been_saved() {
        let temp = TempLibraryDir::new("exists");
        let dir = temp.dir();
        assert!(!dir.script_path(&lib("orders")).is_file());
        dir.save(&lib("orders"), "select * from orders;")
            .expect("save must succeed");
        assert!(dir.script_path(&lib("orders")).is_file());
    }

    #[test]
    fn saving_creates_the_library_directory_if_it_does_not_exist_yet() {
        let temp = TempLibraryDir::new("create-dir");
        assert!(!temp.0.exists());
        temp.dir()
            .save(&lib("first-script"), "select 1;")
            .expect("save must succeed");
        assert!(temp.0.is_dir());
    }

    #[test]
    fn the_library_stays_a_flat_pool_with_no_subdirectories() {
        let temp = TempLibraryDir::new("flat-pool");
        let dir = temp.dir();
        dir.save(&lib("a"), "select 1;").expect("save must succeed");
        dir.save(&lib("b"), "select 2;").expect("save must succeed");

        for entry in std::fs::read_dir(&temp.0).expect("dir must exist") {
            let entry = entry.expect("entry must be readable");
            assert!(
                entry.file_type().expect("must read file type").is_file(),
                "the library directory must hold only flat files, found a directory at {:?}",
                entry.path()
            );
        }
    }

    #[test]
    fn rename_renames_the_file_atomically_via_a_real_rename() {
        let temp = TempLibraryDir::new("rename");
        let dir = temp.dir();
        dir.save(&lib("old-name"), "select 1;")
            .expect("save must succeed");

        dir.rename(&lib("old-name"), &lib("new-name"))
            .expect("rename must succeed");

        assert!(!dir.script_path(&lib("old-name")).is_file());
        assert_eq!(
            dir.load(&lib("new-name")).expect("load must succeed"),
            Some("select 1;".to_owned())
        );
    }

    #[test]
    fn saving_twice_overwrites_rather_than_appends() {
        let temp = TempLibraryDir::new("overwrite");
        let dir = temp.dir();
        dir.save(&lib("orders"), "select 1;")
            .expect("first save must succeed");
        dir.save(&lib("orders"), "select 2;")
            .expect("second save must succeed");

        assert_eq!(
            dir.load(&lib("orders")).expect("load must succeed"),
            Some("select 2;".to_owned())
        );
    }

    #[test]
    fn concurrent_library_and_session_writes_do_not_race_or_corrupt_each_other() {
        use crate::session_store::{
            ConnectionKey, ScriptBacking, ScriptFileName, TabEntrySnapshot, TabKind,
            TabSessionSnapshot,
        };

        let temp = TempLibraryDir::new("concurrent");
        let sessions_root = temp.0.parent().unwrap().join(format!(
            "zsql-library-test-concurrent-sessions-{}",
            std::process::id()
        ));
        let _cleanup = TempLibraryDir(sessions_root.clone());
        let library_root = temp.0.clone();
        let key = ConnectionKey::Saved(uuid::Uuid::new_v4());

        let library_writer = {
            let library_root = library_root.clone();
            move || {
                let dir = LibraryDir::at(&library_root);
                for i in 0..20 {
                    dir.save(&lib("shared-script"), &format!("select 'lib-{i}';"))
                        .expect("library write must not race with a session write");
                }
            }
        };
        let session_writer = {
            let sessions_root = sessions_root.clone();
            move || {
                for i in 0..20 {
                    let snapshot = TabSessionSnapshot {
                        tabs: vec![TabEntrySnapshot {
                            kind: TabKind::Script {
                                backing: ScriptBacking::SessionScratch {
                                    file: ScriptFileName::new("query-1.sql").unwrap(),
                                },
                            },
                            title: "query-1.sql".to_owned(),
                            buffer_text: Some(format!("select 'session-{i}';")),
                        }],
                        active_index: Some(0),
                    };
                    super::super::SessionDir::new(&sessions_root, key)
                        .save_snapshot(&snapshot)
                        .expect("session write must not race with a library write");
                }
            }
        };

        let handle_a = std::thread::spawn(library_writer);
        let handle_b = std::thread::spawn(session_writer);
        handle_a.join().expect("library writer must not panic");
        handle_b.join().expect("session writer must not panic");

        let library_text = LibraryDir::at(&library_root)
            .load(&lib("shared-script"))
            .expect("load must succeed")
            .expect("library script must exist");
        assert!(library_text.starts_with("select 'lib-"));

        let session_snapshot = super::super::SessionDir::new(&sessions_root, key)
            .load_snapshot()
            .expect("load must succeed")
            .expect("session must have a snapshot");
        assert!(
            session_snapshot.tabs[0]
                .buffer_text
                .as_deref()
                .is_some_and(|text| text.starts_with("select 'session-"))
        );
    }

    #[test]
    fn listing_a_library_dir_that_does_not_exist_yet_returns_an_empty_list_not_an_error() {
        let temp = TempLibraryDir::new("list-missing-dir");
        assert!(!temp.0.exists());
        let entries = temp
            .dir()
            .list()
            .expect("listing a missing dir must not error");
        assert!(entries.is_empty());
    }

    #[test]
    fn listing_an_empty_but_existing_library_dir_returns_an_empty_list() {
        let temp = TempLibraryDir::new("list-empty-dir");
        std::fs::create_dir_all(&temp.0).expect("must create dir");
        let entries = temp
            .dir()
            .list()
            .expect("listing an empty dir must not error");
        assert!(entries.is_empty());
    }

    #[test]
    fn listing_skips_a_file_whose_stem_is_not_a_valid_library_name() {
        let temp = TempLibraryDir::new("list-invalid-stem");
        std::fs::create_dir_all(&temp.0).expect("must create dir");
        // Stem "report.sql" still carries the reserved .sql suffix.
        std::fs::write(temp.0.join("report.sql.sql"), "select 1;").expect("must write");
        // Stem "library:x" carries the reserved library: prefix.
        std::fs::write(temp.0.join("library:x.sql"), "select 2;").expect("must write");
        let dir = temp.dir();
        dir.save(&lib("orders"), "select 3;")
            .expect("save must succeed");

        let names: Vec<String> = dir
            .list()
            .expect("list must succeed")
            .into_iter()
            .map(|entry| entry.name)
            .collect();

        assert_eq!(
            names,
            vec!["orders".to_owned()],
            "a file whose stem is not a valid LibraryName must never be listed"
        );
    }

    #[test]
    fn listing_a_populated_library_returns_every_sql_file_by_bare_name() {
        let temp = TempLibraryDir::new("list-populated");
        let dir = temp.dir();
        dir.save(&lib("revenue-report"), "select 1;")
            .expect("save must succeed");
        dir.save(&lib("slow-queries"), "select 2;")
            .expect("save must succeed");

        let mut names: Vec<String> = dir
            .list()
            .expect("list must succeed")
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        names.sort();

        assert_eq!(
            names,
            vec!["revenue-report".to_owned(), "slow-queries".to_owned()]
        );
    }

    /// A library script's buffer only ever carries what a caller passes as
    /// `text` -- there is no connection URL or credential type anywhere on
    /// this module's path from call to disk -- so a persisted library file
    /// can structurally never carry one either.
    #[test]
    fn persisted_library_bytes_never_contain_a_connection_url_or_secret() {
        let temp = TempLibraryDir::new("secrets");
        let dir = temp.dir();
        let buffer_text = "select * from accounts where owner = 'prod-db.internal admin hunter2';";
        dir.save(&lib("innocuous-name"), buffer_text)
            .expect("save must succeed");

        let secret_url = "postgres://admin:hunter2@prod-db.internal:5432/app";
        let bytes = std::fs::read_to_string(dir.script_path(&lib("innocuous-name")))
            .expect("must read back");
        assert!(!bytes.contains(secret_url));
        assert!(!bytes.contains("postgres://"));
    }

    #[test]
    fn rename_errors_without_touching_anything_when_the_destination_already_exists() {
        let temp = TempLibraryDir::new("rename-toctou");
        let dir = temp.dir();
        dir.save(&lib("old-name"), "select 1;")
            .expect("save must succeed");
        dir.save(&lib("new-name"), "select 2;")
            .expect("save must succeed");

        let result = dir.rename(&lib("old-name"), &lib("new-name"));

        assert!(matches!(result, Err(SessionStoreError::Duplicate(_))));
        assert!(dir.script_path(&lib("old-name")).is_file());
        assert_eq!(
            dir.load(&lib("new-name")).expect("load must succeed"),
            Some("select 2;".to_owned()),
            "the pre-existing destination must never be clobbered"
        );
    }
}
