//! One-time migration from the legacy, monolithic `tab_sessions.json` store
//! (keyed by a connection's display name) into the per-connection session
//! directories under [`crate::config::Config::sessions_dir`] (keyed by
//! uuid). See the module-level rationale on [`super::ConnectionKey`].

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use zsql_core::preview_state::PreviewQueryState;

use super::backing::{LibraryName, ScriptFileName};
use super::disk::{is_legacy_unnamed_script_file_name, script_file_name};
use super::session_dir::TABS_FILE_NAME;
use super::{
    ConnectionKey, ScriptBacking, SessionDir, TabEntrySnapshot, TabKind, TabSessionSnapshot,
};
use crate::connections::StoredConnection;

/// Prefix a legacy entry's on-disk key carried for a saved connection,
/// ahead of its display name.
const LEGACY_SAVED_KEY_PREFIX: &str = "saved:";
/// Legacy on-disk key for the unsaved-connection sentinel.
const LEGACY_UNSAVED_KEY: &str = "unsaved";
/// Suffix [`migrate_legacy_sessions`] renames the legacy store file to once
/// every entry it could resolve has been written to its own session
/// directory, so a later startup finding no file at the original path never
/// re-runs the migration.
const LEGACY_STORE_BACKUP_SUFFIX: &str = ".bak";

/// The legacy store file's on-disk shape: every connection's snapshot,
/// keyed by a `saved:<name>`/`unsaved` string.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LegacyStoreFile {
    connections: HashMap<String, LegacyTabSessionSnapshot>,
}

/// The legacy JSON shape of one connection's tab session, predating the
/// `tabs.toml` wire format entirely -- kept only for
/// [`migrate_legacy_sessions`] to deserialize an old `tab_sessions.json`
/// against, then converted into the current domain shape.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LegacyTabSessionSnapshot {
    tabs: Vec<LegacyTabEntrySnapshot>,
    active_index: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct LegacyTabEntrySnapshot {
    kind: LegacyTabEntryKind,
    title: String,
}

/// The legacy externally-tagged shape (`{"Script": {...}}`) every tab kind
/// serialized as before `ScriptBacking` carried identity.
#[derive(Debug, Deserialize)]
enum LegacyTabEntryKind {
    Script {
        buffer_text: String,
        #[serde(default)]
        unnamed: bool,
        #[serde(default)]
        file_name: String,
    },
    Library {
        library_name: String,
        draft_text: Option<String>,
    },
    External {
        path: PathBuf,
        draft_text: Option<String>,
    },
    Generated {
        schema: String,
        relation: String,
        preview_state: PreviewQueryState,
    },
}

/// Convert one legacy entry into the current domain shape, deriving a
/// session file name from `title` when the legacy entry predates
/// [`LegacyTabEntryKind::Script::file_name`] entirely (always empty in that
/// case). Returns `None` for an entry whose identity cannot be resolved into
/// a valid [`ScriptFileName`]/[`LibraryName`] even after that fallback --
/// logged and skipped by the caller, the same as any other unreadable entry.
fn into_domain_entry(legacy: LegacyTabEntrySnapshot) -> Option<TabEntrySnapshot> {
    let LegacyTabEntrySnapshot { kind, title } = legacy;
    let (kind, buffer_text) = match kind {
        LegacyTabEntryKind::Script {
            buffer_text,
            unnamed,
            file_name,
        } => {
            let resolved_name = if file_name.is_empty() {
                script_file_name(&title)
            } else {
                file_name
            };
            let file = ScriptFileName::new(resolved_name).ok()?;
            let backing = if unnamed {
                ScriptBacking::SessionScratch { file }
            } else {
                ScriptBacking::SessionNamed { file }
            };
            (TabKind::Script { backing }, Some(buffer_text))
        }
        LegacyTabEntryKind::Library {
            library_name,
            draft_text,
        } => {
            let name = LibraryName::new(library_name).ok()?;
            (
                TabKind::Script {
                    backing: ScriptBacking::Library {
                        name,
                        saved_text: None,
                    },
                },
                draft_text,
            )
        }
        LegacyTabEntryKind::External { path, draft_text } => (
            TabKind::Script {
                backing: ScriptBacking::External {
                    path,
                    saved_text: None,
                },
            },
            draft_text,
        ),
        LegacyTabEntryKind::Generated {
            schema,
            relation,
            preview_state,
        } => (
            TabKind::Generated {
                schema,
                relation,
                preview: preview_state,
            },
            None,
        ),
    };
    Some(TabEntrySnapshot {
        title,
        kind,
        buffer_text,
    })
}

