use crate::session_store::library::LibraryDir;
use crate::ui::connections::ActiveConnection;

use super::SessionStore;
use crate::session_store::{
    ConnectionKey, LibraryName, ScriptBacking, ScriptFileName, SessionDir, SessionIo,
    SessionStoreError, TabEntrySnapshot, TabKind, TabSessionSnapshot,
};
use crate::session_store::{SaveClaim, SaveClaimFactory};

/// [`SessionDir::save_snapshot`] with this file's older free-function
/// signature, so the many call sites below stay focused on what each test
/// asserts rather than on constructing the directory handle.
fn save_snapshot(
    root: &std::path::Path,
    key: &ConnectionKey,
    snapshot: &TabSessionSnapshot,
) -> Result<(), SessionStoreError> {
    SessionDir::new(root, *key).save_snapshot(snapshot)
}

/// [`SessionDir::load_snapshot`], mirrored like [`save_snapshot`] above.
fn load_snapshot(
    root: &std::path::Path,
    key: &ConnectionKey,
) -> Result<Option<TabSessionSnapshot>, SessionStoreError> {
    SessionDir::new(root, *key).load_snapshot()
}

/// [`SessionDir::save_snapshot_if_current`], mirrored like [`save_snapshot`]
/// above.
fn save_snapshot_if_current(
    root: &std::path::Path,
    key: &ConnectionKey,
    snapshot: &TabSessionSnapshot,
    claim: SaveClaim,
) -> Result<bool, SessionStoreError> {
    SessionDir::new(root, *key).save_snapshot_if_current(snapshot, claim)
}

/// Rename `backing` to `new_file` within `dir`, via the same
/// [`ScriptBacking::rename`] path a real Save-to-connection/Rename flow
/// uses, threading a throwaway [`LibraryDir`] since these tests only ever
/// rename a session-owned backing.
fn rename_backing(
    dir: &std::path::Path,
    backing: &ScriptBacking,
    new_file: &str,
    claim: SaveClaim,
) -> Result<ScriptBacking, SessionStoreError> {
    let session_dir = SessionDir::at(dir);
    let library = LibraryDir::new(dir.join("unused-library"));
    let io = SessionIo {
        dir: &session_dir,
        library: &library,
    };
    backing
        .clone()
        .rename(&ScriptFileName::new(new_file).unwrap(), claim, &io)
}

/// A temp directory this test owns exclusively, removed on drop so tests
/// never leak directories into the real temp dir.
struct TempSessionsRoot(std::path::PathBuf);

impl TempSessionsRoot {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "zsql-session-persistence-test-{label}-{}-{n}",
            std::process::id()
        ));
        Self(path)
    }
}

impl Drop for TempSessionsRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> ScriptBacking {
    ScriptBacking::SessionScratch {
        file: ScriptFileName::new(name).unwrap(),
    }
}

fn named(name: &str) -> ScriptBacking {
    ScriptBacking::SessionNamed {
        file: ScriptFileName::new(name).unwrap(),
    }
}

fn sample_snapshot() -> TabSessionSnapshot {
    use zsql_core::preview_state::PreviewQueryState;
    TabSessionSnapshot {
        tabs: vec![
            TabEntrySnapshot {
                kind: TabKind::Generated {
                    schema: "public".to_owned(),
                    relation: "orders".to_owned(),
                    preview: PreviewQueryState::new(200),
                },
                title: "orders".to_owned(),
                buffer_text: None,
            },
            TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: scratch("query-1.sql"),
                },
                title: "query-1.sql".to_owned(),
                buffer_text: Some("select 1;\nselect 2;\n".to_owned()),
            },
        ],
        active_index: Some(1),
    }
}

#[test]
fn saving_then_loading_from_a_temp_directory_reproduces_the_snapshot_exactly() {
    let temp = TempSessionsRoot::new("round-trip");
    let key = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let snapshot = sample_snapshot();

    save_snapshot(&temp.0, &key, &snapshot).expect("save must succeed");
    let loaded = load_snapshot(&temp.0, &key).expect("load must succeed");

    assert_eq!(loaded, Some(snapshot));
}

#[test]
fn a_generated_tabs_schema_relation_and_preview_state_round_trip_exactly() {
    let temp = TempSessionsRoot::new("generated-round-trip");
    let key = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let mut preview = zsql_core::preview_state::PreviewQueryState::new(500);
    preview.toggle_sort("total_cents");
    preview.add_filter(
        "status",
        "text",
        zsql_core::filter::FilterOperator::Eq,
        "paid",
    );
    let snapshot = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Generated {
                schema: "public".to_owned(),
                relation: "orders".to_owned(),
                preview: preview.clone(),
            },
            title: "orders".to_owned(),
            buffer_text: None,
        }],
        active_index: Some(0),
    };

    save_snapshot(&temp.0, &key, &snapshot).expect("save must succeed");
    let loaded = load_snapshot(&temp.0, &key)
        .expect("load must succeed")
        .expect("a snapshot must have been saved");

    assert_eq!(loaded, snapshot);
}

#[test]
fn a_script_tabs_buffer_lives_in_its_own_sibling_sql_file_not_inline_in_tabs_toml() {
    let temp = TempSessionsRoot::new("sibling-file");
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);
    let snapshot = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: scratch("query-1.sql"),
            },
            title: "query-1.sql".to_owned(),
            buffer_text: Some("select * from orders where id = 42;".to_owned()),
        }],
        active_index: Some(0),
    };
    save_snapshot(&temp.0, &key, &snapshot).expect("save must succeed");

    let dir = temp.0.join(id.to_string());
    let tabs_toml = std::fs::read_to_string(dir.join("tabs.toml")).expect("must read back");
    assert!(
        !tabs_toml.contains("select * from orders"),
        "tabs.toml must never carry inline buffer text: {tabs_toml}"
    );
    assert!(tabs_toml.contains("file = \"scratch/query-1.sql\""));

    let sibling = std::fs::read_to_string(dir.join("scratch").join("query-1.sql"))
        .expect("sibling file must exist");
    assert_eq!(sibling, "select * from orders where id = 42;");
}

