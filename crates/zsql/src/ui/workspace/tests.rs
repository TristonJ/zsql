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

    use gpui::{AppContext as _, Entity, Modifiers, MouseButton};
    use zsql_core::{Catalog, Relation, RelationKind, SchemaNs, SchemaTree};

    use super::{WorkspaceStartup, WorkspaceView};
    use crate::config::{LayoutConfig, ValuePanelConfig};
    use crate::connections::{ConnectionArgs, ConnectionStore};
    use crate::session::{SchemaState, Session, SessionState};
    use crate::session_store::{self, ConnectionKey};
    use crate::ui::connections::ActiveConnection;
    use crate::ui::tab_bar::tab_debug_selector_for_test;

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

    /// A connection store's temp file path, plus a sessions root directory,
    /// both owned exclusively by one test and removed on drop.
    struct PersistenceTestPaths {
        connections: std::path::PathBuf,
        sessions: std::path::PathBuf,
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
                sessions: std::env::temp_dir().join(format!(
                    "zsql-workspace-persistence-test-{label}-{pid}-{n}-sessions"
                )),
            }
        }
    }

    impl Drop for PersistenceTestPaths {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.connections);
            let _ = std::fs::remove_dir_all(&self.sessions);
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
                    sessions_root: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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
                    sessions_root: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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
                    sessions_root: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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
                    sessions_root: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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

    /// Right-clicking a `Generated` or a `Schema` tab must not attach the
    /// Script tab context menu: this stage's menu wiring (see
    /// `crate::ui::tab_bar::render_tab`) only right-click-binds `Script`
    /// tabs. A right-click on a `Script` tab is the positive control,
    /// confirming a right-click at the same simulated point does open the
    /// menu when wiring is present.
    #[gpui::test]
    fn right_clicking_a_generated_or_schema_tab_opens_no_context_menu(
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
                WorkspaceStartup::default(),
                cx,
            )
        });
        vcx.run_until_parked();

        let (generated_id, schema_id, script_id) = workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                let generated_id = tabs.open_or_reuse_generated("public", "orders", cx);
                let schema_id =
                    tabs.open_or_reuse_schema("public", "orders", RelationKind::Table, cx);
                let script_id = tabs.new_script_tab(cx);
                (generated_id, schema_id, script_id)
            })
        });
        vcx.run_until_parked();

        for id in [generated_id, schema_id] {
            let bounds = vcx
                .debug_bounds(tab_debug_selector_for_test(id))
                .expect("every open tab must render in the tab bar");
            vcx.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::default());
            vcx.run_until_parked();

            assert!(
                vcx.debug_bounds("tab-context-menu").is_none(),
                "right-clicking a non-Script tab must not open the Script \
                 tab context menu"
            );
        }

        let script_bounds = vcx
            .debug_bounds(tab_debug_selector_for_test(script_id))
            .expect("the script tab must render");
        vcx.simulate_mouse_down(
            script_bounds.center(),
            MouseButton::Right,
            Modifiers::default(),
        );
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds("tab-context-menu").is_some(),
            "a right-click at the same kind of point on a Script tab must \
             open the context menu, confirming the negative result above is \
             not just a stale/undetectable menu element"
        );
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
                    sessions_root: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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
    /// `TabModel`/`session_store` logic: connecting to "conn-a", mutating its
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
        let conn_a_id = store.connections()[0].id;
        let conn_a = ActiveConnection {
            id: Some(conn_a_id),
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
                    sessions_root: Some(paths.sessions.clone()),
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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

        let saved_a =
            session_store::SessionDir::new(&paths.sessions, ConnectionKey::Saved(conn_a_id))
                .load_snapshot()
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
                    sessions_root: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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
                    sessions_root: Some(paths.sessions.clone()),
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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
        session_store::SessionDir::new(&paths.sessions, ConnectionKey::Saved(conn_a_id))
            .save_snapshot(&session_store::TabSessionSnapshot::default())
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
        let seeded = session_store::TabSessionSnapshot {
            tabs: vec![
                session_store::TabEntrySnapshot {
                    kind: session_store::TabKind::Script {
                        backing: session_store::ScriptBacking::SessionScratch {
                            file: session_store::ScriptFileName::new("query-1.sql").unwrap(),
                        },
                    },
                    title: "query-1.sql".to_owned(),
                    buffer_text: Some("select 1;".to_owned()),
                },
                session_store::TabEntrySnapshot {
                    kind: session_store::TabKind::Generated {
                        schema: "public".to_owned(),
                        relation: "orders".to_owned(),
                        preview: zsql_core::preview_state::PreviewQueryState::new(200),
                    },
                    title: "orders".to_owned(),
                    buffer_text: None,
                },
            ],
            active_index: Some(1),
        };
        session_store::SessionDir::new(&paths.sessions, ConnectionKey::Saved(conn_a_id))
            .save_snapshot(&seeded)
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
                    sessions_root: Some(paths.sessions.clone()),
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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
                    sessions_root: Some(paths.sessions.clone()),
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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

        let saved =
            session_store::SessionDir::new(&paths.sessions, ConnectionKey::Saved(conn_a_id))
                .load_snapshot()
                .expect("load must succeed")
                .expect("flush_tab_session_on_quit must have written conn-a's tabs");
        assert_eq!(saved.tabs.len(), 1);
        assert_eq!(
            saved.tabs[0].kind,
            session_store::TabKind::Script {
                backing: session_store::ScriptBacking::SessionScratch {
                    file: session_store::ScriptFileName::new("query-1.sql").unwrap(),
                },
            }
        );
        assert_eq!(saved.tabs[0].buffer_text.as_deref(), Some("select 42;"));
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
                    sessions_root: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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
        let snapshot = session_store::TabSessionSnapshot {
            tabs: (0..tab_count)
                .map(|i| session_store::TabEntrySnapshot {
                    kind: session_store::TabKind::Script {
                        backing: session_store::ScriptBacking::SessionScratch {
                            file: session_store::ScriptFileName::new(format!("query-{i}.sql"))
                                .unwrap(),
                        },
                    },
                    title: format!("query-{i}.sql"),
                    buffer_text: Some(String::new()),
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

        async fn count_rows(
            &self,
            _schema: &str,
            _relation: &str,
            _filters: &zsql_core::FilterState,
        ) -> Result<RowCount, CoreError> {
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
                    sessions_root: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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
                    sessions_root: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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
                    sessions_root: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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
                    sessions_root: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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
                        sessions_root: None,
                        active_theme_name: "zsql-dark".to_owned(),
                        themes_dir: None,
                        config_path: None,
                        ..Default::default()
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
                    sessions_root: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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
                    sessions_root: None,
                    active_theme_name: "zsql-dark".to_owned(),
                    themes_dir: None,
                    config_path: None,
                    ..Default::default()
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

mod save_flow_tests {
    use std::time::Duration;

    use gpui::{AppContext as _, Entity, TestAppContext, point, px};
    use zsql_core::ResultSet;

    use super::{WorkspaceStartup, WorkspaceView};
    use crate::config::{LayoutConfig, ValuePanelConfig};
    use crate::connections::ConnectionStore;
    use crate::session::{Session, SessionState};
    use crate::session_store::{self, ScriptBacking, SessionDir};
    use crate::ui::connections::ActiveConnection;
    use crate::ui::save_modal::{Destination, SaveModalEvent, SaveModalKind};
    use crate::ui::tabs::TabModel;

    /// A pair of temp directories -- a sessions root and a library root --
    /// this test owns exclusively, removed on drop so tests never leak
    /// directories into the real temp dir.
    struct SaveFlowTestPaths {
        sessions: std::path::PathBuf,
        library: std::path::PathBuf,
    }

    impl SaveFlowTestPaths {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            Self {
                sessions: std::env::temp_dir().join(format!(
                    "zsql-workspace-save-flow-test-{label}-{pid}-{n}-sessions"
                )),
                library: std::env::temp_dir().join(format!(
                    "zsql-workspace-save-flow-test-{label}-{pid}-{n}-library"
                )),
            }
        }
    }

    impl Drop for SaveFlowTestPaths {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.sessions);
            let _ = std::fs::remove_dir_all(&self.library);
        }
    }

    fn empty_store_for_test(label: &str) -> ConnectionStore {
        let path = std::env::temp_dir().join(format!(
            "zsql-workspace-save-flow-test-{label}-{}.toml",
            std::process::id()
        ));
        ConnectionStore::load(&path).expect("loading a nonexistent path must succeed empty")
    }

    fn session_for_test(cx: &mut TestAppContext) -> Entity<Session> {
        cx.update(|cx| {
            cx.new(|_cx| Session::new_for_render_test(SessionState::Empty, ResultSet::default()))
        })
    }

    /// After a successful explicit save, the footer shows the transient
    /// "saved <file>" confirmation, then clears it again on its own after
    /// `save_confirmation_duration` -- without disturbing the footer's
    /// normal connection-status content before or after.
    #[gpui::test]
    fn save_confirmation_appears_then_clears_itself_after_its_configured_duration(
        cx: &mut TestAppContext,
    ) {
        let session = session_for_test(cx);
        let duration = Duration::from_millis(50);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test("confirmation"),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    save_confirmation_duration: duration,
                    ..Default::default()
                },
                cx,
            )
        });
        vcx.run_until_parked();

        let connection_status_before = workspace.read_with(vcx, |workspace, cx| {
            workspace.footer.read(cx).save_confirmation_for_test()
        });
        assert_eq!(
            connection_status_before, None,
            "no save confirmation must show before any save happens"
        );

        workspace.update(vcx, |workspace, cx| {
            workspace.show_save_confirmation("orders.sql", cx);
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            assert_eq!(
                workspace.footer.read(cx).save_confirmation_for_test(),
                Some("saved orders.sql".to_owned())
            );
        });

        vcx.executor().advance_clock(duration * 2);
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            assert_eq!(
                workspace.footer.read(cx).save_confirmation_for_test(),
                None,
                "the confirmation must clear itself once its duration elapses"
            );
        });
    }

    /// Driving `secondary-s` (Ctrl+S) through the real editor keybinding on
    /// an unnamed session tab must open the Save modal and hand keyboard
    /// focus to it, so subsequent keystrokes fill in the modal's name field
    /// rather than typing into the editor buffer (which would silently mark
    /// it dirty/edited instead of naming and saving it) and Enter confirms
    /// the save through the same seam a click on the Save button would.
    #[gpui::test]
    fn secondary_s_focuses_the_save_modal_so_typing_and_enter_reach_it_not_the_editor(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            zsql_editor::init(cx);
            zsql_ui::text_field::init(cx);
            crate::ui::save_modal::init(cx);
        });
        let session = session_for_test(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test("secondary-s-focus"),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup::default(),
                cx,
            )
        });
        vcx.run_until_parked();

        let editor_focus = workspace
            .read_with(vcx, WorkspaceView::editor_focus_handle)
            .expect("a workspace always opens with an active script tab's editor");
        vcx.update(|window, _cx| window.focus(&editor_focus));
        vcx.run_until_parked();

        vcx.simulate_keystrokes("secondary-s");
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            assert!(
                workspace.save_modal.read(cx).is_open(),
                "secondary-s on an unnamed tab must open the Save modal"
            );
        });

        vcx.simulate_input("orders");
        vcx.run_until_parked();

        let editor_text_after_typing = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .expect("still the same tab")
                .editor()
                .read(cx)
                .text()
        });
        assert_eq!(
            editor_text_after_typing, "",
            "typing after secondary-s must reach the Save modal's name field, \
             never the editor buffer underneath it"
        );

        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            assert!(
                !workspace.save_modal.read(cx).is_open(),
                "Enter must confirm the name typed into the modal and close it; \
                 it would stay open on an empty name if the field never \
                 received the typed text"
            );
        });
    }

    /// Driving `secondary-o` (Ctrl+O) through the real editor keybinding
    /// must open the Open Script picker, seeded with the active
    /// connection's (here, "Unsaved", since no connection is configured)
    /// open named tabs and the library listing.
    #[gpui::test]
    fn secondary_o_opens_the_open_script_picker(cx: &mut TestAppContext) {
        cx.update(|cx| {
            zsql_editor::init(cx);
            zsql_ui::text_field::init(cx);
            crate::ui::open_modal::init(cx);
        });
        let session = session_for_test(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test("secondary-o"),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup::default(),
                cx,
            )
        });
        vcx.run_until_parked();

        let editor_focus = workspace
            .read_with(vcx, WorkspaceView::editor_focus_handle)
            .expect("a workspace always opens with an active script tab's editor");
        vcx.update(|window, _cx| window.focus(&editor_focus));
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            assert!(
                !workspace.open_modal.read(cx).is_open(),
                "the picker must start closed"
            );
        });

        vcx.simulate_keystrokes("secondary-o");
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            assert!(
                workspace.open_modal.read(cx).is_open(),
                "secondary-o must open the Open Script picker"
            );
        });
    }

    /// An open library-backed tab must appear in the picker exactly once,
    /// under Library -- never also under "This connection", which would
    /// both double-list it and offer two different-looking rows for the
    /// same open tab.
    #[gpui::test]
    fn an_open_library_tab_appears_in_the_picker_once_under_library_only(cx: &mut TestAppContext) {
        cx.update(|cx| {
            zsql_editor::init(cx);
            zsql_ui::text_field::init(cx);
            crate::ui::open_modal::init(cx);
        });
        let (workspace, _paths, vcx) =
            workspace_with_active_connection("open-library-tab-picker", cx);

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).active_tab().unwrap().id()
        });
        let library_dir = workspace
            .read_with(vcx, |workspace, _cx| workspace.library_dir.clone())
            .unwrap();
        session_store::LibraryDir::at(&library_dir)
            .save(
                &session_store::LibraryName::new("revenue-report").unwrap(),
                "select 1;",
            )
            .expect("seeding the library file must succeed");
        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.convert_to_library_backed(
                    tab_id,
                    "revenue-report".to_owned(),
                    "select 1;".to_owned(),
                    cx,
                );
            });
        });

        let editor_focus = workspace
            .read_with(vcx, WorkspaceView::editor_focus_handle)
            .expect("a workspace always opens with an active script tab's editor");
        vcx.update(|window, _cx| window.focus(&editor_focus));
        vcx.run_until_parked();

        vcx.simulate_keystrokes("secondary-o");
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            let rows = workspace.open_modal.read(cx).rows_for_test();
            let matches: Vec<_> = rows
                .iter()
                .filter(|row| row.label == "revenue-report.sql")
                .collect();
            assert_eq!(
                matches.len(),
                1,
                "an open library tab must appear exactly once, got: {rows:?}"
            );
            assert_eq!(
                matches[0].section,
                crate::ui::open_modal::PickerSection::Library,
                "an open library tab must be listed under Library, not This connection"
            );
            assert_eq!(matches[0].meta, crate::ui::open_modal::PickerRowMeta::Open);
        });
    }

    /// Driving `shift-secondary-o` (Ctrl+Shift+O) must go straight to the
    /// Browse files flow -- invoking the (here, faked) open-file-dialog
    /// seam -- without ever showing the picker modal itself.
    #[gpui::test]
    fn shift_secondary_o_triggers_the_browse_files_flow_directly(cx: &mut TestAppContext) {
        cx.update(|cx| {
            zsql_editor::init(cx);
            zsql_ui::text_field::init(cx);
            crate::ui::open_modal::init(cx);
        });
        let session = session_for_test(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test("shift-secondary-o"),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup::default(),
                cx,
            )
        });
        vcx.run_until_parked();

        let invoked = std::rc::Rc::new(std::cell::Cell::new(false));
        let invoked_for_prompt = invoked.clone();
        workspace.update(vcx, |workspace, _cx| {
            workspace.set_open_files_prompt_for_test(Box::new(move |_cx| {
                invoked_for_prompt.set(true);
                gpui::Task::ready(None)
            }));
        });

        let editor_focus = workspace
            .read_with(vcx, WorkspaceView::editor_focus_handle)
            .expect("a workspace always opens with an active script tab's editor");
        vcx.update(|window, _cx| window.focus(&editor_focus));
        vcx.run_until_parked();

        vcx.simulate_keystrokes("shift-secondary-o");
        vcx.run_until_parked();

        assert!(
            invoked.get(),
            "shift-secondary-o must invoke the Browse files seam directly"
        );
        workspace.read_with(vcx, |workspace, cx| {
            assert!(
                !workspace.open_modal.read(cx).is_open(),
                "Browse files must never show the picker modal itself"
            );
        });
    }

    /// Clicking the scripts pane's pinned "Open external file..." footer
    /// must go straight to the Browse files flow, exactly like driving
    /// `shift-secondary-o` does above.
    #[gpui::test]
    fn clicking_the_scripts_pane_footer_triggers_the_browse_files_flow_directly(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            zsql_editor::init(cx);
            zsql_ui::text_field::init(cx);
            crate::ui::open_modal::init(cx);
        });
        let session = session_for_test(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test("scripts-footer-click"),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup::default(),
                cx,
            )
        });
        vcx.run_until_parked();

        let invoked = std::rc::Rc::new(std::cell::Cell::new(false));
        let invoked_for_prompt = invoked.clone();
        workspace.update(vcx, |workspace, _cx| {
            workspace.set_open_files_prompt_for_test(Box::new(move |_cx| {
                invoked_for_prompt.set(true);
                gpui::Task::ready(None)
            }));
        });

        let scripts_tab_bounds = vcx
            .debug_bounds("sidebar-pane-tab-scripts")
            .expect("the scripts pane tab must be painted");
        vcx.simulate_click(scripts_tab_bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();

        let footer_bounds = vcx
            .debug_bounds("sidebar-scripts-open-external")
            .expect("the footer must be painted once the scripts pane is active");
        vcx.simulate_click(footer_bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();

        assert!(
            invoked.get(),
            "clicking the footer must invoke the Browse files seam directly"
        );
        workspace.read_with(vcx, |workspace, cx| {
            assert!(
                !workspace.open_modal.read(cx).is_open(),
                "Browse files must never show the picker modal itself"
            );
        });
    }

    /// Triggering `shift-secondary-o` twice before the first native dialog
    /// resolves must invoke the (here, faked) prompt seam only once.
    #[gpui::test]
    fn a_second_browse_trigger_before_the_first_resolves_is_ignored(cx: &mut TestAppContext) {
        cx.update(|cx| {
            zsql_editor::init(cx);
            zsql_ui::text_field::init(cx);
            crate::ui::open_modal::init(cx);
        });
        let session = session_for_test(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test("browse-in-flight-guard"),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup::default(),
                cx,
            )
        });
        vcx.run_until_parked();

        let invoke_count = std::rc::Rc::new(std::cell::Cell::new(0));
        let invoke_count_for_prompt = invoke_count.clone();
        workspace.update(vcx, |workspace, _cx| {
            workspace.set_open_files_prompt_for_test(Box::new(move |cx| {
                invoke_count_for_prompt.set(invoke_count_for_prompt.get() + 1);
                // Genuinely never resolves within this test, standing in
                // for a native dialog still open when the second trigger
                // fires.
                cx.spawn(async move |_cx| {
                    std::future::pending::<()>().await;
                    None
                })
            }));
        });

        let editor_focus = workspace
            .read_with(vcx, WorkspaceView::editor_focus_handle)
            .expect("a workspace always opens with an active script tab's editor");
        vcx.update(|window, _cx| window.focus(&editor_focus));
        vcx.run_until_parked();

        vcx.simulate_keystrokes("shift-secondary-o");
        vcx.simulate_keystrokes("shift-secondary-o");
        vcx.run_until_parked();

        assert_eq!(
            invoke_count.get(),
            1,
            "a second trigger before the first dialog resolves must be ignored"
        );
    }

    /// A file picked via Browse whose bytes are not valid UTF-8 must never
    /// open a tab with an empty writable buffer backed by that path --
    /// `Err` (unreadable) and `Ok(None)` (missing) are surfaced as a
    /// distinguishable status-bar error instead. This is what actually
    /// prevents a later Ctrl+S from truncating the original file: with no
    /// tab ever opened for it, there is nothing to save over it with.
    #[gpui::test]
    fn browsing_to_a_non_utf8_file_never_opens_a_tab_or_touches_the_file(cx: &mut TestAppContext) {
        cx.update(|cx| {
            zsql_editor::init(cx);
            zsql_ui::text_field::init(cx);
            crate::ui::open_modal::init(cx);
        });
        let session = session_for_test(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test("browse-non-utf8"),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup::default(),
                cx,
            )
        });
        vcx.run_until_parked();

        let dir = std::env::temp_dir().join(format!(
            "zsql-workspace-non-utf8-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("must create temp dir");
        let path = dir.join("latin1-dump.sql");
        // 0xFF is never valid as the start of a UTF-8 sequence.
        std::fs::write(&path, [b's', b'e', b'l', b'e', b'c', b't', 0xFF, b';'])
            .expect("must write");
        let original_bytes = std::fs::read(&path).expect("must read back for comparison");

        let tab_count_before =
            workspace.read_with(vcx, |workspace, cx| workspace.tabs.read(cx).tabs().len());

        workspace.update(vcx, |workspace, _cx| {
            let picked = path.clone();
            workspace.set_open_files_prompt_for_test(Box::new(move |_cx| {
                gpui::Task::ready(Some(vec![picked.clone()]))
            }));
        });

        let editor_focus = workspace
            .read_with(vcx, WorkspaceView::editor_focus_handle)
            .expect("a workspace always opens with an active script tab's editor");
        vcx.update(|window, _cx| window.focus(&editor_focus));
        vcx.run_until_parked();

        vcx.simulate_keystrokes("shift-secondary-o");
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            assert_eq!(
                workspace.tabs.read(cx).tabs().len(),
                tab_count_before,
                "an unreadable file must never open a tab"
            );
            assert!(
                workspace.footer.read(cx).save_error_showing_for_test(),
                "an unreadable file must surface a distinguishable status-bar error"
            );
        });

        let bytes_after = std::fs::read(&path).expect("must read back");
        assert_eq!(
            bytes_after, original_bytes,
            "the original file must be completely untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tab-session save can fail for reasons having nothing to do with the
    /// tab content itself -- here, the sessions root is not even a
    /// directory. Every such failure must still surface through the same
    /// status-bar error the open/save-modal error paths use, rather than
    /// only a `tracing::warn!` no one watching the log ever sees.
    #[gpui::test]
    fn a_tab_session_save_that_cannot_create_its_directory_surfaces_a_status_bar_error(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = session_for_test(cx);
        let paths = SaveFlowTestPaths::new("save-failure");
        // A plain file sitting where the sessions root should be a
        // directory: every save dispatched for the active connection must
        // fail before it can even create its own session directory.
        std::fs::write(&paths.sessions, b"not a directory").expect("must write a blocking file");
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test("save-failure"),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    sessions_root: Some(paths.sessions.clone()),
                    library_root: Some(paths.library.clone()),
                    ..Default::default()
                },
                cx,
            )
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(
                ActiveConnection {
                    id: None,
                    name: "conn".to_owned(),
                    url: "postgres://localhost/a".to_owned(),
                },
                cx,
            );
        });
        vcx.run_until_parked();

        // The switch's own reload is suppressed from triggering a redundant
        // save (see `SessionStore::take_suppressed`); a further tab change
        // is what actually dispatches the save this test means to fail.
        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.new_script_tab(cx);
            });
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            assert!(
                workspace.footer.read(cx).save_error_showing_for_test(),
                "a save that cannot create its own session directory must surface a \
                 status-bar error"
            );
        });
    }

    /// Confirming the Save modal's "Somewhere else..." destination exports a
    /// copy via the (here, faked) save-file dialog seam and never touches
    /// the source tab's own kind, title, or backing -- even from an unnamed
    /// `query-N` tab, which stays exactly what it was.
    #[gpui::test]
    fn somewhere_else_export_leaves_the_tabs_own_state_completely_unchanged(
        cx: &mut TestAppContext,
    ) {
        let session = session_for_test(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test("somewhere-else"),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup::default(),
                cx,
            )
        });
        vcx.run_until_parked();

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .expect("one tab open")
                .id()
        });
        let title_before = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .tab_title_of(tab_id)
                .expect("tab exists")
                .to_owned()
        });
        let backing_before = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).script_backing_of(tab_id)
        });

        let temp = SaveFlowTestPaths::new("somewhere-else-export");
        std::fs::create_dir_all(&temp.sessions).expect("must create temp dir");
        let export_path = temp.sessions.join("exported.sql");
        let export_path_for_prompt = export_path.clone();
        workspace.update(vcx, |workspace, _cx| {
            workspace.set_save_file_prompt_for_test(Box::new(move |_cx, _dir, _suggested| {
                gpui::Task::ready(Some(export_path_for_prompt.clone()))
            }));
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id,
                    kind: SaveModalKind::SaveAs,
                    name: "exported".to_owned(),
                    destination: Destination::External,
                },
                cx,
            );
        });
        vcx.run_until_parked();

        assert!(
            export_path.is_file(),
            "the export must land at the chosen path"
        );

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(
                tabs.tabs().len(),
                1,
                "the export must not open or close any tab"
            );
            assert_eq!(
                tabs.tab_title_of(tab_id),
                Some(title_before.as_str()),
                "the source tab's title must be untouched"
            );
            assert_eq!(
                tabs.script_backing_of(tab_id),
                backing_before,
                "the source tab's backing must be untouched -- copy semantics, never a retarget"
            );
        });
    }

    /// The "Somewhere else..." export dialog's starting directory must
    /// never live under the active connection's own session directory:
    /// that directory is swept by the next autosave's orphan-file prune,
    /// which would delete the export moments after its own "saved"
    /// confirmation if the dialog's default location pointed there.
    #[gpui::test]
    fn somewhere_else_export_never_starts_inside_the_sessions_directory(cx: &mut TestAppContext) {
        let (workspace, _paths, vcx) = workspace_with_active_connection("export-start-dir", cx);

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).active_tab().unwrap().id()
        });

        let observed_start_dir = std::rc::Rc::new(std::cell::RefCell::new(None));
        let observed_for_prompt = observed_start_dir.clone();
        workspace.update(vcx, |workspace, _cx| {
            workspace.set_save_file_prompt_for_test(Box::new(move |_cx, dir, _suggested| {
                *observed_for_prompt.borrow_mut() = Some(dir.to_owned());
                gpui::Task::ready(None)
            }));
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id,
                    kind: SaveModalKind::SaveAs,
                    name: "exported".to_owned(),
                    destination: Destination::External,
                },
                cx,
            );
        });
        vcx.run_until_parked();

        let start_dir = observed_start_dir
            .borrow()
            .clone()
            .expect("the save-file prompt must have been invoked with a starting directory");
        let session_dir = workspace
            .read_with(vcx, |workspace, _cx| {
                workspace.session_store.active_session_dir()
            })
            .expect("an active connection must resolve a session directory");
        assert!(
            !start_dir.starts_with(&session_dir),
            "the export dialog must never default into the session directory: {} starts \
             with {}",
            start_dir.display(),
            session_dir.display()
        );
    }

    /// "Copy to library" writes the tab's current buffer to the library
    /// under its own title and leaves the tab itself completely unchanged
    /// (kind, title, dirty state) -- copy semantics, never a retarget.
    #[gpui::test]
    fn copy_to_library_writes_a_new_library_file_and_leaves_the_tab_unchanged(
        cx: &mut TestAppContext,
    ) {
        let session = session_for_test(cx);
        let temp = SaveFlowTestPaths::new("copy-to-library");
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test("copy-to-library"),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    library_root: Some(temp.library.clone()),
                    ..Default::default()
                },
                cx,
            )
        });
        vcx.run_until_parked();

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .expect("one tab open")
                .id()
        });
        workspace.update(vcx, |workspace, cx| {
            let editor = workspace
                .tabs
                .read(cx)
                .active_tab()
                .expect("tab exists")
                .editor()
                .clone();
            editor.update(cx, |editor, cx| {
                editor.insert_text_for_test("select 1;", cx);
            });
        });

        let title_before = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .tab_title_of(tab_id)
                .expect("tab exists")
                .to_owned()
        });
        let dirty_before = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).tabs()[0].dirty()
        });
        let backing_before = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).script_backing_of(tab_id)
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.copy_tab_to_library(tab_id, cx);
        });
        vcx.run_until_parked();

        let entries = std::fs::read_dir(&temp.library)
            .expect("library dir must exist after the copy")
            .collect::<Result<Vec<_>, _>>()
            .expect("must read library dir");
        assert_eq!(
            entries.len(),
            1,
            "the library must gain exactly one new file"
        );
        let content = std::fs::read_to_string(entries[0].path()).expect("must read the new file");
        assert_eq!(content, "select 1;");

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(tabs.tab_title_of(tab_id), Some(title_before.as_str()));
            assert_eq!(tabs.tabs()[0].dirty(), dirty_before);
            assert_eq!(tabs.script_backing_of(tab_id), backing_before);
        });
    }

    /// Copy to Library never touches the `tabs` entity, so the sidebar must
    /// resync itself directly rather than relying on its usual
    /// `observe(&tabs)` hook -- the sidebar's LIBRARY rows (and Scripts
    /// count) must reflect the new file immediately, with no other tab
    /// event in between.
    #[gpui::test]
    fn copy_to_library_updates_the_sidebars_library_rows_immediately(cx: &mut TestAppContext) {
        let session = session_for_test(cx);
        let temp = SaveFlowTestPaths::new("copy-to-library-sidebar-resync");
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test("copy-to-library-sidebar-resync"),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    library_root: Some(temp.library.clone()),
                    ..Default::default()
                },
                cx,
            )
        });
        vcx.run_until_parked();

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .expect("one tab open")
                .id()
        });
        workspace.update(vcx, |workspace, cx| {
            let editor = workspace
                .tabs
                .read(cx)
                .active_tab()
                .expect("tab exists")
                .editor()
                .clone();
            editor.update(cx, |editor, cx| {
                editor.insert_text_for_test("select 1;", cx);
            });
        });
        vcx.run_until_parked();

        let rows_before = workspace.read_with(vcx, |workspace, cx| {
            workspace.sidebar.read(cx).script_rows_for_test().len()
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.copy_tab_to_library(tab_id, cx);
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            let rows = workspace.sidebar.read(cx).script_rows_for_test();
            assert_eq!(
                rows.len(),
                rows_before + 1,
                "the sidebar's script rows must include the newly copied library file \
                 immediately, with no other tab event required"
            );
        });
    }

    /// A duplicate name in the library resolves through the same
    /// counter-suffix collision rule session script names use, rather than
    /// overwriting the existing library file.
    #[gpui::test]
    fn copy_to_library_disambiguates_a_colliding_name_with_a_counter_suffix(
        cx: &mut TestAppContext,
    ) {
        let session = session_for_test(cx);
        let temp = SaveFlowTestPaths::new("copy-to-library-collision");
        session_store::LibraryDir::at(&temp.library)
            .save(
                &session_store::LibraryName::new("query-1").unwrap(),
                "select 'existing';",
            )
            .expect("must seed an existing library file");
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test("copy-to-library-collision"),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    library_root: Some(temp.library.clone()),
                    ..Default::default()
                },
                cx,
            )
        });
        vcx.run_until_parked();

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .expect("one tab open")
                .id()
        });
        workspace.update(vcx, |workspace, cx| {
            workspace.copy_tab_to_library(tab_id, cx);
        });
        vcx.run_until_parked();

        assert!(
            session_store::LibraryDir::at(&temp.library)
                .load(&session_store::LibraryName::new("query-1").unwrap())
                .unwrap()
                .is_some_and(|text| text == "select 'existing';"),
            "the pre-existing library file must be untouched"
        );
        assert!(
            session_store::LibraryDir::at(&temp.library)
                .load(&session_store::LibraryName::new("query-1-2").unwrap())
                .expect("load must succeed")
                .is_some(),
            "the colliding copy must land under a counter-suffixed name"
        );
    }

    /// Opening the tab context menu for a `Script` tab and rendering the
    /// workspace does not panic -- the full item list (Save, Save as...,
    /// Rename..., a separator, Close) builds successfully. Rendering itself
    /// cannot be visually verified headless; this is the render-smoke
    /// coverage for it.
    #[gpui::test]
    fn opening_the_script_tab_context_menu_renders_without_panicking(cx: &mut TestAppContext) {
        let session = session_for_test(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test("context-menu"),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup::default(),
                cx,
            )
        });
        vcx.run_until_parked();

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .expect("a workspace always opens with an active script tab")
                .id()
        });

        workspace.update(vcx, |workspace, cx| {
            workspace
                .tab_bar
                .open_context_menu(tab_id, point(px(10.0), px(10.0)));
            cx.notify();
        });
        vcx.run_until_parked();
    }

    /// A workspace with an active connection, its own sessions and library
    /// roots, and the default single unnamed script tab already loaded.
    fn workspace_with_active_connection<'a>(
        label: &str,
        cx: &'a mut TestAppContext,
    ) -> (
        Entity<WorkspaceView>,
        SaveFlowTestPaths,
        &'a mut gpui::VisualTestContext,
    ) {
        let session = session_for_test(cx);
        let paths = SaveFlowTestPaths::new(label);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(label),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup {
                    sessions_root: Some(paths.sessions.clone()),
                    library_root: Some(paths.library.clone()),
                    ..Default::default()
                },
                cx,
            )
        });
        vcx.run_until_parked();

        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(
                ActiveConnection {
                    id: None,
                    name: "conn".to_owned(),
                    url: "postgres://localhost/a".to_owned(),
                },
                cx,
            );
        });
        vcx.run_until_parked();

        (workspace, paths, vcx)
    }

    /// Confirming Save on an unnamed session tab with "This connection"
    /// chosen renames its `query-N.sql` file atomically to the chosen name
    /// and retitles the tab, exercising `WorkspaceView::perform_save`'s
    /// `Connection` branch end to end.
    #[gpui::test]
    fn saving_an_unnamed_session_tab_to_this_connection_renames_the_file_and_retitles_the_tab(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _paths, vcx) = workspace_with_active_connection("save-connection", cx);

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).active_tab().unwrap().id()
        });
        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .unwrap()
                .editor()
                .clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select * from orders;", cx);
        });
        vcx.run_until_parked();

        let session_dir = workspace
            .read_with(vcx, |workspace, _cx| {
                workspace.session_store.active_session_dir()
            })
            .expect("an active connection must resolve a session directory");
        assert!(
            session_dir.join("scratch").join("query-1.sql").is_file(),
            "the unnamed tab's autosave must have written scratch/query-1.sql \
             before Save promotes it"
        );

        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id,
                    kind: SaveModalKind::Save,
                    name: "orders".to_owned(),
                    destination: Destination::Connection,
                },
                cx,
            );
        });
        vcx.run_until_parked();

        assert!(
            !session_dir.join("scratch").join("query-1.sql").exists(),
            "the old scratch/query-1.sql must be gone after the promotion"
        );
        assert!(
            !session_dir.join("orders").exists(),
            "the promoted file must land as orders.sql, never an extensionless \
             stray orders -- a top-level non-.sql file is never pruned"
        );
        assert_eq!(
            std::fs::read_to_string(session_dir.join("scripts").join("orders.sql")).unwrap(),
            "select * from orders;"
        );
        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(tabs.active_tab().unwrap().title(), "orders.sql");
            assert_eq!(
                tabs.script_backing_of(tab_id),
                Some(ScriptBacking::SessionNamed {
                    file: session_store::ScriptFileName::new("orders.sql").unwrap()
                })
            );
        });
    }

    /// The ticket's own example end to end: naming a script `query-7` is
    /// legal (the reserved-name check is gone), lands the file at the
    /// session directory's top level, and a subsequent sidebar/picker
    /// listing shows it as an ordinary named script rather than filtering
    /// it out.
    #[gpui::test]
    fn saving_an_unnamed_tab_as_query_seven_succeeds_and_lists_as_an_ordinary_named_script(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _paths, vcx) = workspace_with_active_connection("save-query-seven", cx);

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).active_tab().unwrap().id()
        });
        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .unwrap()
                .editor()
                .clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select 7;", cx);
        });
        vcx.run_until_parked();

        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id,
                    kind: SaveModalKind::Save,
                    name: "query-7".to_owned(),
                    destination: Destination::Connection,
                },
                cx,
            );
        });
        vcx.run_until_parked();

        let session_dir = workspace
            .read_with(vcx, |workspace, _cx| {
                workspace.session_store.active_session_dir()
            })
            .expect("an active connection must resolve a session directory");
        assert_eq!(
            std::fs::read_to_string(session_dir.join("scripts").join("query-7.sql")).unwrap(),
            "select 7;",
            "query-7 must save at the session directory's top level like any other name"
        );

        let listed = SessionDir::at(&session_dir)
            .list_scripts()
            .expect("list must succeed");
        assert!(
            listed.iter().any(|entry| entry.file_name == "query-7.sql"),
            "query-7.sql must be listed as an ordinary named script, not filtered out: \
             {listed:?}"
        );

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(tabs.active_tab().unwrap().title(), "query-7.sql");
            assert_eq!(
                tabs.script_backing_of(tab_id),
                Some(ScriptBacking::SessionNamed {
                    file: session_store::ScriptFileName::new("query-7.sql").unwrap()
                })
            );
        });
    }

    /// Confirming Save on an unnamed session tab with "Library" chosen
    /// writes the library file, drops the tab's former session file, and
    /// converts the tab to library-backed, exercising
    /// `WorkspaceView::perform_save`'s `Library` branch end to end.
    #[gpui::test]
    fn saving_an_unnamed_session_tab_to_library_converts_the_tab_and_removes_the_old_session_file(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _paths, vcx) = workspace_with_active_connection("save-library", cx);

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).active_tab().unwrap().id()
        });
        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .unwrap()
                .editor()
                .clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select 1;", cx);
        });
        vcx.run_until_parked();

        let session_dir = workspace
            .read_with(vcx, |workspace, _cx| {
                workspace.session_store.active_session_dir()
            })
            .unwrap();
        assert!(session_dir.join("scratch").join("query-1.sql").is_file());
        let library_dir = workspace
            .read_with(vcx, |workspace, _cx| workspace.library_dir.clone())
            .unwrap();

        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id,
                    kind: SaveModalKind::Save,
                    name: "orders".to_owned(),
                    destination: Destination::Library,
                },
                cx,
            );
        });
        vcx.run_until_parked();

        assert!(
            !session_dir.join("scratch").join("query-1.sql").exists(),
            "the former session file must be gone once the tab converts to library-backed"
        );
        assert_eq!(
            session_store::LibraryDir::at(&library_dir)
                .load(&session_store::LibraryName::new("orders").unwrap())
                .unwrap(),
            Some("select 1;".to_owned())
        );
        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(tabs.active_tab().unwrap().title(), "orders.sql");
            assert!(matches!(
                tabs.script_backing_of(tab_id),
                Some(ScriptBacking::Library { name, .. }) if name.as_str() == "orders"
            ));
        });
    }

    /// End-to-end coverage for the exact regression a reopened named script
    /// must never repeat: save an unnamed tab to "This connection" under a
    /// real name, close its tab (dropping its `tabs.toml` entry), reopen it
    /// the way the sidebar/Open Script picker actually would --
    /// `TabModel::open_or_focus_session_script`, straight from disk since no
    /// tab exists for it anymore -- edit it, and confirm the SAME file is
    /// what the next autosave updates rather than a `-2.sql` sibling forked
    /// off because the reopened title's file was no longer on record.
    #[gpui::test]
    fn reopening_a_closed_named_script_and_editing_it_updates_the_same_file_not_a_fork(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _paths, vcx) = workspace_with_active_connection("reopen-same-file", cx);

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).active_tab().unwrap().id()
        });
        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .unwrap()
                .editor()
                .clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select * from customers order by revenue desc;", cx);
        });
        vcx.run_until_parked();

        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id,
                    kind: SaveModalKind::Save,
                    name: "top-customers".to_owned(),
                    destination: Destination::Connection,
                },
                cx,
            );
        });
        vcx.run_until_parked();

        let session_dir = workspace
            .read_with(vcx, |workspace, _cx| {
                workspace.session_store.active_session_dir()
            })
            .expect("an active connection must resolve a session directory");
        assert!(
            session_dir
                .join("scripts")
                .join("top-customers.sql")
                .is_file()
        );

        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.close_tab(tab_id, cx));
        });
        vcx.run_until_parked();

        let reopened_id = workspace
            .update(vcx, |workspace, cx| {
                workspace.tabs.update(cx, |tabs, cx| {
                    tabs.open_or_focus_session_script("top-customers.sql", cx)
                })
            })
            .expect("the file still exists on disk and must reopen successfully");
        vcx.run_until_parked();

        let reopened_editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .tabs()
                .iter()
                .find(|tab| tab.id() == reopened_id)
                .expect("the reopened tab must exist")
                .editor()
                .clone()
        });
        reopened_editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test(" -- edited after reopen", cx);
        });
        vcx.run_until_parked();

        assert!(
            session_dir
                .join("scripts")
                .join("top-customers.sql")
                .is_file(),
            "the same file must still exist after the reopened tab's own autosave"
        );
        assert!(
            !session_dir
                .join("scripts")
                .join("top-customers-2.sql")
                .exists(),
            "reopening and editing a closed named script must never fork a -2.sql sibling"
        );
        let content =
            std::fs::read_to_string(session_dir.join("scripts").join("top-customers.sql"))
                .expect("must read the file back");
        assert!(
            content.contains("-- edited after reopen"),
            "the edit must have landed in the SAME file the tab was reopened from: {content}"
        );
    }

    /// Save as from a session-owned tab writes a copy under the chosen
    /// destination and never touches the source tab's own title or backing.
    #[gpui::test]
    fn save_as_from_a_session_tab_writes_a_copy_and_leaves_the_source_tab_unchanged(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _paths, vcx) = workspace_with_active_connection("save-as-session", cx);

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).active_tab().unwrap().id()
        });
        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .unwrap()
                .editor()
                .clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select * from customers;", cx);
        });
        vcx.run_until_parked();
        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.apply_renamed_title(tab_id, "customers.sql".to_owned(), cx);
            });
        });

        let library_dir = workspace
            .read_with(vcx, |workspace, _cx| workspace.library_dir.clone())
            .unwrap();

        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id,
                    kind: SaveModalKind::SaveAs,
                    name: "customers-copy".to_owned(),
                    destination: Destination::Library,
                },
                cx,
            );
        });
        vcx.run_until_parked();

        assert_eq!(
            session_store::LibraryDir::at(&library_dir)
                .load(&session_store::LibraryName::new("customers-copy").unwrap())
                .unwrap(),
            Some("select * from customers;".to_owned())
        );
        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(
                tabs.active_tab().unwrap().title(),
                "customers.sql",
                "Save as must never retitle the source tab"
            );
            assert_eq!(
                tabs.script_backing_of(tab_id),
                Some(ScriptBacking::SessionNamed {
                    file: session_store::ScriptFileName::new("customers.sql").unwrap()
                }),
                "Save as must never retarget the source tab's own backing"
            );
        });
    }

    /// Save as to "This connection" opens the exported copy as its own tab:
    /// a bare `.sql` written into the session directory with no owning tab
    /// would be deleted by the very next session save's orphan prune. Being
    /// tab-backed, the copy's file must survive a subsequent full session
    /// save.
    #[gpui::test]
    fn save_as_to_this_connection_opens_the_copy_as_a_tab_whose_file_survives_the_next_save(
        cx: &mut TestAppContext,
    ) {
        let (workspace, paths, vcx) = workspace_with_active_connection("save-as-connection", cx);

        let source_id = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).active_tab().unwrap().id()
        });
        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .unwrap()
                .editor()
                .clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select * from orders;", cx);
        });
        vcx.run_until_parked();

        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id: source_id,
                    kind: SaveModalKind::SaveAs,
                    name: "orders-copy".to_owned(),
                    destination: Destination::Connection,
                },
                cx,
            );
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            let copy = tabs.active_tab().unwrap();
            assert_eq!(
                copy.title(),
                "orders-copy.sql",
                "the exported copy opens as its own active tab"
            );
            assert_ne!(copy.id(), source_id, "the source tab is never retargeted");
            assert_eq!(
                tabs.script_backing_of(copy.id()),
                Some(ScriptBacking::SessionNamed {
                    file: session_store::ScriptFileName::new("orders-copy.sql").unwrap()
                }),
                "the copy is an ordinary named session script"
            );
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.save_active_tab_session(cx);
        });
        vcx.run_until_parked();

        let copy_path = paths
            .sessions
            .join("unsaved")
            .join("scripts")
            .join("orders-copy.sql");
        assert_eq!(
            std::fs::read_to_string(&copy_path)
                .expect("the exported copy must still exist after a full session save"),
            "select * from orders;",
        );
    }

    /// Save as from a library-backed tab writes a copy under the chosen
    /// destination and never touches the source tab's own library name,
    /// saved baseline, or the library file it points at.
    #[gpui::test]
    fn save_as_from_a_library_backed_tab_writes_a_copy_and_leaves_the_source_tab_unchanged(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _paths, vcx) = workspace_with_active_connection("save-as-library", cx);

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).active_tab().unwrap().id()
        });
        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .unwrap()
                .editor()
                .clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select 1;", cx);
        });
        vcx.run_until_parked();

        let library_dir = workspace
            .read_with(vcx, |workspace, _cx| workspace.library_dir.clone())
            .unwrap();
        session_store::LibraryDir::at(&library_dir)
            .save(
                &session_store::LibraryName::new("orders").unwrap(),
                "select 1;",
            )
            .expect("seeding the library file must succeed");
        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.convert_to_library_backed(
                    tab_id,
                    "orders".to_owned(),
                    "select 1;".to_owned(),
                    cx,
                );
            });
        });

        let session_dir = workspace
            .read_with(vcx, |workspace, _cx| {
                workspace.session_store.active_session_dir()
            })
            .unwrap();

        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id,
                    kind: SaveModalKind::SaveAs,
                    name: "orders-copy".to_owned(),
                    destination: Destination::Connection,
                },
                cx,
            );
        });
        vcx.run_until_parked();

        assert_eq!(
            std::fs::read_to_string(session_dir.join("scripts").join("orders-copy.sql")).unwrap(),
            "select 1;"
        );
        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(
                tabs.active_tab().unwrap().title(),
                "orders-copy.sql",
                "the exported copy opens as its own active tab"
            );
            assert_eq!(
                tabs.tab_title_of(tab_id),
                Some("orders.sql"),
                "the source tab keeps its title"
            );
            assert!(matches!(
                tabs.script_backing_of(tab_id),
                Some(ScriptBacking::Library { name, .. }) if name.as_str() == "orders"
            ));
        });
        assert_eq!(
            session_store::LibraryDir::at(&library_dir)
                .load(&session_store::LibraryName::new("orders").unwrap())
                .unwrap(),
            Some("select 1;".to_owned()),
            "the source library file must be untouched by a Save-as copy"
        );
    }

    /// Confirming Rename on a session script atomically renames its file
    /// and retitles the tab.
    #[gpui::test]
    fn renaming_a_session_script_renames_the_file_and_retitles_the_tab(cx: &mut TestAppContext) {
        let (workspace, _paths, vcx) = workspace_with_active_connection("rename-session", cx);

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).active_tab().unwrap().id()
        });
        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .unwrap()
                .editor()
                .clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select 1;", cx);
        });
        vcx.run_until_parked();

        let session_dir = workspace
            .read_with(vcx, |workspace, _cx| {
                workspace.session_store.active_session_dir()
            })
            .unwrap();
        assert!(session_dir.join("scratch").join("query-1.sql").is_file());

        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id,
                    kind: SaveModalKind::Rename,
                    name: "customers".to_owned(),
                    destination: Destination::Connection,
                },
                cx,
            );
        });
        vcx.run_until_parked();

        assert!(!session_dir.join("scratch").join("query-1.sql").exists());
        assert!(
            !session_dir.join("customers").exists(),
            "the renamed file must land as customers.sql, never an extensionless \
             stray customers -- a top-level non-.sql file is never pruned"
        );
        assert_eq!(
            std::fs::read_to_string(session_dir.join("scripts").join("customers.sql")).unwrap(),
            "select 1;"
        );
        workspace.read_with(vcx, |workspace, cx| {
            assert_eq!(
                workspace.tabs.read(cx).active_tab().unwrap().title(),
                "customers.sql"
            );
        });
    }

    /// A second Rename, after a first promoted an unnamed tab out of
    /// `scratch/`, must resolve the tab's now-top-level file bare (no
    /// `scratch/` prefix) -- exercising `current_session_script_ref`'s
    /// `SessionNamed` branch, which every other rename test in this module
    /// starts already-unnamed and so never reaches.
    #[gpui::test]
    fn renaming_an_already_named_session_script_a_second_time_resolves_its_top_level_file(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _paths, vcx) = workspace_with_active_connection("rename-twice", cx);

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).active_tab().unwrap().id()
        });
        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .unwrap()
                .editor()
                .clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select 1;", cx);
        });
        vcx.run_until_parked();

        let session_dir = workspace
            .read_with(vcx, |workspace, _cx| {
                workspace.session_store.active_session_dir()
            })
            .unwrap();

        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id,
                    kind: SaveModalKind::Rename,
                    name: "customers".to_owned(),
                    destination: Destination::Connection,
                },
                cx,
            );
        });
        vcx.run_until_parked();
        assert!(session_dir.join("scripts").join("customers.sql").is_file());
        assert!(
            !session_dir.join("customers").exists(),
            "the renamed file must land as customers.sql, never an extensionless \
             stray customers -- a top-level non-.sql file is never pruned"
        );
        workspace.read_with(vcx, |workspace, cx| {
            assert_eq!(
                workspace.tabs.read(cx).script_backing_of(tab_id),
                Some(ScriptBacking::SessionNamed {
                    file: session_store::ScriptFileName::new("customers.sql").unwrap()
                })
            );
        });

        // The second rename: `customers.sql` is already top-level, never
        // under `scratch/`, so this exercises the branch that resolves a
        // named tab's own bare file rather than a scratch-prefixed one.
        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id,
                    kind: SaveModalKind::Rename,
                    name: "clients".to_owned(),
                    destination: Destination::Connection,
                },
                cx,
            );
        });
        vcx.run_until_parked();

        assert!(
            !session_dir.join("scripts").join("customers.sql").exists(),
            "the first name must be gone, not left behind as an orphan"
        );
        assert!(
            !session_dir.join("clients").exists(),
            "the renamed file must land as clients.sql, never an extensionless \
             stray clients -- a top-level non-.sql file is never pruned"
        );
        assert_eq!(
            std::fs::read_to_string(session_dir.join("scripts").join("clients.sql")).unwrap(),
            "select 1;"
        );
        assert!(
            !session_dir.join("scratch").join("customers.sql").exists(),
            "a rename of an already-named tab must never touch scratch/"
        );
        assert!(
            !session_dir.join("scratch").join("clients.sql").exists(),
            "a rename of an already-named tab must never touch scratch/"
        );
        workspace.read_with(vcx, |workspace, cx| {
            assert_eq!(
                workspace.tabs.read(cx).active_tab().unwrap().title(),
                "clients.sql"
            );
        });
    }

    /// A tab converted from a live Generated preview keeps its bare
    /// relation name as its title (see
    /// `renders_a_dirty_converted_script_tab_as_the_active_tab_without_panicking`)
    /// while its autosaved sibling file carries the `.sql` extension and
    /// lives under `scratch/`, since the user has not named it merely by
    /// editing a preview into a script. Renaming it must locate that real
    /// sibling file and promote it to the top level, rather than assuming
    /// the bare title itself named an existing top-level file.
    #[gpui::test]
    fn renaming_a_converted_generated_tab_promotes_its_scratch_backed_sibling_file(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _paths, vcx) =
            workspace_with_active_connection("rename-converted-generated", cx);

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
        editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select 1;", cx);
        });
        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.set_active(converted_id, cx));
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            assert_eq!(
                workspace.tabs.read(cx).active_tab().unwrap().title(),
                "orders"
            );
        });
        let session_dir = workspace
            .read_with(vcx, |workspace, _cx| {
                workspace.session_store.active_session_dir()
            })
            .unwrap();
        assert!(
            session_dir.join("scratch").join("orders.sql").is_file(),
            "the converted tab's sibling file must carry the .sql extension \
             and live under scratch/ even though its title is the bare \
             relation name"
        );
        let buffer_text = workspace
            .read_with(vcx, |workspace, cx| {
                workspace.tabs.read(cx).tab_buffer_text(converted_id, cx)
            })
            .unwrap();

        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id: converted_id,
                    kind: SaveModalKind::Rename,
                    name: "customers".to_owned(),
                    destination: Destination::Connection,
                },
                cx,
            );
        });
        vcx.run_until_parked();

        assert!(!session_dir.join("scratch").join("orders.sql").exists());
        assert_eq!(
            std::fs::read_to_string(session_dir.join("scripts").join("customers.sql")).unwrap(),
            buffer_text
        );
        workspace.read_with(vcx, |workspace, cx| {
            assert_eq!(
                workspace.tabs.read(cx).active_tab().unwrap().title(),
                "customers.sql"
            );
        });
    }

    /// Two relations sharing a bare name in different schemas both convert
    /// to unnamed, scratch-backed scripts under the identical title
    /// `orders` on first edit. Renaming one must locate and promote *that
    /// tab's own* sibling file (disambiguated to `orders-2.sql` on disk),
    /// never the other, same-titled tab's `orders.sql` -- a title alone can
    /// never tell the two apart.
    #[gpui::test]
    fn renaming_one_of_two_same_titled_converted_tabs_never_touches_the_others_file(
        cx: &mut TestAppContext,
    ) {
        let (workspace, _paths, vcx) =
            workspace_with_active_connection("rename-same-titled-converted", cx);

        let (public_id, public_editor) = workspace.update(vcx, |workspace, cx| {
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
        let (archive_id, archive_editor) = workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                let id = tabs.open_or_reuse_generated("archive", "orders", cx);
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
        public_editor.update(vcx, |editor, cx| {
            editor.set_text_for_test("");
            editor.insert_text_for_test("select * from public.orders;", cx);
        });
        archive_editor.update(vcx, |editor, cx| {
            editor.set_text_for_test("");
            editor.insert_text_for_test("select * from archive.orders;", cx);
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            assert_eq!(
                workspace.tabs.read(cx).tab_title_of(public_id),
                Some("orders")
            );
            assert_eq!(
                workspace.tabs.read(cx).tab_title_of(archive_id),
                Some("orders")
            );
        });
        let session_dir = workspace
            .read_with(vcx, |workspace, _cx| {
                workspace.session_store.active_session_dir()
            })
            .unwrap();
        let scratch = session_dir.join("scratch");
        // Edited in this order above, so `public` claims the bare name
        // first and `archive`'s otherwise-identical title disambiguates.
        assert_eq!(
            std::fs::read_to_string(scratch.join("orders.sql")).unwrap(),
            "select * from public.orders;"
        );
        assert_eq!(
            std::fs::read_to_string(scratch.join("orders-2.sql")).unwrap(),
            "select * from archive.orders;",
            "the second same-titled conversion must disambiguate to a distinct scratch file"
        );

        // Rename `archive`'s tab -- the one whose own file is the
        // disambiguated `orders-2.sql`, never the un-suffixed `orders.sql`
        // a title-only lookup would incorrectly resolve to.
        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id: archive_id,
                    kind: SaveModalKind::Rename,
                    name: "renamed".to_owned(),
                    destination: Destination::Connection,
                },
                cx,
            );
        });
        vcx.run_until_parked();

        assert_eq!(
            std::fs::read_to_string(session_dir.join("scripts").join("renamed.sql")).unwrap(),
            "select * from archive.orders;",
            "archive's own buffer must land at the promoted top-level file"
        );
        assert!(
            !scratch.join("orders-2.sql").exists(),
            "archive's own scratch file must be gone, not left behind as an orphan"
        );
        assert_eq!(
            std::fs::read_to_string(scratch.join("orders.sql")).unwrap(),
            "select * from public.orders;",
            "public's own file and content must be completely untouched by archive's rename"
        );
        workspace.read_with(vcx, |workspace, cx| {
            assert_eq!(
                workspace.tabs.read(cx).tab_title_of(archive_id),
                Some("renamed.sql")
            );
            assert_eq!(
                workspace.tabs.read(cx).tab_title_of(public_id),
                Some("orders"),
                "the other same-titled tab must be untouched by the rename"
            );
        });
    }

    /// Confirming Rename on a library-backed tab atomically renames the
    /// library file and retitles the tab, without touching its saved-text
    /// baseline (a clean tab stays clean under its new name).
    #[gpui::test]
    fn renaming_a_library_script_renames_the_file_and_retitles_the_tab(cx: &mut TestAppContext) {
        let (workspace, _paths, vcx) = workspace_with_active_connection("rename-library", cx);

        let tab_id = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).active_tab().unwrap().id()
        });
        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .unwrap()
                .editor()
                .clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select 1;", cx);
        });
        vcx.run_until_parked();

        let library_dir = workspace
            .read_with(vcx, |workspace, _cx| workspace.library_dir.clone())
            .unwrap();
        session_store::LibraryDir::at(&library_dir)
            .save(
                &session_store::LibraryName::new("orders").unwrap(),
                "select 1;",
            )
            .expect("seeding the library file must succeed");
        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.convert_to_library_backed(
                    tab_id,
                    "orders".to_owned(),
                    "select 1;".to_owned(),
                    cx,
                );
            });
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.handle_save_modal_event(
                &SaveModalEvent::Confirmed {
                    tab_id,
                    kind: SaveModalKind::Rename,
                    name: "top-orders".to_owned(),
                    destination: Destination::Library,
                },
                cx,
            );
        });
        vcx.run_until_parked();

        assert_eq!(
            session_store::LibraryDir::at(&library_dir)
                .load(&session_store::LibraryName::new("orders").unwrap())
                .unwrap(),
            None,
            "the old library name must be gone after the rename"
        );
        assert_eq!(
            session_store::LibraryDir::at(&library_dir)
                .load(&session_store::LibraryName::new("top-orders").unwrap())
                .unwrap(),
            Some("select 1;".to_owned())
        );
        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(tabs.active_tab().unwrap().title(), "top-orders.sql");
            assert!(matches!(
                tabs.script_backing_of(tab_id),
                Some(ScriptBacking::Library { name, .. }) if name.as_str() == "top-orders"
            ));
        });
    }

    /// Opening the Rename modal (a real `WorkspaceView::open_rename_modal`
    /// call, which resolves the tab's real session directory) and typing a
    /// name that already names another open, tracked tab's file must not
    /// let Enter confirm: the modal stays open and neither file on disk is
    /// touched. Both files come from real tabs' own autosave, not files
    /// planted directly on disk, since an untracked file would be pruned as
    /// an orphan by the very autosave this test relies on.
    #[gpui::test]
    fn renaming_to_a_colliding_session_name_is_blocked_and_leaves_files_untouched(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            zsql_ui::text_field::init(cx);
            crate::ui::save_modal::init(cx);
        });
        let (workspace, _paths, vcx) = workspace_with_active_connection("rename-collision", cx);

        let customers_id = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).active_tab().unwrap().id()
        });
        let customers_editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .unwrap()
                .editor()
                .clone()
        });
        // Each mutation below is flushed (`run_until_parked`) before the
        // next one fires: overlapping autosave dispatches for evolving tab
        // state can complete out of order, so waiting out each one keeps
        // this setup's on-disk result deterministic.
        customers_editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select 1;", cx);
        });
        vcx.run_until_parked();
        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.apply_renamed_title(customers_id, "customers.sql".to_owned(), cx);
            });
        });
        vcx.run_until_parked();

        let orders_id = workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, TabModel::new_script_tab)
        });
        vcx.run_until_parked();
        let orders_editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .unwrap()
                .editor()
                .clone()
        });
        orders_editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select 2;", cx);
        });
        vcx.run_until_parked();
        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.apply_renamed_title(orders_id, "orders.sql".to_owned(), cx);
            });
        });
        vcx.run_until_parked();

        let session_dir = workspace
            .read_with(vcx, |workspace, _cx| {
                workspace.session_store.active_session_dir()
            })
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(session_dir.join("scripts").join("customers.sql")).unwrap(),
            "select 1;",
            "the first tab's autosave must have written customers.sql by now"
        );
        assert_eq!(
            std::fs::read_to_string(session_dir.join("scripts").join("orders.sql")).unwrap(),
            "select 2;",
            "the second tab's autosave must have written orders.sql by now"
        );

        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.set_active(customers_id, cx));
            workspace.open_rename_modal(customers_id, cx);
        });
        vcx.run_until_parked();

        vcx.simulate_keystrokes("secondary-a");
        vcx.run_until_parked();
        vcx.simulate_input("orders");
        vcx.run_until_parked();
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            assert!(
                workspace.save_modal.read(cx).is_open(),
                "Enter on a name that collides with the other open tab's file \
                 must not confirm; the modal must stay open with the inline \
                 error shown"
            );
        });
        assert_eq!(
            std::fs::read_to_string(session_dir.join("scripts").join("customers.sql")).unwrap(),
            "select 1;"
        );
        assert_eq!(
            std::fs::read_to_string(session_dir.join("scripts").join("orders.sql")).unwrap(),
            "select 2;"
        );
    }
}