fn into_domain_snapshot(legacy: LegacyTabSessionSnapshot) -> TabSessionSnapshot {
    let mut tabs = Vec::with_capacity(legacy.tabs.len());
    let mut surviving_original_indices = Vec::with_capacity(legacy.tabs.len());
    for (original_index, entry) in legacy.tabs.into_iter().enumerate() {
        if let Some(entry) = into_domain_entry(entry) {
            tabs.push(entry);
            surviving_original_indices.push(original_index);
        } else {
            tracing::warn!(original_index, "skipping an unreadable legacy tab entry");
        }
    }
    let active_index = legacy.active_index.and_then(|active| {
        surviving_original_indices
            .iter()
            .position(|&original| original == active)
    });
    TabSessionSnapshot { tabs, active_index }
}

/// Whether `target_key`'s session directory under `sessions_root` is
/// already fully migrated: it must both exist and contain
/// [`TABS_FILE_NAME`], the file every session directory carries once fully
/// saved at least once. Checking for this file (not mere directory
/// existence) is what decides "already migrated": a directory that exists
/// but lacks it is the on-disk signature of a save interrupted mid-write (a
/// crash between creating the directory and writing this file), and must be
/// treated as unmigrated so this run retries it, never as already-complete.
fn already_migrated(sessions_root: &Path, target_key: &ConnectionKey) -> bool {
    sessions_root
        .join(target_key.storage_dir_name())
        .join(TABS_FILE_NAME)
        .is_file()
}

/// Migrate `legacy_path` (the pre-migration `tab_sessions.json`, typically
/// [`crate::config::Config::tab_sessions_path`]) into `sessions_root`
/// (typically [`crate::config::Config::sessions_dir`]): one session
/// directory per legacy entry, keyed by the connection's uuid rather than
/// its display name -- resolved against `connections` (typically
/// [`crate::connections::ConnectionStore::connections`]).
///
/// A legacy entry for a saved connection whose name matches none of
/// `connections` is skipped (not migrated, not treated as a failure). A
/// missing `legacy_path` is a no-op -- there is nothing to migrate, whether
/// because this is a fresh install or because a previous run already
/// completed (see below).
///
/// Idempotent and never destructive on a re-run: an entry whose target
/// session directory is already [`already_migrated`] is skipped, so if a
/// previous run wrote some directories but failed to rename `legacy_path`
/// away (leaving this to run again), sessions the user may have edited in
/// the meantime are never overwritten with the stale legacy snapshot.
///
/// `legacy_path` is renamed to itself plus [`LEGACY_STORE_BACKUP_SUFFIX`]
/// only once *every* entry this run attempted succeeded (see
/// [`resolve_legacy_key`]-skipped entries, which do not count as attempts):
/// a failed entry is left for the next startup to retry against the
/// still-present legacy file, rather than being silently dropped forever.
/// The rename itself never clobbers a pre-existing backup from an earlier,
/// already-completed migration.
#[tracing::instrument(name = "session_store_migration", skip(connections))]
pub fn migrate_legacy_sessions(
    legacy_path: &Path,
    sessions_root: &Path,
    connections: &[StoredConnection],
) {
    if !legacy_path.exists() {
        tracing::debug!("no legacy tab session store to migrate");
        return;
    }

    let text = match fs::read_to_string(legacy_path) {
        Ok(text) => text,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "failed to read the legacy tab session store; leaving it in place"
            );
            return;
        }
    };
    let mut legacy: LegacyStoreFile = match serde_json::from_str(&text) {
        Ok(legacy) => legacy,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "failed to parse the legacy tab session store; leaving it in place"
            );
            return;
        }
    };
    for snapshot in legacy.connections.values_mut() {
        mark_legacy_unnamed_scripts(snapshot);
    }

    tracing::info!(entry_count = legacy.connections.len(), "migration started");

    let mut any_failure = false;
    for (legacy_key, legacy_snapshot) in legacy.connections {
        let Some(target_key) = resolve_legacy_key(&legacy_key, connections) else {
            tracing::warn!(
                key = %legacy_key,
                "skipping legacy session entry: no saved connection matches this name"
            );
            continue;
        };
        if already_migrated(sessions_root, &target_key) {
            tracing::info!(
                key = %legacy_key,
                "skipping legacy session entry: it is already fully migrated \
                 (a previous, possibly interrupted, migration run wrote it)"
            );
            continue;
        }
        let snapshot = into_domain_snapshot(legacy_snapshot);
        if let Err(err) = SessionDir::new(sessions_root, target_key).save_snapshot(&snapshot) {
            any_failure = true;
            tracing::warn!(
                key = %legacy_key,
                error = %err,
                "failed to migrate a legacy session entry; it will be retried on next startup"
            );
        }
    }

    if any_failure {
        tracing::warn!(
            "leaving the legacy tab session store in place: at least one entry failed to \
             migrate and must be retried on next startup"
        );
        return;
    }

    let backup_path = backup_path_for(legacy_path);
    if backup_path.exists() {
        tracing::warn!(
            backup = %backup_path.display(),
            "a backup already exists at this path; leaving the legacy store in place \
             rather than overwriting it"
        );
        return;
    }
    match fs::rename(legacy_path, &backup_path) {
        Ok(()) => tracing::info!(backup = %backup_path.display(), "migration completed"),
        Err(err) => tracing::warn!(
            error = %err,
            "migrated sessions but failed to back up the legacy tab session store"
        ),
    }
}