#[test]
fn two_script_tabs_with_identical_titles_each_keep_their_own_buffer_on_round_trip() {
    let temp = TempSessionsRoot::new("duplicate-titles");
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);
    let snapshot = TabSessionSnapshot {
        tabs: vec![
            TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: named("orders.sql"),
                },
                title: "orders".to_owned(),
                buffer_text: Some("select * from public.orders;".to_owned()),
            },
            TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: named("orders-2.sql"),
                },
                title: "orders".to_owned(),
                buffer_text: Some("select * from archive.orders;".to_owned()),
            },
        ],
        active_index: Some(0),
    };

    save_snapshot(&temp.0, &key, &snapshot).expect("save must succeed");

    let dir = temp.0.join(id.to_string());
    assert!(dir.join("scripts").join("orders.sql").exists());
    assert!(dir.join("scripts").join("orders-2.sql").exists());

    let loaded = load_snapshot(&temp.0, &key)
        .expect("load must succeed")
        .expect("a snapshot must have been saved");
    assert_eq!(
        loaded, snapshot,
        "both tabs' buffers must survive the round trip distinctly"
    );
}

#[test]
fn closing_a_script_tab_deletes_its_sibling_sql_file_on_the_next_save() {
    let temp = TempSessionsRoot::new("close-prunes-file");
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);
    let two_tabs = TabSessionSnapshot {
        tabs: vec![
            TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: scratch("query-1.sql"),
                },
                title: "query-1.sql".to_owned(),
                buffer_text: Some("select 1;".to_owned()),
            },
            TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: scratch("query-2.sql"),
                },
                title: "query-2.sql".to_owned(),
                buffer_text: Some("select 2;".to_owned()),
            },
        ],
        active_index: Some(0),
    };
    save_snapshot(&temp.0, &key, &two_tabs).expect("first save must succeed");

    let dir = temp.0.join(id.to_string());
    let scratch_dir = dir.join("scratch");
    assert!(scratch_dir.join("query-1.sql").exists());
    assert!(scratch_dir.join("query-2.sql").exists());

    let one_tab = TabSessionSnapshot {
        tabs: vec![two_tabs.tabs[0].clone()],
        active_index: Some(0),
    };
    save_snapshot(&temp.0, &key, &one_tab).expect("second save must succeed");

    assert!(scratch_dir.join("query-1.sql").exists());
    assert!(!scratch_dir.join("query-2.sql").exists());
}

#[test]
fn two_distinct_connection_keys_are_persisted_independently() {
    let temp = TempSessionsRoot::new("two-keys");
    let key_a = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let key_b = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let snapshot_a = sample_snapshot();
    let snapshot_b = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: named("query-1.sql"),
            },
            title: "query-1.sql".to_owned(),
            buffer_text: Some("select 'b';".to_owned()),
        }],
        active_index: Some(0),
    };

    save_snapshot(&temp.0, &key_a, &snapshot_a).expect("save a must succeed");
    save_snapshot(&temp.0, &key_b, &snapshot_b).expect("save b must succeed");

    assert_eq!(
        load_snapshot(&temp.0, &key_b).expect("load b must succeed"),
        Some(snapshot_b)
    );
    assert_eq!(
        load_snapshot(&temp.0, &key_a).expect("load a must succeed"),
        Some(snapshot_a)
    );
}

#[test]
fn loading_a_key_with_no_prior_snapshot_returns_none_not_an_error() {
    let temp = TempSessionsRoot::new("missing-key");
    let saved_key = ConnectionKey::Saved(uuid::Uuid::new_v4());
    save_snapshot(&temp.0, &saved_key, &sample_snapshot()).expect("save must succeed");

    let missing_key = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let result = load_snapshot(&temp.0, &missing_key).expect("load must succeed");

    assert_eq!(result, None);
}

#[test]
fn loading_from_a_nonexistent_root_returns_none_not_an_error() {
    let temp = TempSessionsRoot::new("missing-root");
    let result =
        load_snapshot(&temp.0, &ConnectionKey::Unsaved).expect("a missing root must not error");
    assert_eq!(result, None);
}

#[test]
fn persisted_bytes_never_contain_a_connection_url_or_secret() {
    let temp = TempSessionsRoot::new("secrets");
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);
    save_snapshot(&temp.0, &key, &sample_snapshot()).expect("save must succeed");

    let dir = temp.0.join(id.to_string());
    let secret_url = "postgres://admin:hunter2@prod-db.internal:5432/app";
    for path in every_file_under(&dir) {
        let bytes = std::fs::read_to_string(&path).expect("must read back");
        assert!(!bytes.contains(secret_url));
        assert!(!bytes.contains("hunter2"));
        assert!(!bytes.contains("postgres://"));
    }
}

fn every_file_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).expect("dir must exist") {
        let path = entry.expect("entry must be readable").path();
        if path.is_dir() {
            files.extend(every_file_under(&path));
        } else {
            files.push(path);
        }
    }
    files
}

#[test]
fn loading_a_session_with_invalid_toml_returns_a_parse_error_not_a_panic() {
    let temp = TempSessionsRoot::new("corrupt-load");
    let id = uuid::Uuid::new_v4();
    let dir = temp.0.join(id.to_string());
    std::fs::create_dir_all(&dir).expect("must create dir");
    std::fs::write(dir.join("tabs.toml"), b"not valid toml { at all").expect("must write garbage");

    let result = load_snapshot(&temp.0, &ConnectionKey::Saved(id));

    assert!(matches!(result, Err(SessionStoreError::Parse(_))));
}

