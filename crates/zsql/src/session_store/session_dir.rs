//! One connection's session directory: the sole owner of its disk I/O --
//! `tabs.toml` load/mutate, named and scratch sibling script files, drafts,
//! and directory listing/pruning. Every method that reads or writes a
//! session directory's files acquires [`IO_LOCK`] for its own body, so two
//! independent calls (from different threads, or a background executor and
//! the render thread) can never observe or produce a half-written state for
//! the same or a different session directory.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use super::disk::{self, CURRENT_TABS_FILE_VERSION, PersistedTab, ScriptRef, TabsFile};
use super::save_claim::SaveClaim;
use crate::session_store::backing::ScriptFileName;
use crate::session_store::{
    ConnectionKey, SCRIPT_FILE_EXTENSION, SaveClaimFactory, ScriptBacking, SessionStoreError,
    TabEntrySnapshot, TabKind, TabSessionSnapshot,
};

/// Directory, under a session directory, holding every unnamed script's own
/// sibling `.sql` file
pub(crate) const SCRATCH_DIR_NAME: &str = "scratch";
/// Directory, under a session directory, holding every named script's own
/// sibling `.sql` file
pub(crate) const SCRIPTS_DIR_NAME: &str = "scripts";
/// Directory, under a session directory, holding one draft file per
/// currently-diverged library- or external-backed tab.
const DRAFTS_DIR_NAME: &str = "drafts";
/// Suffix appended to a path for the temp file [`atomic_write`] writes to
/// before atomically renaming it into place.
const TMP_SUFFIX: &str = ".tmp";
/// File name of a session directory's tab-order/metadata file
pub(crate) const TABS_FILE_NAME: &str = "tabs.toml";

/// Serializes every read or write this module performs against a session
/// directory (or, sharing this same lock, the shared library and an
/// external file -- see [`crate::session_store::library`] and
/// [`crate::session_store::external`]) against every other one. A
/// connection switch can dispatch two saves close enough together that,
/// without this, both could read a session file before either wrote it
/// back, silently discarding one; guarding a plain load the same way closes
/// the matching read/write race.
///
/// Process-local only: it does nothing to order writes from two separate
/// running instances of this application pointed at the same data
/// directory.
pub(crate) static IO_LOCK: Mutex<()> = Mutex::new(());

/// One named session script as [`SessionDir::list_scripts`] sees it
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionScriptEntry {
    pub file_name: String,
    pub modified: SystemTime,
}

/// Owns one connection's session directory
pub struct SessionDir {
    path: PathBuf,
}

/// Guard indicating that [`IO_LOCK`] is held
pub struct IoGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl IoGuard {
    pub(crate) fn acquire() -> Self {
        Self {
            _guard: IO_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        }
    }
}

/// A [`SessionDir`] view that owns an [`IoGuard`]
pub struct LockedSessionDir<'a> {
    dir: &'a SessionDir,
    _guard: IoGuard,
}

