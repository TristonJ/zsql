//! A connection's tab-session snapshot: the ordered tabs, their buffers, and
//! which one was active, persisted to disk under
//! [`crate::config::Config::tab_sessions_path`] so a reconnect (or app
//! restart) can rebuild the same tabs.
//!
//! Window-independent by design: no gpui or driver crate type appears here,
//! so [`TabSessionSnapshot`] and its persistence are testable in a plain
//! `#[test]`. The gpui-facing side of this feature -- building a snapshot
//! from a live `TabModel` and rebuilding one from a loaded snapshot -- lives
//! in `ui::tabs`; dispatching the actual disk write off the render path
//! lives in `ui::workspace`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use zsql_core::preview_state::PreviewQueryState;

use crate::ui::connections::ActiveConnection;

/// The stable identity a connection's tab-session snapshot is keyed under.
/// Never the raw connection URL: a URL can embed a plaintext password (see
/// `crate::connections`'s own doc comment), and this file carries no
/// equivalent protection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConnectionKey {
    /// A saved connection's stable display name
    /// (`crate::connections::StoredConnection::name`).
    Saved(String),
    /// A connection reached only through a URL fallback (e.g.
    /// `DATABASE_URL`) with no saved name behind it. Every such connection
    /// shares this one key rather than any part of its URL.
    Unsaved,
}

/// Prefix marking a [`ConnectionKey::Saved`] entry in the on-disk map, so a
/// saved connection literally named `unsaved` can never collide with
/// [`ConnectionKey::Unsaved`]'s own key.
const SAVED_KEY_PREFIX: &str = "saved:";
/// On-disk key for [`ConnectionKey::Unsaved`].
const UNSAVED_CONNECTION_KEY: &str = "unsaved";

impl ConnectionKey {
    fn storage_key(&self) -> String {
        match self {
            Self::Saved(name) => format!("{SAVED_KEY_PREFIX}{name}"),
            Self::Unsaved => UNSAVED_CONNECTION_KEY.to_owned(),
        }
    }
}

/// What kind of tab a persisted entry was, mirroring `ui::tabs::TabKind`
/// without any of its gpui state. Only a `Script` or a live (never-edited)
/// `Generated` tab is ever persisted this way -- `ui::tabs::TabModel`
/// converts a generated tab to a script the moment it is first edited, so
/// there is no "edited generated" state for this shape to represent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabEntryKind {
    /// A normal, freely-editable script buffer, carrying its full text as
    /// last saved.
    Script { buffer_text: String },
    /// Auto-generated preview SQL for `schema.relation`, entirely defined by
    /// `preview_state`'s sort, page, and filters. No buffer text is stored:
    /// restoring this entry regenerates its SQL from `preview_state` via the
    /// same windowed query builder a live sort, page, or filter change uses,
    /// so the restored buffer and controls can never disagree.
    Generated {
        schema: String,
        relation: String,
        preview_state: PreviewQueryState,
    },
}

/// One persisted tab: its kind (carrying whatever payload that kind needs)
/// and its display title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabEntrySnapshot {
    pub kind: TabEntryKind,
    pub title: String,
}

/// A connection's entire open-tab state: every tab in order, and which one
/// was active.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TabSessionSnapshot {
    /// Every open tab, in tab-bar order.
    pub tabs: Vec<TabEntrySnapshot>,
    /// Position in `tabs` of the tab that was active, if any.
    pub active_index: Option<usize>,
}

/// The on-disk shape of the tab-session file: every connection's snapshot,
/// keyed by [`ConnectionKey::storage_key`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct TabSessionStoreFile {
    connections: HashMap<String, TabSessionSnapshot>,
}

/// Serializes every [`save_snapshot`] call within this process against the
/// same on-disk store, and against every [`load_snapshot`] call. A
/// connection switch can dispatch two saves (the outgoing connection's tabs,
/// then the incoming one's, once it loads) onto gpui's multi-threaded
/// background executor close enough together that, without this lock, both
/// could read the file before either wrote it back -- the second write would
/// then silently discard the first's change. Guarding `load_snapshot` too
/// closes the matching read/write race: a load that ran concurrently with an
/// in-flight save for the same key could otherwise observe the file
/// mid-write (or just before the write) and report a stale or missing
/// snapshot for a key whose save had already been dispatched.
static SAVE_LOCK: Mutex<()> = Mutex::new(());