#[test]
fn loading_a_script_tab_whose_sibling_sql_file_is_missing_skips_only_that_entry() {
    let temp = TempSessionsRoot::new("missing-sibling");
    let id = uuid::Uuid::new_v4();
    let dir = temp.0.join(id.to_string());
    std::fs::create_dir_all(&dir).expect("must create dir");
    std::fs::write(
        dir.join("tabs.toml"),
        "active = 0\n\n[[tabs]]\nkind = \"script\"\ntitle = \"gone.sql\"\nfile = \"gone.sql\"\n",
    )
    .expect("must write tabs.toml");

    let result = load_snapshot(&temp.0, &ConnectionKey::Saved(id));

    let snapshot = result
        .expect("a missing sibling must not fail the whole load")
        .expect("a tabs.toml exists, so a snapshot must be returned");
    assert!(snapshot.tabs.is_empty());
}

#[test]
fn loading_a_version_zero_tabs_toml_migrates_its_legacy_unnamed_script_into_scratch_and_bumps_the_version()
 {
    let temp = TempSessionsRoot::new("legacy-migration-via-load");
    let id = uuid::Uuid::new_v4();
    let dir = temp.0.join(id.to_string());
    std::fs::create_dir_all(&dir).expect("must create dir");
    std::fs::write(dir.join("query-1.sql"), "select 'legacy';")
        .expect("must write the legacy top-level sibling file");
    std::fs::write(
        dir.join("tabs.toml"),
        "active = 0\n\n[[tabs]]\nkind = \"script\"\ntitle = \"query-1.sql\"\n\
         file = \"query-1.sql\"\n",
    )
    .expect("must write a version-0 tabs.toml (version defaults to 0 when absent)");

    let loaded = load_snapshot(&temp.0, &ConnectionKey::Saved(id))
        .expect("load must succeed")
        .expect("a tabs.toml exists");

    assert_eq!(loaded.tabs.len(), 1);
    assert!(matches!(
        &loaded.tabs[0].kind,
        TabKind::Script {
            backing: ScriptBacking::SessionScratch { .. }
        }
    ));
    assert!(
        !dir.join("query-1.sql").exists(),
        "the legacy top-level file must be moved, not copied"
    );
    assert!(dir.join("scratch").join("query-1.sql").exists());
    let tabs_toml = std::fs::read_to_string(dir.join("tabs.toml")).expect("must read back");
    assert!(
        tabs_toml.contains("version = 1"),
        "the migrated file must be persisted at the current version: {tabs_toml}"
    );

    // A later top-level script that merely matches the legacy unnamed
    // pattern by name must never be swept into scratch/ once the directory
    // is already at the current version.
    std::fs::create_dir_all(dir.join("scripts")).expect("must create scripts dir");
    std::fs::write(dir.join("scripts").join("query-9.sql"), "select 9;")
        .expect("must write a second top-level script named like the legacy pattern");
    std::fs::write(
        dir.join("tabs.toml"),
        "version = 1\nactive = 0\n\n[[tabs]]\nkind = \"script\"\ntitle = \"query-1.sql\"\n\
         file = \"scratch/query-1.sql\"\n\n[[tabs]]\nkind = \"script\"\n\
         title = \"query-9.sql\"\nfile = \"query-9.sql\"\n",
    )
    .expect("must write a version-1 tabs.toml carrying both entries");

    let reloaded = load_snapshot(&temp.0, &ConnectionKey::Saved(id))
        .expect("second load must succeed")
        .expect("a tabs.toml exists");

    assert!(
        dir.join("scripts").join("query-9.sql").exists(),
        "a top-level script named like the legacy pattern must never be swept into scratch/ \
         once the session is already at the current version"
    );
    assert!(!dir.join("scratch").join("query-9.sql").exists());
    assert_eq!(reloaded.tabs.len(), 2);
}

#[test]
fn concurrent_saves_to_two_different_connections_do_not_race_or_corrupt_each_other() {
    let temp = TempSessionsRoot::new("concurrent-two-connections");
    let key_a = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let key_b = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let root_a = temp.0.clone();
    let root_b = temp.0.clone();

    let writer_a = move || {
        for i in 0..20 {
            let snapshot = TabSessionSnapshot {
                tabs: vec![TabEntrySnapshot {
                    kind: TabKind::Script {
                        backing: scratch("query-1.sql"),
                    },
                    title: "query-1.sql".to_owned(),
                    buffer_text: Some(format!("select 'a-{i}';")),
                }],
                active_index: Some(0),
            };
            save_snapshot(&root_a, &key_a, &snapshot).expect("connection a's write must not race");
        }
    };
    let writer_b = move || {
        for i in 0..20 {
            let snapshot = TabSessionSnapshot {
                tabs: vec![TabEntrySnapshot {
                    kind: TabKind::Script {
                        backing: scratch("query-1.sql"),
                    },
                    title: "query-1.sql".to_owned(),
                    buffer_text: Some(format!("select 'b-{i}';")),
                }],
                active_index: Some(0),
            };
            save_snapshot(&root_b, &key_b, &snapshot).expect("connection b's write must not race");
        }
    };

    let handle_a = std::thread::spawn(writer_a);
    let handle_b = std::thread::spawn(writer_b);
    handle_a
        .join()
        .expect("connection a's writer must not panic");
    handle_b
        .join()
        .expect("connection b's writer must not panic");

    let loaded_a = load_snapshot(&temp.0, &key_a)
        .expect("load a must succeed")
        .expect("connection a must have a snapshot");
    let loaded_b = load_snapshot(&temp.0, &key_b)
        .expect("load b must succeed")
        .expect("connection b must have a snapshot");
    assert!(
        loaded_a.tabs[0]
            .buffer_text
            .as_deref()
            .is_some_and(|text| text.starts_with("select 'a-")),
        "connection a's final content must be its own, never connection b's"
    );
    assert!(
        loaded_b.tabs[0]
            .buffer_text
            .as_deref()
            .is_some_and(|text| text.starts_with("select 'b-")),
        "connection b's final content must be its own, never connection a's"
    );
}

