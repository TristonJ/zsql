use gpui::Context;

use super::{WorkspaceStartup, WorkspaceView, clamp_editor_height, clamp_sidebar_width};
use crate::ui::connections::ActiveConnection;
use crate::ui::tabs::{Tab, TabId};

/// Set the tracked active connection
impl WorkspaceView {
    pub fn set_active_connection(&mut self, active: ActiveConnection, cx: &mut Context<Self>) {
        self.connections
            .update(cx, |view, cx| view.set_active(Some(active), cx));
    }
}

mod resize_tests {
    use gpui::px;

    use super::{clamp_editor_height, clamp_sidebar_width};

    #[test]
    fn zero_delta_leaves_sidebar_width_unchanged() {
        let width = clamp_sidebar_width(px(300.0), px(0.0), px(180.0), px(560.0));
        assert_eq!(width, px(300.0));
    }

    #[test]
    fn dragging_sidebar_within_bounds_applies_the_delta_exactly() {
        let width = clamp_sidebar_width(px(300.0), px(50.0), px(180.0), px(560.0));
        assert_eq!(width, px(350.0));
    }

    #[test]
    fn sidebar_max_below_min_widens_to_min_instead_of_panicking() {
        // A misconfigured Config where max < min must not hit the
        // std Ord::clamp assertion that min <= max.
        let width = clamp_sidebar_width(px(300.0), px(1_000.0), px(400.0), px(200.0));
        assert_eq!(width, px(400.0));
    }

    #[test]
    fn dragging_sidebar_below_minimum_clamps_to_minimum() {
        let width = clamp_sidebar_width(px(300.0), px(-1_000.0), px(180.0), px(560.0));
        assert_eq!(width, px(180.0));
    }

    #[test]
    fn dragging_sidebar_above_maximum_clamps_to_maximum() {
        let width = clamp_sidebar_width(px(300.0), px(1_000.0), px(180.0), px(560.0));
        assert_eq!(width, px(560.0));
    }

    #[test]
    fn extreme_negative_sidebar_delta_does_not_panic_or_go_negative() {
        let width = clamp_sidebar_width(px(300.0), gpui::Pixels::MIN, px(180.0), px(560.0));
        assert!(width >= px(180.0));
        assert!(f32::from(width).is_finite());
    }

    #[test]
    fn extreme_positive_sidebar_delta_does_not_panic_or_overflow_past_max() {
        let width = clamp_sidebar_width(px(300.0), gpui::Pixels::MAX, px(180.0), px(560.0));
        assert!(width <= px(560.0));
        assert!(f32::from(width).is_finite());
    }

    #[test]
    fn zero_delta_leaves_editor_height_unchanged() {
        let height =
            clamp_editor_height(px(700.0), px(500.0), px(0.0), px(120.0), px(120.0), px(6.0));
        assert_eq!(height, px(500.0));
    }

    #[test]
    fn dragging_editor_divider_within_bounds_applies_the_delta_exactly() {
        let height = clamp_editor_height(
            px(700.0),
            px(500.0),
            px(50.0),
            px(120.0),
            px(120.0),
            px(6.0),
        );
        assert_eq!(height, px(550.0));
    }

    #[test]
    fn dragging_editor_divider_never_shrinks_results_below_its_minimum() {
        let container = px(700.0);
        let min_editor = px(120.0);
        let min_results = px(150.0);
        let divider = px(6.0);
        let height = clamp_editor_height(
            container,
            px(500.0),
            px(1_000.0),
            min_editor,
            min_results,
            divider,
        );
        let results_height = container - divider - height;
        assert!(results_height >= min_results);
    }

    #[test]
    fn huge_positive_delta_far_larger_than_container_still_respects_results_minimum() {
        let container = px(700.0);
        let min_editor = px(120.0);
        let min_results = px(150.0);
        let divider = px(6.0);
        let height = clamp_editor_height(
            container,
            px(500.0),
            gpui::Pixels::MAX,
            min_editor,
            min_results,
            divider,
        );
        let results_height = container - divider - height;
        assert!(results_height >= min_results);
        assert!(f32::from(height).is_finite());
    }

    #[test]
    fn dragging_editor_divider_upward_past_minimum_clamps_to_editor_minimum() {
        let container = px(700.0);
        let min_editor = px(120.0);
        let min_results = px(150.0);
        let divider = px(6.0);
        let height = clamp_editor_height(
            container,
            px(500.0),
            px(-1_000.0),
            min_editor,
            min_results,
            divider,
        );
        assert_eq!(height, min_editor);
    }

    #[test]
    fn extreme_negative_editor_delta_does_not_panic_or_go_negative() {
        let container = px(700.0);
        let height = clamp_editor_height(
            container,
            px(500.0),
            gpui::Pixels::MIN,
            px(120.0),
            px(150.0),
            px(6.0),
        );
        assert!(height >= px(120.0));
        assert!(f32::from(height).is_finite());
    }

    #[test]
    fn a_container_too_small_for_both_minimums_still_does_not_panic() {
        // The container is smaller than editor_min + divider + results_min:
        // there is no split that satisfies both minimums, so the editor's
        // own minimum wins rather than the clamp asserting min <= max.
        let height =
            clamp_editor_height(px(50.0), px(500.0), px(0.0), px(120.0), px(150.0), px(6.0));
        assert_eq!(height, px(120.0));
    }
}

mod render_tests {
    use std::time::Duration;

    use gpui::{AppContext as _, Entity};
    use zsql_core::{Catalog, Relation, RelationKind, SchemaNs, SchemaTree};

    use super::{WorkspaceStartup, WorkspaceView};
    use crate::config::{LayoutConfig, ValuePanelConfig};
    use crate::connections::{ConnectionArgs, ConnectionStore};
    use crate::session::{SchemaState, Session, SessionState};
    use crate::tab_session::{self, ConnectionKey};
    use crate::ui::connections::ActiveConnection;

    /// An empty connection store backed by a path this test never writes
    /// to: `WorkspaceView`'s render test only exercises rendering, not
    /// persistence.
    fn empty_store_for_test() -> ConnectionStore {
        let path = std::env::temp_dir().join(format!(
            "zsql-workspace-render-test-{}.toml",
            std::process::id()
        ));
        ConnectionStore::load(&path).expect("loading a nonexistent path must succeed empty")
    }

    /// A connection store's temp file path, plus a tab-session store's own
    /// temp path, both owned exclusively by one test and removed on drop.
    struct PersistenceTestPaths {
        connections: std::path::PathBuf,
        tab_sessions: std::path::PathBuf,
    }