/// Suffix appended to the store path for the temp file [`save_snapshot`]
/// writes to before atomically renaming it into place, so a save can never
/// leave the real store file half-written.
const TAB_SESSION_TMP_SUFFIX: &str = ".tmp";

/// Errors loading or saving the tab-session store.
#[derive(Debug, thiserror::Error)]
pub enum TabSessionStoreError {
    /// The store file exists but could not be read.
    #[error("failed to read tab session store: {0}")]
    Read(std::io::Error),
    /// The store file's contents could not be parsed as this shape, or an
    /// in-memory value could not be serialized back to it.
    #[error("failed to (de)serialize tab session store: {0}")]
    Serde(#[from] serde_json::Error),
    /// The store file could not be written.
    #[error("failed to write tab session store: {0}")]
    Write(std::io::Error),
}

/// Load `key`'s snapshot from the tab-session store at `path`. A missing
/// file and a key with no persisted snapshot both return `Ok(None)` rather
/// than an error: neither means anything went wrong, only that there is
/// nothing to restore yet.
///
/// # Errors
/// Returns [`TabSessionStoreError`] if the file exists but cannot be read or
/// parsed.
#[tracing::instrument(name = "tab_session_load", skip(path), fields(key = ?key))]
pub fn load_snapshot(
    path: &Path,
    key: &ConnectionKey,
) -> Result<Option<TabSessionSnapshot>, TabSessionStoreError> {
    let _guard = SAVE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    if !path.exists() {
        tracing::debug!("no tab session store file yet");
        return Ok(None);
    }
    let text = fs::read_to_string(path).map_err(TabSessionStoreError::Read)?;
    let file: TabSessionStoreFile = serde_json::from_str(&text)?;
    let snapshot = file.connections.get(&key.storage_key()).cloned();
    tracing::info!(found = snapshot.is_some(), "tab session load");
    Ok(snapshot)
}

/// Persist `snapshot` under `key` into the tab-session store at `path`,
/// read-modify-write against whatever the file already holds for other
/// connections so saving one connection's tabs never disturbs another's.
/// Creates `path`'s parent directory if needed.
///
/// Intended to run off the render/update path: the caller is responsible
/// for dispatching this onto a background executor rather than calling it
/// directly from a render or event handler (see
/// `ui::workspace::WorkspaceView`, the only caller in this codebase).
///
/// # Errors
/// Returns [`TabSessionStoreError`] if the file cannot be read (when it
/// already exists), parsed, serialized, or written.
#[tracing::instrument(
    name = "tab_session_save",
    skip(path, snapshot),
    fields(key = ?key, tab_count = snapshot.tabs.len())
)]
pub fn save_snapshot(
    path: &Path,
    key: &ConnectionKey,
    snapshot: &TabSessionSnapshot,
) -> Result<(), TabSessionStoreError> {
    let _guard = SAVE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let mut file = if path.exists() {
        let text = fs::read_to_string(path).map_err(TabSessionStoreError::Read)?;
        serde_json::from_str(&text)?
    } else {
        TabSessionStoreFile::default()
    };
    file.connections.insert(key.storage_key(), snapshot.clone());

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(TabSessionStoreError::Write)?;
    }
    let text = serde_json::to_string_pretty(&file).map_err(TabSessionStoreError::Serde)?;

    let tmp_path = tmp_path_for(path);
    fs::write(&tmp_path, text).map_err(TabSessionStoreError::Write)?;
    fs::rename(&tmp_path, path).map_err(TabSessionStoreError::Write)?;

    tracing::info!("tab session saved");
    Ok(())
}

/// The temp file [`save_snapshot`] writes to before renaming it over `path`,
/// alongside `path` so the rename stays on the same filesystem (a cross-
/// filesystem rename is not atomic).
fn tmp_path_for(path: &Path) -> PathBuf {
    let mut file_name = path.file_name().map_or_else(
        || std::ffi::OsString::from("tab_sessions"),
        std::ffi::OsStr::to_os_string,
    );
    file_name.push(TAB_SESSION_TMP_SUFFIX);
    path.with_file_name(file_name)
}