#[test]
fn saving_over_a_corrupt_existing_tabs_toml_errors_instead_of_silently_dropping_it() {
    let temp = TempSessionsRoot::new("corrupt-save");
    let id = uuid::Uuid::new_v4();
    let dir = temp.0.join(id.to_string());
    std::fs::create_dir_all(dir.join("scratch")).expect("must create dirs");
    std::fs::write(dir.join("tabs.toml"), b"not valid toml { at all").expect("must write garbage");
    // A scratch sibling the corrupt file's entries may still reference: a
    // save that replaced the corrupt file wholesale would let its prune
    // sweep delete this.
    std::fs::write(dir.join("scratch").join("query-1.sql"), "select 1;")
        .expect("must write the scratch sibling");

    let result = save_snapshot(&temp.0, &ConnectionKey::Saved(id), &sample_snapshot());

    assert!(
        result.is_err(),
        "a save over a corrupt tabs.toml must error, never replace it wholesale"
    );
    let preserved = std::fs::read_to_string(dir.join("tabs.toml")).expect("must read back");
    assert_eq!(
        preserved, "not valid toml { at all",
        "the corrupt file must be preserved for inspection, not overwritten"
    );
    assert!(
        dir.join("scratch").join("query-1.sql").exists(),
        "scratch files the corrupt tabs.toml may reference must not be pruned"
    );
}

#[test]
fn a_hand_edited_tabs_toml_with_a_nested_scratch_ref_is_rejected_not_followed() {
    let temp = TempSessionsRoot::new("load-rejects-nested-scratch");
    let id = uuid::Uuid::new_v4();
    let dir = temp.0.join(id.to_string());
    std::fs::create_dir_all(dir.join("scratch").join("sub")).expect("must create dirs");
    std::fs::write(
        dir.join("scratch").join("sub").join("query-1.sql"),
        "select 1;",
    )
    .expect("must write the nested file");
    std::fs::create_dir_all(dir.join("scripts")).expect("must create scripts dir");
    std::fs::write(dir.join("scripts").join("good.sql"), "select 2;")
        .expect("must write a good script");
    std::fs::write(
        dir.join("tabs.toml"),
        "active = 0\n\n[[tabs]]\nkind = \"script\"\ntitle = \"query-1.sql\"\n\
         file = \"scratch/sub/query-1.sql\"\n\n[[tabs]]\nkind = \"script\"\n\
         title = \"good.sql\"\nfile = \"good.sql\"\n",
    )
    .expect("must write tabs.toml");

    let loaded = load_snapshot(&temp.0, &ConnectionKey::Saved(id))
        .expect("load must succeed overall")
        .expect("a tabs.toml exists");

    assert_eq!(
        loaded.tabs.len(),
        1,
        "a ref nested below scratch/ must be rejected even though it resolves inside \
         the session directory; the rest of the session still loads"
    );
}

#[test]
fn a_library_backed_tab_with_a_draft_round_trips_with_the_draft_field_present() {
    let temp = TempSessionsRoot::new("library-with-draft");
    let key = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let snapshot = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: ScriptBacking::Library {
                    name: LibraryName::new("revenue-report").unwrap(),
                    saved_text: None,
                },
            },
            title: "revenue-report.sql".to_owned(),
            buffer_text: Some("select * from revenue where diverged = true;".to_owned()),
        }],
        active_index: Some(0),
    };

    save_snapshot(&temp.0, &key, &snapshot).expect("save must succeed");

    let dir = temp.0.join(match key {
        ConnectionKey::Saved(id) => id.to_string(),
        ConnectionKey::Unsaved => unreachable!(),
    });
    let tabs_toml = std::fs::read_to_string(dir.join("tabs.toml")).expect("must read back");
    assert!(tabs_toml.contains("file = \"library:revenue-report.sql\""));
    assert!(tabs_toml.contains("draft ="));
    assert!(
        dir.join("drafts")
            .join("library-revenue-report.sql")
            .exists()
    );

    let loaded = load_snapshot(&temp.0, &key)
        .expect("load must succeed")
        .expect("a snapshot must have been saved");
    assert_eq!(loaded, snapshot);
}

#[test]
fn a_library_backed_tab_without_a_draft_omits_the_draft_field_entirely() {
    let temp = TempSessionsRoot::new("library-no-draft");
    let key = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let snapshot = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: ScriptBacking::Library {
                    name: LibraryName::new("slow-queries").unwrap(),
                    saved_text: None,
                },
            },
            title: "slow-queries.sql".to_owned(),
            buffer_text: None,
        }],
        active_index: Some(0),
    };

    save_snapshot(&temp.0, &key, &snapshot).expect("save must succeed");

    let dir = temp.0.join(match key {
        ConnectionKey::Saved(id) => id.to_string(),
        ConnectionKey::Unsaved => unreachable!(),
    });
    let tabs_toml = std::fs::read_to_string(dir.join("tabs.toml")).expect("must read back");
    assert!(tabs_toml.contains("file = \"library:slow-queries.sql\""));
    assert!(!tabs_toml.contains("draft"));

    let loaded = load_snapshot(&temp.0, &key)
        .expect("load must succeed")
        .expect("a snapshot must have been saved");
    assert_eq!(loaded, snapshot);
}