/// Ctrl+F's routing between the sidebar's own find row and the results
/// grid's quick find: hover over the sidebar wins regardless of where
/// keyboard focus sits, and the results grid's own binding is otherwise
/// untouched.
mod find_routing_tests {
    use std::time::Duration;

    use gpui::{AppContext as _, Focusable, Modifiers, TestAppContext};
    use zsql_core::{Catalog, Relation, RelationKind, SchemaNs, SchemaTree};

    use super::{WorkspaceStartup, WorkspaceView};
    use crate::config::{LayoutConfig, ValuePanelConfig};
    use crate::connections::ConnectionStore;
    use crate::session::{SchemaState, Session};

    fn empty_store_for_test() -> ConnectionStore {
        let path = std::env::temp_dir().join(format!(
            "zsql-workspace-find-routing-test-{}.toml",
            std::process::id()
        ));
        ConnectionStore::load(&path).expect("loading a nonexistent path must succeed empty")
    }

    fn sample_schema_session(cx: &mut TestAppContext) -> gpui::Entity<Session> {
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

    fn build(
        cx: &mut TestAppContext,
    ) -> (gpui::Entity<WorkspaceView>, &mut gpui::VisualTestContext) {
        cx.update(|cx| {
            zsql_editor::init(cx);
            zsql_ui::text_field::init(cx);
            crate::ui::results::init(cx, "ctrl-shift-enter");
            crate::ui::sidebar::init(cx);
        });
        let session = sample_schema_session(cx);
        cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                WorkspaceStartup::default(),
                cx,
            )
        })
    }

    #[gpui::test]
    fn ctrl_f_over_the_hovered_sidebar_opens_its_find_row_even_without_focus(
        cx: &mut TestAppContext,
    ) {
        let (workspace, vcx) = build(cx);
        vcx.run_until_parked();

        // Focus the editor, not the sidebar: this stands in for the
        // ordinary case where the user is mid-query and moves the mouse
        // over the sidebar to search it without clicking into it first.
        let editor_focus = workspace
            .read_with(vcx, WorkspaceView::editor_focus_handle)
            .expect("a workspace always opens with an active editor");
        vcx.update(|window, _cx| window.focus(&editor_focus));
        vcx.run_until_parked();

        let sidebar_bounds = vcx
            .debug_bounds("sidebar-root")
            .expect("the sidebar must be painted");
        vcx.simulate_mouse_move(sidebar_bounds.center(), None, Modifiers::default());
        vcx.run_until_parked();

        vcx.simulate_keystrokes("secondary-f");
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            assert!(
                workspace.sidebar.read(cx).find_is_open_for_test(),
                "hovering the sidebar must route Ctrl+F to its own find row"
            );
            assert!(
                !workspace.results.read(cx).quick_find_is_open_for_test(),
                "the results grid's quick find must not open while the sidebar is hovered"
            );
        });
    }

    #[gpui::test]
    fn ctrl_f_with_the_sidebar_neither_focused_nor_hovered_still_opens_the_results_quick_find(
        cx: &mut TestAppContext,
    ) {
        let (workspace, vcx) = build(cx);
        vcx.run_until_parked();

        let results_focus = workspace.read_with(vcx, |workspace, cx| {
            workspace.results.read(cx).focus_handle(cx)
        });
        vcx.update(|window, _cx| window.focus(&results_focus));
        vcx.run_until_parked();

        vcx.simulate_keystrokes("secondary-f");
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            assert!(
                workspace.results.read(cx).quick_find_is_open_for_test(),
                "Ctrl+F with the results grid focused must still open its own quick find"
            );
            assert!(
                !workspace.sidebar.read(cx).find_is_open_for_test(),
                "the sidebar's find row must not have opened"
            );
        });
    }
}
