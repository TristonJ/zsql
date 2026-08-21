//! What a script tab's canonical content is and where it lives on disk,
//! independent of any windowing toolkit's view: whether it autosaves
//! continuously (session-owned) or requires an explicit save because it is
//! shared across connections (library-backed)

use std::fmt;
use std::path::{Path, PathBuf};

use super::disk::ScriptRef;
use super::external;
use super::library::LibraryDir;
use super::save_claim::SaveClaim;
use super::session_dir::{SCRATCH_DIR_NAME, SessionDir};
use super::{LIBRARY_FILE_PREFIX, SCRIPT_FILE_EXTENSION, SessionStoreError};

/// A session directory's own sibling script file's bare name: never
/// `scratch/`-prefixed and never a path (no separator, never `..`)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScriptFileName(String);

/// `name` is not a valid bare sibling script file name.
#[derive(Debug, thiserror::Error)]
#[error("invalid script file name: {0:?}")]
pub struct InvalidScriptFileName(String);

impl ScriptFileName {
    /// # Errors
    /// Returns [`InvalidScriptFileName`] if `name` is empty, is `scratch/`-
    /// prefixed, contains a path separator, or is exactly `..`.
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidScriptFileName> {
        let name = name.into();
        let scratch_prefix = format!("{SCRATCH_DIR_NAME}/");
        if name.is_empty()
            || name.starts_with(&scratch_prefix)
            || name.contains('/')
            || name.contains('\\')
            || name == ".."
        {
            return Err(InvalidScriptFileName(name));
        }
        Ok(Self(name))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ScriptFileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A shared library script's bare name: no `.sql` extension, no `library:`
/// prefix, and never a path (no separator, never `..`)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LibraryName(String);

/// `name` is not a valid bare library script name.
#[derive(Debug, thiserror::Error)]
#[error("invalid library name: {0:?}")]
pub struct InvalidLibraryName(String);

impl LibraryName {
    /// # Errors
    /// Returns [`InvalidLibraryName`] if `name` is empty, ends with
    /// [`SCRIPT_FILE_EXTENSION`], starts with [`LIBRARY_FILE_PREFIX`],
    /// contains a path separator, or is exactly `..`.
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidLibraryName> {
        let name = name.into();
        if name.is_empty()
            || name.ends_with(SCRIPT_FILE_EXTENSION)
            || name.starts_with(LIBRARY_FILE_PREFIX)
            || name.contains('/')
            || name.contains('\\')
            || name == ".."
        {
            return Err(InvalidLibraryName(name));
        }
        Ok(Self(name))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LibraryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a script tab's canonical content lives, and how it saves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptBacking {
    /// An autosaved script whose own file lives under the connection's
    /// session directory's `scratch/` subdirectory
    SessionScratch { file: ScriptFileName },
    /// An autosaved, explicitly named `.sql` at the connection's session
    /// directory's top level
    SessionNamed { file: ScriptFileName },
    /// A shared library script. The buffer autosaves only to a session-
    /// scoped draft while it diverges from `saved_text` (the library file's
    /// last explicitly-saved content)
    Library {
        name: LibraryName,
        /// The library file's content as of the last explicit save (or, on
        /// restore, as loaded from disk). `None` means no confirmed
        /// baseline exists yet e.g. restore hit a transient read error
        /// against a tab with a draft
        saved_text: Option<String>,
    },
    /// A file opened from outside any session or library directory
    External {
        path: PathBuf,
        /// The external file's content as of the last explicit save (or, on
        /// restore, as loaded from disk). `None` means no confirmed
        /// baseline exists yet e.g. restore hit a transient read error
        /// against a tab with a draft
        saved_text: Option<String>,
    },
}

/// What pressing Save does for a tab
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveAction {
    /// Open the Save modal
    OpenModal,
    /// Already autosaved continuously so pressing Save writes nothing new.
    NoOp,
    /// Write the named library file directly with the buffer's current
    /// text, no modal.
    WriteLibrary,
    /// Write the external file directly with the buffer's current text, no
    /// modal.
    WriteExternal,
}

/// What a save/rename needs, threaded per call from the caller that owns the
/// active connection's session and the shared library
pub struct SessionIo<'a> {
    pub dir: &'a SessionDir,
    pub library: &'a LibraryDir,
}

impl ScriptBacking {
    /// Whether `buffer_text` currently diverges from this backing's last
    /// explicitly-saved content. Always `false` for a session-owned backing.
    /// For a library or external backing, `true` whenever no confirmed
    /// baseline exists yet (`saved_text` is `None`), or whenever
    /// `buffer_text` differs from the baseline it does carry.
    #[must_use]
    pub fn diverged(&self, buffer_text: &str) -> bool {
        match self {
            Self::SessionScratch { .. } | Self::SessionNamed { .. } => false,
            Self::Library { saved_text, .. } | Self::External { saved_text, .. } => {
                saved_text.as_deref() != Some(buffer_text)
            }
        }
    }

    /// The library name this backing carries, if it is [`Self::Library`].
    #[must_use]
    pub fn library_name(&self) -> Option<&str> {
        match self {
            Self::Library { name, .. } => Some(name.as_str()),
            Self::SessionScratch { .. } | Self::SessionNamed { .. } | Self::External { .. } => None,
        }
    }

    /// The absolute path this backing carries, if it is [`Self::External`].
    #[must_use]
    pub fn external_path(&self) -> Option<&Path> {
        match self {
            Self::External { path, .. } => Some(path.as_path()),
            Self::SessionScratch { .. } | Self::SessionNamed { .. } | Self::Library { .. } => None,
        }
    }

    /// This backing's own bare sibling file name, if it is session-owned.
    #[must_use]
    pub fn session_file(&self) -> Option<&ScriptFileName> {
        match self {
            Self::SessionScratch { file } | Self::SessionNamed { file } => Some(file),
            Self::Library { .. } | Self::External { .. } => None,
        }
    }

    /// A stable identity for this backing's remembered parameter values,
    /// distinct across every kind of backing so switching tabs or scripts
    /// never shows another script's history.
    #[must_use]
    pub fn param_history_key(&self) -> String {
        match self {
            Self::SessionScratch { file } => format!("scratch:{}", file.as_str()),
            Self::SessionNamed { file } => format!("session:{}", file.as_str()),
            Self::Library { name, .. } => format!("library:{}", name.as_str()),
            Self::External { path, .. } => format!("external:{}", path.display()),
        }
    }

    /// What pressing Save does for a tab with this backing.
    #[must_use]
    pub fn save_action(&self) -> SaveAction {
        match self {
            Self::SessionScratch { .. } => SaveAction::OpenModal,
            Self::SessionNamed { .. } => SaveAction::NoOp,
            Self::Library { .. } => SaveAction::WriteLibrary,
            Self::External { .. } => SaveAction::WriteExternal,
        }
    }

    /// Whether this backing's file can be renamed in place
    #[must_use]
    pub fn supports_rename(&self) -> bool {
        !matches!(self, Self::External { .. })
    }

    /// The on-disk path this backing's content lives at.
    #[must_use]
    pub fn disk_path(&self, dir: &SessionDir, library: &LibraryDir) -> PathBuf {
        match self {
            Self::SessionScratch { file } => dir.scratch_path(file),
            Self::SessionNamed { file } => dir.named_path(file),
            Self::Library { name, .. } => library.script_path(name),
            Self::External { path, .. } => path.clone(),
        }
    }

    /// Write `buffer_text` to this backing's real target: a session
    /// variant's own file, a library file, or an external file.
    /// A library or external write also deletes the backing's draft.
    /// A draft-delete failure is logged, not returned.
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if the underlying write fails.
    #[tracing::instrument(name = "script_backing_write", skip(self, buffer_text, io))]
    pub fn write(&self, buffer_text: &str, io: &SessionIo<'_>) -> Result<(), SessionStoreError> {
        match self {
            Self::SessionScratch { file } => io.dir.write_scratch(file, buffer_text),
            Self::SessionNamed { file } => io.dir.write_named(file, buffer_text),
            Self::Library { name, .. } => {
                io.library.save(name, buffer_text)?;
                self.delete_stale_draft(io);
                Ok(())
            }
            Self::External { path, .. } => {
                external::save(path, buffer_text)?;
                self.delete_stale_draft(io);
                Ok(())
            }
        }
    }

    fn delete_stale_draft(&self, io: &SessionIo<'_>) {
        if let Some(draft_file) = crate::session_store::draft_file_name(self)
            && let Err(err) = io.dir.delete_draft(&draft_file)
        {
            tracing::warn!(
                error = %err,
                "wrote the script but failed to delete its now-stale draft"
            );
        }
    }

    /// Consuming rename: claim `new_name` as this backing's new identity via
    /// `io.dir`, deleting the old file once the new one is confirmed unique,
    /// and return the renamed backing. Always promotes a scratch-backed tab
    /// to [`Self::SessionNamed`]
    ///
    /// # Errors
    /// Returns [`SessionStoreError::Duplicate`] if `new_name` already names
    /// an existing top-level session file, or [`SessionStoreError`] if the
    /// rename itself fails.
    #[tracing::instrument(name = "script_backing_rename", skip(self, io))]
    pub fn rename(
        self,
        new_name: &ScriptFileName,
        claim: SaveClaim,
        io: &SessionIo<'_>,
    ) -> Result<Self, SessionStoreError> {
        match &self {
            Self::SessionScratch { file } | Self::SessionNamed { file } => {
                let old_ref = if matches!(self, Self::SessionScratch { .. }) {
                    ScriptRef::Scratch(file.clone())
                } else {
                    ScriptRef::Session(file.clone())
                };
                debug_assert_eq!(
                    claim.dir(),
                    io.dir.path(),
                    "a rename's claim must be minted for the directory it renames within"
                );
                // Recorded before the rename itself runs, so a stale
                // autosave dispatched before this call can never land after
                // it and resurrect (or clobber) the renamed file
                claim.record_written();
                io.dir.rename_script(&old_ref, new_name)?;
                Ok(Self::SessionNamed {
                    file: new_name.clone(),
                })
            }
            Self::Library { .. } | Self::External { .. } => Err(SessionStoreError::UnsafeRef(
                "only a session-owned backing can be renamed this way".to_owned(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        InvalidLibraryName, InvalidScriptFileName, LibraryDir, LibraryName, SaveAction,
        ScriptBacking, ScriptFileName, SessionDir, SessionIo,
    };
    use crate::session_store::SaveClaimFactory;

    fn file(name: &str) -> ScriptFileName {
        ScriptFileName::new(name).expect("must be a valid name")
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "zsql-backing-test-{label}-{}-{n}",
                std::process::id()
            ));
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn lib_name(name: &str) -> LibraryName {
        LibraryName::new(name).expect("must be a valid name")
    }

    #[test]
    fn script_file_name_rejects_a_scratch_prefixed_name() {
        assert!(matches!(
            ScriptFileName::new("scratch/query-1.sql"),
            Err(InvalidScriptFileName(_))
        ));
    }

    #[test]
    fn script_file_name_rejects_a_path_separator_or_parent_component() {
        assert!(ScriptFileName::new("a/b.sql").is_err());
        assert!(ScriptFileName::new("a\\b.sql").is_err());
        assert!(ScriptFileName::new("..").is_err());
        assert!(ScriptFileName::new("").is_err());
    }

    #[test]
    fn script_file_name_accepts_an_ordinary_bare_name() {
        assert_eq!(file("orders.sql").as_str(), "orders.sql");
    }

    #[test]
    fn library_name_rejects_a_sql_suffix() {
        assert!(matches!(
            LibraryName::new("orders.sql"),
            Err(InvalidLibraryName(_))
        ));
    }

    #[test]
    fn library_name_rejects_a_library_prefix() {
        assert!(matches!(
            LibraryName::new("library:orders"),
            Err(InvalidLibraryName(_))
        ));
    }

    #[test]
    fn library_name_rejects_a_path_separator_or_parent_component() {
        for name in ["../escape", "a/b", "a\\b", ".."] {
            assert!(
                matches!(LibraryName::new(name), Err(InvalidLibraryName(_))),
                "{name:?} must be rejected"
            );
        }
    }

    #[test]
    fn library_name_accepts_an_ordinary_bare_name() {
        assert_eq!(lib_name("orders").as_str(), "orders");
    }

    #[test]
    fn a_session_scratch_backing_never_diverges_regardless_of_edits() {
        let backing = ScriptBacking::SessionScratch {
            file: file("query-1.sql"),
        };
        assert!(!backing.diverged("select 1;"));
        assert!(!backing.diverged("select 1; select 2; select 3;"));
    }

    #[test]
    fn a_session_named_backing_never_diverges_regardless_of_edits() {
        let backing = ScriptBacking::SessionNamed {
            file: file("orders.sql"),
        };
        assert!(!backing.diverged("select 1;"));
        assert!(!backing.diverged(""));
    }

    #[test]
    fn a_library_backing_diverges_exactly_when_the_buffer_differs_from_the_saved_text() {
        let backing = ScriptBacking::Library {
            name: lib_name("revenue-report"),
            saved_text: Some("select * from revenue;".to_owned()),
        };
        assert!(!backing.diverged("select * from revenue;"));
        assert!(backing.diverged("select * from revenue where year = 2026;"));
    }

    #[test]
    fn library_name_is_only_present_for_a_library_backing() {
        assert_eq!(
            ScriptBacking::SessionNamed {
                file: file("orders.sql")
            }
            .library_name(),
            None
        );
        let backing = ScriptBacking::Library {
            name: lib_name("orders"),
            saved_text: Some(String::new()),
        };
        assert_eq!(backing.library_name(), Some("orders"));
    }

    #[test]
    fn save_on_a_scratch_session_tab_opens_the_modal() {
        assert_eq!(
            ScriptBacking::SessionScratch {
                file: file("query-1.sql")
            }
            .save_action(),
            SaveAction::OpenModal
        );
    }

    #[test]
    fn save_on_a_named_session_tab_is_a_no_op() {
        assert_eq!(
            ScriptBacking::SessionNamed {
                file: file("orders.sql")
            }
            .save_action(),
            SaveAction::NoOp
        );
    }

    #[test]
    fn save_on_a_library_backed_tab_writes_the_library_file_directly_with_no_modal() {
        let backing = ScriptBacking::Library {
            name: lib_name("orders"),
            saved_text: Some("select 1;".to_owned()),
        };
        assert_eq!(backing.save_action(), SaveAction::WriteLibrary);
    }

    #[test]
    fn an_external_backing_diverges_exactly_when_the_buffer_differs_from_the_saved_text() {
        let backing = ScriptBacking::External {
            path: PathBuf::from("/home/t/work/migrate.sql"),
            saved_text: Some("select 1;".to_owned()),
        };
        assert!(!backing.diverged("select 1;"));
        assert!(backing.diverged("select 2;"));
    }

    #[test]
    fn a_library_backing_with_no_confirmed_baseline_always_diverges() {
        let backing = ScriptBacking::Library {
            name: lib_name("orders"),
            saved_text: None,
        };
        assert!(backing.diverged("select 1;"));
        assert!(backing.diverged(""));
    }

    #[test]
    fn external_path_is_only_present_for_an_external_backing() {
        assert_eq!(
            ScriptBacking::SessionNamed {
                file: file("orders.sql")
            }
            .external_path(),
            None
        );
        let backing = ScriptBacking::External {
            path: PathBuf::from("/tmp/migrate.sql"),
            saved_text: Some(String::new()),
        };
        assert_eq!(
            backing.external_path(),
            Some(PathBuf::from("/tmp/migrate.sql").as_path())
        );
    }

    #[test]
    fn save_on_an_external_backed_tab_writes_the_external_file_directly_with_no_modal() {
        let backing = ScriptBacking::External {
            path: PathBuf::from("/tmp/migrate.sql"),
            saved_text: Some("select 1;".to_owned()),
        };
        assert_eq!(backing.save_action(), SaveAction::WriteExternal);
    }

    #[test]
    fn param_history_key_distinguishes_every_kind_of_backing() {
        let scratch = ScriptBacking::SessionScratch {
            file: file("query-1.sql"),
        };
        let named = ScriptBacking::SessionNamed {
            file: file("query-1.sql"),
        };
        let library = ScriptBacking::Library {
            name: lib_name("query-1"),
            saved_text: None,
        };
        let external = ScriptBacking::External {
            path: PathBuf::from("/tmp/query-1.sql"),
            saved_text: None,
        };
        let keys = [
            scratch.param_history_key(),
            named.param_history_key(),
            library.param_history_key(),
            external.param_history_key(),
        ];
        let mut unique = keys.to_vec();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            keys.len(),
            "every backing kind must key uniquely: {keys:?}"
        );
    }

    #[test]
    fn param_history_key_is_stable_for_the_same_backing() {
        let backing = ScriptBacking::SessionNamed {
            file: file("orders.sql"),
        };
        assert_eq!(backing.param_history_key(), backing.param_history_key());
    }

    #[test]
    fn session_file_is_only_present_for_a_session_owned_backing() {
        assert_eq!(
            ScriptBacking::SessionScratch {
                file: file("query-1.sql")
            }
            .session_file()
            .map(ScriptFileName::as_str),
            Some("query-1.sql")
        );
        assert_eq!(
            ScriptBacking::Library {
                name: lib_name("orders"),
                saved_text: None
            }
            .session_file(),
            None
        );
    }

    fn io_over<'a>(dir: &'a SessionDir, library: &'a LibraryDir) -> SessionIo<'a> {
        SessionIo { dir, library }
    }

    #[test]
    fn writing_a_session_scratch_backing_writes_its_scratch_sibling() {
        let temp = TempDir::new("write-scratch");
        let dir = SessionDir::at(&temp.0);
        let library = LibraryDir::new(temp.0.join("unused-library"));
        let backing = ScriptBacking::SessionScratch {
            file: file("query-1.sql"),
        };

        backing
            .write("select 1;", &io_over(&dir, &library))
            .expect("write must succeed");

        assert_eq!(
            std::fs::read_to_string(temp.0.join("scratch").join("query-1.sql"))
                .expect("must read back"),
            "select 1;"
        );
    }

    #[test]
    fn writing_a_session_named_backing_writes_its_scripts_sibling() {
        let temp = TempDir::new("write-named");
        let dir = SessionDir::at(&temp.0);
        let library = LibraryDir::new(temp.0.join("unused-library"));
        let backing = ScriptBacking::SessionNamed {
            file: file("orders.sql"),
        };

        backing
            .write("select * from orders;", &io_over(&dir, &library))
            .expect("write must succeed");

        assert_eq!(
            std::fs::read_to_string(temp.0.join("scripts").join("orders.sql"))
                .expect("must read back"),
            "select * from orders;"
        );
    }

    #[test]
    fn renaming_a_library_backing_is_refused() {
        let temp = TempDir::new("rename-library-refused");
        let dir = SessionDir::at(&temp.0);
        let library = LibraryDir::new(temp.0.join("unused-library"));
        let claim = SaveClaimFactory::new().mint(&temp.0);
        let backing = ScriptBacking::Library {
            name: lib_name("orders"),
            saved_text: None,
        };

        let result = backing.rename(&file("orders-2.sql"), claim, &io_over(&dir, &library));

        assert!(matches!(
            result,
            Err(super::SessionStoreError::UnsafeRef(_))
        ));
    }

    #[test]
    fn renaming_an_external_backing_is_refused() {
        let temp = TempDir::new("rename-external-refused");
        let dir = SessionDir::at(&temp.0);
        let library = LibraryDir::new(temp.0.join("unused-library"));
        let claim = SaveClaimFactory::new().mint(&temp.0);
        let backing = ScriptBacking::External {
            path: PathBuf::from("/tmp/migrate.sql"),
            saved_text: None,
        };

        let result = backing.rename(&file("migrate-2.sql"), claim, &io_over(&dir, &library));

        assert!(matches!(
            result,
            Err(super::SessionStoreError::UnsafeRef(_))
        ));
    }
}