#[test]
fn save_snapshot_if_current_writes_when_the_generation_is_the_first_seen() {
    let temp = TempSessionsRoot::new("generation-newer");
    let key = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let snapshot = sample_snapshot();
    let claims = SaveClaimFactory::new();
    let dir = temp.0.join(match key {
        ConnectionKey::Saved(id) => id.to_string(),
        ConnectionKey::Unsaved => unreachable!(),
    });
    let generation = claims.mint(&dir);

    let wrote =
        save_snapshot_if_current(&temp.0, &key, &snapshot, generation).expect("must succeed");
    assert!(wrote);
    assert_eq!(load_snapshot(&temp.0, &key).unwrap(), Some(snapshot));
}

#[test]
fn save_snapshot_if_current_never_lets_an_older_generation_overwrite_a_newer_one() {
    let temp = TempSessionsRoot::new("generation-out-of-order");
    let key = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let claims = SaveClaimFactory::new();
    let dir = temp.0.join(match key {
        ConnectionKey::Saved(id) => id.to_string(),
        ConnectionKey::Unsaved => unreachable!(),
    });

    let older_generation = claims.mint(&dir);
    let newer_generation = claims.mint(&dir);

    let older = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: scratch("query-1.sql"),
            },
            title: "query-1.sql".to_owned(),
            buffer_text: Some("select 'older';".to_owned()),
        }],
        active_index: Some(0),
    };
    let newer = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: named("query-1.sql"),
            },
            title: "query-1.sql".to_owned(),
            buffer_text: Some("select 'newer';".to_owned()),
        }],
        active_index: Some(0),
    };

    let wrote_newer = save_snapshot_if_current(&temp.0, &key, &newer, newer_generation)
        .expect("newer write must succeed");
    assert!(wrote_newer);
    let wrote_older = save_snapshot_if_current(&temp.0, &key, &older, older_generation)
        .expect("the call itself must not error, even though it is a no-op");
    assert!(
        !wrote_older,
        "an older generation arriving after a newer one must be refused"
    );

    let loaded = load_snapshot(&temp.0, &key)
        .expect("load must succeed")
        .expect("a snapshot must exist");
    assert_eq!(loaded, newer);
}

#[test]
fn a_stale_autosave_dispatched_before_a_reclaim_cannot_land_after_it_and_erase_the_placeholder() {
    let temp = TempSessionsRoot::new("reclaim-generation-guard");
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);
    let dir = temp.0.join(id.to_string());
    let claims = SaveClaimFactory::new();
    let named_snapshot = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: named("top-customers.sql"),
            },
            title: "top-customers.sql".to_owned(),
            buffer_text: Some("select * from customers;".to_owned()),
        }],
        active_index: Some(0),
    };

    let gen1 = claims.mint(&dir);
    save_snapshot_if_current(&temp.0, &key, &named_snapshot, gen1)
        .expect("first save must succeed");

    // Generation 2: an autosave dispatched before the tab closed and
    // reopened, still in flight.
    let gen2 = claims.mint(&dir);
    // The reopen's reclaim mints generation 3 internally: after gen2, but
    // landing first.
    SessionDir::at(&dir).reclaim_named_script_ref(
        "top-customers.sql",
        "top-customers.sql",
        &claims,
    );

    let landed = save_snapshot_if_current(&temp.0, &key, &TabSessionSnapshot::default(), gen2)
        .expect("the call itself must not error, even though it is refused");
    assert!(
        !landed,
        "a save dispatched before the reclaim must be refused once the reclaim has \
         recorded a newer generation"
    );

    let tabs_toml = std::fs::read_to_string(dir.join("tabs.toml")).expect("must read back");
    assert!(
        tabs_toml.contains("file = \"top-customers.sql\""),
        "the reclaim's placeholder must survive the stale save landing after it: {tabs_toml}"
    );

    let gen4 = claims.mint(&dir);
    save_snapshot_if_current(&temp.0, &key, &named_snapshot, gen4)
        .expect("save after reclaim must succeed");
    assert!(dir.join("scripts").join("top-customers.sql").exists());
    assert!(!dir.join("scripts").join("top-customers-2.sql").exists());
}

#[test]
fn reclaiming_a_file_already_referenced_by_tabs_toml_does_not_duplicate_its_entry() {
    let temp = TempSessionsRoot::new("reclaim-already-owned");
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);
    let dir = temp.0.join(id.to_string());
    let claims = SaveClaimFactory::new();
    let snapshot = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: named("orders.sql"),
            },
            title: "orders.sql".to_owned(),
            buffer_text: Some("select 1;".to_owned()),
        }],
        active_index: Some(0),
    };
    let gen1 = claims.mint(&dir);
    save_snapshot_if_current(&temp.0, &key, &snapshot, gen1).expect("save must succeed");

    SessionDir::at(&dir).reclaim_named_script_ref("orders.sql", "orders.sql", &claims);

    let loaded = load_snapshot(&temp.0, &key)
        .expect("load must succeed")
        .expect("a snapshot must exist");
    assert_eq!(
        loaded.tabs.len(),
        1,
        "reclaiming a file tabs.toml already references must be a no-op, not a duplicate entry"
    );
    assert_eq!(loaded, snapshot);
}

#[test]
fn a_stale_autosave_for_a_scripts_pre_promotion_scratch_snapshot_cannot_land_after_the_promoting_rename()
 {
    let temp = TempSessionsRoot::new("promotion-generation-guard");
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);
    let dir = temp.0.join(id.to_string());
    let claims = SaveClaimFactory::new();
    let unnamed = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: scratch("query-1.sql"),
            },
            title: "query-1.sql".to_owned(),
            buffer_text: Some("select 1;".to_owned()),
        }],
        active_index: Some(0),
    };
    let gen1 = claims.mint(&dir);
    save_snapshot_if_current(&temp.0, &key, &unnamed, gen1).expect("first save must succeed");
    assert!(dir.join("scratch").join("query-1.sql").exists());

    let gen2 = claims.mint(&dir);
    let gen3 = claims.mint(&dir);
    rename_backing(&dir, &scratch("query-1.sql"), "top-customers.sql", gen3)
        .expect("promotion must succeed");
    assert!(dir.join("scripts").join("top-customers.sql").exists());
    assert!(!dir.join("scratch").join("query-1.sql").exists());

    let landed = save_snapshot_if_current(&temp.0, &key, &unnamed, gen2)
        .expect("the call itself must not error, even though it is refused");
    assert!(!landed);
    assert!(!dir.join("scratch").join("query-1.sql").exists());
    let content = std::fs::read_to_string(dir.join("scripts").join("top-customers.sql"))
        .expect("the promoted top-level file must survive the stale autosave");
    assert_eq!(content, "select 1;");
}