/// A workspace's tab-session persistence state: where the store lives on
/// disk, which key the currently displayed tabs belong to, the active
/// connection this store last saw (so a caller can tell an actual switch
/// apart from an unrelated notification), the per-key cache of the latest
/// dispatched-for-save snapshot, and the one-shot suppression flag that keeps
/// a freshly loaded snapshot from immediately triggering its own redundant
/// save.
///
/// Gpui-free: no window, entity, or task type appears anywhere in its API, so
/// every save/load decision here is unit-testable without a running app. The
/// caller (`ui::workspace::WorkspaceView`) owns the gpui bridging: reading
/// `tabs` for a snapshot, applying a resolved snapshot back onto `tabs`, and
/// spawning the actual disk write onto a background executor.
pub struct TabSessionStore {
    path: Option<PathBuf>,
    active_key: Option<ConnectionKey>,
    last_active_connection: Option<ActiveConnection>,
    suppress_next_save: bool,
    session_cache: HashMap<ConnectionKey, TabSessionSnapshot>,
}

impl TabSessionStore {
    /// A store rooted at `path`, or one that persists nothing if `path` is
    /// `None` (e.g. no config directory could be resolved at startup).
    #[must_use]
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            active_key: None,
            last_active_connection: None,
            suppress_next_save: false,
            session_cache: HashMap::new(),
        }
    }

    /// Whether `new_active` differs from the connection this store last saw
    /// as active, i.e. whether a caller observing a change on the connection
    /// manager should treat it as an actual switch rather than an unrelated
    /// notification (e.g. the connection modal opening or closing).
    #[must_use]
    pub fn active_connection_changed(&self, new_active: Option<&ActiveConnection>) -> bool {
        new_active != self.last_active_connection.as_ref()
    }

    /// Record `new_key`/`new_active` as the store's current connection and
    /// resolve which snapshot should replace whatever tabs are currently
    /// open: this session's own cached snapshot for `new_key` if one has
    /// already been dispatched for save, else a synchronous on-disk read,
    /// else `None` for the caller to fall back to a default tab set.
    ///
    /// A key this session has already dispatched a save for always wins over
    /// disk: [`Self::dispatch_save`] records it here synchronously, before
    /// handing the actual write to a background executor, so this in-memory
    /// copy can never be older than whatever is (or, for a write still in
    /// flight, will shortly be) on disk for that key. Consulting disk
    /// instead in that case could observe the pre-write file if the
    /// background write has not run yet -- e.g. a quick switch back to a
    /// connection right after switching away from it -- and wrongly show its
    /// default tabs.
    ///
    /// Always arms the suppression flag (see [`Self::take_suppressed`]) so
    /// the reload this resolved snapshot feeds into does not trigger its own
    /// save.
    pub fn begin_switch(
        &mut self,
        new_key: Option<ConnectionKey>,
        new_active: Option<ActiveConnection>,
    ) -> Option<TabSessionSnapshot> {
        self.last_active_connection = new_active;

        let snapshot = match new_key.as_ref() {
            Some(key) if self.session_cache.contains_key(key) => {
                self.session_cache.get(key).cloned()
            }
            // Read synchronously rather than via a background executor: this
            // runs before the tab strip for the newly active connection is
            // ever shown, and the store is a small, local JSON file, so the
            // blocking read finishes long before it would be worth the
            // complexity of loading in the background and re-applying the
            // result once it completed a frame or more later (which would
            // show the wrong tabs briefly).
            Some(key) => match &self.path {
                Some(path) => match load_snapshot(path, key) {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            "failed to load tab session; falling back to the default tab"
                        );
                        None
                    }
                },
                None => None,
            },
            None => None,
        };

        self.active_key = new_key;
        self.suppress_next_save = true;
        snapshot
    }

    /// Consume the suppression flag armed by [`Self::begin_switch`],
    /// resetting it to `false`. Returns `true` exactly once per switch, for
    /// the very next save-triggering call the caller makes after a reload --
    /// letting that call skip a save that would otherwise race the outgoing
    /// connection's own save for no benefit, since the freshly loaded tabs
    /// already match what is (or is not) on disk for that key.
    pub fn take_suppressed(&mut self) -> bool {
        std::mem::take(&mut self.suppress_next_save)
    }

    /// Whether [`Self::dispatch_save`] could actually persist anything right
    /// now, i.e. both a tracked active key and a resolved store path exist.
    /// Lets a caller skip building an expensive snapshot (e.g. cloning every
    /// open tab's buffer text) when the result would just be discarded.
    #[must_use]
    pub fn can_persist(&self) -> bool {
        self.active_key.is_some() && self.path.is_some()
    }

    /// Record `snapshot` as the active key's latest known state (consulted
    /// first by a future [`Self::begin_switch`] back to this key) and hand
    /// back what the caller needs to actually write it to disk: the store
    /// path, the key, and the snapshot itself. `None` when there is no
    /// active key or no resolved path -- nothing meaningful to persist.
    ///
    /// The cache is updated synchronously, before the caller has written
    /// anything to disk, so a switch back to this key sees this session's
    /// own latest tabs even while the actual write is still in flight on a
    /// background executor.
    pub fn dispatch_save(
        &mut self,
        snapshot: TabSessionSnapshot,
    ) -> Option<(PathBuf, ConnectionKey, TabSessionSnapshot)> {
        let key = self.active_key.clone()?;
        let path = self.path.clone()?;
        tracing::info!(
            key = ?key,
            tab_count = snapshot.tabs.len(),
            "dispatching tab session save"
        );
        self.session_cache.insert(key.clone(), snapshot.clone());
        Some((path, key, snapshot))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConnectionKey, TabEntryKind, TabEntrySnapshot, TabSessionSnapshot, TabSessionStoreError,
        load_snapshot, save_snapshot,
    };

    /// A temp file path this test owns exclusively, removed on drop so
    /// tests never leak files into the real temp dir.
    struct TempStorePath(std::path::PathBuf);

    impl TempStorePath {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "zsql-tab-sessions-test-{label}-{}-{n}.json",
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

    fn sample_snapshot() -> TabSessionSnapshot {
        use zsql_core::preview_state::PreviewQueryState;

        TabSessionSnapshot {
            tabs: vec![
                TabEntrySnapshot {
                    kind: TabEntryKind::Generated {
                        schema: "public".to_owned(),
                        relation: "orders".to_owned(),
                        preview_state: PreviewQueryState::new(200),
                    },
                    title: "orders".to_owned(),
                },
                TabEntrySnapshot {
                    kind: TabEntryKind::Script {
                        buffer_text: "select 1;\nselect 2;\n".to_owned(),
                    },
                    title: "query-1.sql".to_owned(),
                },
            ],
            active_index: Some(1),
        }
    }

    #[test]
    fn a_snapshot_with_a_generated_and_a_script_tab_round_trips_through_json() {
        let snapshot = sample_snapshot();
        let text = serde_json::to_string(&snapshot).expect("must serialize");
        let parsed: TabSessionSnapshot = serde_json::from_str(&text).expect("must parse back");
        assert_eq!(parsed, snapshot);
    }

    #[test]
    fn saving_then_loading_from_a_temp_directory_reproduces_the_snapshot_exactly() {
        let temp = TempStorePath::new("round-trip");
        let key = ConnectionKey::Saved("local pg".to_owned());
        let snapshot = sample_snapshot();

        save_snapshot(&temp.0, &key, &snapshot).expect("save must succeed");
        let loaded = load_snapshot(&temp.0, &key).expect("load must succeed");

        assert_eq!(loaded, Some(snapshot));
    }

    #[test]
    fn two_distinct_connection_keys_are_persisted_independently() {
        let temp = TempStorePath::new("two-keys");
        let key_a = ConnectionKey::Saved("connection a".to_owned());
        let key_b = ConnectionKey::Saved("connection b".to_owned());
        let snapshot_a = sample_snapshot();
        let snapshot_b = TabSessionSnapshot {
            tabs: vec![TabEntrySnapshot {
                kind: TabEntryKind::Script {
                    buffer_text: "select 'b';".to_owned(),
                },
                title: "query-1.sql".to_owned(),
            }],
            active_index: Some(0),
        };

        save_snapshot(&temp.0, &key_a, &snapshot_a).expect("save a must succeed");
        save_snapshot(&temp.0, &key_b, &snapshot_b).expect("save b must succeed");

        assert_eq!(
            load_snapshot(&temp.0, &key_b).expect("load b must succeed"),
            Some(snapshot_b),
            "loading b must not return a's tabs"
        );
        assert_eq!(
            load_snapshot(&temp.0, &key_a).expect("load a must succeed"),
            Some(snapshot_a),
            "a must still be independently loadable after b was saved"
        );
    }

    #[test]
    fn loading_a_key_with_no_prior_snapshot_returns_none_not_an_error() {
        let temp = TempStorePath::new("missing-key");
        let saved_key = ConnectionKey::Saved("has a snapshot".to_owned());
        save_snapshot(&temp.0, &saved_key, &sample_snapshot()).expect("save must succeed");

        let missing_key = ConnectionKey::Saved("never saved".to_owned());
        let result = load_snapshot(&temp.0, &missing_key).expect("load must succeed");

        assert_eq!(result, None);
    }

    #[test]
    fn loading_from_a_nonexistent_file_returns_none_not_an_error() {
        let temp = TempStorePath::new("missing-file");
        let result =
            load_snapshot(&temp.0, &ConnectionKey::Unsaved).expect("a missing file must not error");
        assert_eq!(result, None);
    }

    #[test]
    fn the_unsaved_and_a_saved_key_never_collide() {
        let temp = TempStorePath::new("unsaved-vs-saved");
        let unsaved_snapshot = TabSessionSnapshot {
            tabs: vec![TabEntrySnapshot {
                kind: TabEntryKind::Script {
                    buffer_text: "select 'fallback';".to_owned(),
                },
                title: "query-1.sql".to_owned(),
            }],
            active_index: Some(0),
        };
        save_snapshot(&temp.0, &ConnectionKey::Unsaved, &unsaved_snapshot)
            .expect("save must succeed");
        save_snapshot(
            &temp.0,
            &ConnectionKey::Saved("unsaved".to_owned()),
            &sample_snapshot(),
        )
        .expect("save must succeed");

        assert_eq!(
            load_snapshot(&temp.0, &ConnectionKey::Unsaved).expect("load must succeed"),
            Some(unsaved_snapshot),
            "a saved connection literally named 'unsaved' must not shadow the sentinel key"
        );
    }

    /// A connection's real URL (e.g. a saved connection's URL, which can
    /// embed a plaintext password) is never handed to this module at all --
    /// only its stable display name is. This proves the persisted bytes
    /// never contain such a URL or credential even for a connection whose
    /// name suggests a production, secret-bearing database.
    #[test]
    fn persisted_bytes_never_contain_a_connection_url_or_secret() {
        let temp = TempStorePath::new("secrets");
        let key = ConnectionKey::Saved("prod db".to_owned());
        save_snapshot(&temp.0, &key, &sample_snapshot()).expect("save must succeed");

        let bytes = std::fs::read_to_string(&temp.0).expect("must read back");
        let secret_url = "postgres://admin:hunter2@prod-db.internal:5432/app";
        assert!(!bytes.contains(secret_url));
        assert!(!bytes.contains("hunter2"));
        assert!(!bytes.contains("postgres://"));
    }

    #[test]
    fn loading_a_store_file_with_invalid_json_returns_a_serde_error_not_a_panic() {
        let temp = TempStorePath::new("corrupt-load");
        std::fs::write(&temp.0, b"not valid json { at all").expect("must write garbage");

        let result = load_snapshot(&temp.0, &ConnectionKey::Saved("anything".to_owned()));

        assert!(
            matches!(result, Err(TabSessionStoreError::Serde(_))),
            "a corrupt store file must surface as a Serde error, got {result:?}"
        );
    }

    /// A store file carrying a top-level `buffer_text` on every entry and an
    /// `edited` flag on `Generated` -- an on-disk shape this build does not
    /// accept -- is not specially migrated: it fails to parse against the
    /// current shape and surfaces the same way any other corrupt file does,
    /// letting the existing fallback-to-default handling take over.
    #[test]
    fn loading_an_old_format_store_file_surfaces_as_a_serde_error() {
        let temp = TempStorePath::new("old-format");
        let old_format = r#"{
            "connections": {
                "saved:conn-a": {
                    "tabs": [
                        {
                            "kind": "Script",
                            "title": "query-1.sql",
                            "buffer_text": "select 1;"
                        }
                    ],
                    "active_index": 0
                }
            }
        }"#;
        std::fs::write(&temp.0, old_format).expect("must write the old-format fixture");

        let result = load_snapshot(&temp.0, &ConnectionKey::Saved("conn-a".to_owned()));

        assert!(
            matches!(result, Err(TabSessionStoreError::Serde(_))),
            "an old-format store file must surface as a Serde error, got {result:?}"
        );
    }

    #[test]
    fn saving_over_a_corrupt_existing_file_errors_instead_of_silently_dropping_it() {
        let temp = TempStorePath::new("corrupt-save");
        std::fs::write(&temp.0, b"not valid json { at all").expect("must write garbage");

        let result = save_snapshot(
            &temp.0,
            &ConnectionKey::Saved("connection a".to_owned()),
            &sample_snapshot(),
        );

        assert!(
            matches!(result, Err(TabSessionStoreError::Serde(_))),
            "saving over a corrupt file must error rather than overwrite it, got {result:?}"
        );
        let bytes_after = std::fs::read_to_string(&temp.0).expect("must read back");
        assert_eq!(
            bytes_after, "not valid json { at all",
            "a failed save must leave the corrupt file untouched, not partially written"
        );
    }
}