impl SessionDir {
    /// Acquire [`IO_LOCK`] and return the locked view over this directory
    #[must_use]
    pub fn locked(&self) -> LockedSessionDir<'_> {
        LockedSessionDir::new(self, IoGuard::acquire())
    }

    #[must_use]
    pub fn new(root: &Path, key: ConnectionKey) -> Self {
        Self {
            path: root.join(key.storage_dir_name()),
        }
    }

    /// A `SessionDir` wrapping `path` directly
    #[must_use]
    pub fn at(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The path `file` resolves to as a top-level sibling script.
    #[must_use]
    pub fn named_path(&self, file: &ScriptFileName) -> PathBuf {
        self.path.join(SCRIPTS_DIR_NAME).join(file.as_str())
    }

    /// The path `file` resolves to as a `scratch/` sibling script.
    #[must_use]
    pub fn scratch_path(&self, file: &ScriptFileName) -> PathBuf {
        self.path.join(SCRATCH_DIR_NAME).join(file.as_str())
    }

    /// Read-modify-write `tabs.toml`: load it (or start from
    /// [`TabsFile::default`] if absent), apply `f`, then write the result
    /// back
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if the existing file cannot be parsed,
    /// or the result cannot be serialized or written.
    pub fn mutate_tabs(&self, f: impl FnOnce(&mut TabsFile)) -> Result<(), SessionStoreError> {
        self.locked().mutate_tabs(f)
    }

    /// Write `text` as `file`'s top-level sibling script.
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if the write fails.
    pub fn write_named(&self, file: &ScriptFileName, text: &str) -> Result<(), SessionStoreError> {
        self.locked().write_named(file, text)
    }

    /// Write `text` as `file`'s sibling under `scratch/`.
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if the write fails.
    pub fn write_scratch(
        &self,
        file: &ScriptFileName,
        text: &str,
    ) -> Result<(), SessionStoreError> {
        self.locked().write_scratch(file, text)
    }

    /// Delete `draft_file` under `drafts/`, if it exists
    ///
    /// # Errors
    /// Returns [`SessionStoreError::Write`] if it exists but cannot be
    /// removed.
    pub fn delete_draft(&self, draft_file: &str) -> Result<(), SessionStoreError> {
        self.locked().delete_draft(draft_file)
    }

    /// Delete `file`'s sibling under `scratch/`, if it exists
    ///
    /// # Errors
    /// Returns [`SessionStoreError::Write`] if it exists but cannot be
    /// removed.
    pub fn delete_scratch(&self, file: &ScriptFileName) -> Result<(), SessionStoreError> {
        self.locked().delete_scratch(file)
    }

    /// Every `.sql` file that is a direct child of this session directory,
    /// its exact file name and last-modified time. A missing session
    /// directory returns an empty list rather than an error.
    ///
    /// # Errors
    /// Returns [`SessionStoreError::Read`] if the directory exists but
    /// cannot be read, or an entry's metadata cannot be read.
    #[tracing::instrument(name = "session_dir_list_scripts", skip(self), fields(dir = %self.path.display()))]
    pub fn list_scripts(&self) -> Result<Vec<SessionScriptEntry>, SessionStoreError> {
        let _locked = self.locked();
        let entries = match fs::read_dir(self.path.join(SCRIPTS_DIR_NAME)) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(SessionStoreError::Read(err)),
        };
        let mut scripts = Vec::new();
        for entry in entries {
            let entry = entry.map_err(SessionStoreError::Read)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            if !lower.ends_with(&SCRIPT_FILE_EXTENSION.to_ascii_lowercase()) {
                continue;
            }
            let metadata = entry.metadata().map_err(SessionStoreError::Read)?;
            if !metadata.is_file() {
                continue;
            }
            let modified = metadata.modified().map_err(SessionStoreError::Read)?;
            scripts.push(SessionScriptEntry {
                file_name: name,
                modified,
            });
        }
        Ok(scripts)
    }

    /// Rename a session-owned script's file from `old` to `new`, promoting
    /// it into the session directory's `scripts/` subdirectory, and update
    /// the `tabs.toml` entry for it (if any) to `new` in the same operation
    ///
    /// # Errors
    /// Returns [`SessionStoreError::Duplicate`] if `new` already exists at
    /// the time the rename is about to run, or [`SessionStoreError::Write`]
    /// if the rename itself fails.
    #[tracing::instrument(name = "session_dir_rename_script", skip(self), fields(dir = %self.path.display()))]
    pub fn rename_script(
        &self,
        old: &ScriptRef,
        new: &ScriptFileName,
    ) -> Result<(), SessionStoreError> {
        self.locked().rename_script(old, new)
    }

    /// Ensure this directory's `tabs.toml` has a `Script` entry mapping
    /// `title` to `file_name`, inserting a placeholder one if none
    /// references it yet.
    pub fn reclaim_named_script_ref(
        &self,
        title: &str,
        file_name: &str,
        claims: &SaveClaimFactory,
    ) {
        claims.mint(self.path()).record_written();
        let Ok(file) = ScriptFileName::new(file_name) else {
            tracing::debug!(file_name, "refusing to reclaim an invalid script file name");
            return;
        };
        self.reclaim_named_ref(title, &file);
    }

    /// Ensure `tabs.toml` has a `Script` entry mapping `title` to
    /// `file_name`, inserting a placeholder one if no entry already
    /// references `file_name`
    #[tracing::instrument(name = "session_dir_reclaim_named_ref", skip(self), fields(dir = %self.path.display()))]
    fn reclaim_named_ref(&self, title: &str, file_name: &ScriptFileName) {
        let result = self.mutate_tabs(|file| {
            let already_owned = file.tabs.iter().any(
                |tab| matches!(tab, PersistedTab::Script { file, .. } if file == file_name.as_str()),
            );
            if already_owned {
                return;
            }
            file.tabs.push(PersistedTab::Script {
                title: title.to_owned(),
                file: file_name.as_str().to_owned(),
                draft: None,
            });
        });
        if let Err(err) = result {
            tracing::debug!(error = %err, "could not reclaim a reopened script ref");
        }
    }

    fn tabs_path(&self) -> PathBuf {
        self.path.join(TABS_FILE_NAME)
    }
}