#[test]
fn rename_session_script_promotes_a_scratch_file_to_the_top_level_and_retargets_tabs_toml() {
    let temp = TempSessionsRoot::new("rename-promote");
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);
    let dir = temp.0.join(id.to_string());
    let claims = SaveClaimFactory::new();
    let unnamed = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: scratch("query-1.sql"),
            },
            title: "query-1.sql".to_owned(),
            buffer_text: Some("select 1;".to_owned()),
        }],
        active_index: Some(0),
    };
    save_snapshot(&temp.0, &key, &unnamed).expect("first save must succeed");

    let generation = claims.mint(&dir);
    rename_backing(
        &dir,
        &scratch("query-1.sql"),
        "top-customers.sql",
        generation,
    )
    .expect("rename must succeed");

    assert!(dir.join("scripts").join("top-customers.sql").exists());
    assert!(!dir.join("scratch").join("query-1.sql").exists());
    let tabs_toml = std::fs::read_to_string(dir.join("tabs.toml")).expect("must read back");
    assert!(tabs_toml.contains("file = \"top-customers.sql\""));
}

#[test]
fn rename_session_script_refuses_to_clobber_an_existing_destination() {
    let temp = TempSessionsRoot::new("rename-duplicate");
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);
    let dir = temp.0.join(id.to_string());
    let claims = SaveClaimFactory::new();
    let two = TabSessionSnapshot {
        tabs: vec![
            TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: named("a.sql"),
                },
                title: "a.sql".to_owned(),
                buffer_text: Some("select 1;".to_owned()),
            },
            TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: named("b.sql"),
                },
                title: "b.sql".to_owned(),
                buffer_text: Some("select 2;".to_owned()),
            },
        ],
        active_index: Some(0),
    };
    save_snapshot(&temp.0, &key, &two).expect("save must succeed");

    let generation = claims.mint(&dir);
    let result = rename_backing(&dir, &named("a.sql"), "b.sql", generation);
    assert!(matches!(result, Err(SessionStoreError::Duplicate(_))));
}

#[test]
fn list_session_scripts_lists_every_top_level_sql_file() {
    let temp = TempSessionsRoot::new("list-scripts");
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);
    let snapshot = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: named("orders.sql"),
            },
            title: "orders.sql".to_owned(),
            buffer_text: Some("select 1;".to_owned()),
        }],
        active_index: Some(0),
    };
    save_snapshot(&temp.0, &key, &snapshot).expect("save must succeed");

    let scripts = SessionDir::new(&temp.0, key)
        .list_scripts()
        .expect("list must succeed");
    assert_eq!(scripts.len(), 1);
    assert_eq!(scripts[0].file_name, "orders.sql");
}

#[test]
fn listing_scripts_for_a_missing_session_directory_returns_an_empty_list() {
    let temp = TempSessionsRoot::new("list-missing");
    let key = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let scripts = SessionDir::new(&temp.0, key)
        .list_scripts()
        .expect("listing must not error");
    assert!(scripts.is_empty());
}

#[test]
fn the_unsaved_and_a_saved_key_never_collide() {
    let temp = TempSessionsRoot::new("unsaved-vs-saved");
    let unsaved_snapshot = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: scratch("query-1.sql"),
            },
            title: "query-1.sql".to_owned(),
            buffer_text: Some("select 'fallback';".to_owned()),
        }],
        active_index: Some(0),
    };
    save_snapshot(&temp.0, &ConnectionKey::Unsaved, &unsaved_snapshot).expect("save must succeed");
    save_snapshot(
        &temp.0,
        &ConnectionKey::Saved(uuid::Uuid::new_v4()),
        &sample_snapshot(),
    )
    .expect("save must succeed");

    assert_eq!(
        load_snapshot(&temp.0, &ConnectionKey::Unsaved).expect("load must succeed"),
        Some(unsaved_snapshot),
        "a saved connection's own uuid directory must never shadow the sentinel key"
    );
}

#[test]
fn a_named_scripts_file_survives_a_save_whose_snapshot_no_longer_carries_a_tab_for_it() {
    let temp = TempSessionsRoot::new("named-survives-close");
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);
    let with_named = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: named("top-customers.sql"),
            },
            title: "top-customers.sql".to_owned(),
            buffer_text: Some("select * from customers order by revenue desc;".to_owned()),
        }],
        active_index: Some(0),
    };
    save_snapshot(&temp.0, &key, &with_named).expect("first save must succeed");
    let dir = temp.0.join(id.to_string());
    assert!(dir.join("scripts").join("top-customers.sql").exists());

    // The tab closes: the next save carries a fresh, unrelated tab, no
    // reference to "top-customers.sql" at all.
    let after_close = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: scratch("query-1.sql"),
            },
            title: "query-1.sql".to_owned(),
            buffer_text: Some(String::new()),
        }],
        active_index: Some(0),
    };
    save_snapshot(&temp.0, &key, &after_close).expect("second save must succeed");

    assert!(
        dir.join("scripts").join("top-customers.sql").exists(),
        "a named, explicitly-saved script must never be deleted just because its tab closed"
    );
}