#[cfg(test)]
mod store_tests {
    use super::{
        ConnectionKey, TabEntryKind, TabEntrySnapshot, TabSessionSnapshot, TabSessionStore,
        save_snapshot,
    };
    use crate::ui::connections::ActiveConnection;

    /// A temp file path this test owns exclusively, removed on drop so
    /// tests never leak files into the real temp dir.
    struct TempStorePath(std::path::PathBuf);

    impl TempStorePath {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "zsql-tab-session-store-test-{label}-{}-{n}.json",
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

    fn snapshot_with_tab_count(count: usize) -> TabSessionSnapshot {
        TabSessionSnapshot {
            tabs: (0..count)
                .map(|i| TabEntrySnapshot {
                    kind: TabEntryKind::Script {
                        buffer_text: format!("select {i};"),
                    },
                    title: format!("query-{i}.sql"),
                })
                .collect(),
            active_index: if count == 0 { None } else { Some(0) },
        }
    }

    fn active_connection(name: &str) -> ActiveConnection {
        ActiveConnection {
            id: None,
            name: name.to_owned(),
            url: format!("postgres://localhost/{name}"),
        }
    }

    #[test]
    fn dispatch_save_populates_the_cache_synchronously_before_any_write_completes() {
        let temp = TempStorePath::new("dispatch-cache");
        let mut store = TabSessionStore::new(Some(temp.0.clone()));
        let key = ConnectionKey::Saved("conn-a".to_owned());
        store.begin_switch(Some(key.clone()), Some(active_connection("conn-a")));

        let snapshot = snapshot_with_tab_count(2);
        let dispatched = store
            .dispatch_save(snapshot.clone())
            .expect("an active key and path must dispatch a save");

        assert_eq!(dispatched.0, temp.0);
        assert_eq!(dispatched.1, key);
        assert_eq!(dispatched.2, snapshot);
        assert_eq!(
            store.session_cache.get(&key),
            Some(&snapshot),
            "the cache must hold the dispatched snapshot even though no disk write ever ran"
        );
    }