impl<'a> LockedSessionDir<'a> {
    /// The locked view over `dir`, for a caller that acquired its
    /// [`IoGuard`] itself
    #[must_use]
    pub fn new(dir: &'a SessionDir, guard: IoGuard) -> Self {
        Self { dir, _guard: guard }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.dir.path
    }

    /// Load and parse `tabs.toml`, or `None` if this session directory (or
    /// just its `tabs.toml`) does not exist yet.
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if `tabs.toml` exists but cannot be
    /// read or parsed.
    #[tracing::instrument(name = "session_dir_load_tabs", skip(self), fields(dir = %self.path().display()))]
    pub fn load_tabs(&self) -> Result<Option<TabsFile>, SessionStoreError> {
        let tabs_path = self.dir.tabs_path();
        if !tabs_path.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(&tabs_path).map_err(SessionStoreError::Read)?;
        Ok(Some(toml::from_str(&text)?))
    }

    pub fn write_tabs(&self, file: &TabsFile) -> Result<(), SessionStoreError> {
        fs::create_dir_all(&self.dir.path).map_err(SessionStoreError::Write)?;
        let text = toml::to_string_pretty(file)?;
        atomic_write(&self.dir.tabs_path(), &text)
    }

    /// See [`SessionDir::mutate_tabs`].
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if the existing file cannot be parsed,
    /// or the result cannot be serialized or written.
    #[tracing::instrument(name = "session_dir_mutate_tabs", skip(self, f), fields(dir = %self.path().display()))]
    pub fn mutate_tabs(&self, f: impl FnOnce(&mut TabsFile)) -> Result<(), SessionStoreError> {
        let mut file = self.load_tabs()?.unwrap_or_default();
        f(&mut file);
        self.write_tabs(&file)
    }

    /// Write `text` as `file`'s top-level sibling script.
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if the write fails.
    #[tracing::instrument(name = "session_dir_write_named", skip(self, text), fields(dir = %self.path().display(), file = %file))]
    pub fn write_named(&self, file: &ScriptFileName, text: &str) -> Result<(), SessionStoreError> {
        let scripts_dir = self.dir.path.join(SCRIPTS_DIR_NAME);
        fs::create_dir_all(&scripts_dir).map_err(SessionStoreError::Write)?;
        atomic_write(&scripts_dir.join(file.as_str()), text)
    }

    /// Write `text` as `file`'s sibling under `scratch/`.
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if the write fails.
    #[tracing::instrument(name = "session_dir_write_scratch", skip(self, text), fields(dir = %self.path().display(), file = %file))]
    pub fn write_scratch(
        &self,
        file: &ScriptFileName,
        text: &str,
    ) -> Result<(), SessionStoreError> {
        let scratch_dir = self.dir.path.join(SCRATCH_DIR_NAME);
        fs::create_dir_all(&scratch_dir).map_err(SessionStoreError::Write)?;
        atomic_write(&scratch_dir.join(file.as_str()), text)
    }

    /// Read `file`'s top-level sibling script content.
    ///
    /// # Errors
    /// Returns [`SessionStoreError::Read`] if it cannot be read.
    pub fn read_named(&self, file: &ScriptFileName) -> Result<String, SessionStoreError> {
        fs::read_to_string(self.dir.named_path(file)).map_err(SessionStoreError::Read)
    }

    /// Read `file`'s `scratch/` sibling script content.
    ///
    /// # Errors
    /// Returns [`SessionStoreError::Read`] if it cannot be read.
    pub fn read_scratch(&self, file: &ScriptFileName) -> Result<String, SessionStoreError> {
        fs::read_to_string(self.dir.path.join(SCRATCH_DIR_NAME).join(file.as_str()))
            .map_err(SessionStoreError::Read)
    }

    pub fn write_draft(&self, draft_file: &str, text: &str) -> Result<(), SessionStoreError> {
        let drafts_dir = self.dir.path.join(DRAFTS_DIR_NAME);
        fs::create_dir_all(&drafts_dir).map_err(SessionStoreError::Write)?;
        atomic_write(&drafts_dir.join(draft_file), text)
    }

    /// Read `drafts/<draft_file>` after confirming it resolves inside this
    /// session directory's own `drafts/` subdirectory.
    ///
    /// # Errors
    /// Returns [`SessionStoreError::UnsafeRef`] if `draft_file` escapes
    /// `drafts/` once joined and normalized, or [`SessionStoreError::Read`]
    /// if it cannot be read.
    pub fn read_draft(&self, draft_file: &str) -> Result<String, SessionStoreError> {
        let drafts_dir = self.dir.path.join(DRAFTS_DIR_NAME);
        let path = resolve_within(&drafts_dir, draft_file)
            .ok_or_else(|| SessionStoreError::UnsafeRef(draft_file.to_owned()))?;
        fs::read_to_string(path).map_err(SessionStoreError::Read)
    }