/// Mark every `Script` entry in `snapshot` whose title matches the bare
/// top-level shape a legacy `tab_sessions.json` gives an unnamed script
/// (`query-<digits>.sql`) as unnamed. The legacy JSON format predates
/// [`LegacyTabEntryKind::Script`]'s `unnamed` field entirely, so every entry
/// deserializes with it defaulting to `false` regardless of where its
/// script actually lived -- this is the one place that title pattern is
/// consulted to recover the fact a plain file-location check can no longer
/// make for this particular legacy shape, since the legacy JSON never had a
/// `scratch/` subdirectory to check against in the first place.
fn mark_legacy_unnamed_scripts(snapshot: &mut LegacyTabSessionSnapshot) {
    for tab in &mut snapshot.tabs {
        if let LegacyTabEntryKind::Script { unnamed, .. } = &mut tab.kind
            && is_legacy_unnamed_script_file_name(&tab.title)
        {
            *unnamed = true;
        }
    }
}

/// Resolve a legacy on-disk key (`saved:<name>` or `unsaved`) to the
/// [`ConnectionKey`] its tabs must migrate under. `None` for a `saved:<name>`
/// entry whose name matches none of `connections`. If more than one saved
/// connection shares `name`, the first match (in `connections`' own order)
/// wins and a warning is logged -- ambiguous, but never a failure, since a
/// user free to name two connections identically should still get a
/// migrated session for one of them rather than neither.
fn resolve_legacy_key(legacy_key: &str, connections: &[StoredConnection]) -> Option<ConnectionKey> {
    if legacy_key == LEGACY_UNSAVED_KEY {
        return Some(ConnectionKey::Unsaved);
    }
    let name = legacy_key.strip_prefix(LEGACY_SAVED_KEY_PREFIX)?;
    let matches: Vec<&StoredConnection> = connections
        .iter()
        .filter(|connection| connection.name == name)
        .collect();
    if matches.len() > 1 {
        tracing::warn!(
            name,
            match_count = matches.len(),
            "multiple saved connections share this display name; \
             migrating this legacy entry to the first match"
        );
    }
    matches
        .first()
        .map(|connection| ConnectionKey::Saved(connection.id))
}