    #[test]
    fn a_cached_dispatched_save_wins_over_a_stale_on_disk_snapshot_when_switching_back() {
        let temp = TempStorePath::new("cache-wins");
        let mut store = TabSessionStore::new(Some(temp.0.clone()));
        let key_a = ConnectionKey::Saved("conn-a".to_owned());

        store.begin_switch(Some(key_a.clone()), Some(active_connection("conn-a")));
        let fresh = snapshot_with_tab_count(3);
        store
            .dispatch_save(fresh.clone())
            .expect("dispatch must succeed with a path and active key set");

        // Disk disagrees with what this session already knows for "conn-a"
        // -- standing in for a background write dispatched earlier that has
        // not actually landed yet.
        let stale = TabSessionSnapshot::default();
        save_snapshot(&temp.0, &key_a, &stale).expect("seeding a stale snapshot must succeed");

        // Switch away, then back.
        let key_b = ConnectionKey::Saved("conn-b".to_owned());
        store.begin_switch(Some(key_b), Some(active_connection("conn-b")));
        let resolved = store.begin_switch(Some(key_a), Some(active_connection("conn-a")));

        assert_eq!(
            resolved,
            Some(fresh),
            "the cached snapshot must win over the stale on-disk copy"
        );
    }

    #[test]
    fn suppress_next_save_is_armed_by_begin_switch_and_consumed_exactly_once() {
        let mut store = TabSessionStore::new(None);
        let key = ConnectionKey::Saved("conn-a".to_owned());

        store.begin_switch(Some(key), Some(active_connection("conn-a")));
        assert!(
            store.suppress_next_save,
            "begin_switch must arm the suppression flag before the caller reloads tabs"
        );

        assert!(
            store.take_suppressed(),
            "the first save-triggering call after a switch must be suppressed"
        );
        assert!(
            !store.take_suppressed(),
            "a second call must not still be suppressed"
        );
    }