    /// Delete `drafts/<draft_file>` if it exists
    ///
    /// # Errors
    /// Returns [`SessionStoreError::Write`] if it exists but cannot be
    /// removed.
    #[tracing::instrument(name = "session_dir_delete_draft", skip(self), fields(dir = %self.path().display(), draft_file))]
    pub fn delete_draft(&self, draft_file: &str) -> Result<(), SessionStoreError> {
        match fs::remove_file(self.dir.path.join(DRAFTS_DIR_NAME).join(draft_file)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(SessionStoreError::Write(err)),
        }
    }

    /// Delete `scratch/<file>` if it exists
    ///
    /// # Errors
    /// Returns [`SessionStoreError::Write`] if it exists but cannot be
    /// removed.
    #[tracing::instrument(name = "session_dir_delete_scratch", skip(self), fields(dir = %self.path().display(), file = %file))]
    pub fn delete_scratch(&self, file: &ScriptFileName) -> Result<(), SessionStoreError> {
        match fs::remove_file(self.dir.path.join(SCRATCH_DIR_NAME).join(file.as_str())) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(SessionStoreError::Write(err)),
        }
    }

    /// Renames a session-owned script's file from `old` to `new`, promoting
    /// it into the session directory's `scripts/` subdirectory, and update
    /// the `tabs.toml` entry for it (if any) to `new` in the same operation.
    ///
    /// # Errors
    /// Returns [`SessionStoreError::Duplicate`] if `new` already exists at
    /// the time the rename is about to run, or [`SessionStoreError::Write`]
    /// if the rename itself fails.
    #[tracing::instrument(name = "session_dir_rename_script", skip(self), fields(dir = %self.path().display()))]
    pub fn rename_script(
        &self,
        old: &ScriptRef,
        new: &ScriptFileName,
    ) -> Result<(), SessionStoreError> {
        let old_path = match old {
            ScriptRef::Session(file) => self.dir.named_path(file),
            ScriptRef::Scratch(file) => self.dir.path.join(SCRATCH_DIR_NAME).join(file.as_str()),
            ScriptRef::Library(_) => {
                return Err(SessionStoreError::UnsafeRef(
                    "a library ref cannot be renamed as a session script".to_owned(),
                ));
            }
        };
        let new_path = self.dir.named_path(new);
        if old_path != new_path && new_path.is_file() {
            return Err(SessionStoreError::Duplicate(new.as_str().to_owned()));
        }
        fs::create_dir_all(self.dir.path.join(SCRIPTS_DIR_NAME))
            .map_err(SessionStoreError::Write)?;
        fs::rename(&old_path, &new_path).map_err(SessionStoreError::Write)?;
        self.retarget_ref(&old.to_ref_string(), new);
        tracing::info!(
            old = %old.to_ref_string(),
            new = %new.as_str(),
            "renamed session script file"
        );
        Ok(())
    }

    /// Best-effort: if `tabs.toml` has a `Script` entry whose `file` ref is
    /// exactly `old_ref`, rewrite that entry's `title` and `file` to `new`.
    /// A missing or unparseable `tabs.toml`, or one with no matching entry,
    /// is logged and otherwise ignored
    fn retarget_ref(&self, old_ref: &str, new: &ScriptFileName) {
        let result =
            self.mutate_tabs(|file| {
                let Some(entry) = file.tabs.iter_mut().find(
                    |tab| matches!(tab, PersistedTab::Script { file, .. } if file == old_ref),
                ) else {
                    return;
                };
                let PersistedTab::Script { title, file, .. } = entry else {
                    unreachable!("matched above on PersistedTab::Script");
                };
                title.clear();
                title.push_str(new.as_str());
                file.clear();
                file.push_str(new.as_str());
            });
        if let Err(err) = result {
            tracing::debug!(error = %err, "could not retarget tabs.toml after a renamed script ref");
        }
    }

    /// Prune every file left over from a save: a stray `.tmp`, an
    /// unlisted `scratch/` sibling, or an unlisted draft
    fn prune_after_save(
        &self,
        keep_scratch: &HashSet<String>,
        keep_drafts: &HashSet<String>,
    ) -> Result<(), SessionStoreError> {
        self.prune_orphan_scratch_files(keep_scratch)?;
        self.prune_orphan_drafts(keep_drafts)?;
        Ok(())
    }

    /// Sweep stray `.tmp` files a crashed mid-write left behind
    fn prune_stray_tmp_files(&self) -> Result<(), SessionStoreError> {
        prune_stray_tmp_files_recursive(&self.dir.path)
    }

    fn prune_orphan_scratch_files(&self, keep: &HashSet<String>) -> Result<(), SessionStoreError> {
        let scratch_dir = self.dir.path.join(SCRATCH_DIR_NAME);
        let entries = match fs::read_dir(&scratch_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(SessionStoreError::Read(err)),
        };
        for entry in entries {
            let entry = entry.map_err(SessionStoreError::Read)?;
            let file_type = entry.file_type().map_err(SessionStoreError::Read)?;
            if !file_type.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.ends_with(TMP_SUFFIX) {
                continue;
            }
            if !keep.contains(name) {
                fs::remove_file(entry.path()).map_err(SessionStoreError::Write)?;
                tracing::info!(file = name, "pruned orphaned scratch script file");
            }
        }
        Ok(())
    }

    fn prune_orphan_drafts(&self, keep: &HashSet<String>) -> Result<(), SessionStoreError> {
        let drafts_dir = self.dir.path.join(DRAFTS_DIR_NAME);
        let entries = match fs::read_dir(&drafts_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(SessionStoreError::Read(err)),
        };
        for entry in entries {
            let entry = entry.map_err(SessionStoreError::Read)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !keep.contains(name) {
                fs::remove_file(entry.path()).map_err(SessionStoreError::Write)?;
                tracing::info!(file = name, "pruned orphaned draft file");
            }
        }
        Ok(())
    }
}