/// `legacy_path` with [`LEGACY_STORE_BACKUP_SUFFIX`] appended, alongside
/// `legacy_path` so the rename stays on the same filesystem.
fn backup_path_for(legacy_path: &Path) -> PathBuf {
    let mut file_name = legacy_path.file_name().map_or_else(
        || std::ffi::OsString::from("tab_sessions.json"),
        OsStr::to_os_string,
    );
    file_name.push(LEGACY_STORE_BACKUP_SUFFIX);
    legacy_path.with_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use super::{migrate_legacy_sessions, resolve_legacy_key};
    use crate::connections::StoredConnection;
    use crate::session_store::{
        ConnectionKey, ScriptBacking, ScriptFileName, SessionDir, TabEntrySnapshot, TabKind,
        TabSessionSnapshot,
    };

    /// A temp directory this test owns exclusively (a legacy file path plus
    /// a sessions root beside it), removed on drop so tests never leak
    /// files into the real temp dir.
    struct TempMigrationPaths {
        legacy: std::path::PathBuf,
        sessions: std::path::PathBuf,
    }

    impl TempMigrationPaths {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let base = std::env::temp_dir().join(format!(
                "zsql-session-migration-test-{label}-{}-{n}",
                std::process::id()
            ));
            Self {
                legacy: base.join("tab_sessions.json"),
                sessions: base.join("sessions"),
            }
        }

        fn backup(&self) -> std::path::PathBuf {
            let mut name = self
                .legacy
                .file_name()
                .expect("legacy path has a name")
                .to_owned();
            name.push(".bak");
            self.legacy.with_file_name(name)
        }
    }

    impl Drop for TempMigrationPaths {
        fn drop(&mut self) {
            let _ =
                std::fs::remove_dir_all(self.legacy.parent().expect("legacy path has a parent"));
        }
    }

    fn sample_stored_connection(name: &str) -> StoredConnection {
        StoredConnection {
            id: uuid::Uuid::new_v4(),
            name: name.to_owned(),
            display_kind: "postgres".to_owned(),
            display_host: "localhost".to_owned(),
            ssh: None,
        }
    }

    fn write_legacy_file(path: &std::path::Path, json: &str) {
        std::fs::create_dir_all(path.parent().expect("legacy path has a parent"))
            .expect("must create legacy parent dir");
        std::fs::write(path, json).expect("must write legacy fixture");
    }

    fn legacy_fixture_json() -> String {
        r#"{
            "connections": {
                "saved:conn-a": {
                    "tabs": [
                        {"kind": {"Script": {"buffer_text": "select 1;"}}, "title": "query-1.sql"}
                    ],
                    "active_index": 0
                },
                "unsaved": {
                    "tabs": [
                        {"kind": {"Script": {"buffer_text": "select 2;"}}, "title": "query-1.sql"}
                    ],
                    "active_index": 0
                },
                "saved:ghost-connection": {
                    "tabs": [
                        {"kind": {"Script": {"buffer_text": "select 3;"}}, "title": "query-1.sql"}
                    ],
                    "active_index": 0
                }
            }
        }"#
        .to_owned()
    }

    #[test]
    fn resolve_legacy_key_maps_unsaved_and_a_matching_saved_name_to_their_targets() {
        let conn = sample_stored_connection("conn-a");
        let id = conn.id;
        let connections = [conn];

        assert_eq!(
            resolve_legacy_key("unsaved", &connections),
            Some(ConnectionKey::Unsaved)
        );
        assert_eq!(
            resolve_legacy_key("saved:conn-a", &connections),
            Some(ConnectionKey::Saved(id))
        );
        assert_eq!(
            resolve_legacy_key("saved:no-such-connection", &connections),
            None
        );
    }

    #[test]
    fn migrating_a_legacy_store_resolves_saved_names_to_uuids_and_migrates_unsaved() {
        let paths = TempMigrationPaths::new("basic");
        write_legacy_file(&paths.legacy, &legacy_fixture_json());
        let conn_a = sample_stored_connection("conn-a");
        let conn_a_id = conn_a.id;

        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[conn_a]);

        let saved = SessionDir::new(&paths.sessions, ConnectionKey::Saved(conn_a_id))
            .load_snapshot()
            .expect("load must succeed")
            .expect("conn-a's session must have been migrated");
        assert_eq!(
            saved.tabs[0].kind,
            TabKind::Script {
                backing: ScriptBacking::SessionScratch {
                    file: ScriptFileName::new("query-1.sql").unwrap(),
                },
            },
            "a legacy query-1.sql entry's title marks it unnamed, since the legacy JSON \
             format predates the unnamed marker and always deserializes it as false"
        );
        assert_eq!(saved.tabs[0].buffer_text.as_deref(), Some("select 1;"));

        let unsaved = SessionDir::new(&paths.sessions, ConnectionKey::Unsaved)
            .load_snapshot()
            .expect("load must succeed")
            .expect("the unsaved sentinel session must have been migrated");
        assert_eq!(
            unsaved.tabs[0].kind,
            TabKind::Script {
                backing: ScriptBacking::SessionScratch {
                    file: ScriptFileName::new("query-1.sql").unwrap(),
                },
            }
        );
        assert_eq!(unsaved.tabs[0].buffer_text.as_deref(), Some("select 2;"));
    }

    #[test]
    fn migrating_a_legacy_query_n_titled_entry_lands_its_sibling_file_under_scratch() {
        let paths = TempMigrationPaths::new("legacy-lands-in-scratch");
        write_legacy_file(&paths.legacy, &legacy_fixture_json());
        let conn_a = sample_stored_connection("conn-a");
        let conn_a_id = conn_a.id;

        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[conn_a]);

        let dir = paths.sessions.join(conn_a_id.to_string());
        assert!(
            dir.join("scratch").join("query-1.sql").is_file(),
            "a legacy query-1.sql entry must be written under scratch/, not at the top level"
        );
        assert!(
            !dir.join("query-1.sql").exists(),
            "a legacy query-1.sql entry must never also exist at the top level"
        );
    }

    #[test]
    fn a_legacy_entry_titled_unlike_the_query_n_pattern_is_migrated_as_named() {
        let paths = TempMigrationPaths::new("legacy-named-title-stays-named");
        write_legacy_file(
            &paths.legacy,
            r#"{
                "connections": {
                    "saved:conn-a": {
                        "tabs": [
                            {"kind": {"Script": {"buffer_text": "select 1;"}}, "title": "top-customers.sql"}
                        ],
                        "active_index": 0
                    }
                }
            }"#,
        );
        let conn_a = sample_stored_connection("conn-a");
        let conn_a_id = conn_a.id;

        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[conn_a]);

        let saved = SessionDir::new(&paths.sessions, ConnectionKey::Saved(conn_a_id))
            .load_snapshot()
            .expect("load must succeed")
            .expect("conn-a's session must have been migrated");
        assert_eq!(
            saved.tabs[0].kind,
            TabKind::Script {
                backing: ScriptBacking::SessionNamed {
                    file: ScriptFileName::new("top-customers.sql").unwrap(),
                },
            },
            "a legacy entry whose title does not match the query-N pattern stays named"
        );
        let dir = paths.sessions.join(conn_a_id.to_string());
        assert!(dir.join("scripts").join("top-customers.sql").is_file());
    }

    #[test]
    fn an_entry_with_no_matching_connection_is_skipped_without_failing_the_rest() {
        let paths = TempMigrationPaths::new("unmatched-skip");
        write_legacy_file(&paths.legacy, &legacy_fixture_json());
        let conn_a = sample_stored_connection("conn-a");
        let conn_a_id = conn_a.id;

        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[conn_a]);

        // The unresolvable "saved:ghost-connection" entry has no uuid to
        // migrate under at all, so the only reachable assertion is that
        // migration produced exactly the two directories a legacy entry
        // *can* resolve to (conn-a's own uuid, and the unsaved sentinel) --
        // not a third for the ghost entry under any name.
        let entries: std::collections::HashSet<String> = std::fs::read_dir(&paths.sessions)
            .expect("sessions root must exist")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries,
            std::collections::HashSet::from([conn_a_id.to_string(), "unsaved".to_owned()]),
            "an unresolvable legacy entry must not migrate to any directory"
        );
        assert!(
            SessionDir::new(&paths.sessions, ConnectionKey::Unsaved)
                .load_snapshot()
                .expect("load must succeed")
                .is_some(),
            "a skipped entry must not prevent other entries from migrating"
        );
    }

    #[test]
    fn a_legacy_library_entry_with_an_invalid_name_is_skipped_without_failing_the_rest() {
        let paths = TempMigrationPaths::new("invalid-library-name-skip");
        write_legacy_file(
            &paths.legacy,
            r#"{
                "connections": {
                    "saved:conn-a": {
                        "tabs": [
                            {"kind": {"Library": {"library_name": "orders.sql", "draft_text": null}}, "title": "orders.sql"},
                            {"kind": {"Script": {"buffer_text": "select 1;"}}, "title": "top-customers.sql"}
                        ],
                        "active_index": 1
                    }
                }
            }"#,
        );
        let conn_a = sample_stored_connection("conn-a");
        let conn_a_id = conn_a.id;

        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[conn_a]);

        let saved = SessionDir::new(&paths.sessions, ConnectionKey::Saved(conn_a_id))
            .load_snapshot()
            .expect("load must succeed")
            .expect("conn-a's session must have been migrated");
        assert_eq!(
            saved.tabs.len(),
            1,
            "the entry with an invalid library name must be skipped, not fabricated"
        );
        assert_eq!(
            saved.tabs[0].kind,
            TabKind::Script {
                backing: ScriptBacking::SessionNamed {
                    file: ScriptFileName::new("top-customers.sql").unwrap(),
                },
            },
            "the rest of the session must still migrate"
        );
        assert_eq!(
            saved.active_index,
            Some(0),
            "active_index must be remapped once the skipped entry ahead of it is dropped"
        );
    }

    #[test]
    fn duplicate_display_names_resolve_to_the_first_match() {
        let first = sample_stored_connection("shared-name");
        let second = sample_stored_connection("shared-name");
        let connections = [first.clone(), second];

        let resolved = resolve_legacy_key("saved:shared-name", &connections);

        assert_eq!(resolved, Some(ConnectionKey::Saved(first.id)));
    }

    #[test]
    fn a_successful_migration_renames_the_legacy_file_to_a_bak_suffix() {
        let paths = TempMigrationPaths::new("backup-rename");
        write_legacy_file(&paths.legacy, &legacy_fixture_json());
        let conn_a = sample_stored_connection("conn-a");

        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[conn_a]);

        assert!(
            !paths.legacy.exists(),
            "the legacy file must be renamed away"
        );
        assert!(
            paths.backup().exists(),
            "the .bak file must exist after migration"
        );
    }

    #[test]
    fn a_legacy_file_with_invalid_json_is_left_in_place_and_nothing_is_migrated() {
        let paths = TempMigrationPaths::new("corrupt-json");
        write_legacy_file(&paths.legacy, "not valid json { at all");
        let conn_a = sample_stored_connection("conn-a");

        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[conn_a]);

        assert!(
            paths.legacy.exists(),
            "a corrupt legacy file must be left in place for later manual recovery"
        );
        assert!(
            !paths.backup().exists(),
            "a corrupt legacy file must never be backed up as though migration succeeded"
        );
        assert!(
            !paths.sessions.exists(),
            "a corrupt legacy file must not create any session directory"
        );
    }

    #[test]
    fn an_unreadable_legacy_file_is_left_in_place_and_nothing_is_migrated() {
        let paths = TempMigrationPaths::new("unreadable");
        // A directory at the legacy path exists but cannot be read as a
        // file, standing in for any read failure (permissions, I/O error).
        std::fs::create_dir_all(&paths.legacy).expect("must create a directory at the legacy path");
        let conn_a = sample_stored_connection("conn-a");

        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[conn_a]);

        assert!(
            paths.legacy.exists(),
            "an unreadable legacy file must be left in place for later manual recovery"
        );
        assert!(
            !paths.backup().exists(),
            "an unreadable legacy file must never be backed up as though migration succeeded"
        );
        assert!(
            !paths.sessions.exists(),
            "an unreadable legacy file must not create any session directory"
        );
    }

    #[test]
    fn a_missing_legacy_file_is_a_no_op() {
        let paths = TempMigrationPaths::new("no-legacy-file");
        std::fs::create_dir_all(paths.legacy.parent().unwrap()).unwrap();

        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[]);

        assert!(
            !paths.sessions.exists(),
            "nothing to migrate must create no sessions root"
        );
        assert!(!paths.backup().exists());
    }

    #[test]
    fn a_second_startup_with_only_the_bak_file_present_does_not_re_migrate() {
        let paths = TempMigrationPaths::new("second-startup");
        write_legacy_file(&paths.legacy, &legacy_fixture_json());
        let conn_a = sample_stored_connection("conn-a");
        let conn_a_id = conn_a.id;
        migrate_legacy_sessions(
            &paths.legacy,
            &paths.sessions,
            std::slice::from_ref(&conn_a),
        );

        // Mutate the migrated session so a re-migration would be observable
        // if it incorrectly ran again.
        let mutated = TabSessionSnapshot {
            tabs: vec![TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: ScriptBacking::SessionNamed {
                        file: ScriptFileName::new("query-1.sql").unwrap(),
                    },
                },
                title: "query-1.sql".to_owned(),
                buffer_text: Some("select 'mutated after migration';".to_owned()),
            }],
            active_index: Some(0),
        };
        SessionDir::new(&paths.sessions, ConnectionKey::Saved(conn_a_id))
            .save_snapshot(&mutated)
            .expect("seeding a post-migration mutation must succeed");

        // A second startup: only the .bak file is present, matching what a
        // completed migration leaves behind.
        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[conn_a]);

        let saved = SessionDir::new(&paths.sessions, ConnectionKey::Saved(conn_a_id))
            .load_snapshot()
            .expect("load must succeed")
            .expect("the session must still exist");
        assert_eq!(
            saved, mutated,
            "a second startup with no legacy file present must not re-run migration \
             and overwrite the post-migration state"
        );
    }

    #[test]
    fn retrying_migration_over_already_migrated_session_dirs_does_not_duplicate_or_corrupt() {
        let paths = TempMigrationPaths::new("idempotent-retry");
        write_legacy_file(&paths.legacy, &legacy_fixture_json());
        let conn_a = sample_stored_connection("conn-a");
        let conn_a_id = conn_a.id;

        migrate_legacy_sessions(
            &paths.legacy,
            &paths.sessions,
            std::slice::from_ref(&conn_a),
        );
        assert!(!paths.legacy.exists());

        // Simulate a retry after an interrupted prior run: the legacy file
        // is restored (as if the backup rename never completed) while the
        // already-migrated session directory is left exactly as it was.
        std::fs::rename(paths.backup(), &paths.legacy).expect("restore legacy file for retry");

        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[conn_a]);

        let saved = SessionDir::new(&paths.sessions, ConnectionKey::Saved(conn_a_id))
            .load_snapshot()
            .expect("load must succeed")
            .expect("the session must still exist after the retry");
        assert_eq!(
            saved.tabs.len(),
            1,
            "retrying migration must not duplicate tabs"
        );
        assert_eq!(
            saved.tabs[0].kind,
            TabKind::Script {
                backing: ScriptBacking::SessionScratch {
                    file: ScriptFileName::new("query-1.sql").unwrap(),
                },
            }
        );
        assert!(
            !paths.legacy.exists(),
            "the retry must back up the legacy file again"
        );
    }

    #[test]
    fn a_session_directory_without_tabs_toml_is_treated_as_unmigrated() {
        let paths = TempMigrationPaths::new("partial-dir-unmigrated");
        write_legacy_file(&paths.legacy, &legacy_fixture_json());
        let conn_a = sample_stored_connection("conn-a");
        let conn_a_id = conn_a.id;
        // Standing in for a crash mid-`save_snapshot`: the directory exists
        // (already created) but `tabs.toml` was never written into it.
        std::fs::create_dir_all(paths.sessions.join(conn_a_id.to_string()))
            .expect("must create the partial dir");

        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[conn_a]);

        let saved = SessionDir::new(&paths.sessions, ConnectionKey::Saved(conn_a_id))
            .load_snapshot()
            .expect("load must succeed")
            .expect(
                "a dir without tabs.toml must be treated as unmigrated, so migration \
                 actually wrote it rather than skipping it as already-done",
            );
        assert_eq!(
            saved.tabs[0].kind,
            TabKind::Script {
                backing: ScriptBacking::SessionScratch {
                    file: ScriptFileName::new("query-1.sql").unwrap(),
                },
            }
        );
    }

    #[test]
    fn a_failing_entry_leaves_the_legacy_file_in_place_for_retry_on_next_startup() {
        let paths = TempMigrationPaths::new("failing-entry");
        write_legacy_file(&paths.legacy, &legacy_fixture_json());
        let conn_a = sample_stored_connection("conn-a");
        let conn_a_id = conn_a.id;
        // Force conn-a's target session directory to already exist as a
        // plain file, so `save_snapshot` fails when it tries to create a
        // directory there.
        std::fs::create_dir_all(&paths.sessions).expect("must create sessions root");
        std::fs::write(
            paths.sessions.join(conn_a_id.to_string()),
            b"not a directory",
        )
        .expect("must create the blocking file");

        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[conn_a]);

        assert!(
            paths.legacy.exists(),
            "a failing entry must leave the legacy file in place so a later startup retries it"
        );
        assert!(
            !paths.backup().exists(),
            "a run with a failed entry must never back up the legacy file as though it fully succeeded"
        );
    }

    #[test]
    fn a_pre_existing_bak_file_is_never_clobbered_by_a_second_migration_attempt() {
        let paths = TempMigrationPaths::new("bak-survives");
        write_legacy_file(&paths.legacy, &legacy_fixture_json());
        let conn_a = sample_stored_connection("conn-a");
        // Standing in for a backup left behind by an earlier, already-
        // completed migration cycle that this run's legacy file should
        // never be allowed to overwrite.
        write_legacy_file(&paths.backup(), "an earlier completed migration's backup");

        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[conn_a]);

        let backup_contents = std::fs::read_to_string(paths.backup()).expect("must read backup");
        assert_eq!(
            backup_contents, "an earlier completed migration's backup",
            "a pre-existing .bak file must survive a second migration attempt intact"
        );
    }

    #[test]
    fn retrying_migration_never_overwrites_a_session_edited_after_the_first_run() {
        let paths = TempMigrationPaths::new("retry-preserves-edits");
        write_legacy_file(&paths.legacy, &legacy_fixture_json());
        let conn_a = sample_stored_connection("conn-a");
        let conn_a_id = conn_a.id;
        let key = ConnectionKey::Saved(conn_a_id);

        migrate_legacy_sessions(
            &paths.legacy,
            &paths.sessions,
            std::slice::from_ref(&conn_a),
        );

        // The user edits the migrated session before the retry happens.
        let edited = TabSessionSnapshot {
            tabs: vec![TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: ScriptBacking::SessionNamed {
                        file: ScriptFileName::new("query-1.sql").unwrap(),
                    },
                },
                title: "query-1.sql".to_owned(),
                buffer_text: Some("select 42; -- edited after migration".to_owned()),
            }],
            active_index: Some(0),
        };
        SessionDir::new(&paths.sessions, key)
            .save_snapshot(&edited)
            .expect("edit must save");

        // Simulate the first run having failed to rename the legacy file
        // away: it reappears, and the next startup re-runs migration.
        write_legacy_file(&paths.legacy, &legacy_fixture_json());
        migrate_legacy_sessions(&paths.legacy, &paths.sessions, &[conn_a]);

        let saved = SessionDir::new(&paths.sessions, key)
            .load_snapshot()
            .expect("load must succeed")
            .expect("the session must still exist");
        assert_eq!(
            saved.tabs[0].kind,
            TabKind::Script {
                backing: ScriptBacking::SessionNamed {
                    file: ScriptFileName::new("query-1.sql").unwrap(),
                },
            },
            "a re-run must keep the user's post-migration edit, not restore \
             the stale legacy snapshot"
        );
        assert_eq!(
            saved.tabs[0].buffer_text.as_deref(),
            Some("select 42; -- edited after migration")
        );
    }
}