    #[test]
    fn switching_to_a_never_seen_key_with_no_cache_and_no_disk_snapshot_falls_back_to_none() {
        let temp = TempStorePath::new("never-seen");
        let mut store = TabSessionStore::new(Some(temp.0.clone()));
        let key = ConnectionKey::Saved("brand new connection".to_owned());

        let resolved =
            store.begin_switch(Some(key), Some(active_connection("brand new connection")));

        assert_eq!(
            resolved, None,
            "a key with no cache entry and nothing on disk must fall back to None"
        );
    }

    #[test]
    fn switching_to_a_key_whose_store_file_is_corrupt_falls_back_to_none_without_panicking() {
        let temp = TempStorePath::new("corrupt-switch");
        std::fs::write(&temp.0, b"not valid json { at all").expect("must write garbage");
        let mut store = TabSessionStore::new(Some(temp.0.clone()));
        let key = ConnectionKey::Saved("conn-a".to_owned());

        let resolved = store.begin_switch(Some(key), Some(active_connection("conn-a")));

        assert_eq!(
            resolved, None,
            "a corrupt store file must fall back to None rather than propagate the load error"
        );
    }

    #[test]
    fn switching_with_no_tab_sessions_path_never_touches_disk_and_falls_back_to_none() {
        let mut store = TabSessionStore::new(None);
        let key = ConnectionKey::Saved("conn-a".to_owned());

        let resolved = store.begin_switch(Some(key), Some(active_connection("conn-a")));

        assert_eq!(resolved, None);
    }