fn prune_stray_tmp_files_recursive(dir: &Path) -> Result<(), SessionStoreError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(SessionStoreError::Read(err)),
    };
    for entry in entries {
        let entry = entry.map_err(SessionStoreError::Read)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.ends_with(TMP_SUFFIX) {
            fs::remove_file(entry.path()).map_err(SessionStoreError::Write)?;
            tracing::info!(file = name, "pruned a stray temp file");
        } else if entry.path().is_dir() {
            prune_stray_tmp_files_recursive(&entry.path())?;
        }
    }
    Ok(())
}

/// Join `relative` onto `base` and confirm the result still lexically
/// resolves inside `base`, without touching the filesystem. Returns `None`
/// for an empty `relative`, one containing a `..` component that climbs
/// above `base`, or one that resolves to `base` itself.
pub(crate) fn resolve_within(base: &Path, relative: &str) -> Option<PathBuf> {
    if relative.is_empty() {
        return None;
    }
    let mut normalized = base.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            std::path::Component::Normal(part) => normalized.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => return None,
        }
    }
    (normalized != base && normalized.starts_with(base)).then_some(normalized)
}

fn tmp_path_for(path: &Path) -> PathBuf {
    let mut file_name = path.file_name().map_or_else(
        || std::ffi::OsString::from("session"),
        std::ffi::OsStr::to_os_string,
    );
    file_name.push(TMP_SUFFIX);
    path.with_file_name(file_name)
}

/// Write `text` to `path` via a temp file plus rename, so a save can never
/// leave `path` half-written. Shared with
/// [`crate::session_store::library`] and [`crate::session_store::external`].
pub(crate) fn atomic_write(path: &Path, text: &str) -> Result<(), SessionStoreError> {
    let tmp_path = tmp_path_for(path);
    fs::write(&tmp_path, text).map_err(SessionStoreError::Write)?;
    fs::rename(&tmp_path, path).map_err(SessionStoreError::Write)?;
    Ok(())
}

impl SessionDir {
    /// Load this directory's snapshot. A missing session directory and a
    /// key with nothing persisted yet both return `Ok(None)` rather than an
    /// error.
    ///
    /// A single entry whose sibling script file or draft is missing,
    /// unreadable, or resolves outside the session directory is logged and
    /// skipped rather than failing the whole load.
    ///
    /// Holds [`IO_LOCK`] for the entire load.
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if `tabs.toml` itself cannot be read
    /// or parsed.
    #[tracing::instrument(name = "session_store_load", skip(self), fields(dir = %self.path().display()))]
    pub fn load_snapshot(&self) -> Result<Option<TabSessionSnapshot>, SessionStoreError> {
        let locked = self.locked();
        let Some(mut file) = locked.load_tabs()? else {
            tracing::debug!("no session directory yet");
            return Ok(None);
        };
        if let Err(err) = locked.prune_stray_tmp_files() {
            tracing::warn!(error = %err, "failed to sweep stray tmp files on session load");
        }

        if file.version < CURRENT_TABS_FILE_VERSION {
            let migrated = disk::migrate_legacy_unnamed_scripts(locked.path(), &mut file);
            file.version = CURRENT_TABS_FILE_VERSION;
            if let Err(err) = locked.write_tabs(&file) {
                tracing::warn!(
                    error = %err,
                    "failed to persist tabs.toml after migrating legacy unnamed scripts"
                );
            } else if migrated {
                tracing::info!("migrated legacy unnamed session scripts into scratch/");
            }
        }

        Ok(Some(TabSessionSnapshot::from_file(file, &locked)))
    }