    impl PersistenceTestPaths {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            Self {
                connections: std::env::temp_dir().join(format!(
                    "zsql-workspace-persistence-test-{label}-{pid}-{n}-connections.toml"
                )),
                tab_sessions: std::env::temp_dir().join(format!(
                    "zsql-workspace-persistence-test-{label}-{pid}-{n}-tab-sessions.json"
                )),
            }
        }
    }

    impl Drop for PersistenceTestPaths {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.connections);
            let _ = std::fs::remove_file(&self.tab_sessions);
        }
    }

    /// A connection store, persisted to `path`, holding two saved
    /// connections ("conn-a" and "conn-b") so `active_tab_session_key`
    /// resolves each to a stable [`ConnectionKey::Saved`] rather than the
    /// [`ConnectionKey::Unsaved`] fallback.
    fn store_with_two_saved_connections(path: &std::path::Path) -> ConnectionStore {
        let mut store =
            ConnectionStore::load(path).expect("loading a nonexistent path must succeed empty");
        store
            .add(ConnectionArgs {
                name: "conn-a".to_owned(),
                url: "postgres://localhost/a".to_owned(),
                ssh: None,
                ssh_secret: None,
            })
            .expect("add conn-a must succeed");
        store
            .add(ConnectionArgs {
                name: "conn-b".to_owned(),
                url: "postgres://localhost/b".to_owned(),
                ssh: None,
                ssh_secret: None,
            })
            .expect("add conn-b must succeed");
        store
    }

    fn sample_schema_session(cx: &mut gpui::TestAppContext) -> gpui::Entity<Session> {
        let tree = SchemaTree {
            catalogs: vec![Catalog {
                name: "zsql".to_owned(),
                schemas: vec![SchemaNs {
                    name: "public".to_owned(),
                    tables: vec![Relation {
                        name: "orders".to_owned(),
                        kind: RelationKind::Table,
                        columns: vec![],
                    }],
                }],
            }],
        };
        cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(tree)))
    }

    #[gpui::test]
    fn renders_the_sidebar_editor_and_results_without_panicking(cx: &mut gpui::TestAppContext) {
        let session = sample_schema_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });

        workspace.read_with(vcx, |workspace, cx| {
            assert_eq!(
                workspace.results.read(cx).source_label_for_test(),
                "query",
                "a workspace with no saved tabs must start on the default Script tab's label"
            );
        });
    }

    /// Renders an active `Schema` tab: its own read-only structural view
    /// fills the pane in place of the editor, divider, and shared results
    /// grid.
    #[gpui::test]
    fn renders_an_active_schema_tab_without_panicking(cx: &mut gpui::TestAppContext) {
        let session = sample_schema_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.open_or_reuse_schema("public", "orders", RelationKind::Table, cx);
            });
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            let active = tabs.active_tab().expect("a schema tab is active");
            assert!(active.is_schema());
            assert!(active.schema_view().is_some());
            // A schema tab's editor is never rendered, so it must not be
            // offered up for keyboard focus.
            assert!(
                workspace.editor_focus_handle(cx).is_none(),
                "a read-only schema tab must not expose an editor focus handle"
            );
        });
    }

    /// Renders a compact `Generated` tab (the active tab, shown as the
    /// compact SQL strip) alongside a `Script` tab, both listed in the tab
    /// bar above the results grid.
    #[gpui::test]
    fn renders_a_generated_and_a_script_tab_with_results_without_panicking(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = sample_schema_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });

        let generated_id = workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                let generated_id = tabs.open_or_reuse_generated("public", "orders", cx);
                tabs.new_script_tab(cx);
                generated_id
            })
        });
        vcx.run_until_parked();

        // Re-focus the generated tab so this frame renders both the tab
        // bar's script tab entry and the active tab's compact strip body.
        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.set_active(generated_id, cx));
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            // Plus the one empty script tab every workspace opens with.
            assert_eq!(tabs.tabs().len(), 3);
            assert_eq!(
                tabs.tabs().iter().filter(|tab| tab.is_generated()).count(),
                1
            );
            assert_eq!(
                tabs.tabs().iter().filter(|tab| !tab.is_generated()).count(),
                2
            );
            assert_eq!(tabs.active_id(), Some(generated_id));
        });
    }

    /// Renders a dirty, converted-from-generated `Script` tab as the active
    /// tab: the trailing `*` unsaved marker, the solid (not dashed) active
    /// underline, the full-height editor body, and no leftover generated
    /// theming all have to render without panicking once a generated tab
    /// has been edited.
    #[gpui::test]
    fn renders_a_dirty_converted_script_tab_as_the_active_tab_without_panicking(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = sample_schema_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });

        let (converted_id, editor) = workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                let id = tabs.open_or_reuse_generated("public", "orders", cx);
                let editor = tabs
                    .tabs()
                    .iter()
                    .find(|tab| tab.id() == id)
                    .unwrap()
                    .editor()
                    .clone();
                (id, editor)
            })
        });
        // The buffer's own `EditListener` reports back to the same
        // `TabModel` entity that owns it, so the edit must happen outside
        // any in-progress `tabs.update` call -- gpui forbids re-entrant
        // updates of the same entity.
        editor.update(vcx, |editor, cx| editor.insert_text_for_test("x", cx));
        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.set_active(converted_id, cx));
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            let tab = tabs
                .tabs()
                .iter()
                .find(|tab| tab.id() == converted_id)
                .unwrap();
            assert!(!tab.is_generated(), "the edit must have converted the tab");
            assert!(tab.dirty(), "an edited tab is dirty");
            assert_eq!(tabs.active_id(), Some(converted_id));
        });
    }

    #[gpui::test]
    fn initial_pane_sizes_match_the_layout_configs_defaults(cx: &mut gpui::TestAppContext) {
        let session = sample_schema_session(cx);
        let layout = LayoutConfig::default();
        let expected_sidebar_width = layout.sidebar_default_width;
        let expected_editor_height = layout.editor_default_height;
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                layout,
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });
        workspace.read_with(vcx, |workspace, _cx| {
            assert_eq!(workspace.sidebar_width, expected_sidebar_width);
            assert_eq!(workspace.editor_height, expected_editor_height);
        });
    }

    /// Exercises the full connect-switch wiring, not just the pure
    /// `TabModel`/`tab_session` logic: connecting to "conn-a", mutating its
    /// tabs, then switching to "conn-b" must flush "conn-a"'s tabs to disk
    /// under its own key before "conn-b"'s (snapshot-less, default) tabs
    /// replace what `TabModel` shows -- the two saves this triggers must not
    /// race each other or corrupt the store.
    #[gpui::test]
    fn switching_the_active_connection_persists_the_outgoing_tabs_and_shows_the_incoming_default(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = sample_schema_session(cx);
        let paths = PersistenceTestPaths::new("switch");
        let store = store_with_two_saved_connections(&paths.connections);
        let conn_a = ActiveConnection {
            id: Some(store.connections()[0].id),
            name: "conn-a".to_owned(),
            url: "postgres://localhost/a".to_owned(),
        };
        let conn_b = ActiveConnection {
            id: Some(store.connections()[1].id),
            name: "conn-b".to_owned(),
            url: "postgres://localhost/b".to_owned(),
        };
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                store,
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: Some(paths.tab_sessions.clone()),
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(conn_a, cx);
        });
        vcx.run_until_parked();

        // Mutate connection A's tabs beyond the default single empty
        // script, so its persisted snapshot is distinguishable from B's.
        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.new_script_tab(cx);
            });
        });
        vcx.run_until_parked();

        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(conn_b, cx);
        });
        vcx.run_until_parked();

        let saved_a = tab_session::load_snapshot(
            &paths.tab_sessions,
            &ConnectionKey::Saved("conn-a".to_owned()),
        )
        .expect("load must succeed")
        .expect("conn-a's tabs must have been persisted before switching away from it");
        assert_eq!(
            saved_a.tabs.len(),
            2,
            "the persisted snapshot must reflect conn-a's mutated tab set"
        );

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(
                tabs.tabs().len(),
                1,
                "conn-b has no snapshot yet, so it must show the default single tab, \
                 not conn-a's leftover tabs"
            );
            assert!(!tabs.tabs()[0].dirty());
        });
    }

    /// Build a workspace with two saved connections ("conn-a", already
    /// active with an open generated tab standing in for its own tabs and
    /// captured results, and "conn-b" at `conn_b_url`), for the connect-index
    /// switch-reset tests below.
    fn build_switch_fixture<'a>(
        cx: &'a mut gpui::TestAppContext,
        label: &str,
        conn_b_url: &str,
    ) -> (
        Entity<Session>,
        Entity<WorkspaceView>,
        &'a mut gpui::VisualTestContext,
        super::TabId,
    ) {
        let session = sample_schema_session(cx);
        let paths = PersistenceTestPaths::new(label);
        let mut store = ConnectionStore::load(&paths.connections).expect("load must succeed");
        store
            .add(ConnectionArgs {
                name: "conn-a".to_owned(),
                url: "postgres://localhost/a".to_owned(),
                ssh: None,
                ssh_secret: None,
            })
            .expect("add conn-a must succeed");
        store
            .add(ConnectionArgs {
                name: "conn-b".to_owned(),
                url: conn_b_url.to_owned(),
                ssh: None,
                ssh_secret: None,
            })
            .expect("add conn-b must succeed");

        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session.clone(),
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                store,
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });

        // Mark "conn-a" active (without touching the session -- it is
        // already `Connected`/`Ready` by construction) and open a generated
        // tab against it, standing in for connection A's own open tabs and
        // captured results that the switch under test must discard.
        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(
                ActiveConnection {
                    id: None,
                    name: "conn-a".to_owned(),
                    url: "postgres://localhost/a".to_owned(),
                },
                cx,
            );
        });
        vcx.run_until_parked();

        let generated_id = workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.open_or_reuse_generated("public", "orders", cx)
            })
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert!(
                tabs.tabs().iter().any(|tab| tab.id() == generated_id),
                "connection A's generated tab must be open before the switch"
            );
        });

        (session, workspace, vcx, generated_id)
    }

    /// Dispatching a switch through the real `ConnectionManagerView::connect`
    /// path must reset the schema tree and swap the open tabs synchronously,
    /// at the moment the switch is dispatched -- not once (or if) the
    /// connect attempt it kicks off actually resolves. Reads every assertion
    /// right after the dispatching call returns, before awaiting its task or
    /// advancing the executor at all, then lets the (successful) connect
    /// play out to prove the ordinary happy path is otherwise unaffected.
    #[gpui::test]
    async fn switching_via_connect_resets_the_tree_and_tabs_before_the_connect_resolves(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let (session, workspace, vcx, generated_id) =
            build_switch_fixture(cx, "connect-index-sync-reset", "sqlite::memory:");

        // Dispatch the switch to "conn-b" and read everything back
        // synchronously -- no `.await` on the returned task, no
        // `run_until_parked`, yet.
        let connections = workspace.read_with(vcx, |workspace, _cx| workspace.connections.clone());
        let conn_b_id = connections.read_with(vcx, |connections, _app| {
            connections.connections()[1].connection.id
        });
        let task = connections.update(vcx, |connections, cx| connections.connect(conn_b_id, cx));

        session.read_with(vcx, |session, _app| {
            assert!(
                matches!(session.schema(), SchemaState::NotLoaded),
                "expected the schema tree to reset synchronously, got {:?}",
                session.schema()
            );
        });
        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(
                tabs.tabs().len(),
                1,
                "expected only the fresh default tab, got {:?}",
                tabs.tabs()
                    .iter()
                    .map(super::Tab::title)
                    .collect::<Vec<_>>()
            );
            assert!(
                !tabs.tabs().iter().any(|tab| tab.id() == generated_id),
                "connection A's generated tab must not survive the switch"
            );
            let results = workspace.results.read(cx);
            assert_eq!(
                results.source_label_for_test(),
                "query",
                "the results pane must no longer show connection A's captured label"
            );
        });

        task.await;

        session.read_with(vcx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "expected the switch to succeed, got {:?}",
                session.state()
            );
        });
        connections.read_with(vcx, |connections, _app| {
            assert_eq!(
                connections.active().map(|active| active.name.as_str()),
                Some("conn-b"),
                "expected the successful switch to leave conn-b active"
            );
            assert!(
                connections
                    .status()
                    .is_some_and(|status| status.contains("Connected to conn-b")),
                "expected the usual success status text, got {:?}",
                connections.status()
            );
        });
        workspace.read_with(vcx, |workspace, cx| {
            assert_eq!(
                workspace.tabs.read(cx).tabs().len(),
                1,
                "a successful connect must not add or remove tabs beyond the reset"
            );
        });
    }

    /// The same switch as above, but the target connection fails: the
    /// workspace must not revert to connection A's tree or tabs, and
    /// `active` must stay pointed at the failed target rather than
    /// resurrecting the connection that preceded it.
    #[gpui::test]
    async fn a_failed_switch_via_connect_leaves_the_reset_tree_and_tabs_in_place(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let (session, workspace, vcx, generated_id) =
            build_switch_fixture(cx, "connect-index-sync-reset-fail", "cassandra://host/db");

        let connections = workspace.read_with(vcx, |workspace, _cx| workspace.connections.clone());
        let conn_b_id = connections.read_with(vcx, |connections, _app| {
            connections.connections()[1].connection.id
        });
        let task = connections.update(vcx, |connections, cx| connections.connect(conn_b_id, cx));
        task.await;

        session.read_with(vcx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Error(_)),
                "expected the switch to fail, got {:?}",
                session.state()
            );
            assert!(
                matches!(session.schema(), SchemaState::NotLoaded),
                "a failed switch must not resurrect connection A's schema tree, got {:?}",
                session.schema()
            );
        });
        connections.read_with(vcx, |connections, _app| {
            assert_eq!(
                connections.active().map(|active| active.name.as_str()),
                Some("conn-b"),
                "active must stay pointed at the failed target, not revert to conn-a"
            );
        });
        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(
                tabs.tabs().len(),
                1,
                "expected only conn-b's fresh default tab to remain"
            );
            assert!(
                !tabs.tabs().iter().any(|tab| tab.id() == generated_id),
                "connection A's generated tab must not have been resurrected"
            );
        });
    }

    /// A key this session has already loaded/saved must never lose to a
    /// disk read that happens to observe an older state than what this
    /// process already knows -- e.g. a background write still in flight
    /// when the user reconnects to a connection they only just switched
    /// away from. Forces disk out of sync with the in-memory cache directly
    /// (standing in for a write that has not landed yet) and asserts the
    /// switch back to "conn-a" still shows this session's own latest tabs.
    #[gpui::test]
    fn switching_back_to_a_cached_connection_ignores_a_stale_disk_snapshot(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = sample_schema_session(cx);
        let paths = PersistenceTestPaths::new("cache-wins");
        let store = store_with_two_saved_connections(&paths.connections);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                store,
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: Some(paths.tab_sessions.clone()),
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });

        let conn_a_id = uuid::Uuid::new_v4();
        let conn_a = ActiveConnection {
            id: Some(conn_a_id),
            name: "conn-a".to_owned(),
            url: "postgres://localhost/a".to_owned(),
        };
        let conn_b = ActiveConnection {
            id: Some(uuid::Uuid::new_v4()),
            name: "conn-b".to_owned(),
            url: "postgres://localhost/b".to_owned(),
        };

        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(conn_a, cx);
        });
        vcx.run_until_parked();
        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.new_script_tab(cx);
            });
        });
        vcx.run_until_parked();

        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(conn_b, cx);
        });
        vcx.run_until_parked();

        // Disk now disagrees with what this session already knows for
        // "conn-a" -- standing in for a background write dispatched earlier
        // that has not actually landed yet by the time the user reconnects.
        tab_session::save_snapshot(
            &paths.tab_sessions,
            &ConnectionKey::Saved("conn-a".to_owned()),
            &tab_session::TabSessionSnapshot::default(),
        )
        .expect("overwrite must succeed");

        let conn_a_again = ActiveConnection {
            id: Some(conn_a_id),
            name: "conn-a".to_owned(),
            url: "postgres://localhost/a".to_owned(),
        };
        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(conn_a_again, cx);
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(
                tabs.tabs().len(),
                2,
                "this session's own cached tabs for conn-a must win over the \
                 stale snapshot forced onto disk"
            );
        });
    }

    /// The app-restart path: a fresh workspace (empty in-memory cache) that
    /// connects to a saved connection whose tabs are already on disk must
    /// read that snapshot back from the file and rebuild the tab model from
    /// it -- order, kind, title, buffer text, and active tab all exactly as
    /// persisted. Exercises the cache-miss disk-read branch end to end, which
    /// the cache-wins and switch tests never reach.
    #[gpui::test]
    fn connecting_to_a_saved_connection_restores_its_tabs_from_disk(cx: &mut gpui::TestAppContext) {
        let session = sample_schema_session(cx);
        let paths = PersistenceTestPaths::new("restore-from-disk");
        let store = store_with_two_saved_connections(&paths.connections);
        let conn_a_id = store.connections()[0].id;

        // Seed conn-a's tab session on disk before the workspace exists, as
        // if written by a previous run of the app.
        let seeded = tab_session::TabSessionSnapshot {
            tabs: vec![
                tab_session::TabEntrySnapshot {
                    kind: tab_session::TabEntryKind::Script,
                    title: "query-1.sql".to_owned(),
                    buffer_text: "select 1;".to_owned(),
                },
                tab_session::TabEntrySnapshot {
                    kind: tab_session::TabEntryKind::Generated {
                        schema: "public".to_owned(),
                        relation: "orders".to_owned(),
                        edited: false,
                    },
                    title: "orders".to_owned(),
                    buffer_text: "SELECT * FROM \"public\".\"orders\" LIMIT 200".to_owned(),
                },
            ],
            active_index: Some(1),
        };
        tab_session::save_snapshot(
            &paths.tab_sessions,
            &ConnectionKey::Saved("conn-a".to_owned()),
            &seeded,
        )
        .expect("seeding conn-a's snapshot on disk must succeed");

        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                store,
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: Some(paths.tab_sessions.clone()),
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });

        // First-ever connect to conn-a: the in-memory cache has no entry, so
        // this must fall through to the on-disk snapshot.
        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(
                ActiveConnection {
                    id: Some(conn_a_id),
                    name: "conn-a".to_owned(),
                    url: "postgres://localhost/a".to_owned(),
                },
                cx,
            );
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(tabs.tabs().len(), 2, "both persisted tabs must be restored");

            let script = &tabs.tabs()[0];
            assert!(!script.is_generated());
            assert_eq!(script.title(), "query-1.sql");
            assert_eq!(script.editor().read(cx).text(), "select 1;");

            let generated = &tabs.tabs()[1];
            assert!(generated.is_generated());
            assert_eq!(generated.title(), "orders");
            assert_eq!(
                generated.editor().read(cx).text(),
                "SELECT * FROM \"public\".\"orders\" LIMIT 200"
            );

            assert_eq!(
                tabs.active_id(),
                Some(generated.id()),
                "the persisted active tab (index 1) must be the active one after restore"
            );
        });
    }

    /// `flush_tab_session_on_quit` returns a `Task` the caller must await
    /// before the app actually exits; this proves awaiting it actually
    /// finishes the write rather than the caller racing process exit
    /// against a fire-and-forget save.
    #[gpui::test]
    async fn flush_tab_session_on_quit_persists_the_active_connections_current_tabs(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = sample_schema_session(cx);
        let paths = PersistenceTestPaths::new("quit-flush");
        let store = store_with_two_saved_connections(&paths.connections);
        let conn_a_id = store.connections()[0].id;
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                store,
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: Some(paths.tab_sessions.clone()),
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(
                ActiveConnection {
                    id: Some(conn_a_id),
                    name: "conn-a".to_owned(),
                    url: "postgres://localhost/a".to_owned(),
                },
                cx,
            );
        });
        vcx.run_until_parked();

        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).tabs()[0].editor().clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select 42;", cx);
        });
        vcx.run_until_parked();

        let flush = workspace.update(vcx, WorkspaceView::flush_tab_session_on_quit);
        flush.await;

        let saved = tab_session::load_snapshot(
            &paths.tab_sessions,
            &ConnectionKey::Saved("conn-a".to_owned()),
        )
        .expect("load must succeed")
        .expect("flush_tab_session_on_quit must have written conn-a's tabs");
        assert_eq!(saved.tabs.len(), 1);
        assert_eq!(saved.tabs[0].buffer_text, "select 42;");
    }

    /// A workspace with no saved connections, so `WorkspaceView::new`'s
    /// default single script tab is the only tab present at construction.
    fn fresh_workspace(
        cx: &mut gpui::TestAppContext,
    ) -> (
        Entity<WorkspaceView>,
        super::TabId,
        &mut gpui::VisualTestContext,
    ) {
        let session = sample_schema_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });
        vcx.run_until_parked();
        let first_tab = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_id()
                .expect("a fresh workspace has one active tab")
        });
        (workspace, first_tab, vcx)
    }

    /// Opens `count` additional script tabs beyond the workspace's initial
    /// one, returning every open tab's id in order (index 0 is the initial
    /// tab this test module always starts with).
    fn open_extra_script_tabs(
        workspace: &Entity<WorkspaceView>,
        vcx: &mut gpui::VisualTestContext,
        count: usize,
    ) -> Vec<super::TabId> {
        for _ in 0..count {
            workspace.update(vcx, |workspace, cx| {
                workspace.tabs.update(cx, |tabs, cx| {
                    tabs.new_script_tab(cx);
                });
            });
        }
        vcx.run_until_parked();
        workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .tabs()
                .iter()
                .map(super::Tab::id)
                .collect()
        })
    }

    /// Enough extra tabs (on top of the workspace's initial one) to overflow
    /// the tab strip's available width at the default layout/tab-width
    /// config, comfortably past the point a single extra tab could round up
    /// to.
    const OVERFLOWING_EXTRA_TABS: usize = 19;

    /// Opening enough tabs to overflow the strip's available width must
    /// clip and scroll the strip itself, never grow the workspace's own
    /// outer container past the window it was given.
    #[gpui::test]
    fn opening_enough_tabs_to_overflow_does_not_widen_the_workspace_root(
        cx: &mut gpui::TestAppContext,
    ) {
        let (workspace, first_tab, vcx) = fresh_workspace(cx);
        let _ = first_tab;

        let width_before = vcx
            .debug_bounds("workspace-root")
            .expect("workspace-root must be painted")
            .size
            .width;
        let viewport_before = vcx
            .debug_bounds("tab-bar-scroll-viewport")
            .expect("the scroll viewport must be painted");

        open_extra_script_tabs(&workspace, vcx, OVERFLOWING_EXTRA_TABS);

        let width_after = vcx
            .debug_bounds("workspace-root")
            .expect("workspace-root must still be painted with many tabs open")
            .size
            .width;
        let viewport_after = vcx
            .debug_bounds("tab-bar-scroll-viewport")
            .expect("the scroll viewport must still be painted with many tabs open");

        assert_eq!(
            width_before, width_after,
            "opening enough tabs to overflow the tab strip must not widen the workspace's own \
             outer container"
        );
        // The strip's own viewport is where the clipping actually happens:
        // if its min-width/overflow wiring regressed, the viewport itself
        // would stretch to the tabs' summed width even while an ancestor's
        // min_w_0 kept the workspace root at the window size.
        assert_eq!(
            viewport_before.size.width, viewport_after.size.width,
            "the tab strip's scroll viewport must keep its width when its content overflows, \
             not stretch to fit the tabs"
        );
    }

    /// The new-tab button is a sibling after the scroll viewport, so it
    /// must stay painted at the strip's trailing edge -- outside the
    /// scrolled region -- no matter how far the strip is scrolled.
    #[gpui::test]
    fn the_new_tab_button_stays_reachable_while_the_strip_is_scrolled(
        cx: &mut gpui::TestAppContext,
    ) {
        let (workspace, first_tab, vcx) = fresh_workspace(cx);
        let _ = first_tab;

        let tab_ids = open_extra_script_tabs(&workspace, vcx, OVERFLOWING_EXTRA_TABS);
        let last_tab = *tab_ids.last().expect("at least one tab was opened");

        // Scroll to the strip's far end by activating the last tab.
        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.set_active(last_tab, cx));
        });
        vcx.run_until_parked();

        let root = vcx
            .debug_bounds("workspace-root")
            .expect("workspace-root must be painted");
        let viewport = vcx
            .debug_bounds("tab-bar-scroll-viewport")
            .expect("the scroll viewport must be painted");
        let button = vcx
            .debug_bounds("workspace-new-tab")
            .expect("the new-tab button must stay painted while the strip is scrolled");

        assert!(
            button.origin.x >= viewport.origin.x + viewport.size.width,
            "the new-tab button must sit after the scroll viewport's trailing edge, not inside \
             the scrolled region, got {button:?} against viewport {viewport:?}"
        );
        assert!(
            button.origin.x + button.size.width <= root.origin.x + root.size.width,
            "the new-tab button must stay within the workspace's own bounds, got {button:?} \
             against root {root:?}"
        );
    }

    /// Before the tab bar has anything to scroll to it, a tab far enough
    /// along the overflowing strip paints outside the visible viewport;
    /// activating it must scroll the strip so its whole width lands back
    /// inside the viewport.
    #[gpui::test]
    fn the_last_tab_is_not_reachable_until_it_is_scrolled_into_view(cx: &mut gpui::TestAppContext) {
        let (workspace, first_tab, vcx) = fresh_workspace(cx);

        let tab_ids = open_extra_script_tabs(&workspace, vcx, OVERFLOWING_EXTRA_TABS);
        let last_tab = *tab_ids.last().expect("at least one tab was opened");

        // Opening a tab activates it, which already scrolls it into view;
        // reactivate the first tab to scroll back to the strip's start
        // before checking the last tab's own reachability.
        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.set_active(first_tab, cx));
        });
        vcx.run_until_parked();

        let viewport = vcx
            .debug_bounds("tab-bar-scroll-viewport")
            .expect("the scroll viewport must be painted");
        let last_selector = crate::ui::tab_bar::tab_debug_selector_for_test(last_tab);
        let last_bounds_before = vcx
            .debug_bounds(last_selector)
            .expect("the last tab must still be painted (clipped, not removed) off-screen");
        assert!(
            last_bounds_before.origin.x + last_bounds_before.size.width
                > viewport.origin.x + viewport.size.width,
            "the last tab must sit past the viewport's trailing edge before it is scrolled into \
             view"
        );

        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.set_active(last_tab, cx));
        });
        vcx.run_until_parked();

        let last_bounds_after = vcx
            .debug_bounds(last_selector)
            .expect("the last tab must be painted once scrolled into view");
        assert!(
            last_bounds_after.origin.x >= viewport.origin.x
                && last_bounds_after.origin.x + last_bounds_after.size.width
                    <= viewport.origin.x + viewport.size.width,
            "activating the last tab must scroll the strip so its whole width lands fully \
             within the viewport, got {last_bounds_after:?} against viewport {viewport:?}"
        );
    }

    /// Opening enough tabs to overflow the strip in one burst -- each new
    /// tab activating itself the moment it opens, the way a rapid run of
    /// "+" clicks would -- must still land the final active tab back in
    /// view without any further activation from the test: the strip's own
    /// bookkeeping must not mistake a viewport measurement still describing
    /// the pre-burst tab count for proof the newly active tab is already
    /// visible.
    #[gpui::test]
    fn a_newly_active_tab_created_mid_overflow_burst_is_reachable_without_further_activation(
        cx: &mut gpui::TestAppContext,
    ) {
        let (workspace, first_tab, vcx) = fresh_workspace(cx);
        let _ = first_tab;

        let tab_ids = open_extra_script_tabs(&workspace, vcx, OVERFLOWING_EXTRA_TABS);
        let last_tab = *tab_ids.last().expect("at least one tab was opened");

        let viewport = vcx
            .debug_bounds("tab-bar-scroll-viewport")
            .expect("the scroll viewport must be painted");
        let selector = crate::ui::tab_bar::tab_debug_selector_for_test(last_tab);
        let bounds = vcx
            .debug_bounds(selector)
            .expect("the newly active last tab must be painted");

        assert!(
            bounds.origin.x >= viewport.origin.x
                && bounds.origin.x + bounds.size.width <= viewport.origin.x + viewport.size.width,
            "the tab that just became active by opening it must land fully within the \
             viewport without any further activation, got {bounds:?} against viewport \
             {viewport:?}"
        );
    }

    /// `TabModel::restore_tabs` (reached through `load_for_connection`) can
    /// restore a session whose active tab sits far enough along an
    /// overflowing strip that it was never on screen even once. That tab
    /// must land in view too, without any further activation from the
    /// test, the same as opening or clicking a tab already does.
    #[gpui::test]
    fn restoring_a_session_with_an_off_screen_active_tab_scrolls_it_into_view(
        cx: &mut gpui::TestAppContext,
    ) {
        let (workspace, first_tab, vcx) = fresh_workspace(cx);
        let _ = first_tab;

        let tab_count = OVERFLOWING_EXTRA_TABS + 1;
        let snapshot = tab_session::TabSessionSnapshot {
            tabs: (0..tab_count)
                .map(|i| tab_session::TabEntrySnapshot {
                    kind: tab_session::TabEntryKind::Script,
                    title: format!("query-{i}.sql"),
                    buffer_text: String::new(),
                })
                .collect(),
            active_index: Some(tab_count - 1),
        };

        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.load_for_connection(Some(&snapshot), cx);
            });
        });
        vcx.run_until_parked();

        let last_tab = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .tabs()
                .last()
                .expect("the snapshot restored at least one tab")
                .id()
        });

        let viewport = vcx
            .debug_bounds("tab-bar-scroll-viewport")
            .expect("the scroll viewport must be painted");
        let selector = crate::ui::tab_bar::tab_debug_selector_for_test(last_tab);
        let bounds = vcx
            .debug_bounds(selector)
            .expect("the restored active tab must be painted");

        assert!(
            bounds.origin.x >= viewport.origin.x
                && bounds.origin.x + bounds.size.width <= viewport.origin.x + viewport.size.width,
            "restoring a session whose active tab is off screen must scroll it into view \
             without any further activation, got {bounds:?} against viewport {viewport:?}"
        );
    }

    /// Isolates the active-tab-scroll-into-view behavior itself: activating
    /// an off-screen tab moves the strip; re-activating an already-visible
    /// tab causes no further scroll.
    #[gpui::test]
    fn activating_an_already_visible_tab_causes_no_further_scroll(cx: &mut gpui::TestAppContext) {
        let (workspace, first_tab, vcx) = fresh_workspace(cx);

        let tab_ids = open_extra_script_tabs(&workspace, vcx, OVERFLOWING_EXTRA_TABS);
        let last_tab = *tab_ids.last().expect("at least one tab was opened");
        let _ = first_tab;

        // Settle the strip on the last tab first: opening many tabs in a
        // tight burst can itself land one render behind the true content
        // extent, so this activation (not the burst itself) is what must
        // have already brought the tab fully into view.
        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.set_active(last_tab, cx));
        });
        vcx.run_until_parked();

        let selector = crate::ui::tab_bar::tab_debug_selector_for_test(last_tab);
        let bounds_before = vcx
            .debug_bounds(selector)
            .expect("the active last tab must already be painted in view");

        // Re-activating the same (already visible) tab must not move the
        // strip any further.
        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.set_active(last_tab, cx));
        });
        vcx.run_until_parked();

        let bounds_after = vcx
            .debug_bounds(selector)
            .expect("the tab must still be painted after a no-op reactivation");
        assert_eq!(
            bounds_before, bounds_after,
            "reactivating an already-visible tab must not move the strip's scroll offset"
        );
    }

    /// A shift-held mouse-wheel/trackpad gesture over the tab strip must pan
    /// it horizontally, mirroring `zsql_ui::scrollable::wrapper`'s own
    /// `shift_held_wheel_scrolls_the_horizontal_axis` test. Also proves the
    /// active-tab-scroll-into-view correction does not fight a manual
    /// scroll: the active tab never changes here, only the offset.
    #[gpui::test]
    fn shift_held_wheel_over_the_tab_strip_scrolls_it_horizontally(cx: &mut gpui::TestAppContext) {
        use gpui::{Modifiers, ScrollDelta, ScrollWheelEvent, TouchPhase, point, px};

        let (workspace, first_tab, vcx) = fresh_workspace(cx);
        let tab_ids = open_extra_script_tabs(&workspace, vcx, OVERFLOWING_EXTRA_TABS);
        let last_tab = *tab_ids.last().expect("at least one tab was opened");

        // Scroll back to the strip's start, so a wheel gesture has visible
        // room to move it.
        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.set_active(first_tab, cx));
        });
        vcx.run_until_parked();

        let viewport = vcx
            .debug_bounds("tab-bar-scroll-viewport")
            .expect("the scroll viewport must be painted");
        let last_selector = crate::ui::tab_bar::tab_debug_selector_for_test(last_tab);
        let bounds_before = vcx
            .debug_bounds(last_selector)
            .expect("the last tab must still be painted (clipped) before scrolling");

        vcx.simulate_event(ScrollWheelEvent {
            position: viewport.center(),
            delta: ScrollDelta::Pixels(point(px(-120.0), px(0.0))),
            modifiers: Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            touch_phase: TouchPhase::Moved,
        });
        vcx.run_until_parked();

        let bounds_after = vcx
            .debug_bounds(last_selector)
            .expect("the last tab must still be painted after scrolling");
        assert!(
            bounds_after.origin.x < bounds_before.origin.x,
            "a shift-held wheel gesture over the tab strip must scroll it toward later tabs, \
             got {bounds_before:?} before and {bounds_after:?} after"
        );
    }

    /// A horizontal trackpad swipe with no shift key held -- delta.x
    /// populated, no modifier -- must still pan the strip: the tab strip
    /// has no vertical axis of its own for shift to disambiguate against.
    #[gpui::test]
    fn a_plain_horizontal_trackpad_swipe_scrolls_the_tab_strip_without_shift(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{Modifiers, ScrollDelta, ScrollWheelEvent, TouchPhase, point, px};

        let (workspace, first_tab, vcx) = fresh_workspace(cx);
        let tab_ids = open_extra_script_tabs(&workspace, vcx, OVERFLOWING_EXTRA_TABS);
        let last_tab = *tab_ids.last().expect("at least one tab was opened");

        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.set_active(first_tab, cx));
        });
        vcx.run_until_parked();

        let viewport = vcx
            .debug_bounds("tab-bar-scroll-viewport")
            .expect("the scroll viewport must be painted");
        let last_selector = crate::ui::tab_bar::tab_debug_selector_for_test(last_tab);
        let bounds_before = vcx
            .debug_bounds(last_selector)
            .expect("the last tab must still be painted (clipped) before scrolling");

        vcx.simulate_event(ScrollWheelEvent {
            position: viewport.center(),
            delta: ScrollDelta::Pixels(point(px(-120.0), px(0.0))),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        });
        vcx.run_until_parked();

        let bounds_after = vcx
            .debug_bounds(last_selector)
            .expect("the last tab must still be painted after scrolling");
        assert!(
            bounds_after.origin.x < bounds_before.origin.x,
            "a plain (non-shift) horizontal trackpad swipe over the tab strip must scroll it \
             toward later tabs, got {bounds_before:?} before and {bounds_after:?} after"
        );
    }

    /// A plain vertical wheel notch -- no shift, no horizontal delta
    /// component -- over the tab strip must also pan it horizontally,
    /// matching how editors commonly treat a bare wheel scroll over an
    /// overflowing tab bar.
    #[gpui::test]
    fn a_plain_vertical_wheel_notch_scrolls_the_tab_strip_horizontally(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::{Modifiers, ScrollDelta, ScrollWheelEvent, TouchPhase, point, px};

        let (workspace, first_tab, vcx) = fresh_workspace(cx);
        let tab_ids = open_extra_script_tabs(&workspace, vcx, OVERFLOWING_EXTRA_TABS);
        let last_tab = *tab_ids.last().expect("at least one tab was opened");

        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.set_active(first_tab, cx));
        });
        vcx.run_until_parked();

        let viewport = vcx
            .debug_bounds("tab-bar-scroll-viewport")
            .expect("the scroll viewport must be painted");
        let last_selector = crate::ui::tab_bar::tab_debug_selector_for_test(last_tab);
        let bounds_before = vcx
            .debug_bounds(last_selector)
            .expect("the last tab must still be painted (clipped) before scrolling");

        vcx.simulate_event(ScrollWheelEvent {
            position: viewport.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-120.0))),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        });
        vcx.run_until_parked();

        let bounds_after = vcx
            .debug_bounds(last_selector)
            .expect("the last tab must still be painted after scrolling");
        assert!(
            bounds_after.origin.x < bounds_before.origin.x,
            "a plain vertical wheel notch over the tab strip must scroll it toward later tabs, \
             got {bounds_before:?} before and {bounds_after:?} after"
        );
    }
}

