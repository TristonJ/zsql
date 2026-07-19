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
/// without any of its gpui state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabEntryKind {
    /// A normal, freely-editable script buffer.
    Script,
    /// Auto-generated preview SQL for `schema.relation`. `edited` is `true`
    /// once the buffer received a manual edit -- such an entry restores as
    /// [`TabEntryKind::Script`] instead, consistent with the live
    /// generated-to-script conversion `ui::tabs::TabModel` performs on first
    /// edit.
    Generated {
        schema: String,
        relation: String,
        edited: bool,
    },
}

/// One persisted tab: its kind, display title, and full buffer text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabEntrySnapshot {
    pub kind: TabEntryKind,
    pub title: String,
    pub buffer_text: String,
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
        TabSessionSnapshot {
            tabs: vec![
                TabEntrySnapshot {
                    kind: TabEntryKind::Generated {
                        schema: "public".to_owned(),
                        relation: "orders".to_owned(),
                        edited: false,
                    },
                    title: "orders".to_owned(),
                    buffer_text: "SELECT * FROM \"public\".\"orders\" LIMIT 200".to_owned(),
                },
                TabEntrySnapshot {
                    kind: TabEntryKind::Script,
                    title: "query-1.sql".to_owned(),
                    buffer_text: "select 1;\nselect 2;\n".to_owned(),
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
                kind: TabEntryKind::Script,
                title: "query-1.sql".to_owned(),
                buffer_text: "select 'b';".to_owned(),
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
                kind: TabEntryKind::Script,
                title: "query-1.sql".to_owned(),
                buffer_text: "select 'fallback';".to_owned(),
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