    #[test]
    fn dispatch_save_is_a_no_op_when_no_key_has_ever_been_tracked() {
        let temp = TempStorePath::new("no-active-key");
        let mut store = TabSessionStore::new(Some(temp.0.clone()));

        assert!(store.dispatch_save(snapshot_with_tab_count(1)).is_none());
    }

    #[test]
    fn dispatch_save_is_a_no_op_when_there_is_no_tab_sessions_path() {
        let mut store = TabSessionStore::new(None);
        let key = ConnectionKey::Saved("conn-a".to_owned());
        store.begin_switch(Some(key), Some(active_connection("conn-a")));

        assert!(store.dispatch_save(snapshot_with_tab_count(1)).is_none());
    }

    #[test]
    fn active_connection_changed_detects_a_switch_and_ignores_a_repeat() {
        let mut store = TabSessionStore::new(None);
        let conn_a = active_connection("conn-a");

        assert!(
            store.active_connection_changed(Some(&conn_a)),
            "no connection was ever tracked, so any Some(..) must count as a change"
        );

        store.begin_switch(
            Some(ConnectionKey::Saved("conn-a".to_owned())),
            Some(conn_a.clone()),
        );

        assert!(
            !store.active_connection_changed(Some(&conn_a)),
            "the same connection must not be reported as a change"
        );
        assert!(
            store.active_connection_changed(None),
            "disconnecting must be reported as a change"
        );
    }
}