    /// Persist `snapshot` into this directory unconditionally: `tabs.toml`
    /// describing tab order/kind, plus one sibling `.sql` file per script
    /// tab holding its buffer text, plus a draft file per diverged
    /// library/external tab. Any file left over from a tab no longer
    /// present in `snapshot` is deleted. Creates the session directory if
    /// needed.
    ///
    /// # Errors
    /// Returns [`SessionStoreError`] if the directory, `tabs.toml`, or a
    /// script's buffer file cannot be written.
    #[tracing::instrument(
        name = "session_store_save",
        skip(self, snapshot),
        fields(dir = %self.path().display(), tab_count = snapshot.tabs.len())
    )]
    pub fn save_snapshot(&self, snapshot: &TabSessionSnapshot) -> Result<(), SessionStoreError> {
        self.locked().persist(snapshot)
    }

    /// [`Self::save_snapshot`], but a no-op (returning `Ok(false)`) if
    /// `claim` was superseded.
    ///
    /// # Errors
    /// See [`Self::save_snapshot`].
    #[tracing::instrument(
        name = "session_store_save_if_current",
        skip(self, snapshot, claim),
        fields(dir = %self.path().display(), tab_count = snapshot.tabs.len())
    )]
    pub fn save_snapshot_if_current(
        &self,
        snapshot: &TabSessionSnapshot,
        claim: SaveClaim,
    ) -> Result<bool, SessionStoreError> {
        debug_assert_eq!(
            claim.dir(),
            self.path(),
            "a save's claim must be minted for the directory it writes"
        );
        claim.write_if_current(|guard| LockedSessionDir::new(self, guard).persist(snapshot))
    }
}

impl LockedSessionDir<'_> {
    /// Build every entry's persisted shape (writing its sibling/draft file
    /// as a side effect), then replace `tabs.toml` wholesale and prune
    /// anything left over.
    fn persist(&self, snapshot: &TabSessionSnapshot) -> Result<(), SessionStoreError> {
        // A corrupt existing tabs.toml fails the save rather than being
        // replaced wholesale: overwriting it would also let the prune sweep
        // below delete whatever scratch/draft files its entries still
        // reference.
        self.load_tabs()?;

        let mut keep_scratch = HashSet::new();
        let mut keep_drafts = HashSet::new();
        let mut persisted_tabs = Vec::with_capacity(snapshot.tabs.len());

        for entry in &snapshot.tabs {
            if matches!(entry.kind, TabKind::Schema { .. }) {
                debug_assert!(
                    false,
                    "Schema tabs are filtered out before a snapshot is built"
                );
                continue;
            }
            collect_keep(entry, &mut keep_scratch, &mut keep_drafts);
            persisted_tabs.push(entry.persist(self)?);
        }

        let file = TabsFile {
            active: snapshot.active_index,
            tabs: persisted_tabs,
            version: CURRENT_TABS_FILE_VERSION,
        };
        self.write_tabs(&file)?;
        self.prune_after_save(&keep_scratch, &keep_drafts)?;
        tracing::info!("tab session saved");
        Ok(())
    }
}