/// Tests for the workspace header's Run button: it dispatches a run for the
/// active tab through the same `Session`/`QueryRunner` path
/// `TabModel::run_for_tab` already uses, for both a `Script` and a
/// `Generated` active tab, independent of keyboard focus.
mod header_tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use gpui::{AppContext as _, Entity, Modifiers, TestAppContext};
    use zsql_core::{
        BatchSink, Connection, CoreError, QueryHandle, ResultSet, RowCount, SchemaTree,
    };

    use super::{WorkspaceStartup, WorkspaceView};
    use crate::config::{LayoutConfig, ValuePanelConfig};
    use crate::connections::ConnectionStore;
    use crate::session::{Session, SessionState};

    /// A `Connection` double that records every SQL string `stream_query` is
    /// called with, in place of a real session/database, so a test can
    /// assert the header's Run button reached `Session::run_query`.
    struct RecordingConnection {
        queries: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Connection for RecordingConnection {
        fn stream_query(&self, sql: String, _sink: BatchSink) -> QueryHandle {
            self.queries
                .lock()
                .expect("queries lock poisoned")
                .push(sql);
            let (cancel_tx, _cancel_rx) = flume::unbounded();
            QueryHandle::new(cancel_tx)
        }

        async fn introspect(&self) -> Result<SchemaTree, CoreError> {
            Ok(SchemaTree::default())
        }

        async fn ping(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn count_rows(&self, _schema: &str, _relation: &str) -> Result<RowCount, CoreError> {
            Ok(RowCount::Exact(0))
        }

        async fn describe_relation(
            &self,
            _schema: &str,
            _relation: &str,
        ) -> Result<zsql_core::RelationSchema, CoreError> {
            Ok(zsql_core::RelationSchema::default())
        }
    }

    /// An empty connection store backed by a path this test never writes to.
    fn empty_store_for_test() -> ConnectionStore {
        let path = std::env::temp_dir().join(format!(
            "zsql-workspace-header-test-{}.toml",
            std::process::id()
        ));
        ConnectionStore::load(&path).expect("loading a nonexistent path must succeed empty")
    }

    /// A session backed by a [`RecordingConnection`], and the queries it
    /// records.
    fn recording_session(cx: &mut TestAppContext) -> (Entity<Session>, Arc<Mutex<Vec<String>>>) {
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection: Arc<dyn Connection> = Arc::new(RecordingConnection {
            queries: queries.clone(),
        });
        let session = cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)));
        (session, queries)
    }

    #[gpui::test]
    fn header_run_button_dispatches_run_for_the_active_script_tab(cx: &mut TestAppContext) {
        let (session, queries) = recording_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });

        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .expect("a workspace always opens with an active script tab")
                .editor()
                .clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.set_text("select * from orders", cx);
        });

        workspace.update(vcx, WorkspaceView::run_active_tab);
        vcx.run_until_parked();

        assert_eq!(
            queries.lock().expect("queries lock poisoned").as_slice(),
            ["select * from orders"],
            "the header's Run button must dispatch the active script tab's SQL"
        );
    }

    #[gpui::test]
    fn header_run_button_dispatches_run_for_the_active_generated_tab(cx: &mut TestAppContext) {
        let (session, queries) = recording_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.open_or_reuse_generated("public", "orders", cx);
            });
        });
        vcx.run_until_parked();
        // Opening a generated tab already dispatches its own preview run;
        // isolate what the header's own click contributes.
        queries.lock().expect("queries lock poisoned").clear();

        workspace.update(vcx, WorkspaceView::run_active_tab);
        vcx.run_until_parked();

        assert_eq!(
            queries.lock().expect("queries lock poisoned").as_slice(),
            ["SELECT * FROM \"public\".\"orders\" LIMIT 200"],
            "the header's Run button must dispatch the active generated tab's SQL"
        );
    }

    #[gpui::test]
    fn header_renders_above_both_a_script_and_a_generated_active_tab_without_panicking(
        cx: &mut TestAppContext,
    ) {
        let (session, _queries) = recording_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });
        // A fresh workspace's active tab is a Script tab: this first frame
        // already exercises the header above a Script tab's full editor.
        vcx.run_until_parked();

        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.open_or_reuse_generated("public", "orders", cx);
            });
        });
        vcx.run_until_parked();
        // The newly opened generated tab is now active: this frame exercises
        // the same header above a Generated tab's compact strip instead.
        workspace.read_with(vcx, |workspace, cx| {
            assert!(
                workspace
                    .tabs
                    .read(cx)
                    .active_tab()
                    .is_some_and(super::Tab::is_generated),
                "the generated tab must be active for this frame to cover it"
            );
        });
    }

    #[gpui::test]
    fn header_run_button_is_a_no_op_when_all_tabs_are_closed(cx: &mut TestAppContext) {
        let (session, queries) = recording_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });

        // Close the default script tab, leaving no active tab
        workspace.update(vcx, |workspace, cx| {
            let tab_id = workspace
                .tabs
                .read(cx)
                .active_id()
                .expect("workspace should have an active tab initially");
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.close_tab(tab_id, cx);
            });
        });
        vcx.run_until_parked();

        // Verify there's no active tab
        workspace.read_with(vcx, |workspace, cx| {
            assert_eq!(
                workspace.tabs.read(cx).active_id(),
                None,
                "all tabs should be closed"
            );
        });

        // Attempt to run the active tab when none exists
        workspace.update(vcx, WorkspaceView::run_active_tab);
        vcx.run_until_parked();

        // Verify no queries were recorded (it was a no-op)
        let locked_queries = queries.lock().expect("queries lock poisoned");
        assert!(
            locked_queries.is_empty(),
            "running when no active tab exists must be a no-op"
        );
    }

    /// A session in `state` holding no connection at all, for the disabled
    /// states the Run button and `RunQuery` must both reject: `Empty`,
    /// `Connecting`, and an `Error` reached without ever completing a
    /// connect.
    fn disconnected_session(state: SessionState, cx: &mut TestAppContext) -> Entity<Session> {
        cx.update(|cx| cx.new(|_cx| Session::new_for_render_test(state, ResultSet::default())))
    }

    #[gpui::test]
    fn header_run_button_click_is_inert_while_not_connected(cx: &mut TestAppContext) {
        for state in [
            SessionState::Empty,
            SessionState::Connecting,
            SessionState::Error("connection refused".to_owned()),
        ] {
            let expected_state = state.clone();
            let session = disconnected_session(state, cx);
            let (workspace, vcx) = cx.add_window_view(|_window, cx| {
                WorkspaceView::new(
                    session.clone(),
                    LayoutConfig::default(),
                    ValuePanelConfig::default(),
                    empty_store_for_test(),
                    Duration::from_secs(2),
                    zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                    WorkspaceStartup {
                        tab_sessions_path: None,
                        active_theme_name: "zsql-dark".to_owned(),
                        themes_dir: None,
                        config_path: None,
                    },
                    cx,
                )
            });
            vcx.run_until_parked();

            let editor = workspace.read_with(vcx, |workspace, cx| {
                workspace
                    .tabs
                    .read(cx)
                    .active_tab()
                    .expect("a workspace always opens with an active script tab")
                    .editor()
                    .clone()
            });
            editor.update(vcx, |editor, cx| {
                editor.set_text("select 1", cx);
            });

            let bounds = vcx
                .debug_bounds("workspace-run-query-button")
                .expect("the Run button must still paint while disabled");
            vcx.simulate_click(bounds.center(), Modifiers::default());
            vcx.run_until_parked();

            session.read_with(vcx, |session, _app| {
                assert_eq!(
                    session.state(),
                    &expected_state,
                    "clicking Run while not connected must not mutate SessionState \
                     (started from {expected_state:?})"
                );
            });
        }
    }

    #[gpui::test]
    fn header_run_button_click_dispatches_run_while_connected(cx: &mut TestAppContext) {
        let (session, queries) = recording_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });
        vcx.run_until_parked();

        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .expect("a workspace always opens with an active script tab")
                .editor()
                .clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.set_text("select * from orders", cx);
        });

        let bounds = vcx
            .debug_bounds("workspace-run-query-button")
            .expect("the Run button must paint while connected");
        vcx.simulate_click(bounds.center(), Modifiers::default());
        vcx.run_until_parked();

        assert_eq!(
            queries.lock().expect("queries lock poisoned").as_slice(),
            ["select * from orders"],
            "clicking the Run button while connected must dispatch the active tab's SQL"
        );
    }

    #[gpui::test]
    fn dispatching_run_query_while_not_connected_does_not_dispatch_or_mutate_state(
        cx: &mut TestAppContext,
    ) {
        let starting_state = SessionState::Error("connection refused".to_owned());
        let session = disconnected_session(starting_state.clone(), cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session.clone(),
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    tab_sessions_path: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                },
                cx,
            )
        });
        vcx.run_until_parked();

        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .expect("a workspace always opens with an active script tab")
                .editor()
                .clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.set_text("select 1", cx);
        });
        let focus_handle = workspace
            .read_with(vcx, WorkspaceView::editor_focus_handle)
            .expect("the active script tab's editor must have a focus handle");
        vcx.update(|window, _cx| window.focus(&focus_handle));

        vcx.dispatch_action(zsql_editor::RunQuery);
        vcx.run_until_parked();

        session.read_with(vcx, |session, _app| {
            assert_eq!(
                session.state(),
                &starting_state,
                "RunQuery while not connected must not mutate SessionState"
            );
        });
    }
}