#[test]
fn a_stale_draft_file_is_pruned_once_its_tab_stops_diverging() {
    let temp = TempSessionsRoot::new("prune-stale-draft");
    let key = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let diverged = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: ScriptBacking::Library {
                    name: LibraryName::new("orders").unwrap(),
                    saved_text: None,
                },
            },
            title: "orders.sql".to_owned(),
            buffer_text: Some("select * from orders where diverged;".to_owned()),
        }],
        active_index: Some(0),
    };
    save_snapshot(&temp.0, &key, &diverged).expect("first save must succeed");
    let dir = temp.0.join(match key {
        ConnectionKey::Saved(id) => id.to_string(),
        ConnectionKey::Unsaved => unreachable!(),
    });
    assert!(dir.join("drafts").join("library-orders.sql").exists());

    let clean = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: ScriptBacking::Library {
                    name: LibraryName::new("orders").unwrap(),
                    saved_text: None,
                },
            },
            title: "orders.sql".to_owned(),
            buffer_text: None,
        }],
        active_index: Some(0),
    };
    save_snapshot(&temp.0, &key, &clean).expect("second save must succeed");

    assert!(
        !dir.join("drafts").join("library-orders.sql").exists(),
        "a draft that stopped diverging must be pruned on the next save"
    );
}

#[test]
fn closing_a_library_backed_tab_prunes_its_draft_and_leaves_the_library_untouched() {
    // Standing in for "open, edit without saving, close": the first save
    // carries the diverged draft, the second (post-close) save carries no
    // entry for that tab at all.
    let temp = TempSessionsRoot::new("close-prunes-draft");
    let key = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let with_draft = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: ScriptBacking::Library {
                    name: LibraryName::new("orders").unwrap(),
                    saved_text: None,
                },
            },
            title: "orders.sql".to_owned(),
            buffer_text: Some("select * from orders where diverged;".to_owned()),
        }],
        active_index: Some(0),
    };
    save_snapshot(&temp.0, &key, &with_draft).expect("first save must succeed");
    let dir = temp.0.join(match key {
        ConnectionKey::Saved(id) => id.to_string(),
        ConnectionKey::Unsaved => unreachable!(),
    });
    assert!(dir.join("drafts").join("library-orders.sql").exists());

    let closed = TabSessionSnapshot {
        tabs: vec![],
        active_index: None,
    };
    save_snapshot(&temp.0, &key, &closed).expect("second save must succeed");

    assert!(
        !dir.join("drafts").join("library-orders.sql").exists(),
        "closing the tab must prune its draft"
    );
}

#[test]
fn an_external_backed_tabs_absolute_path_round_trips_as_the_file_ref() {
    let temp = TempSessionsRoot::new("external-round-trip");
    let key = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let external_path = std::path::PathBuf::from("/home/t/work/migrate.sql");
    let snapshot = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: ScriptBacking::External {
                    path: external_path.clone(),
                    saved_text: None,
                },
            },
            title: "migrate.sql".to_owned(),
            buffer_text: Some("select * from migrate where diverged;".to_owned()),
        }],
        active_index: Some(0),
    };

    save_snapshot(&temp.0, &key, &snapshot).expect("save must succeed");

    let dir = temp.0.join(match key {
        ConnectionKey::Saved(id) => id.to_string(),
        ConnectionKey::Unsaved => unreachable!(),
    });
    let tabs_toml = std::fs::read_to_string(dir.join("tabs.toml")).expect("must read back");
    assert!(tabs_toml.contains(&external_path.display().to_string()));
    assert!(
        !dir.join("migrate.sql").exists(),
        "an external tab must never write a sibling file inside the session directory"
    );

    let loaded = load_snapshot(&temp.0, &key)
        .expect("load must succeed")
        .expect("a snapshot must have been saved");
    assert_eq!(loaded, snapshot);
}

#[test]
fn an_external_backed_tab_without_a_draft_omits_the_draft_field_and_reads_nothing_on_load() {
    let temp = TempSessionsRoot::new("external-no-draft");
    let key = ConnectionKey::Saved(uuid::Uuid::new_v4());
    let snapshot = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: ScriptBacking::External {
                    path: std::path::PathBuf::from("/tmp/does-not-exist-anywhere.sql"),
                    saved_text: None,
                },
            },
            title: "does-not-exist-anywhere.sql".to_owned(),
            buffer_text: None,
        }],
        active_index: Some(0),
    };

    save_snapshot(&temp.0, &key, &snapshot).expect("save must succeed");

    // The external file itself is never touched or required to exist by
    // this module -- only the caller (restore) resolves it.
    let loaded = load_snapshot(&temp.0, &key)
        .expect("load must succeed, even though the external file does not exist")
        .expect("a snapshot must have been saved");
    assert_eq!(loaded, snapshot);
}

// -- SessionStore state-machine tests --------------------------------------

fn snapshot_with_tab_count(count: usize) -> TabSessionSnapshot {
    TabSessionSnapshot {
        tabs: (0..count)
            .map(|i| {
                let title = format!("query-{i}.sql");
                let file = ScriptFileName::new(title.clone()).expect("valid file name");
                TabEntrySnapshot {
                    kind: TabKind::Script {
                        backing: ScriptBacking::SessionNamed { file },
                    },
                    title,
                    buffer_text: Some(format!("select {i};")),
                }
            })
            .collect(),
        active_index: if count == 0 { None } else { Some(0) },
    }
}

fn active_connection(id: uuid::Uuid, name: &str) -> ActiveConnection {
    ActiveConnection {
        id: Some(id),
        name: name.to_owned(),
        url: format!("postgres://localhost/{name}"),
    }
}