/// A named top-level session script is never pruned by a save regardless of
/// whether any open tab still references it. Only an unnamed `scratch/`
/// sibling (never meant to outlive its tab) and a diverged library/external
/// tab's draft are ever pruned.
fn collect_keep(
    entry: &TabEntrySnapshot,
    keep_scratch: &mut HashSet<String>,
    keep_drafts: &mut HashSet<String>,
) {
    if let TabKind::Script { backing } = &entry.kind {
        match backing {
            ScriptBacking::SessionNamed { .. } => {}
            ScriptBacking::SessionScratch { file } => {
                keep_scratch.insert(file.as_str().to_owned());
            }
            ScriptBacking::Library { .. } | ScriptBacking::External { .. } => {
                if entry.buffer_text.is_some()
                    && let Some(draft) = disk::draft_file_name(backing)
                {
                    keep_drafts.insert(draft);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{SessionDir, resolve_within};
    use crate::session_store::backing::ScriptFileName;

    fn file(name: &str) -> ScriptFileName {
        ScriptFileName::new(name).unwrap()
    }

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "zsql-session-dir-test-{label}-{}-{n}",
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

    #[test]
    fn mutate_tabs_applies_the_closure_to_a_freshly_defaulted_file_when_none_exists_yet() {
        let temp = TempDir::new("mutate-tabs-fresh");
        let dir = SessionDir::at(&temp.0);

        dir.mutate_tabs(|file| file.active = Some(3))
            .expect("mutate must succeed even with no existing tabs.toml");

        let loaded = dir
            .locked()
            .load_tabs()
            .expect("load must succeed")
            .expect("mutate_tabs must have created tabs.toml");
        assert_eq!(loaded.active, Some(3));
    }

    #[test]
    fn write_draft_then_read_draft_round_trips() {
        let temp = TempDir::new("draft-round-trip");
        let dir = SessionDir::at(&temp.0);
        let locked = dir.locked();

        locked
            .write_draft("library-orders.sql", "select 1;")
            .expect("write must succeed");
        assert_eq!(
            locked
                .read_draft("library-orders.sql")
                .expect("read must succeed"),
            "select 1;"
        );
    }

    #[test]
    fn delete_draft_removes_the_file_and_is_a_no_op_when_already_gone() {
        let temp = TempDir::new("delete-draft");
        let dir = SessionDir::at(&temp.0);
        dir.locked()
            .write_draft("library-orders.sql", "select 1;")
            .expect("write must succeed");
        assert!(temp.0.join("drafts").join("library-orders.sql").exists());

        dir.delete_draft("library-orders.sql")
            .expect("delete must succeed");
        assert!(!temp.0.join("drafts").join("library-orders.sql").exists());

        dir.delete_draft("library-orders.sql")
            .expect("deleting an already-gone file must not error");
    }

    #[test]
    fn resolve_within_rejects_a_bare_current_dir_reference() {
        let base = std::path::Path::new("/tmp/example/session-dir");
        assert_eq!(resolve_within(base, "."), None);
    }

    #[test]
    fn resolve_within_accepts_a_scratch_prefixed_relative_ref() {
        let base = std::path::Path::new("/tmp/example/session-dir");
        assert_eq!(
            resolve_within(base, "scratch/query-1.sql"),
            Some(base.join("scratch").join("query-1.sql"))
        );
    }

    #[test]
    fn resolve_within_rejects_a_scratch_prefixed_traversal_ref() {
        let base = std::path::Path::new("/tmp/example/session-dir");
        assert_eq!(resolve_within(base, "scratch/../../etc/passwd"), None);
    }

    #[test]
    fn delete_scratch_removes_the_file_and_is_a_no_op_when_already_gone() {
        let temp = TempDir::new("delete-scratch");
        let dir = SessionDir::at(&temp.0);
        dir.write_scratch(&file("query-1.sql"), "select 1;")
            .expect("write must succeed");
        assert!(temp.0.join("scratch").join("query-1.sql").exists());

        dir.delete_scratch(&file("query-1.sql"))
            .expect("delete must succeed");
        assert!(!temp.0.join("scratch").join("query-1.sql").exists());

        dir.delete_scratch(&file("query-1.sql"))
            .expect("deleting an already-gone file must not error");
    }

    #[test]
    fn list_scripts_returns_only_top_level_sql_files_never_scratch_ones() {
        let temp = TempDir::new("list-scripts-excludes-scratch");
        let dir = SessionDir::at(&temp.0);
        dir.write_named(&file("top-customers.sql"), "select * from customers;")
            .expect("write must succeed");
        dir.write_scratch(&file("query-1.sql"), "select 1;")
            .expect("write must succeed");
        std::fs::create_dir_all(temp.0.join("drafts")).expect("must create drafts dir");
        std::fs::write(
            temp.0.join("drafts").join("library-orders.sql"),
            "select * from orders where diverged;",
        )
        .expect("must write a draft file");

        let scripts = dir.list_scripts().expect("list must succeed");
        let names: Vec<&str> = scripts.iter().map(|s| s.file_name.as_str()).collect();

        assert_eq!(names, vec!["top-customers.sql"]);
    }

    #[test]
    fn list_scripts_never_returns_a_file_placed_only_under_scratch_even_when_a_same_named_file_exists_at_the_top_level()
     {
        let temp = TempDir::new("list-scripts-scratch-shadow");
        let dir = SessionDir::at(&temp.0);
        dir.write_named(&file("report.sql"), "select 'top-level';")
            .expect("write must succeed");
        dir.write_scratch(&file("report.sql"), "select 'scratch';")
            .expect("write must succeed");
        dir.write_scratch(&file("only-in-scratch.sql"), "select 'hidden';")
            .expect("write must succeed");

        let scripts = dir.list_scripts().expect("list must succeed");
        let names: Vec<&str> = scripts.iter().map(|s| s.file_name.as_str()).collect();

        assert_eq!(
            names,
            vec!["report.sql"],
            "the top-level report.sql must be listed, and the scratch-only file must never \
             appear regardless of its name"
        );
    }

    #[test]
    fn list_scripts_skips_a_directory_that_happens_to_be_named_like_a_script() {
        let temp = TempDir::new("list-scripts-dir-named-like-script");
        std::fs::create_dir_all(temp.0.join("scripts").join("foo.sql"))
            .expect("must create a dir named foo.sql");
        std::fs::write(temp.0.join("scripts").join("real.sql"), "select 1;")
            .expect("must write a real script");
        let dir = SessionDir::at(&temp.0);

        let scripts = dir.list_scripts().expect("list must succeed");
        let names: Vec<&str> = scripts.iter().map(|s| s.file_name.as_str()).collect();

        assert_eq!(
            names,
            vec!["real.sql"],
            "a directory merely named like a script must never be listed as one"
        );
    }

    #[test]
    fn list_scripts_on_a_directory_that_does_not_exist_yet_is_empty_not_an_error() {
        let temp = TempDir::new("list-scripts-missing-dir");
        let dir = SessionDir::at(&temp.0);
        assert!(dir.list_scripts().expect("must not error").is_empty());
    }

    #[test]
    fn loading_a_session_sweeps_stray_tmp_files_in_the_session_scratch_and_drafts_directories() {
        let temp = TempDir::new("stray-tmp-cleanup");
        let dir = SessionDir::at(&temp.0);
        std::fs::create_dir_all(temp.0.join("drafts")).expect("must create drafts dir");
        std::fs::create_dir_all(temp.0.join("scratch")).expect("must create scratch dir");
        std::fs::write(temp.0.join("foo.sql.tmp"), "half-written")
            .expect("must write a stray session tmp file");
        std::fs::write(temp.0.join("drafts").join("x.sql.tmp"), "half-written")
            .expect("must write a stray draft tmp file");
        std::fs::write(temp.0.join("scratch").join("y.sql.tmp"), "half-written")
            .expect("must write a stray scratch tmp file");

        dir.locked()
            .prune_stray_tmp_files()
            .expect("sweep must succeed");

        assert!(!temp.0.join("foo.sql.tmp").exists());
        assert!(!temp.0.join("drafts").join("x.sql.tmp").exists());
        assert!(!temp.0.join("scratch").join("y.sql.tmp").exists());
    }

    #[test]
    fn prune_after_save_leaves_a_stray_directory_under_scratch_alone_instead_of_failing() {
        let temp = TempDir::new("scratch-stray-dir");
        std::fs::create_dir_all(temp.0.join("scratch").join("stray-subdir"))
            .expect("must create a stray directory under scratch/");
        let dir = SessionDir::at(&temp.0);

        dir.locked()
            .prune_after_save(&HashSet::new(), &HashSet::new())
            .expect("prune must succeed");

        assert!(
            temp.0.join("scratch").join("stray-subdir").is_dir(),
            "a directory under scratch/ is skipped by the sweep, never removed and never \
             an error that fails the save"
        );
    }

    #[test]
    fn prune_after_save_never_deletes_a_named_top_level_file_regardless_of_the_keep_sets() {
        let temp = TempDir::new("prune-never-touches-top-level");
        let dir = SessionDir::at(&temp.0);
        dir.write_named(&file("archived-report.sql"), "select 3;")
            .expect("write must succeed");
        dir.write_named(&file("query-9.sql"), "select 9;")
            .expect("a top-level file named like the legacy unnamed pattern is still named");

        dir.locked()
            .prune_after_save(&HashSet::new(), &HashSet::new())
            .expect("prune with an empty keep-set must still succeed");

        assert!(
            temp.0.join("scripts").join("archived-report.sql").exists(),
            "a named top-level .sql file is never pruned, regardless of the keep-set's contents"
        );
        assert!(
            temp.0.join("scripts").join("query-9.sql").exists(),
            "the top-level sweep never inspects .sql files at all, regardless of their name"
        );
    }
}