#[test]
fn dispatch_save_populates_the_cache_synchronously_before_any_write_completes() {
    let temp = TempSessionsRoot::new("dispatch-cache");
    let id = uuid::Uuid::new_v4();
    let mut store = SessionStore::new(Some(temp.0.clone()));
    let key = ConnectionKey::Saved(id);
    store.begin_switch(Some(key), Some(active_connection(id, "conn-a")));

    let snapshot = snapshot_with_tab_count(2);
    let dispatched = store
        .dispatch_save(snapshot.clone())
        .expect("an active key and root must dispatch a save");

    assert_eq!(dispatched.0, temp.0);
    assert_eq!(dispatched.1, key);
    assert_eq!(*dispatched.2, snapshot);
    assert_eq!(
        store.session_cache.get(&key).map(|s| &**s),
        Some(&snapshot),
        "the cache must hold the dispatched snapshot even though no disk write ever ran"
    );
}

#[test]
fn a_cached_dispatched_save_wins_over_a_stale_on_disk_snapshot_when_switching_back() {
    let temp = TempSessionsRoot::new("cache-wins");
    let id_a = uuid::Uuid::new_v4();
    let mut store = SessionStore::new(Some(temp.0.clone()));
    let key_a = ConnectionKey::Saved(id_a);

    store.begin_switch(Some(key_a), Some(active_connection(id_a, "conn-a")));
    let fresh = snapshot_with_tab_count(3);
    store
        .dispatch_save(fresh.clone())
        .expect("dispatch must succeed with a root and active key set");

    let stale = TabSessionSnapshot::default();
    SessionDir::new(&temp.0, key_a)
        .save_snapshot(&stale)
        .expect("seeding a stale snapshot must succeed");

    let id_b = uuid::Uuid::new_v4();
    let key_b = ConnectionKey::Saved(id_b);
    store.begin_switch(Some(key_b), Some(active_connection(id_b, "conn-b")));
    let resolved = store.begin_switch(Some(key_a), Some(active_connection(id_a, "conn-a")));

    assert_eq!(
        resolved.as_deref(),
        Some(&fresh),
        "the cached snapshot must win over the stale on-disk copy"
    );
}

#[test]
fn suppress_next_save_is_armed_by_begin_switch_and_consumed_exactly_once() {
    let mut store = SessionStore::new(None);
    let key = ConnectionKey::Saved(uuid::Uuid::new_v4());

    store.begin_switch(
        Some(key),
        Some(active_connection(uuid::Uuid::new_v4(), "conn-a")),
    );
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
    let temp = TempSessionsRoot::new("never-seen");
    let mut store = SessionStore::new(Some(temp.0.clone()));
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);

    let resolved = store.begin_switch(Some(key), Some(active_connection(id, "brand new")));

    assert_eq!(
        resolved, None,
        "a key with no cache entry and nothing on disk must fall back to None"
    );
}

#[test]
fn switching_to_a_key_whose_session_file_is_corrupt_falls_back_to_none_without_panicking() {
    let temp = TempSessionsRoot::new("corrupt-switch");
    let id = uuid::Uuid::new_v4();
    let dir = temp.0.join(id.to_string());
    std::fs::create_dir_all(&dir).expect("must create dir");
    std::fs::write(dir.join("tabs.toml"), b"not valid toml { at all").expect("must write garbage");
    let mut store = SessionStore::new(Some(temp.0.clone()));
    let key = ConnectionKey::Saved(id);

    let resolved = store.begin_switch(Some(key), Some(active_connection(id, "conn-a")));

    assert_eq!(
        resolved, None,
        "a corrupt session file must fall back to None rather than propagate the load error"
    );
}

#[test]
fn switching_with_no_sessions_root_never_touches_disk_and_falls_back_to_none() {
    let mut store = SessionStore::new(None);
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);

    let resolved = store.begin_switch(Some(key), Some(active_connection(id, "conn-a")));

    assert_eq!(resolved, None);
}

#[test]
fn dispatch_save_is_a_no_op_when_no_key_has_ever_been_tracked() {
    let temp = TempSessionsRoot::new("no-active-key");
    let mut store = SessionStore::new(Some(temp.0.clone()));

    assert!(store.dispatch_save(snapshot_with_tab_count(1)).is_none());
}

#[test]
fn dispatch_save_is_a_no_op_when_there_is_no_sessions_root() {
    let mut store = SessionStore::new(None);
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);
    store.begin_switch(Some(key), Some(active_connection(id, "conn-a")));

    assert!(store.dispatch_save(snapshot_with_tab_count(1)).is_none());
}

#[test]
fn active_connection_changed_detects_a_switch_and_ignores_a_repeat() {
    let mut store = SessionStore::new(None);
    let id = uuid::Uuid::new_v4();
    let conn_a = active_connection(id, "conn-a");

    assert!(
        store.active_connection_changed(Some(&conn_a)),
        "no connection was ever tracked, so any Some(..) must count as a change"
    );

    store.begin_switch(Some(ConnectionKey::Saved(id)), Some(conn_a.clone()));

    assert!(
        !store.active_connection_changed(Some(&conn_a)),
        "the same connection must not be reported as a change"
    );
    assert!(
        store.active_connection_changed(None),
        "disconnecting must be reported as a change"
    );
}

#[test]
fn renaming_a_saved_connection_never_orphans_its_tab_session() {
    let temp = TempSessionsRoot::new("rename-safety");
    let id = uuid::Uuid::new_v4();
    let key = ConnectionKey::Saved(id);
    let snapshot = snapshot_with_tab_count(2);
    SessionDir::new(&temp.0, key)
        .save_snapshot(&snapshot)
        .expect("save must succeed");

    let mut store = SessionStore::new(Some(temp.0.clone()));
    let resolved = store.begin_switch(Some(key), Some(active_connection(id, "renamed")));

    assert_eq!(
        resolved.as_deref(),
        Some(&snapshot),
        "the tabs saved before the rename must still load after it"
    );
}
