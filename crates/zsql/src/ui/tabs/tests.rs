use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use gpui::{AppContext as _, Entity, SharedString, TestAppContext};
use zsql_core::{
    BatchSink, ColumnMeta, Connection, CoreError, PreviewQueryArgs, QueryEvent, QueryHandle,
    RowCount, SchemaTree,
};

use zsql_core::preview_state::PreviewQueryState;

use super::{
    PreviewControlsChanged, ResultsChanged, ResultsSnapshot, SaveRequested, Tab, TabKind, TabModel,
};
use crate::session::Session;
use crate::session_store::{ScriptBacking, ScriptFileName, TabEntrySnapshot, TabSessionSnapshot};
use crate::ui::results::ResultsView;
use crate::ui::results::pager::PreviewAction;

/// Test-only accessors: a tab's captured run, and its sort/page window
/// (`Some` only while `kind` is `Generated`).
impl Tab {
    pub(crate) fn last_run_for_test(&self) -> Option<&ResultsSnapshot> {
        self.last_run.as_ref()
    }

    pub(crate) fn preview_state(&self) -> &zsql_core::preview_state::PreviewQueryState {
        match &self.kind {
            TabKind::Generated { preview, .. } => preview,
            TabKind::Script { .. } | TabKind::Schema { .. } => {
                panic!("preview_state() called on a non-Generated tab in a test")
            }
        }
    }
}

/// A `Connection` double that records nothing and never resolves a
/// query -- these tests only care about the tab model's own state, not
/// what actually streams back from a database.
struct FakeConnection;

#[async_trait]
impl Connection for FakeConnection {
    fn stream_query(&self, _sql: String, _sink: BatchSink) -> QueryHandle {
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

/// A `Connection` double that hands back every `stream_query` call's
/// sink, in call order, letting a test control exactly when (and
/// whether) a dispatched run's events arrive -- unlike `FakeConnection`,
/// whose sinks a test can never reach, so its runs never resolve.
struct RecordingConnection {
    sinks: Arc<Mutex<Vec<BatchSink>>>,
    /// The relation total [`Connection::count_rows`] reports, so a
    /// pager test can page through more than a single (empty) page.
    total_rows: u64,
}

#[async_trait]
impl Connection for RecordingConnection {
    fn stream_query(&self, _sql: String, sink: BatchSink) -> QueryHandle {
        self.sinks.lock().expect("sinks lock poisoned").push(sink);
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
        Ok(RowCount::Exact(self.total_rows))
    }

    async fn describe_relation(
        &self,
        _schema: &str,
        _relation: &str,
    ) -> Result<zsql_core::RelationSchema, CoreError> {
        Ok(zsql_core::RelationSchema::default())
    }
}

/// Mirror the workspace's tab-event wiring: a standalone [`ResultsView`]
/// fed by the model's [`ResultsChanged`]/[`PreviewControlsChanged`] events,
/// plus the latest total those events would push to the footer's row count.
fn wire_display(
    model: &Entity<TabModel>,
    session: Entity<Session>,
    cx: &mut TestAppContext,
) -> (Entity<ResultsView>, Rc<RefCell<Option<RowCount>>>) {
    let results = cx.update(|cx| cx.new(|cx| ResultsView::new(session, "", cx)));
    let row_count: Rc<RefCell<Option<RowCount>>> = Rc::new(RefCell::new(None));
    let results_for_changed = results.clone();
    let results_for_controls = results.clone();
    let row_count_for_controls = row_count.clone();
    cx.update(|cx| {
        cx.subscribe(model, move |_model, evt: &ResultsChanged, cx| {
            results_for_changed.update(cx, |results, cx| match evt {
                ResultsChanged::Live(label) => results.show_live(label, cx),
                ResultsChanged::Snapshot(snap) => results.show_snapshot(snap.clone(), cx),
            });
        })
        .detach();
        cx.subscribe(model, move |_model, evt: &PreviewControlsChanged, cx| {
            results_for_controls.update(cx, |results, cx| {
                results.set_preview_controls(evt.0.clone(), cx);
            });
            *row_count_for_controls.borrow_mut() = evt
                .0
                .as_ref()
                .and_then(|controls| controls.state.total_rows());
        })
        .detach();
    });
    (results, row_count)
}

/// Like [`build_model_with_results`], but backed by a
/// [`RecordingConnection`] so a test can independently complete (or
/// leave in flight) each tab's own dispatched run, by sending directly
/// on the sink `stream_query` was called with.
fn build_model_with_recording_connection(
    cx: &mut TestAppContext,
) -> (Entity<TabModel>, Arc<Mutex<Vec<BatchSink>>>) {
    build_model_with_recording_connection_and_total_rows(cx, 0)
}

#[gpui::test]
fn re_running_a_generated_preview_tab_refreshes_the_relation_row_count(cx: &mut TestAppContext) {
    let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
    let connection: Arc<dyn Connection> = Arc::new(RecordingConnection {
        sinks,
        total_rows: 0,
    });
    let session = cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)));
    let model = cx.update(|cx| cx.new(|cx| TabModel::new(session.clone(), cx)));

    // Opening a generated preview tab fetches the relation's total row count.
    let id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();
    session.read_with(cx, |session, _cx| {
        assert_eq!(session.row_count(), Some(RowCount::Exact(0)));
    });

    // Re-running that preview tab (the Run button / RunQuery path) must
    // refresh the count, not clear it the way a plain run_query would.
    model.update(cx, |model, cx| {
        model.run_for_tab(id, "SELECT 1".to_owned(), cx);
    });
    cx.run_until_parked();
    session.read_with(cx, |session, _cx| {
        assert_eq!(
            session.row_count(),
            Some(RowCount::Exact(0)),
            "re-running a generated preview tab must refresh its relation row count"
        );
    });
}

fn build_model(cx: &mut TestAppContext) -> Entity<TabModel> {
    build_model_with_results(cx).0
}

/// Like [`build_model`], but also returns the shared `ResultsView`
/// entity so a test can assert on what it is currently showing.
fn build_model_with_results(cx: &mut TestAppContext) -> (Entity<TabModel>, Entity<ResultsView>) {
    let connection: Arc<dyn Connection> = Arc::new(FakeConnection);
    let session = cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)));
    let model = cx.update(|cx| cx.new(|cx| TabModel::new(session.clone(), cx)));
    let (results, _row_count) = wire_display(&model, session, cx);
    (model, results)
}

#[gpui::test]
fn a_generated_tab_displays_the_shared_default_preview_form(cx: &mut TestAppContext) {
    let model = build_model(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx);
    });

    model.read_with(cx, |model, app| {
        assert_eq!(
            model.tabs()[0].editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200"
        );
    });
}

/// A `Connection` double whose `preview_query` returns a form no dialect
/// this codebase ships actually emits, so a test asserting a generated
/// tab's displayed text against it can only pass if that text was truly
/// built from `Connection::preview_query` rather than a hardcoded
/// `LIMIT` string.
struct DialectRecordingConnection {
    queries: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Connection for DialectRecordingConnection {
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

    fn preview_query(&self, schema: &str, relation: &str, args: PreviewQueryArgs) -> String {
        format!("SELECT TOP ({}) * FROM [{schema}].[{relation}]", args.limit)
    }
}

/// The core of this fix: a generated tab's displayed buffer and the SQL
/// `Session::preview_relation` actually executes are built from the same
/// call, so they can never diverge -- including for a dialect (modeled
/// here by a connection whose `preview_query` looks nothing like the
/// default `LIMIT` form) where the two used to differ.
#[gpui::test]
fn a_generated_tabs_displayed_sql_matches_what_preview_relation_executes(cx: &mut TestAppContext) {
    let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let connection: Arc<dyn Connection> = Arc::new(DialectRecordingConnection {
        queries: queries.clone(),
    });
    let session = cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)));
    let model = cx.update(|cx| cx.new(|cx| TabModel::new(session, cx)));

    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("dbo", "orders", cx);
    });
    cx.run_until_parked();

    let displayed = model.read_with(cx, |model, app| {
        model.active_tab().unwrap().editor().read(app).text()
    });
    let executed = queries
        .lock()
        .expect("queries lock poisoned")
        .first()
        .cloned()
        .expect("opening a generated tab must dispatch exactly one query");

    assert_eq!(displayed, executed);
    assert_eq!(displayed, "SELECT TOP (200) * FROM [dbo].[orders]");
}

#[gpui::test]
fn opening_a_relation_creates_one_generated_tab_and_activates_it(cx: &mut TestAppContext) {
    let model = build_model(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx);
    });

    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs().len(), 1);
        let tab = &model.tabs()[0];
        assert!(matches!(
            tab.kind(),
            TabKind::Generated { schema, relation, .. }
                if schema == "public" && relation == "orders"
        ));
        assert_eq!(tab.title(), "orders");
        assert!(!tab.dirty());
        assert_eq!(model.active_id(), Some(tab.id()));
    });
}

#[gpui::test]
fn reopening_the_same_relation_reuses_the_tab_instead_of_duplicating(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let first_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });

    // Focus a different tab first so reopening has to actively
    // re-focus, not just happen to already be active.
    model.update(cx, |model, cx| {
        model.new_script_tab(cx);
    });
    let second_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });

    assert_eq!(first_id, second_id);
    model.read_with(cx, |model, _app| {
        assert_eq!(
            model.tabs().len(),
            2,
            "reopening must not create a duplicate"
        );
        assert_eq!(model.active_id(), Some(first_id));
    });
}

#[gpui::test]
fn opening_two_different_relations_creates_two_generated_tabs(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let orders_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    let users_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "users", cx)
    });

    assert_ne!(orders_id, users_id);
    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs().len(), 2);
        assert_eq!(model.active_id(), Some(users_id));
    });
}

#[gpui::test]
fn editing_a_generated_tab_converts_it_to_a_script_permanently(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    let editor = model.read_with(cx, |model, _app| model.tabs()[0].editor().clone());

    editor.update(cx, |editor, cx| editor.insert_text_for_test("x", cx));

    model.read_with(cx, |model, app| {
        let tab = model.tabs().iter().find(|tab| tab.id() == id).unwrap();
        assert!(matches!(tab.kind(), TabKind::Script { .. }));
        assert!(tab.dirty());
        assert_eq!(tab.title(), "orders", "conversion keeps the original title");
        assert!(!tab.editor().read(app).is_compact());
    });
}

#[gpui::test]
fn reopening_a_relation_whose_tab_was_edited_creates_a_new_generated_tab(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let first_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    let first_editor = model.read_with(cx, |model, _app| model.tabs()[0].editor().clone());
    first_editor.update(cx, |editor, cx| editor.insert_text_for_test("x", cx));

    let second_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });

    assert_ne!(first_id, second_id, "a converted tab must not be reused");
    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs().len(), 2);
        let first_tab = model
            .tabs()
            .iter()
            .find(|tab| tab.id() == first_id)
            .unwrap();
        assert!(
            matches!(first_tab.kind(), TabKind::Script { .. }),
            "the old, edited tab is left untouched as a script"
        );
        let second_tab = model
            .tabs()
            .iter()
            .find(|tab| tab.id() == second_id)
            .unwrap();
        assert!(matches!(
            second_tab.kind(),
            TabKind::Generated { schema, relation, .. }
                if schema == "public" && relation == "orders"
        ));
        assert_eq!(model.active_id(), Some(second_id));
    });
}

#[gpui::test]
fn editing_back_to_the_original_generated_sql_does_not_revert_to_generated(
    cx: &mut TestAppContext,
) {
    let model = build_model(cx);
    let id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    let editor = model.read_with(cx, |model, _app| model.tabs()[0].editor().clone());
    let original_sql = editor.read_with(cx, |editor, _app| editor.text());

    editor.update(cx, |editor, cx| editor.insert_text_for_test("x", cx));
    editor.update(cx, |editor, cx| editor.set_text(&original_sql, cx));

    model.read_with(cx, |model, _app| {
        let tab = model.tabs().iter().find(|tab| tab.id() == id).unwrap();
        assert!(
            matches!(tab.kind(), TabKind::Script { .. }),
            "recreating the original SQL text must not un-convert the tab"
        );
    });
}

#[gpui::test]
fn new_script_tab_opens_empty_and_active_with_a_numbered_title(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let id = model.update(cx, TabModel::new_script_tab);

    model.read_with(cx, |model, app| {
        let tab = model.tabs().iter().find(|tab| tab.id() == id).unwrap();
        assert!(matches!(tab.kind(), TabKind::Script { .. }));
        assert_eq!(tab.title(), "query-1.sql");
        assert!(!tab.dirty());
        assert_eq!(tab.editor().read(app).text(), "");
        assert_eq!(model.active_id(), Some(id));
    });

    let second_id = model.update(cx, TabModel::new_script_tab);
    model.read_with(cx, |model, _app| {
        let tab = model
            .tabs()
            .iter()
            .find(|tab| tab.id() == second_id)
            .unwrap();
        assert_eq!(tab.title(), "query-2.sql");
    });
}

#[gpui::test]
fn named_open_scripts_by_file_excludes_scratch_tabs_and_includes_any_named_title(
    cx: &mut TestAppContext,
) {
    let model = build_model(cx);
    // Editing a generated tab converts it to a scratch-backed script whose
    // title is the plain relation name -- the exact shape that must stay out
    // of the named listing despite not looking like an unnamed script.
    let converted_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    let editor = model.read_with(cx, |model, _app| model.tabs()[0].editor().clone());
    editor.update(cx, |editor, cx| editor.insert_text_for_test("x", cx));

    let named_id = model.update(cx, TabModel::new_script_tab);
    model.update(cx, |model, cx| {
        model.apply_renamed_title(named_id, "query-7.sql".to_owned(), cx);
    });

    model.read_with(cx, |model, _app| {
        let converted = model
            .tabs()
            .iter()
            .find(|tab| tab.id() == converted_id)
            .unwrap();
        assert!(
            matches!(converted.kind(), TabKind::Script { .. }),
            "the edit must convert it"
        );
        assert_eq!(
            model.named_open_scripts_by_file(),
            vec![("query-7.sql".to_owned(), named_id)],
            "a scratch-backed tab stays out of the named listing regardless of how plain \
             its title looks, while a promoted tab is listed even under a title matching \
             the legacy unnamed pattern"
        );
    });
}

#[gpui::test]
fn closing_the_active_tab_focuses_the_tab_that_slides_into_its_place(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let first = model.update(cx, TabModel::new_script_tab);
    let second = model.update(cx, TabModel::new_script_tab);
    let third = model.update(cx, TabModel::new_script_tab);
    model.update(cx, |model, cx| model.set_active(second, cx));

    model.update(cx, |model, cx| model.close_tab(second, cx));

    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs().len(), 2);
        assert_eq!(
            model.active_id(),
            Some(third),
            "closing the active tab focuses the tab that took its slot"
        );
        assert!(model.tabs().iter().any(|tab| tab.id() == first));
        assert!(model.tabs().iter().any(|tab| tab.id() == third));
    });
}

#[gpui::test]
fn closing_the_last_tab_leaves_no_tab_active(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let only = model.update(cx, TabModel::new_script_tab);

    model.update(cx, |model, cx| model.close_tab(only, cx));

    model.read_with(cx, |model, _app| {
        assert!(model.tabs().is_empty());
        assert_eq!(model.active_id(), None);
    });
}

#[gpui::test]
fn closing_an_inactive_tab_leaves_the_active_tab_unchanged(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let first = model.update(cx, TabModel::new_script_tab);
    let second = model.update(cx, TabModel::new_script_tab);
    model.update(cx, |model, cx| model.set_active(first, cx));

    model.update(cx, |model, cx| model.close_tab(second, cx));

    model.read_with(cx, |model, _app| {
        assert_eq!(model.active_id(), Some(first));
        assert_eq!(model.tabs().len(), 1);
    });
}

#[gpui::test]
fn closing_a_live_generated_tab_frees_its_relation_for_reuse(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let first_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });

    model.update(cx, |model, cx| model.close_tab(first_id, cx));
    let second_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });

    assert_ne!(
        first_id, second_id,
        "the relation's map entry must have been freed by closing its tab"
    );
    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs().len(), 1);
    });
}

#[gpui::test]
fn opening_a_relation_schema_creates_one_schema_tab_and_activates_it(cx: &mut TestAppContext) {
    let model = build_model(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_schema("public", "orders", zsql_core::RelationKind::Table, cx);
    });

    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs().len(), 1);
        let tab = &model.tabs()[0];
        assert_eq!(
            tab.kind(),
            &TabKind::Schema {
                schema: "public".to_owned(),
                relation: "orders".to_owned()
            }
        );
        assert_eq!(tab.title(), "orders");
        assert!(!tab.dirty(), "a schema tab is never dirty");
        assert!(tab.schema_view().is_some());
        assert_eq!(model.active_id(), Some(tab.id()));
    });
}

#[gpui::test]
fn reopening_the_same_relation_schema_reuses_the_tab_instead_of_duplicating(
    cx: &mut TestAppContext,
) {
    let model = build_model(cx);
    let first_id = model.update(cx, |model, cx| {
        model.open_or_reuse_schema("public", "orders", zsql_core::RelationKind::Table, cx)
    });

    model.update(cx, |model, cx| {
        model.new_script_tab(cx);
    });
    let second_id = model.update(cx, |model, cx| {
        model.open_or_reuse_schema("public", "orders", zsql_core::RelationKind::Table, cx)
    });

    assert_eq!(first_id, second_id);
    model.read_with(cx, |model, _app| {
        assert_eq!(
            model.tabs().len(),
            2,
            "reopening must not create a duplicate schema tab"
        );
        assert_eq!(model.active_id(), Some(first_id));
    });
}

#[gpui::test]
fn opening_a_relation_schema_and_a_relation_preview_creates_two_distinct_tabs(
    cx: &mut TestAppContext,
) {
    let model = build_model(cx);
    let generated_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    let schema_id = model.update(cx, |model, cx| {
        model.open_or_reuse_schema("public", "orders", zsql_core::RelationKind::Table, cx)
    });

    assert_ne!(generated_id, schema_id);
    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs().len(), 2);
    });
}

#[gpui::test]
fn closing_an_open_schema_tab_frees_its_relation_for_reuse(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let first_id = model.update(cx, |model, cx| {
        model.open_or_reuse_schema("public", "orders", zsql_core::RelationKind::Table, cx)
    });

    model.update(cx, |model, cx| model.close_tab(first_id, cx));
    let second_id = model.update(cx, |model, cx| {
        model.open_or_reuse_schema("public", "orders", zsql_core::RelationKind::Table, cx)
    });

    assert_ne!(
        first_id, second_id,
        "the relation's schema-tab map entry must have been freed by closing its tab"
    );
    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs().len(), 1);
    });
}

#[gpui::test]
fn opening_a_second_schema_tab_while_the_first_describe_is_in_flight_does_not_panic(
    cx: &mut TestAppContext,
) {
    let model = build_model_with_recording_connection(cx).0;
    model.update(cx, |model, cx| {
        model.open_or_reuse_schema("public", "orders", zsql_core::RelationKind::Table, cx);
    });
    model.update(cx, |model, cx| {
        model.open_or_reuse_schema("public", "users", zsql_core::RelationKind::Table, cx);
    });
    cx.run_until_parked();

    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs().len(), 2);
    });
}

#[gpui::test]
fn switching_the_active_tab_does_not_touch_either_tabs_text_or_dirty_state(
    cx: &mut TestAppContext,
) {
    let model = build_model(cx);
    let generated_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    let script_id = model.update(cx, TabModel::new_script_tab);
    let script_editor = model.read_with(cx, |model, _app| {
        model.active_tab().unwrap().editor().clone()
    });
    script_editor.update(cx, |editor, cx| editor.insert_text_for_test("select 1", cx));

    model.update(cx, |model, cx| model.set_active(generated_id, cx));
    model.update(cx, |model, cx| model.set_active(script_id, cx));

    model.read_with(cx, |model, app| {
        assert_eq!(model.active_id(), Some(script_id));
        let generated_tab = model
            .tabs()
            .iter()
            .find(|tab| tab.id() == generated_id)
            .unwrap();
        let script_tab = model
            .tabs()
            .iter()
            .find(|tab| tab.id() == script_id)
            .unwrap();
        assert_eq!(
            generated_tab.editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200"
        );
        assert!(!generated_tab.dirty());
        assert_eq!(script_tab.editor().read(app).text(), "select 1");
        assert!(script_tab.dirty());
    });
}

#[gpui::test]
fn setting_active_to_an_unknown_id_is_a_noop(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let id = model.update(cx, TabModel::new_script_tab);

    model.update(cx, |model, cx| model.set_active(9999, cx));

    model.read_with(cx, |model, _app| {
        assert_eq!(model.active_id(), Some(id));
    });
}

#[gpui::test]
fn closing_an_unknown_id_is_a_noop(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let id = model.update(cx, TabModel::new_script_tab);

    model.update(cx, |model, cx| model.close_tab(9999, cx));

    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs().len(), 1);
        assert_eq!(model.active_id(), Some(id));
    });
}

#[gpui::test]
fn opening_a_generated_tab_shows_it_live_then_captures_its_finished_run(cx: &mut TestAppContext) {
    let (model, results) = build_model_with_results(cx);
    let id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });

    results.read_with(cx, |results, _app| {
        assert_eq!(results.source_label_for_test(), "public.orders");
        assert!(
            !results.is_frozen_for_test(),
            "the tab whose query session is running must be shown live"
        );
    });

    cx.run_until_parked();

    model.read_with(cx, |model, _app| {
        let tab = model.tabs().iter().find(|tab| tab.id() == id).unwrap();
        assert_eq!(
            tab.last_run_for_test().map(|run| run.source_label.clone()),
            Some(SharedString::from("public.orders")),
            "the finished run must be captured onto its own tab"
        );
    });
}

#[gpui::test]
fn switching_to_a_tab_that_has_never_run_shows_an_empty_placeholder(cx: &mut TestAppContext) {
    let (model, results) = build_model_with_results(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    model.update(cx, TabModel::new_script_tab);

    results.read_with(cx, |results, _app| {
        assert_eq!(
            results.source_label_for_test(),
            "query",
            "a never-run script tab must not keep showing another tab's label"
        );
        assert!(
            results.is_frozen_for_test(),
            "a never-run tab is not the one the session is running for, so it is frozen"
        );
    });
}

#[gpui::test]
fn reopening_a_relation_whose_tab_lost_live_ownership_restores_its_own_snapshot(
    cx: &mut TestAppContext,
) {
    let (model, results) = build_model_with_results(cx);
    let orders_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    // Running a different tab's query hands live ownership of the
    // shared session to it, so the generated tab's own display now has
    // to come from its captured snapshot rather than the session.
    let script_id = model.update(cx, TabModel::new_script_tab);
    model.update(cx, |model, cx| {
        model.run_for_tab(script_id, "select 1".to_owned(), cx);
    });
    cx.run_until_parked();
    results.read_with(cx, |results, _app| {
        assert_eq!(results.source_label_for_test(), "query");
    });

    let reopened_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });

    assert_eq!(
        orders_id, reopened_id,
        "the relation's live generated tab must still be reused"
    );
    results.read_with(cx, |results, _app| {
        assert_eq!(
            results.source_label_for_test(),
            "public.orders",
            "reopening must restore the relation tab's own results, not the \
             script tab's"
        );
        assert!(
            results.is_frozen_for_test(),
            "the relation tab is no longer the session's live owner, so its \
             restored display is a frozen snapshot"
        );
    });
}

#[gpui::test]
fn a_superseded_runs_late_completion_does_not_overwrite_the_new_owners_last_run(
    cx: &mut TestAppContext,
) {
    let (model, sinks) = build_model_with_recording_connection(cx);

    let orders_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    // Opening a second relation's generated tab dispatches its own run
    // before "orders"'s has reached a terminal state, superseding it.
    let users_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "users", cx)
    });
    cx.run_until_parked();

    let (orders_sink, users_sink) = {
        let sinks = sinks.lock().expect("sinks lock poisoned");
        assert_eq!(sinks.len(), 2, "expected exactly two stream_query calls");
        (sinks[0].clone(), sinks[1].clone())
    };

    // "users" (the current owner) finishes first.
    users_sink
        .send(Ok(QueryEvent::Columns(vec![ColumnMeta {
            name: "id".to_owned(),
            type_name: "int4".to_owned(),
            nullable: false,
        }])))
        .expect("users sink send failed");
    users_sink
        .send(Ok(QueryEvent::Done { affected: None }))
        .expect("users sink send failed");
    cx.run_until_parked();

    // A stale event now drains out of "orders"'s own, already-superseded
    // channel -- exactly enough to unblock its task per
    // `Session::run_query`'s own generation check, without it ever
    // reaching a terminal state of its own.
    orders_sink
        .send(Ok(QueryEvent::Columns(vec![ColumnMeta {
            name: "stale".to_owned(),
            type_name: "text".to_owned(),
            nullable: true,
        }])))
        .expect("orders sink send failed");
    cx.run_until_parked();

    model.read_with(cx, |model, _app| {
        let orders_tab = model
            .tabs()
            .iter()
            .find(|tab| tab.id() == orders_id)
            .unwrap();
        assert!(
            orders_tab.last_run_for_test().is_none(),
            "a superseded tab's late, stale completion must not capture the \
             current live owner's results under its own label"
        );
        let users_tab = model
            .tabs()
            .iter()
            .find(|tab| tab.id() == users_id)
            .unwrap();
        assert_eq!(
            users_tab
                .last_run_for_test()
                .map(|run| run.source_label.clone()),
            Some(SharedString::from("public.users")),
            "the actual live owner's run must still be captured onto its own tab"
        );
    });
}

// ---- tab session snapshot / restore ------------------------------------

fn two_tab_snapshot() -> TabSessionSnapshot {
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
                    backing: ScriptBacking::SessionScratch {
                        file: ScriptFileName::new("query-1.sql").unwrap(),
                    },
                },
                title: "query-1.sql".to_owned(),
                buffer_text: Some("select 1;\n".to_owned()),
            },
        ],
        active_index: Some(1),
    }
}

#[gpui::test]
fn snapshot_captures_every_tabs_kind_title_buffer_and_the_active_index(cx: &mut TestAppContext) {
    let model = build_model(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx);
    });
    let script_id = model.update(cx, TabModel::new_script_tab);
    let editor = model.read_with(cx, |model, _app| {
        model
            .tabs()
            .iter()
            .find(|tab| tab.id() == script_id)
            .unwrap()
            .editor()
            .clone()
    });
    editor.update(cx, |editor, cx| {
        editor.insert_text_for_test("select 1;", cx);
    });

    let snapshot = model.read_with(cx, TabModel::snapshot);

    assert_eq!(snapshot.tabs.len(), 2);
    assert_eq!(
        snapshot.tabs[0].kind,
        TabKind::Generated {
            schema: "public".to_owned(),
            relation: "orders".to_owned(),
            preview: PreviewQueryState::new(200),
        },
        "a freshly opened generated tab's captured preview state matches a \
         fresh, unsorted, unfiltered page one"
    );
    assert_eq!(snapshot.tabs[0].title, "orders");
    assert_eq!(
        snapshot.tabs[1].kind,
        TabKind::Script {
            backing: ScriptBacking::SessionScratch {
                file: ScriptFileName::new("query-1.sql").unwrap(),
            },
        }
    );
    assert_eq!(snapshot.tabs[1].buffer_text.as_deref(), Some("select 1;"));
    assert_eq!(snapshot.tabs[1].title, "query-1.sql");
    assert_eq!(
        snapshot.active_index,
        Some(1),
        "the active tab is the script tab, at index 1"
    );
}

#[gpui::test]
fn a_top_level_backed_tab_titled_exactly_like_an_unnamed_scripts_own_title_is_session_named(
    cx: &mut TestAppContext,
) {
    // Pins the ticket's own example: a script whose title happens to look
    // exactly like an unnamed tab's own minted title (`query-7.sql`) but
    // whose backing lives at the session directory's top level is simply a
    // named script -- classification comes from the tab's structural
    // backing marker, never from a text pattern applied to `title`.
    let model = build_model(cx);
    let id = model.update(cx, |model, cx| {
        model.new_script_tab_with_content("query-7.sql".to_owned(), "select 1;", cx)
    });

    model.read_with(cx, |model, _app| {
        assert_eq!(
            model.script_backing_of(id),
            Some(ScriptBacking::SessionNamed {
                file: ScriptFileName::new("query-7.sql").unwrap()
            })
        );
    });
}

#[gpui::test]
fn restoring_a_script_entry_classifies_it_from_the_persisted_unnamed_marker_not_its_title(
    cx: &mut TestAppContext,
) {
    let model = build_model(cx);
    let snapshot = TabSessionSnapshot {
        tabs: vec![
            TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: ScriptBacking::SessionScratch {
                        file: ScriptFileName::new("query-1.sql").unwrap(),
                    },
                },
                title: "query-1.sql".to_owned(),
                buffer_text: Some("select 1;".to_owned()),
            },
            TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: ScriptBacking::SessionNamed {
                        file: ScriptFileName::new("query-7.sql").unwrap(),
                    },
                },
                title: "query-7.sql".to_owned(),
                buffer_text: Some("select 7;".to_owned()),
            },
        ],
        active_index: Some(0),
    };

    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&snapshot), cx);
    });

    model.read_with(cx, |model, _app| {
        let unnamed_id = model.tabs()[0].id();
        let named_id = model.tabs()[1].id();
        assert_eq!(
            model.script_backing_of(unnamed_id),
            Some(ScriptBacking::SessionScratch {
                file: ScriptFileName::new("query-1.sql").unwrap()
            })
        );
        assert_eq!(
            model.script_backing_of(named_id),
            Some(ScriptBacking::SessionNamed {
                file: ScriptFileName::new("query-7.sql").unwrap()
            }),
            "a restored query-7.sql entry marked unnamed: false must classify as named, \
             matching its persisted location rather than its title text"
        );
    });
}

#[gpui::test]
fn two_relations_sharing_a_bare_name_convert_to_distinct_session_files_under_the_same_title(
    cx: &mut TestAppContext,
) {
    // `public.orders` and `archive.orders` both convert to unnamed scripts
    // keeping the bare relation name `orders` as their shared display
    // title -- but a rename must be able to tell them apart, so each must
    // land on its own distinct sibling file the instant it converts, never
    // waiting for a save round trip to discover the collision.
    let model = build_model(cx);

    let public_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    let public_editor = model.read_with(cx, |model, _app| {
        model
            .tabs()
            .iter()
            .find(|tab| tab.id() == public_id)
            .unwrap()
            .editor()
            .clone()
    });
    public_editor.update(cx, |editor, cx| {
        editor.insert_text_for_test("select 1;", cx);
    });

    let archive_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("archive", "orders", cx)
    });
    let archive_editor = model.read_with(cx, |model, _app| {
        model
            .tabs()
            .iter()
            .find(|tab| tab.id() == archive_id)
            .unwrap()
            .editor()
            .clone()
    });
    archive_editor.update(cx, |editor, cx| {
        editor.insert_text_for_test("select 2;", cx);
    });

    model.read_with(cx, |model, _app| {
        assert_eq!(model.tab_title_of(public_id), Some("orders"));
        assert_eq!(model.tab_title_of(archive_id), Some("orders"));
        let public_file = model
            .script_backing_of(public_id)
            .and_then(|b| b.session_file().map(|f| f.as_str().to_owned()))
            .unwrap();
        let archive_file = model
            .script_backing_of(archive_id)
            .and_then(|b| b.session_file().map(|f| f.as_str().to_owned()))
            .unwrap();
        assert_ne!(
            public_file, archive_file,
            "two same-titled unnamed tabs must never share a session file"
        );
        assert_eq!(public_file, "orders.sql");
        assert_eq!(archive_file, "orders-2.sql");
    });
}

#[gpui::test]
fn restoring_a_snapshot_rebuilds_the_expected_tabs_order_and_active_tab(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let snapshot = two_tab_snapshot();

    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&snapshot), cx);
    });

    model.read_with(cx, |model, app| {
        assert_eq!(model.tabs().len(), 2);
        assert!(matches!(
            model.tabs()[0].kind(),
            TabKind::Generated {
                schema,
                relation,
                ..
            } if schema == "public" && relation == "orders"
        ));
        assert_eq!(model.tabs()[0].title(), "orders");
        assert_eq!(
            model.tabs()[0].editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200"
        );
        assert!(!model.tabs()[0].dirty());

        assert!(matches!(model.tabs()[1].kind(), TabKind::Script { .. }));
        assert_eq!(model.tabs()[1].title(), "query-1.sql");
        assert_eq!(model.tabs()[1].editor().read(app).text(), "select 1;\n");

        assert_eq!(
            model.active_id(),
            Some(model.tabs()[1].id()),
            "the active tab must be the one at the snapshot's active_index"
        );
    });
}

#[gpui::test]
fn restoring_a_snapshot_with_no_active_index_activates_the_first_tab(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let mut snapshot = two_tab_snapshot();
    snapshot.active_index = None;

    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&snapshot), cx);
    });

    model.read_with(cx, |model, _app| {
        assert_eq!(model.active_id(), Some(model.tabs()[0].id()));
    });
}

#[gpui::test]
fn restoring_a_snapshot_with_an_out_of_range_active_index_activates_the_first_tab(
    cx: &mut TestAppContext,
) {
    let model = build_model(cx);
    let mut snapshot = two_tab_snapshot();
    snapshot.active_index = Some(99);

    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&snapshot), cx);
    });

    model.read_with(cx, |model, _app| {
        assert_eq!(model.active_id(), Some(model.tabs()[0].id()));
    });
}

#[gpui::test]
fn a_new_tab_after_restoring_query_1_sql_gets_a_distinct_title(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let snapshot = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: ScriptBacking::SessionScratch {
                    file: ScriptFileName::new("query-1.sql").unwrap(),
                },
            },
            title: "query-1.sql".to_owned(),
            buffer_text: Some(String::new()),
        }],
        active_index: Some(0),
    };

    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&snapshot), cx);
    });
    let new_id = model.update(cx, TabModel::new_script_tab);

    model.read_with(cx, |model, _app| {
        let new_tab = model.tabs().iter().find(|tab| tab.id() == new_id).unwrap();
        assert_ne!(
            new_tab.title(),
            "query-1.sql",
            "a new tab must not collide with a restored title"
        );
        assert_eq!(new_tab.title(), "query-2.sql");
    });
}

#[gpui::test]
fn connecting_with_no_snapshot_resets_script_numbering_to_one(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let snapshot = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: ScriptBacking::SessionScratch {
                    file: ScriptFileName::new("query-5.sql").unwrap(),
                },
            },
            title: "query-5.sql".to_owned(),
            buffer_text: Some(String::new()),
        }],
        active_index: Some(0),
    };
    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&snapshot), cx);
    });

    model.update(cx, |model, cx| {
        model.load_for_connection(None, cx);
    });

    model.read_with(cx, |model, _app| {
        assert_eq!(
            model.tabs()[0].title(),
            "query-1.sql",
            "a snapshot-less connection must not carry over a prior connection's \
             script numbering"
        );
    });
}

#[gpui::test]
fn a_restored_unedited_generated_tab_stays_eligible_for_reuse(cx: &mut TestAppContext) {
    let model = build_model(cx);
    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&two_tab_snapshot()), cx);
    });
    let restored_id = model.read_with(cx, |model, _app| model.tabs()[0].id());

    let reused_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });

    assert_eq!(
        restored_id, reused_id,
        "a restored, never-edited generated tab must still be reused rather \
         than duplicated"
    );
}

#[gpui::test]
fn restoring_a_snapshot_never_dispatches_a_query(cx: &mut TestAppContext) {
    let (model, results) = build_model_with_results(cx);

    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&two_tab_snapshot()), cx);
    });
    cx.run_until_parked();

    results.read_with(cx, |results, _app| {
        assert!(
            results.is_frozen_for_test(),
            "restoring tabs must never leave the results view tracking a live \
             session run"
        );
    });
}

#[gpui::test]
fn connecting_with_no_snapshot_yields_the_default_single_empty_script_tab(cx: &mut TestAppContext) {
    let model = build_model(cx);

    model.update(cx, |model, cx| {
        model.load_for_connection(None, cx);
    });

    model.read_with(cx, |model, app| {
        assert_eq!(model.tabs().len(), 1);
        let tab = &model.tabs()[0];
        assert!(matches!(tab.kind(), TabKind::Script { .. }));
        assert_eq!(tab.editor().read(app).text(), "");
        assert_eq!(model.active_id(), Some(tab.id()));
    });
}

#[gpui::test]
fn switching_to_a_connection_with_no_snapshot_after_one_with_tabs_replaces_them(
    cx: &mut TestAppContext,
) {
    let model = build_model(cx);
    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&two_tab_snapshot()), cx);
    });
    model.read_with(cx, |model, _app| assert_eq!(model.tabs().len(), 2));

    model.update(cx, |model, cx| {
        model.load_for_connection(None, cx);
    });

    model.read_with(cx, |model, app| {
        assert_eq!(
            model.tabs().len(),
            1,
            "switching connections must replace, not merge with, the prior tab set"
        );
        assert!(matches!(model.tabs()[0].kind(), TabKind::Script { .. }));
        assert_eq!(model.tabs()[0].editor().read(app).text(), "");
    });
}

#[gpui::test]
fn switching_between_two_connections_snapshots_swaps_the_whole_tab_set(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let snapshot_a = two_tab_snapshot();
    let snapshot_b = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: ScriptBacking::SessionNamed {
                    file: ScriptFileName::new("b-query.sql").unwrap(),
                },
            },
            title: "b-query.sql".to_owned(),
            buffer_text: Some("select 'b';".to_owned()),
        }],
        active_index: Some(0),
    };

    // Connect to A, then mutate its tabs beyond what the snapshot held.
    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&snapshot_a), cx);
        model.new_script_tab(cx);
    });
    model.read_with(cx, |model, _app| assert_eq!(model.tabs().len(), 3));

    // Switch to B: the mutated A tab set must not leak through.
    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&snapshot_b), cx);
    });

    model.read_with(cx, |model, app| {
        assert_eq!(model.tabs().len(), 1, "B's tab set must fully replace A's");
        assert_eq!(model.tabs()[0].title(), "b-query.sql");
        assert_eq!(model.tabs()[0].editor().read(app).text(), "select 'b';");
        assert_eq!(model.active_id(), Some(model.tabs()[0].id()));
    });
}

/// Like [`build_model_with_results`], but backed by a
/// [`RecordingConnection`] so a test can independently complete (or
/// leave in flight) each tab's own dispatched run, by sending directly
/// on the sink `stream_query` was called with. `total_rows` seeds every
/// relation's reported row count, so a pager test has more than one
/// page to step through.
fn build_model_with_recording_connection_and_total_rows(
    cx: &mut TestAppContext,
    total_rows: u64,
) -> (Entity<TabModel>, Arc<Mutex<Vec<BatchSink>>>) {
    let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
    let connection: Arc<dyn Connection> = Arc::new(RecordingConnection {
        sinks: sinks.clone(),
        total_rows,
    });
    let session = cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)));
    let model = cx.update(|cx| cx.new(|cx| TabModel::new(session, cx)));
    (model, sinks)
}

/// Sort/pager clicks apply to the tab active when `action` fires,
/// exactly what the pager and header UI reach through
/// [`TabModel::preview_dispatch`].
fn dispatch(model: &Entity<TabModel>, cx: &mut TestAppContext, action: PreviewAction) {
    model.update(cx, |model, cx| model.dispatch_preview_action(action, cx));
    cx.run_until_parked();
}

#[gpui::test]
fn clicking_a_new_column_sorts_ascending_rewrites_the_buffer_and_reruns(cx: &mut TestAppContext) {
    let (model, sinks) = build_model_with_recording_connection(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();
    let runs_before = sinks.lock().expect("sinks lock poisoned").len();

    dispatch(&model, cx, PreviewAction::Sort("total_cents".to_owned()));

    model.read_with(cx, |model, app| {
        let tab = &model.tabs()[0];
        assert_eq!(tab.preview_state().sort_column(), Some("total_cents"));
        assert_eq!(
            tab.preview_state().sort_direction(),
            zsql_core::SortDirection::Asc,
            "a fresh sort on a new column starts ascending"
        );
        assert_eq!(
            tab.editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" ORDER BY \"total_cents\" ASC LIMIT 200",
            "the editor buffer must equal exactly the SQL that was rerun"
        );
    });
    assert_eq!(
        sinks.lock().expect("sinks lock poisoned").len(),
        runs_before + 1,
        "sorting a live generated tab must re-run its query"
    );
}

#[gpui::test]
fn clicking_the_already_active_sort_column_flips_direction(cx: &mut TestAppContext) {
    let (model, _sinks) = build_model_with_recording_connection(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    dispatch(&model, cx, PreviewAction::Sort("id".to_owned()));
    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs()[0].preview_state().sort_column(), Some("id"));
    });

    dispatch(&model, cx, PreviewAction::Sort("id".to_owned()));
    model.read_with(cx, |model, app| {
        let tab = &model.tabs()[0];
        assert_eq!(tab.preview_state().sort_column(), Some("id"));
        assert_eq!(
            tab.editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" ORDER BY \"id\" DESC LIMIT 200",
            "a second click on the active column must flip ASC to DESC"
        );
    });
}

#[gpui::test]
fn each_generated_tab_keeps_its_own_independent_sort_and_page(cx: &mut TestAppContext) {
    let (model, _sinks) = build_model_with_recording_connection_and_total_rows(cx, 10_000);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();
    dispatch(&model, cx, PreviewAction::Sort("total_cents".to_owned()));
    dispatch(&model, cx, PreviewAction::NextPage);

    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "customers", cx)
    });
    cx.run_until_parked();

    model.read_with(cx, |model, app| {
        let orders = model
            .tabs()
            .iter()
            .find(|tab| tab.title() == "orders")
            .expect("orders tab still open");
        assert_eq!(orders.preview_state().sort_column(), Some("total_cents"));
        assert_eq!(orders.preview_state().page(), 2);

        let customers = model
            .tabs()
            .iter()
            .find(|tab| tab.title() == "customers")
            .expect("customers tab active");
        assert_eq!(
            customers.preview_state().sort_column(),
            None,
            "opening a second generated tab must not inherit the first tab's sort"
        );
        assert_eq!(customers.preview_state().page(), 1);
        assert_eq!(
            customers.editor().read(app).text(),
            "SELECT * FROM \"public\".\"customers\" LIMIT 200",
            "the second tab's own buffer must not carry the first tab's ORDER BY/OFFSET"
        );
    });
}

#[gpui::test]
fn editing_a_generated_tabs_buffer_makes_further_sort_and_page_actions_inert(
    cx: &mut TestAppContext,
) {
    let (model, sinks) = build_model_with_recording_connection(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    let id = model.read_with(cx, |model, _app| model.tabs()[0].id());
    let editor = model.read_with(cx, |model, _app| model.tabs()[0].editor().clone());
    editor.update(cx, |editor, cx| {
        editor.insert_text_for_test("-- edited\n", cx);
    });
    cx.run_until_parked();

    model.read_with(cx, |model, _app| {
        assert!(model.tab(id).unwrap().dirty());
        assert!(matches!(
            model.tab(id).unwrap().kind(),
            TabKind::Script { .. }
        ));
    });

    let text_before = editor.read_with(cx, |editor, _app| editor.text());
    let runs_before = sinks.lock().expect("sinks lock poisoned").len();

    dispatch(&model, cx, PreviewAction::Sort("id".to_owned()));
    dispatch(&model, cx, PreviewAction::NextPage);

    let text_after = editor.read_with(cx, |editor, _app| editor.text());
    assert_eq!(
        text_before, text_after,
        "sort/page actions on an edited (now Script) tab must not touch its buffer"
    );
    assert_eq!(
        sinks.lock().expect("sinks lock poisoned").len(),
        runs_before,
        "sort/page actions on an edited tab must not dispatch a run"
    );
}

#[gpui::test]
fn last_page_jumps_using_the_sessions_already_fetched_total_row_count(cx: &mut TestAppContext) {
    let (model, _sinks) = build_model_with_recording_connection(cx);
    let session = model.read_with(cx, |model, _app| model.session.clone());
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    // Simulate the initial preview_relation count fetch resolving with
    // 450 total rows -- last_page_active_tab must use exactly this
    // already-known total, not issue any count query of its own.
    session.update(cx, |session, cx| {
        session.set_row_count_for_test(Some(zsql_core::RowCount::Exact(450)));
        cx.notify();
    });
    cx.run_until_parked();

    dispatch(&model, cx, PreviewAction::LastPage);

    model.read_with(cx, |model, app| {
        let tab = &model.tabs()[0];
        // 450 rows at 200/page: pages of 200, 200, 50 -> last page 3.
        assert_eq!(tab.preview_state().page(), 3);
        assert_eq!(tab.preview_state().offset(), 400);
        assert_eq!(
            tab.editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200 OFFSET 400"
        );
    });
}

#[gpui::test]
fn next_page_rewrites_limit_offset_and_the_buffer_stays_in_sync(cx: &mut TestAppContext) {
    let (model, _sinks) = build_model_with_recording_connection_and_total_rows(cx, 10_000);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    dispatch(&model, cx, PreviewAction::NextPage);

    model.read_with(cx, |model, app| {
        let tab = &model.tabs()[0];
        assert_eq!(tab.preview_state().page(), 2);
        assert_eq!(tab.preview_state().offset(), 200);
        assert_eq!(
            tab.editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200 OFFSET 200"
        );
    });
}

#[gpui::test]
fn prev_page_is_a_no_op_and_does_not_rerun_when_already_on_page_one(cx: &mut TestAppContext) {
    let (model, sinks) = build_model_with_recording_connection(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();
    let runs_before = sinks.lock().expect("sinks lock poisoned").len();

    dispatch(&model, cx, PreviewAction::PrevPage);

    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs()[0].preview_state().page(), 1);
    });
    assert_eq!(
        sinks.lock().expect("sinks lock poisoned").len(),
        runs_before,
        "prev page at the first page must not dispatch another run"
    );
}

#[gpui::test]
fn sorting_re_anchors_the_page_to_one_dropping_any_offset(cx: &mut TestAppContext) {
    let (model, _sinks) = build_model_with_recording_connection_and_total_rows(cx, 10_000);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    dispatch(&model, cx, PreviewAction::NextPage);
    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs()[0].preview_state().page(), 2);
    });

    dispatch(&model, cx, PreviewAction::Sort("id".to_owned()));
    model.read_with(cx, |model, app| {
        let tab = &model.tabs()[0];
        assert_eq!(tab.preview_state().page(), 1);
        assert_eq!(tab.preview_state().offset(), 0);
        assert!(
            !tab.editor().read(app).text().contains("OFFSET"),
            "page 1 must not carry an OFFSET clause: {}",
            tab.editor().read(app).text()
        );
    });
}

#[gpui::test]
fn the_rendered_pager_snapshot_reflects_the_total_once_the_async_count_resolves(
    cx: &mut TestAppContext,
) {
    let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
    let connection: Arc<dyn Connection> = Arc::new(RecordingConnection {
        sinks,
        total_rows: 450, // 3 pages at the default 200/page.
    });
    let session = cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)));
    let model = cx.update(|cx| cx.new(|cx| TabModel::new(session.clone(), cx)));
    let (results, _row_count) = wire_display(&model, session, cx);

    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    // The count fetch resolves on its own schedule after the first page
    // renders; when it lands it must reach the rendered pager snapshot,
    // not just the owning tab's own preview state.
    results.read_with(cx, |results, _cx| {
        assert_eq!(
            results.preview_last_page_number_for_test(),
            Some(3),
            "the pager snapshot must reflect the relation total once the count resolves"
        );
    });
}

/// A `Connection` double whose `count_rows` reports a distinct total per
/// relation (falling back to `RowCount::Exact(0)` for an unlisted one) and
/// records every relation it was called for, so a test can both assert on a
/// specific relation's reported total and verify tab-switching never
/// triggers a redundant fetch.
struct PerRelationCountConnection {
    sinks: Arc<Mutex<Vec<BatchSink>>>,
    totals: HashMap<(String, String), RowCount>,
    count_calls: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait]
impl Connection for PerRelationCountConnection {
    fn stream_query(&self, _sql: String, sink: BatchSink) -> QueryHandle {
        self.sinks.lock().expect("sinks lock poisoned").push(sink);
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
        schema: &str,
        relation: &str,
        _filters: &zsql_core::FilterState,
    ) -> Result<RowCount, CoreError> {
        let key = (schema.to_owned(), relation.to_owned());
        self.count_calls
            .lock()
            .expect("count_calls lock poisoned")
            .push(key.clone());
        Ok(self.totals.get(&key).copied().unwrap_or(RowCount::Exact(0)))
    }

    async fn describe_relation(
        &self,
        _schema: &str,
        _relation: &str,
    ) -> Result<zsql_core::RelationSchema, CoreError> {
        Ok(zsql_core::RelationSchema::default())
    }
}

/// Open a preview of relation A (its count resolves to one total), open a
/// preview of relation B (a different total), then switch back to A's tab.
/// The displayed total -- both the status bar's and the pager's -- must
/// follow the active tab, not whichever relation was most recently fetched.
#[gpui::test]
fn switching_back_to_a_previously_previewed_tab_shows_that_tabs_own_total_row_count(
    cx: &mut TestAppContext,
) {
    let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
    let count_calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let totals = HashMap::from([
        (
            ("public".to_owned(), "orders".to_owned()),
            RowCount::Exact(300),
        ),
        (
            ("public".to_owned(), "customers".to_owned()),
            RowCount::Exact(999),
        ),
    ]);
    let connection: Arc<dyn Connection> = Arc::new(PerRelationCountConnection {
        sinks,
        totals,
        count_calls: count_calls.clone(),
    });
    let session = cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)));
    let model = cx.update(|cx| cx.new(|cx| TabModel::new(session.clone(), cx)));
    let (results, row_count) = wire_display(&model, session.clone(), cx);

    let orders_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();
    assert_eq!(
        *row_count.borrow(),
        Some(RowCount::Exact(300)),
        "orders' own count must be pushed for display while its tab is active"
    );

    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "customers", cx)
    });
    cx.run_until_parked();
    assert_eq!(
        *row_count.borrow(),
        Some(RowCount::Exact(999)),
        "customers' own count must be pushed for display while its tab is active"
    );

    let calls_before_switch = count_calls.lock().expect("count_calls lock poisoned").len();

    model.update(cx, |model, cx| model.set_active(orders_id, cx));
    cx.run_until_parked();

    // Session::row_count still reflects the most recently fetched relation
    // (customers' 999); switching tabs never re-runs a query for orders. The
    // status bar must derive the displayed total from the active tab's own
    // frozen preview state rather than this session-global value.
    session.read_with(cx, |session, _cx| {
        assert_eq!(
            session.row_count(),
            Some(RowCount::Exact(999)),
            "the session-global row count still reflects the most recently fetched relation"
        );
    });

    assert_eq!(
        *row_count.borrow(),
        Some(RowCount::Exact(300)),
        "switching back to orders' tab must push its own total, not customers'"
    );
    results.read_with(cx, |results, _cx| {
        assert_eq!(
            results.preview_last_page_number_for_test(),
            Some(2), // 300 rows at 200/page.
            "the pager must agree with the status bar on orders' own total"
        );
    });
    assert_eq!(
        count_calls.lock().expect("count_calls lock poisoned").len(),
        calls_before_switch,
        "switching tabs must not re-issue a count_rows call"
    );
}

/// A generated tab whose count fetch has not resolved yet must keep
/// rendering no total -- neither while it is active nor after switching
/// away and back -- rather than fabricating one from another tab.
#[gpui::test]
fn a_tab_whose_count_fetch_is_still_pending_shows_no_total_across_a_tab_switch(
    cx: &mut TestAppContext,
) {
    let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
    let count_calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    // "orders" has no entry in `totals`, but that alone would resolve to
    // `Exact(0)`; what actually matters here is that this connection is
    // never parked past its `count_rows` call for "orders" before the
    // assertions below run, so its fetch is still genuinely in flight.
    let connection: Arc<dyn Connection> = Arc::new(PerRelationCountConnection {
        sinks,
        totals: HashMap::new(),
        count_calls,
    });
    let session = cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)));
    let model = cx.update(|cx| cx.new(|cx| TabModel::new(session.clone(), cx)));
    let (_results, row_count) = wire_display(&model, session.clone(), cx);

    let orders_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    // Deliberately not parked: the count fetch for "orders" has not
    // resolved yet.
    assert_eq!(
        *row_count.borrow(),
        None,
        "a not-yet-resolved count must be pushed as absent, not zero or borrowed"
    );

    let script_id = model.update(cx, TabModel::new_script_tab);
    assert_eq!(
        *row_count.borrow(),
        None,
        "a script tab that has never run a preview pushes no total"
    );

    model.update(cx, |model, cx| model.set_active(orders_id, cx));
    assert_eq!(
        *row_count.borrow(),
        None,
        "switching back to a tab whose count is still pending must not fabricate a total"
    );

    let _ = script_id;
}

/// A generated tab converted to a script by a manual edit keeps showing no
/// total row count, unchanged by which tab was active before it.
#[gpui::test]
fn an_edited_generated_tab_shows_no_total_row_count(cx: &mut TestAppContext) {
    let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
    let connection: Arc<dyn Connection> = Arc::new(RecordingConnection {
        sinks: sinks.clone(),
        total_rows: 450,
    });
    let session = cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)));
    let model = cx.update(|cx| cx.new(|cx| TabModel::new(session.clone(), cx)));
    let (_results, row_count) = wire_display(&model, session, cx);
    let id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();
    assert_eq!(
        *row_count.borrow(),
        Some(RowCount::Exact(450)),
        "a live generated tab shows its own resolved total"
    );

    let editor = model.read_with(cx, |model, _app| model.tab(id).unwrap().editor().clone());
    editor.update(cx, |editor, cx| {
        editor.insert_text_for_test("-- edited\n", cx);
    });
    cx.run_until_parked();

    assert_eq!(
        *row_count.borrow(),
        None,
        "editing a generated tab must clear its pushed total row count"
    );
    let _ = sinks;
}

// -- filters --------------------------------------------------------------

#[gpui::test]
fn adding_a_filter_rewrites_the_buffer_reruns_and_refetches_the_count(cx: &mut TestAppContext) {
    let (model, sinks) = build_model_with_recording_connection(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();
    let runs_before = sinks.lock().expect("sinks lock poisoned").len();

    dispatch(
        &model,
        cx,
        PreviewAction::AddFilter {
            column: "status".to_owned(),
            type_name: "text".to_owned(),
            operator: zsql_core::FilterOperator::Eq,
            value: "paid".to_owned(),
        },
    );

    model.read_with(cx, |model, app| {
        let tab = &model.tabs()[0];
        assert_eq!(tab.preview_state().filters().len(), 1);
        assert_eq!(
            tab.editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" WHERE \"status\" = 'paid' LIMIT 200",
            "the editor buffer must equal exactly the SQL that was rerun"
        );
    });
    assert_eq!(
        sinks.lock().expect("sinks lock poisoned").len(),
        runs_before + 1,
        "adding a filter to a live generated tab must re-run its query"
    );
}

#[gpui::test]
fn a_filter_value_that_parses_as_an_expression_is_never_quoted(cx: &mut TestAppContext) {
    let (model, _sinks) = build_model_with_recording_connection(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    dispatch(
        &model,
        cx,
        PreviewAction::AddFilter {
            column: "placed_at".to_owned(),
            type_name: "timestamptz".to_owned(),
            operator: zsql_core::FilterOperator::Gt,
            value: "now() - interval '7 days'".to_owned(),
        },
    );

    model.read_with(cx, |model, app| {
        assert_eq!(
            model.tabs()[0].editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" WHERE \"placed_at\" > now() - interval '7 days' \
             LIMIT 200"
        );
    });
}

#[gpui::test]
fn multiple_filters_combine_with_their_own_and_or_connectors_in_chip_order(
    cx: &mut TestAppContext,
) {
    let (model, _sinks) = build_model_with_recording_connection(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    dispatch(
        &model,
        cx,
        PreviewAction::AddFilter {
            column: "status".to_owned(),
            type_name: "text".to_owned(),
            operator: zsql_core::FilterOperator::Eq,
            value: "paid".to_owned(),
        },
    );
    dispatch(
        &model,
        cx,
        PreviewAction::AddFilter {
            column: "status".to_owned(),
            type_name: "text".to_owned(),
            operator: zsql_core::FilterOperator::Eq,
            value: "pending".to_owned(),
        },
    );

    let first_id = model.read_with(cx, |model, _app| {
        model.tabs()[0].preview_state().filters().conditions()[0].id()
    });
    dispatch(&model, cx, PreviewAction::ToggleFilterConnector(0));

    model.read_with(cx, |model, app| {
        assert_eq!(
            model.tabs()[0].editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" WHERE \"status\" = 'paid' OR \"status\" = \
             'pending' LIMIT 200"
        );
    });

    dispatch(&model, cx, PreviewAction::RemoveFilter(first_id));
    model.read_with(cx, |model, app| {
        assert_eq!(
            model.tabs()[0].editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" WHERE \"status\" = 'pending' LIMIT 200"
        );
    });
}

#[gpui::test]
fn updating_a_filters_operator_and_value_rewrites_the_buffer(cx: &mut TestAppContext) {
    let (model, _sinks) = build_model_with_recording_connection(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    let id = model.update(cx, |model, cx| {
        model.dispatch_preview_action(
            PreviewAction::AddFilter {
                column: "total_cents".to_owned(),
                type_name: "int4".to_owned(),
                operator: zsql_core::FilterOperator::Eq,
                value: "100".to_owned(),
            },
            cx,
        );
        model.tabs()[0].preview_state().filters().conditions()[0].id()
    });
    cx.run_until_parked();

    dispatch(
        &model,
        cx,
        PreviewAction::UpdateFilter {
            id,
            operator: zsql_core::FilterOperator::Ge,
            value: "500".to_owned(),
        },
    );

    model.read_with(cx, |model, app| {
        assert_eq!(
            model.tabs()[0].editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" WHERE \"total_cents\" >= 500 LIMIT 200"
        );
    });
}

#[gpui::test]
fn clear_filters_removes_every_condition_and_reruns_unfiltered(cx: &mut TestAppContext) {
    let (model, sinks) = build_model_with_recording_connection(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();
    dispatch(
        &model,
        cx,
        PreviewAction::AddFilter {
            column: "status".to_owned(),
            type_name: "text".to_owned(),
            operator: zsql_core::FilterOperator::Eq,
            value: "paid".to_owned(),
        },
    );
    let runs_before = sinks.lock().expect("sinks lock poisoned").len();

    dispatch(&model, cx, PreviewAction::ClearFilters);

    model.read_with(cx, |model, app| {
        let tab = &model.tabs()[0];
        assert!(tab.preview_state().filters().is_empty());
        assert_eq!(
            tab.editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200"
        );
    });
    assert_eq!(
        sinks.lock().expect("sinks lock poisoned").len(),
        runs_before + 1,
        "clearing filters on a live generated tab must re-run its query"
    );
}

#[gpui::test]
fn a_filter_change_re_anchors_the_page_to_one_dropping_any_offset(cx: &mut TestAppContext) {
    let (model, _sinks) = build_model_with_recording_connection_and_total_rows(cx, 10_000);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    dispatch(&model, cx, PreviewAction::NextPage);
    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs()[0].preview_state().page(), 2);
    });

    dispatch(
        &model,
        cx,
        PreviewAction::AddFilter {
            column: "status".to_owned(),
            type_name: "text".to_owned(),
            operator: zsql_core::FilterOperator::Eq,
            value: "paid".to_owned(),
        },
    );

    model.read_with(cx, |model, app| {
        let tab = &model.tabs()[0];
        assert_eq!(tab.preview_state().page(), 1);
        assert!(
            !tab.editor().read(app).text().contains("OFFSET"),
            "page 1 must not carry an OFFSET clause: {}",
            tab.editor().read(app).text()
        );
    });
}

#[gpui::test]
fn editing_a_generated_tabs_buffer_makes_further_filter_actions_inert(cx: &mut TestAppContext) {
    let (model, sinks) = build_model_with_recording_connection(cx);
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    let editor = model.read_with(cx, |model, _app| model.tabs()[0].editor().clone());
    editor.update(cx, |editor, cx| {
        editor.insert_text_for_test("-- edited\n", cx);
    });
    cx.run_until_parked();

    let text_before = editor.read_with(cx, |editor, _app| editor.text());
    let runs_before = sinks.lock().expect("sinks lock poisoned").len();

    dispatch(
        &model,
        cx,
        PreviewAction::AddFilter {
            column: "status".to_owned(),
            type_name: "text".to_owned(),
            operator: zsql_core::FilterOperator::Eq,
            value: "paid".to_owned(),
        },
    );

    let text_after = editor.read_with(cx, |editor, _app| editor.text());
    assert_eq!(
        text_before, text_after,
        "a filter action on an edited (now Script) tab must not touch its buffer"
    );
    // A converted tab is `TabKind::Script`, which carries no preview state
    // at all -- there is structurally nothing left for a filter action to
    // commit into, unlike the pre-refactor design where `preview_state` was
    // carried (but ignored) on every tab regardless of kind.
    assert!(matches!(
        model.read_with(cx, |model, _app| model.tabs()[0].kind().clone()),
        TabKind::Script { .. }
    ));
    assert_eq!(
        sinks.lock().expect("sinks lock poisoned").len(),
        runs_before,
        "a filter action on an edited tab must not dispatch a run"
    );
}

// -- persisted preview state round-trips -----------------------------------

/// A generated tab's sort, an OR-connected filter set (including an
/// fx-classified expression value), and its page all survive a full
/// snapshot -> disk -> restore round trip: the regenerated buffer text
/// matches a direct `preview_sql_windowed` call against that exact window,
/// and the restored `PreviewQueryState` matches the original in every field
/// but its row counts, which a fresh restore never carries.
#[gpui::test]
fn a_filtered_sorted_paged_generated_tab_round_trips_through_a_snapshot(cx: &mut TestAppContext) {
    let (model, _sinks) = build_model_with_recording_connection_and_total_rows(cx, 10_000);
    let session = model.read_with(cx, |model, _app| model.session.clone());
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();

    dispatch(&model, cx, PreviewAction::Sort("total_cents".to_owned()));
    dispatch(
        &model,
        cx,
        PreviewAction::AddFilter {
            column: "status".to_owned(),
            type_name: "text".to_owned(),
            operator: zsql_core::FilterOperator::Eq,
            value: "paid".to_owned(),
        },
    );
    dispatch(
        &model,
        cx,
        PreviewAction::AddFilter {
            column: "placed_at".to_owned(),
            type_name: "timestamptz".to_owned(),
            operator: zsql_core::FilterOperator::Gt,
            value: "now() - interval '7 days'".to_owned(),
        },
    );
    dispatch(&model, cx, PreviewAction::ToggleFilterConnector(0));
    dispatch(&model, cx, PreviewAction::NextPage);
    dispatch(&model, cx, PreviewAction::NextPage);

    let original = model.read_with(cx, |model, _app| model.tabs()[0].preview_state().clone());
    assert_eq!(original.page(), 3);
    assert_eq!(
        original.filters().connectors(),
        [zsql_core::FilterConnector::Or]
    );
    assert_eq!(
        original.total_rows(),
        Some(RowCount::Exact(10_000)),
        "the live tab's total must be resolved before snapshotting it"
    );

    let snapshot = model.read_with(cx, TabModel::snapshot);
    let sessions_dir = TempDir::new("snapshot-wire-roundtrip");
    let key = crate::session_store::ConnectionKey::Saved(uuid::Uuid::new_v4());
    crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .save_snapshot(&snapshot)
        .expect("save must succeed");
    let parsed = crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .load_snapshot()
        .expect("load must succeed")
        .expect("snapshot must exist");

    let expected_sql = session.read_with(cx, |session, _app| {
        session.preview_sql_windowed(
            "public",
            "orders",
            original.sort_pair(),
            original.page_size(),
            original.offset(),
            original.filters(),
        )
    });

    let restored = cx.update(|cx| cx.new(|cx| TabModel::new(session, cx)));
    restored.update(cx, |model, cx| {
        model.load_for_connection(Some(&parsed), cx);
    });

    restored.read_with(cx, |model, app| {
        let tab = &model.tabs()[0];
        assert_eq!(tab.editor().read(app).text(), expected_sql);

        let restored_state = tab.preview_state();
        assert_eq!(restored_state.sort_column(), original.sort_column());
        assert_eq!(restored_state.sort_direction(), original.sort_direction());
        assert_eq!(restored_state.page(), original.page());
        assert_eq!(restored_state.page_size(), original.page_size());
        assert_eq!(restored_state.filters(), original.filters());
        assert_eq!(
            restored_state.total_rows(),
            None,
            "a restored preview state never carries its old total row count"
        );
        assert_eq!(restored_state.base_total_rows(), None);
    });
}

/// A generated tab's total row count is cleared by a restore and refetched
/// the moment the restored tab runs again, through the same count path any
/// other run uses -- no restore-specific special-casing.
#[gpui::test]
fn restoring_a_snapshot_clears_totals_and_a_rerun_refetches_them(cx: &mut TestAppContext) {
    let (model, _sinks) = build_model_with_recording_connection_and_total_rows(cx, 300);
    let session = model.read_with(cx, |model, _app| model.session.clone());
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();
    model.read_with(cx, |model, _app| {
        assert_eq!(
            model.tabs()[0].preview_state().total_rows(),
            Some(RowCount::Exact(300))
        );
    });

    let snapshot = model.read_with(cx, TabModel::snapshot);
    let sessions_dir = TempDir::new("snapshot-wire-roundtrip");
    let key = crate::session_store::ConnectionKey::Saved(uuid::Uuid::new_v4());
    crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .save_snapshot(&snapshot)
        .expect("save must succeed");
    let parsed = crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .load_snapshot()
        .expect("load must succeed")
        .expect("snapshot must exist");

    let restored = cx.update(|cx| cx.new(|cx| TabModel::new(session, cx)));
    restored.update(cx, |model, cx| {
        model.load_for_connection(Some(&parsed), cx);
    });
    restored.read_with(cx, |model, _app| {
        assert_eq!(
            model.tabs()[0].preview_state().total_rows(),
            None,
            "restore must clear the persisted total row count"
        );
        assert_eq!(model.tabs()[0].preview_state().base_total_rows(), None);
    });

    let restored_id = restored.read_with(cx, |model, _app| model.tabs()[0].id());
    restored.update(cx, |model, cx| {
        model.run_for_tab(restored_id, String::new(), cx);
    });
    cx.run_until_parked();

    restored.read_with(cx, |model, _app| {
        assert_eq!(
            model.tabs()[0].preview_state().total_rows(),
            Some(RowCount::Exact(300)),
            "rerunning a restored tab must refetch its total through the existing count path"
        );
    });
}

/// A connection switch-back hands `load_for_connection` a `TabSessionSnapshot`
/// straight from the in-memory session cache, never through JSON -- so
/// clearing a `Generated` tab's totals cannot rely on `#[serde(skip)]` alone.
/// Restoring from such a snapshot must clear them just as reliably as
/// restoring from disk does.
#[gpui::test]
fn switching_back_to_a_cached_session_clears_totals_without_a_serde_round_trip(
    cx: &mut TestAppContext,
) {
    let (model, _sinks) = build_model_with_recording_connection_and_total_rows(cx, 300);
    let session = model.read_with(cx, |model, _app| model.session.clone());
    model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    cx.run_until_parked();
    model.read_with(cx, |model, _app| {
        assert_eq!(
            model.tabs()[0].preview_state().total_rows(),
            Some(RowCount::Exact(300))
        );
    });

    let cached_snapshot = model.read_with(cx, TabModel::snapshot);

    let restored = cx.update(|cx| cx.new(|cx| TabModel::new(session, cx)));
    restored.update(cx, |model, cx| {
        model.load_for_connection(Some(&cached_snapshot), cx);
    });

    restored.read_with(cx, |model, _app| {
        assert_eq!(
            model.tabs()[0].preview_state().total_rows(),
            None,
            "a cache-backed restore must clear the previous total row count"
        );
        assert_eq!(model.tabs()[0].preview_state().base_total_rows(), None);
    });
}

/// A script tab's buffer text -- including surrounding whitespace and
/// non-ASCII content a user typed -- survives a snapshot/save-to-disk/
/// load/restore round trip byte for byte.
#[gpui::test]
fn a_script_tabs_buffer_text_round_trips_byte_for_byte(cx: &mut TestAppContext) {
    let model = build_model(cx);
    model.update(cx, TabModel::new_script_tab);
    let editor = model.read_with(cx, |model, _app| model.tabs()[0].editor().clone());
    let text = "  select 'caf\u{e9}', '\u{1f600}';\n\n\t".to_owned();
    editor.update(cx, |editor, cx| editor.set_text(&text, cx));

    let snapshot = model.read_with(cx, TabModel::snapshot);
    let sessions_dir = TempDir::new("snapshot-wire-roundtrip");
    let key = crate::session_store::ConnectionKey::Saved(uuid::Uuid::new_v4());
    crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .save_snapshot(&snapshot)
        .expect("save must succeed");
    let parsed = crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .load_snapshot()
        .expect("load must succeed")
        .expect("snapshot must exist");

    let restored = build_model(cx);
    restored.update(cx, |model, cx| {
        model.load_for_connection(Some(&parsed), cx);
    });

    restored.read_with(cx, |model, app| {
        assert_eq!(model.tabs()[0].editor().read(app).text(), text);
    });
}

/// A session-owned script tab -- unnamed `query-N.sql` -- never reports
/// diverged, regardless of how many manual edits its buffer receives.
#[gpui::test]
fn a_session_owned_unnamed_tab_never_diverges_even_after_multiple_edits(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let id = model.update(cx, TabModel::new_script_tab);
    let editor = model.read_with(cx, |model, _app| model.tabs()[0].editor().clone());

    for text in ["select 1", "select 1, 2", "select 1, 2, 3"] {
        editor.update(cx, |editor, cx| editor.insert_text_for_test(text, cx));
        model.read_with(cx, |model, app| {
            let tab = model.tab(id).unwrap();
            assert!(
                !tab.diverged(app),
                "an unnamed session tab must never diverge, even after {} edits",
                text.len()
            );
        });
    }
}

/// A tab converted to library-backed reports diverged exactly when its
/// buffer differs from the library file's last saved text: clean right
/// after conversion, diverged once edited, clean again once a save moves
/// the saved baseline to match.
#[gpui::test]
fn a_library_backed_tab_diverges_exactly_when_its_buffer_differs_from_the_saved_text(
    cx: &mut TestAppContext,
) {
    let model = build_model(cx);
    let id = model.update(cx, TabModel::new_script_tab);
    let editor = model.read_with(cx, |model, _app| model.tabs()[0].editor().clone());
    editor.update(cx, |editor, cx| {
        editor.set_text("select * from orders;", cx);
    });
    model.update(cx, |model, cx| {
        model.convert_to_library_backed(
            id,
            "orders".to_owned(),
            "select * from orders;".to_owned(),
            cx,
        );
    });

    model.read_with(cx, |model, app| {
        let tab = model.tab(id).unwrap();
        assert!(matches!(
            tab.script_backing(),
            Some(ScriptBacking::Library { .. })
        ));
        assert!(
            !tab.diverged(app),
            "a freshly converted tab must be clean when its buffer already matches the saved text"
        );
    });

    editor.update(cx, |editor, cx| {
        editor.insert_text_for_test(" -- edited", cx);
    });
    model.read_with(cx, |model, app| {
        assert!(
            model.tab(id).unwrap().diverged(app),
            "editing a library-backed tab's buffer must diverge it from the saved text"
        );
    });

    let current_text = editor.read_with(cx, |editor, _app| editor.text());
    model.update(cx, |model, cx| {
        model.mark_backing_saved(id, current_text, cx);
    });
    model.read_with(cx, |model, app| {
        assert!(
            !model.tab(id).unwrap().diverged(app),
            "saving back to the current buffer text must clear divergence"
        );
    });
}

/// A completed library rename folds back into the tab's own name and title,
/// leaving its divergence baseline (the file's saved content) untouched.
#[gpui::test]
fn apply_library_rename_updates_the_name_and_title_but_not_the_saved_baseline(
    cx: &mut TestAppContext,
) {
    let model = build_model(cx);
    let id = model.update(cx, TabModel::new_script_tab);
    let editor = model.read_with(cx, |model, _app| model.tabs()[0].editor().clone());
    editor.update(cx, |editor, cx| editor.set_text("select 1;", cx));
    model.update(cx, |model, cx| {
        model.convert_to_library_backed(id, "orders".to_owned(), "select 1;".to_owned(), cx);
    });

    model.update(cx, |model, cx| {
        model.apply_library_rename(id, "top-orders", cx);
    });

    model.read_with(cx, |model, app| {
        let tab = model.tab(id).unwrap();
        assert_eq!(tab.library_name(), Some("top-orders"));
        assert_eq!(tab.title(), "top-orders.sql");
        assert!(
            !tab.diverged(app),
            "a rename must not touch the saved-text baseline, so a clean \
             tab must stay clean"
        );
    });
}

/// `Save` on an unnamed session tab must always ask the embedding app to
/// open the modal -- never write a file itself.
#[gpui::test]
fn save_on_an_unnamed_session_tab_requests_the_modal_and_writes_nothing(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let id = model.update(cx, TabModel::new_script_tab);
    let events = subscribe_save_events(&model, cx);

    model.update(cx, |model, cx| model.trigger_save(id, cx));

    let events: Vec<SaveRequested> = events.borrow().clone();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        SaveRequested::OpenSaveModal { as_copy: false, .. }
    ));
}

/// `Save` on a named session tab is a silent no-op: no modal, no event.
#[gpui::test]
fn save_on_a_named_session_tab_emits_nothing(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let id = model.update(cx, TabModel::new_script_tab);
    model.update(cx, |model, cx| {
        model.apply_renamed_title(id, "top-customers.sql".to_owned(), cx);
    });
    let events = subscribe_save_events(&model, cx);

    model.update(cx, |model, cx| model.trigger_save(id, cx));

    assert!(
        events.borrow().is_empty(),
        "a named session tab's Save must not request anything: {:?}",
        events.borrow()
    );
}

/// `Save` on a library-backed tab always asks the embedding app to write
/// the library file directly, with no modal.
#[gpui::test]
fn save_on_a_library_backed_tab_requests_a_direct_write(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let id = model.update(cx, TabModel::new_script_tab);
    model.update(cx, |model, cx| {
        model.convert_to_library_backed(id, "orders".to_owned(), "select 1;".to_owned(), cx);
    });
    let events = subscribe_save_events(&model, cx);

    model.update(cx, |model, cx| model.trigger_save(id, cx));

    let events: Vec<SaveRequested> = events.borrow().clone();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        SaveRequested::WriteLibraryDirect { tab_id } if tab_id == id
    ));
}

/// `Save as` always opens the modal, regardless of the source tab's
/// backing, and never on its own retargets anything (retargeting only
/// ever happens once the modal is confirmed, in `ui::workspace`).
#[gpui::test]
fn save_as_always_opens_the_modal_regardless_of_backing(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let unnamed_id = model.update(cx, TabModel::new_script_tab);
    let library_id = model.update(cx, TabModel::new_script_tab);
    model.update(cx, |model, cx| {
        model.convert_to_library_backed(
            library_id,
            "orders".to_owned(),
            "select 1;".to_owned(),
            cx,
        );
    });
    let events = subscribe_save_events(&model, cx);

    model.update(cx, |model, cx| model.trigger_save_as(unnamed_id, cx));
    model.update(cx, |model, cx| model.trigger_save_as(library_id, cx));

    let events: Vec<SaveRequested> = events.borrow().clone();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0],
        SaveRequested::OpenSaveModal { tab_id, as_copy: true } if tab_id == unnamed_id
    ));
    assert!(matches!(
        events[1],
        SaveRequested::OpenSaveModal { tab_id, as_copy: true } if tab_id == library_id
    ));

    model.read_with(cx, |model, _app| {
        assert!(
            matches!(
                model.tab(library_id).unwrap().script_backing(),
                Some(ScriptBacking::Library { .. })
            ),
            "Save as must never retarget the source tab's own backing"
        );
    });
}

/// A temp directory this test owns exclusively, removed on drop so tests
/// never leak directories into the real temp dir.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "zsql-tabs-restore-test-{label}-{}-{n}",
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

/// Restarting the app (`save_snapshot` then a fresh `load_snapshot`)
/// restores a library-backed tab as library-backed, pointing at the same
/// library file.
#[gpui::test]
fn restarting_restores_a_library_backed_tab_as_library_backed(cx: &mut TestAppContext) {
    let library_dir = TempDir::new("restart-library-backed");
    let sessions_dir = TempDir::new("restart-sessions");
    let key = crate::session_store::ConnectionKey::Saved(uuid::Uuid::new_v4());

    let model = build_model(cx);
    model.update(cx, |model, _cx| {
        model.set_library_dir(Some(library_dir.0.clone()));
    });
    let id = model.update(cx, TabModel::new_script_tab);
    model.update(cx, |model, cx| {
        model.convert_to_library_backed(id, "orders".to_owned(), "select 1;".to_owned(), cx);
    });

    let snapshot = model.read_with(cx, TabModel::snapshot);
    crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .save_snapshot(&snapshot)
        .expect("save must succeed");
    let loaded = crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .load_snapshot()
        .expect("load must succeed")
        .expect("snapshot must exist");

    let restored = build_model(cx);
    restored.update(cx, |model, _cx| {
        model.set_library_dir(Some(library_dir.0.clone()));
    });
    restored.update(cx, |model, cx| model.load_for_connection(Some(&loaded), cx));

    restored.read_with(cx, |model, _app| {
        let tab = &model.tabs()[0];
        assert!(matches!(
            tab.script_backing(),
            Some(ScriptBacking::Library { .. })
        ));
        assert_eq!(tab.library_name(), Some("orders"));
    });
}

/// On restore, a draft (if present) wins over the library file's on-disk
/// content, and the restored tab is diverged.
#[gpui::test]
fn restore_with_a_draft_present_uses_the_draft_and_stays_diverged(cx: &mut TestAppContext) {
    let library_dir = TempDir::new("restore-draft-wins");
    let sessions_dir = TempDir::new("restore-draft-wins-sessions");
    let key = crate::session_store::ConnectionKey::Saved(uuid::Uuid::new_v4());
    crate::session_store::LibraryDir::at(&library_dir.0)
        .save(
            &crate::session_store::LibraryName::new("orders").unwrap(),
            "select 1;",
        )
        .expect("seeding the library file must succeed");

    let model = build_model(cx);
    model.update(cx, |model, _cx| {
        model.set_library_dir(Some(library_dir.0.clone()));
    });
    let id = model.update(cx, TabModel::new_script_tab);
    let editor = model.read_with(cx, |model, _app| model.tabs()[0].editor().clone());
    editor.update(cx, |editor, cx| editor.set_text("select 1;", cx));
    model.update(cx, |model, cx| {
        model.convert_to_library_backed(id, "orders".to_owned(), "select 1;".to_owned(), cx);
    });
    editor.update(cx, |editor, cx| {
        editor.insert_text_for_test(" -- draft edit", cx);
    });

    let snapshot = model.read_with(cx, TabModel::snapshot);
    crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .save_snapshot(&snapshot)
        .expect("save must succeed");
    let loaded = crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .load_snapshot()
        .expect("load must succeed")
        .expect("snapshot must exist");

    let restored = build_model(cx);
    restored.update(cx, |model, _cx| {
        model.set_library_dir(Some(library_dir.0.clone()));
    });
    restored.update(cx, |model, cx| model.load_for_connection(Some(&loaded), cx));

    restored.read_with(cx, |model, app| {
        let tab = &model.tabs()[0];
        assert_eq!(tab.editor().read(app).text(), " -- draft editselect 1;");
        assert!(
            tab.diverged(app),
            "a restored draft must leave the tab reporting diverged"
        );
    });
}

/// A transient read error (as opposed to the file simply not existing) on
/// the library file at restore must never be treated as "the draft already
/// matches the file": the tab must keep reporting diverged so a close does
/// not prune real unsaved changes and the dirty marker does not lie clean.
#[cfg(unix)]
#[gpui::test]
fn restore_with_a_draft_and_an_unreadable_library_file_stays_diverged_not_clean(
    cx: &mut TestAppContext,
) {
    use std::os::unix::fs::PermissionsExt;

    let library_dir = TempDir::new("restore-draft-unreadable-library");
    let sessions_dir = TempDir::new("restore-draft-unreadable-library-sessions");
    let key = crate::session_store::ConnectionKey::Saved(uuid::Uuid::new_v4());
    crate::session_store::LibraryDir::at(&library_dir.0)
        .save(
            &crate::session_store::LibraryName::new("orders").unwrap(),
            "select 1;",
        )
        .expect("seeding the library file must succeed");

    let model = build_model(cx);
    model.update(cx, |model, _cx| {
        model.set_library_dir(Some(library_dir.0.clone()));
    });
    let id = model.update(cx, TabModel::new_script_tab);
    let editor = model.read_with(cx, |model, _app| model.tabs()[0].editor().clone());
    editor.update(cx, |editor, cx| editor.set_text("select 1;", cx));
    model.update(cx, |model, cx| {
        model.convert_to_library_backed(id, "orders".to_owned(), "select 1;".to_owned(), cx);
    });
    editor.update(cx, |editor, cx| {
        editor.insert_text_for_test(" -- draft edit", cx);
    });

    let snapshot = model.read_with(cx, TabModel::snapshot);
    crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .save_snapshot(&snapshot)
        .expect("save must succeed");
    let loaded = crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .load_snapshot()
        .expect("load must succeed")
        .expect("snapshot must exist");

    let library_file = library_dir.0.join("orders.sql");
    let original_permissions = std::fs::metadata(&library_file)
        .expect("must stat library file")
        .permissions();
    std::fs::set_permissions(&library_file, std::fs::Permissions::from_mode(0o000))
        .expect("must revoke read permission");

    let restored = build_model(cx);
    restored.update(cx, |model, _cx| {
        model.set_library_dir(Some(library_dir.0.clone()));
    });
    restored.update(cx, |model, cx| model.load_for_connection(Some(&loaded), cx));

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        restored.read_with(cx, |model, app| {
            let tab = &model.tabs()[0];
            assert!(
                tab.diverged(app),
                "an unreadable library file at restore must never be treated as \
                 matching an unconfirmed draft"
            );
        });
    }));
    // Restore permissions before propagating any assertion failure, so a
    // failing assertion never leaves an unreadable temp file behind for the
    // next run to trip over.
    std::fs::set_permissions(&library_file, original_permissions)
        .expect("must restore permissions");
    if let Err(err) = result {
        std::panic::resume_unwind(err);
    }
}

/// On restore, with no draft present, the buffer loads from the library
/// file itself and the tab is clean.
#[gpui::test]
fn restore_with_no_draft_loads_the_library_file_and_stays_clean(cx: &mut TestAppContext) {
    let library_dir = TempDir::new("restore-no-draft");
    let sessions_dir = TempDir::new("restore-no-draft-sessions");
    let key = crate::session_store::ConnectionKey::Saved(uuid::Uuid::new_v4());
    crate::session_store::LibraryDir::at(&library_dir.0)
        .save(
            &crate::session_store::LibraryName::new("orders").unwrap(),
            "select 1;",
        )
        .expect("seeding the library file must succeed");

    let model = build_model(cx);
    model.update(cx, |model, _cx| {
        model.set_library_dir(Some(library_dir.0.clone()));
    });
    let id = model.update(cx, TabModel::new_script_tab);
    let editor = model.read_with(cx, |model, _app| model.tabs()[0].editor().clone());
    editor.update(cx, |editor, cx| editor.set_text("select 1;", cx));
    model.update(cx, |model, cx| {
        model.convert_to_library_backed(id, "orders".to_owned(), "select 1;".to_owned(), cx);
    });

    let snapshot = model.read_with(cx, TabModel::snapshot);
    crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .save_snapshot(&snapshot)
        .expect("save must succeed");
    let loaded = crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .load_snapshot()
        .expect("load must succeed")
        .expect("snapshot must exist");

    let restored = build_model(cx);
    restored.update(cx, |model, _cx| {
        model.set_library_dir(Some(library_dir.0.clone()));
    });
    restored.update(cx, |model, cx| model.load_for_connection(Some(&loaded), cx));

    restored.read_with(cx, |model, app| {
        let tab = &model.tabs()[0];
        assert_eq!(tab.editor().read(app).text(), "select 1;");
        assert!(
            !tab.diverged(app),
            "a clean restore must not report diverged"
        );
    });
}

/// An external-backed tab reports diverged exactly when its buffer differs
/// from the file's last-saved text, the same rule a library-backed tab
/// follows.
#[gpui::test]
fn an_external_backed_tab_diverges_exactly_when_its_buffer_differs_from_the_saved_text(
    cx: &mut TestAppContext,
) {
    let model = build_model(cx);
    let path = std::path::PathBuf::from("/home/t/work/migrate.sql");
    let id = model.update(cx, |model, cx| {
        model.new_external_tab(path.clone(), "migrate.sql".to_owned(), "select 1;", cx)
    });
    let editor = model.read_with(cx, |model, _app| model.tab(id).unwrap().editor().clone());

    model.read_with(cx, |model, app| {
        let tab = model.tab(id).unwrap();
        assert!(matches!(
            tab.script_backing(),
            Some(ScriptBacking::External { .. })
        ));
        assert_eq!(tab.external_path(), Some(path.as_path()));
        assert!(
            !tab.diverged(app),
            "a freshly opened external tab must start clean"
        );
    });

    editor.update(cx, |editor, cx| {
        editor.insert_text_for_test(" -- edited", cx);
    });
    model.read_with(cx, |model, app| {
        assert!(
            model.tab(id).unwrap().diverged(app),
            "editing an external-backed tab's buffer must diverge it from the saved text"
        );
    });

    let current_text = editor.read_with(cx, |editor, _app| editor.text());
    model.update(cx, |model, cx| {
        model.mark_backing_saved(id, current_text, cx);
    });
    model.read_with(cx, |model, app| {
        assert!(
            !model.tab(id).unwrap().diverged(app),
            "saving back to the current buffer text must clear divergence"
        );
    });
}

/// `Save` on an external-backed tab always asks the embedding app to write
/// the file directly, with no modal.
#[gpui::test]
fn save_on_an_external_backed_tab_requests_a_direct_write(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let path = std::path::PathBuf::from("/home/t/work/migrate.sql");
    let id = model.update(cx, |model, cx| {
        model.new_external_tab(path, "migrate.sql".to_owned(), "select 1;", cx)
    });
    let events = subscribe_save_events(&model, cx);

    model.update(cx, |model, cx| model.trigger_save(id, cx));

    let events: Vec<SaveRequested> = events.borrow().clone();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        SaveRequested::WriteExternalDirect { tab_id } if tab_id == id
    ));
}

/// Restarting the app (`save_snapshot` then a fresh `load_snapshot`)
/// restores an external-backed tab as external-backed, pointing at the same
/// path, loading the file's current on-disk content.
#[gpui::test]
fn restarting_restores_an_external_backed_tab_as_external_backed(cx: &mut TestAppContext) {
    let sessions_dir = TempDir::new("restart-external-sessions");
    let external_dir = TempDir::new("restart-external-file");
    let key = crate::session_store::ConnectionKey::Saved(uuid::Uuid::new_v4());
    let path = external_dir.0.join("migrate.sql");
    crate::session_store::external::save(&path, "select 1;")
        .expect("seeding the external file must succeed");

    let model = build_model(cx);
    let id = model.update(cx, |model, cx| {
        model.new_external_tab(path.clone(), "migrate.sql".to_owned(), "select 1;", cx)
    });
    let _ = id;

    let snapshot = model.read_with(cx, TabModel::snapshot);
    crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .save_snapshot(&snapshot)
        .expect("save must succeed");
    let loaded = crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .load_snapshot()
        .expect("load must succeed")
        .expect("snapshot must exist");

    let restored = build_model(cx);
    restored.update(cx, |model, cx| model.load_for_connection(Some(&loaded), cx));

    restored.read_with(cx, |model, app| {
        let tab = &model.tabs()[0];
        assert!(matches!(
            tab.script_backing(),
            Some(ScriptBacking::External { .. })
        ));
        assert_eq!(tab.external_path(), Some(path.as_path()));
        assert_eq!(tab.editor().read(app).text(), "select 1;");
        assert!(!tab.diverged(app));
    });
}

/// On restore, a draft (if present) wins over an external file's on-disk
/// content, and the restored tab is diverged -- even when the file itself
/// is still perfectly readable.
#[gpui::test]
fn restore_with_a_draft_present_for_an_external_tab_uses_the_draft_and_stays_diverged(
    cx: &mut TestAppContext,
) {
    let sessions_dir = TempDir::new("restore-external-draft-sessions");
    let external_dir = TempDir::new("restore-external-draft-file");
    let key = crate::session_store::ConnectionKey::Saved(uuid::Uuid::new_v4());
    let path = external_dir.0.join("migrate.sql");
    crate::session_store::external::save(&path, "select 1;")
        .expect("seeding the external file must succeed");

    let model = build_model(cx);
    let id = model.update(cx, |model, cx| {
        model.new_external_tab(path.clone(), "migrate.sql".to_owned(), "select 1;", cx)
    });
    let editor = model.read_with(cx, |model, _app| model.tab(id).unwrap().editor().clone());
    editor.update(cx, |editor, cx| {
        editor.insert_text_for_test(" -- draft edit", cx);
    });

    let snapshot = model.read_with(cx, TabModel::snapshot);
    crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .save_snapshot(&snapshot)
        .expect("save must succeed");
    let loaded = crate::session_store::SessionDir::new(&sessions_dir.0, key)
        .load_snapshot()
        .expect("load must succeed")
        .expect("snapshot must exist");

    let restored = build_model(cx);
    restored.update(cx, |model, cx| model.load_for_connection(Some(&loaded), cx));

    restored.read_with(cx, |model, app| {
        let tab = &model.tabs()[0];
        assert_eq!(tab.editor().read(app).text(), " -- draft editselect 1;");
        assert!(
            tab.diverged(app),
            "a restored draft must leave the tab reporting diverged"
        );
    });
}

/// Restoring a session whose external file is missing (moved or deleted
/// since it was last open) and has no draft to fall back to skips only that
/// one tab, with every other tab in the snapshot restoring successfully --
/// unlike a missing session-owned sibling script, which stays a hard error
/// for the whole load (see `session_store::persistence`'s own
/// `loading_a_script_tab_whose_sibling_sql_file_is_missing_returns_a_read_error`,
/// left untouched).
#[gpui::test]
fn restoring_a_session_with_one_missing_external_file_skips_only_that_tab(cx: &mut TestAppContext) {
    use crate::session_store::{ScriptFileName, TabEntrySnapshot, TabKind, TabSessionSnapshot};

    let snapshot = TabSessionSnapshot {
        tabs: vec![
            TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: ScriptBacking::SessionScratch {
                        file: ScriptFileName::new("query-1.sql").unwrap(),
                    },
                },
                title: "query-1.sql".to_owned(),
                buffer_text: Some("select 1;".to_owned()),
            },
            TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: ScriptBacking::External {
                        path: std::path::PathBuf::from(
                            "/tmp/zsql-test-definitely-missing/gone.sql",
                        ),
                        saved_text: None,
                    },
                },
                title: "gone.sql".to_owned(),
                buffer_text: None,
            },
            TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: ScriptBacking::SessionScratch {
                        file: ScriptFileName::new("query-2.sql").unwrap(),
                    },
                },
                title: "query-2.sql".to_owned(),
                buffer_text: Some("select 2;".to_owned()),
            },
        ],
        active_index: Some(1),
    };

    let model = build_model(cx);
    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&snapshot), cx);
    });

    model.read_with(cx, |model, app| {
        assert_eq!(
            model.tabs().len(),
            2,
            "only the tab with the missing external file must be skipped"
        );
        assert_eq!(model.tabs()[0].title(), "query-1.sql");
        assert_eq!(model.tabs()[0].editor().read(app).text(), "select 1;");
        assert_eq!(model.tabs()[1].title(), "query-2.sql");
        assert_eq!(model.tabs()[1].editor().read(app).text(), "select 2;");
        assert!(
            model.active_tab().is_some(),
            "restore must still resolve to a valid active tab despite the skipped entry"
        );
    });
}

/// An external entry a restore could not open (file temporarily
/// unavailable, no draft) must not vanish from the next save's snapshot --
/// it is carried forward verbatim until a future restore can actually open
/// it as a live tab again.
#[gpui::test]
fn a_skipped_external_entry_is_carried_forward_into_the_next_snapshot(cx: &mut TestAppContext) {
    use crate::session_store::{ScriptFileName, TabEntrySnapshot, TabKind, TabSessionSnapshot};

    let skipped_entry = TabEntrySnapshot {
        kind: TabKind::Script {
            backing: ScriptBacking::External {
                path: std::path::PathBuf::from("/tmp/zsql-test-definitely-missing/gone.sql"),
                saved_text: None,
            },
        },
        title: "gone.sql".to_owned(),
        buffer_text: None,
    };
    let snapshot = TabSessionSnapshot {
        tabs: vec![
            TabEntrySnapshot {
                kind: TabKind::Script {
                    backing: ScriptBacking::SessionScratch {
                        file: ScriptFileName::new("query-1.sql").unwrap(),
                    },
                },
                title: "query-1.sql".to_owned(),
                buffer_text: Some("select 1;".to_owned()),
            },
            skipped_entry.clone(),
        ],
        active_index: Some(0),
    };

    let model = build_model(cx);
    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&snapshot), cx);
    });

    let resaved = model.read_with(cx, TabModel::snapshot);

    assert!(
        resaved.tabs.contains(&skipped_entry),
        "the unavailable external entry must still be present in the next snapshot, \
         not silently dropped: {:?}",
        resaved.tabs
    );
}

/// Once a live tab exists again for a path a past restore carried forward
/// (the file was temporarily unreachable), reopening that same path via
/// `open_or_focus_external` -- the seam "Browse files..." uses -- must
/// retire the stale carried-forward record. Without this, the next snapshot
/// would persist both the live tab's own entry and the stale one side by
/// side, and the save after that would restore two tabs racing each other
/// over the same file.
#[gpui::test]
fn open_or_focus_external_retires_a_stale_carried_forward_entry_for_the_same_path(
    cx: &mut TestAppContext,
) {
    let temp = TempDir::new("open-external-retires-carried-forward");
    std::fs::create_dir_all(&temp.0).expect("must create temp dir");
    let path = temp.0.join("report.sql");
    std::fs::write(&path, "select 1;").expect("must write the external file");

    let model = build_model(cx);
    let stale_entry = TabEntrySnapshot {
        kind: TabKind::Script {
            backing: ScriptBacking::External {
                path: path.clone(),
                saved_text: None,
            },
        },
        title: "report.sql".to_owned(),
        buffer_text: None,
    };
    model.update(cx, |model, _cx| {
        model.carried_forward_entries.push(stale_entry.clone());
    });

    model.update(cx, |model, cx| {
        model.open_or_focus_external(&path, "report.sql".to_owned(), "select 1;", cx);
    });

    let resaved = model.read_with(cx, TabModel::snapshot);
    let matching_count = resaved
        .tabs
        .iter()
        .filter(|entry| matches!(&entry.kind, TabKind::Script { backing: ScriptBacking::External { path: p, .. } } if p == &path))
        .count();
    assert_eq!(
        matching_count, 1,
        "reopening the same path live must retire the stale carried-forward record, not \
         leave a duplicate that would race the live tab on the next restore: {:?}",
        resaved.tabs
    );
}

/// A legacy `tabs.toml` can carry two `External` entries for the exact same
/// path (hand-edited, or written by an older version that did not dedupe on
/// save). Restoring both must never open two live tabs racing one inode --
/// `restore_tabs` itself dedupes by the same canonicalized-path comparison
/// [`TabModel::open_or_focus_external`] already uses.
#[gpui::test]
fn restore_tabs_dedupes_two_entries_for_the_same_external_path_into_a_single_tab(
    cx: &mut TestAppContext,
) {
    let temp = TempDir::new("restore-dedupes-duplicate-external-entries");
    std::fs::create_dir_all(&temp.0).expect("must create temp dir");
    let path = temp.0.join("report.sql");
    std::fs::write(&path, "select 1;").expect("must write the external file");

    let duplicated_entry = TabEntrySnapshot {
        kind: TabKind::Script {
            backing: ScriptBacking::External {
                path: path.clone(),
                saved_text: None,
            },
        },
        title: "report.sql".to_owned(),
        buffer_text: None,
    };
    let snapshot = TabSessionSnapshot {
        tabs: vec![duplicated_entry.clone(), duplicated_entry],
        active_index: Some(0),
    };

    let model = build_model(cx);
    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&snapshot), cx);
    });

    model.read_with(cx, |model, _app| {
        let matching_count = model
            .tabs()
            .iter()
            .filter(|tab| tab.external_path() == Some(path.as_path()))
            .count();
        assert_eq!(
            matching_count,
            1,
            "a legacy snapshot with two entries for the same path must restore exactly one \
             live tab, not two racing the same inode: {:?}",
            model.tabs().iter().map(Tab::title).collect::<Vec<_>>()
        );
    });

    let resaved = model.read_with(cx, TabModel::snapshot);
    let matching_count = resaved
        .tabs
        .iter()
        .filter(|entry| matches!(&entry.kind, TabKind::Script { backing: ScriptBacking::External { path: p, .. } } if p == &path))
        .count();
    assert_eq!(
        matching_count, 1,
        "the next snapshot must carry only the one surviving entry, not both: {:?}",
        resaved.tabs
    );
}

/// `open_or_focus_library`'s fresh-open branch creates a new library-backed
/// tab, loading its content from the library file.
#[gpui::test]
fn open_or_focus_library_opens_a_fresh_tab_for_a_name_not_already_open(cx: &mut TestAppContext) {
    let library_dir = TempDir::new("open-or-focus-library-fresh");
    crate::session_store::LibraryDir::at(&library_dir.0)
        .save(
            &crate::session_store::LibraryName::new("orders").unwrap(),
            "select * from orders;",
        )
        .expect("seeding the library file must succeed");

    let model = build_model(cx);
    model.update(cx, |model, _cx| {
        model.set_library_dir(Some(library_dir.0.clone()));
    });

    let id = model
        .update(cx, |model, cx| model.open_or_focus_library("orders", cx))
        .expect("a valid library name must open a tab");

    model.read_with(cx, |model, app| {
        assert_eq!(model.tabs().len(), 1);
        let tab = model.tab(id).unwrap();
        assert!(matches!(
            tab.script_backing(),
            Some(ScriptBacking::Library { .. })
        ));
        assert_eq!(tab.library_name(), Some("orders"));
        assert_eq!(tab.editor().read(app).text(), "select * from orders;");
        assert_eq!(model.active_id(), Some(id));
    });
}

/// A name that is not a valid `LibraryName` (e.g. one whose stem still
/// carries a reserved `.sql` suffix) must be refused rather than panic --
/// see `LibraryName::new`'s own validation.
#[gpui::test]
fn open_or_focus_library_refuses_a_name_that_is_not_a_valid_library_name(cx: &mut TestAppContext) {
    let library_dir = TempDir::new("open-or-focus-library-invalid-name");
    let model = build_model(cx);
    model.update(cx, |model, _cx| {
        model.set_library_dir(Some(library_dir.0.clone()));
    });

    let id = model.update(cx, |model, cx| {
        model.open_or_focus_library("report.sql", cx)
    });

    assert_eq!(id, None);
    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs().len(), 0);
    });
}

/// `open_or_focus_library` called twice for the same name must never create
/// a second tab: the second call focuses the tab the first call opened.
#[gpui::test]
fn open_or_focus_library_focuses_the_existing_tab_instead_of_duplicating_it(
    cx: &mut TestAppContext,
) {
    let library_dir = TempDir::new("open-or-focus-library-dedupe");
    crate::session_store::LibraryDir::at(&library_dir.0)
        .save(
            &crate::session_store::LibraryName::new("orders").unwrap(),
            "select 1;",
        )
        .expect("seeding the library file must succeed");

    let model = build_model(cx);
    model.update(cx, |model, _cx| {
        model.set_library_dir(Some(library_dir.0.clone()));
    });
    let first = model
        .update(cx, |model, cx| model.open_or_focus_library("orders", cx))
        .expect("a valid library name must open a tab");
    // Open a second tab and activate it, so the dedupe path has to actively
    // re-focus the first tab rather than trivially finding it already active.
    model.update(cx, TabModel::new_script_tab);

    let second = model
        .update(cx, |model, cx| model.open_or_focus_library("orders", cx))
        .expect("a valid library name must open a tab");

    assert_eq!(
        first, second,
        "opening an already-open library name must reuse its tab id"
    );
    model.read_with(cx, |model, _app| {
        assert_eq!(
            model.tabs().len(),
            2,
            "no duplicate tab must be created for a name already open"
        );
        assert_eq!(model.active_id(), Some(first));
    });
}

/// A named session script saved to disk, with no open tab for it, is still
/// reopenable by file name -- the single place both the sidebar and the
/// Open Script picker's "This connection" section route through for a
/// not-currently-open row.
#[gpui::test]
fn open_or_focus_session_script_opens_a_fresh_tab_loading_content_from_disk(
    cx: &mut TestAppContext,
) {
    let session_dir = TempDir::new("open-or-focus-session-script-fresh");
    std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
    std::fs::write(
        session_dir.0.join("scripts").join("top-customers.sql"),
        "select * from customers order by revenue desc;",
    )
    .expect("must write the session script");

    let model = build_model(cx);
    model.update(cx, |model, _cx| {
        model.set_session_dir(Some(session_dir.0.clone()));
    });

    let id = model
        .update(cx, |model, cx| {
            model.open_or_focus_session_script("top-customers.sql", cx)
        })
        .expect("the file exists and is readable, so this must open a tab");

    model.read_with(cx, |model, app| {
        let tab = model.tab(id).unwrap();
        assert!(!matches!(
            tab.script_backing(),
            Some(ScriptBacking::Library { .. })
        ));
        assert!(!matches!(
            tab.script_backing(),
            Some(ScriptBacking::External { .. })
        ));
        assert_eq!(tab.title(), "top-customers.sql");
        assert_eq!(
            tab.editor().read(app).text(),
            "select * from customers order by revenue desc;"
        );
        assert_eq!(model.active_id(), Some(id));
    });
}

/// A second call for a file name already open as a tab must focus the
/// existing tab rather than opening a duplicate.
#[gpui::test]
fn open_or_focus_session_script_focuses_the_existing_tab_instead_of_duplicating_it(
    cx: &mut TestAppContext,
) {
    let session_dir = TempDir::new("open-or-focus-session-script-dedupe");
    std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
    std::fs::write(
        session_dir.0.join("scripts").join("top-customers.sql"),
        "select 1;",
    )
    .expect("must write the session script");

    let model = build_model(cx);
    model.update(cx, |model, _cx| {
        model.set_session_dir(Some(session_dir.0.clone()));
    });
    let first = model
        .update(cx, |model, cx| {
            model.open_or_focus_session_script("top-customers.sql", cx)
        })
        .expect("the file exists and is readable, so this must open a tab");
    model.update(cx, TabModel::new_script_tab);

    let second = model
        .update(cx, |model, cx| {
            model.open_or_focus_session_script("top-customers.sql", cx)
        })
        .expect("an already-open tab must always be found and focused");

    assert_eq!(first, second);
    model.read_with(cx, |model, _app| {
        assert_eq!(
            model.tabs().len(),
            2,
            "no duplicate tab must be created for a file name already open"
        );
        assert_eq!(model.active_id(), Some(first));
    });
}

/// An unnamed, scratch-backed tab whose title happens to collide with a
/// named top-level file's own name (e.g. after the user names a script
/// "query-7", closes its tab, and a later unnamed tab mints that same
/// title) must never be treated as that file's own open tab: opening the
/// named file has to load it from disk into its own tab, never focus the
/// unrelated scratch-backed buffer.
#[gpui::test]
fn open_or_focus_session_script_never_matches_a_scratch_backed_tab_with_the_same_title(
    cx: &mut TestAppContext,
) {
    let session_dir = TempDir::new("open-or-focus-session-script-scratch-collision");
    std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
    std::fs::write(
        session_dir.0.join("scripts").join("query-7.sql"),
        "select 'top-level';",
    )
    .expect("must write the named top-level script");

    let model = build_model(cx);
    model.update(cx, |model, _cx| {
        model.set_session_dir(Some(session_dir.0.clone()));
    });
    let scratch_snapshot = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabKind::Script {
                backing: ScriptBacking::SessionScratch {
                    file: ScriptFileName::new("query-7.sql").unwrap(),
                },
            },
            title: "query-7.sql".to_owned(),
            buffer_text: Some("select 'scratch';".to_owned()),
        }],
        active_index: Some(0),
    };
    let scratch_id = model.update(cx, |model, cx| {
        model.load_for_connection(Some(&scratch_snapshot), cx);
        model.tabs()[0].id()
    });

    let opened_id = model
        .update(cx, |model, cx| {
            model.open_or_focus_session_script("query-7.sql", cx)
        })
        .expect("the named top-level file exists and is readable");

    assert_ne!(
        opened_id, scratch_id,
        "the scratch-backed tab must never be mistaken for the named file's own tab"
    );
    model.read_with(cx, |model, app| {
        assert_eq!(
            model.tabs().len(),
            2,
            "a fresh tab must be opened for the named file"
        );
        let opened = model.tab(opened_id).unwrap();
        assert_eq!(
            opened.editor().read(app).text(),
            "select 'top-level';",
            "the fresh tab must load the named file's own on-disk content"
        );
    });
}

/// `schema_by_relation` must be cleared on every connection switch, the
/// same as `generated_by_relation` -- otherwise a schema tab opened for a
/// relation key seen on an earlier connection resolves to a stale, already-
/// closed `TabId` and `open_or_reuse_schema` silently no-ops forever after.
#[gpui::test]
fn switching_connections_clears_the_schema_relation_reuse_map(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let first_id = model.update(cx, |model, cx| {
        model.open_or_reuse_schema("public", "orders", zsql_core::RelationKind::Table, cx)
    });

    // Standing in for a connection switch: every open tab (including the
    // schema tab) is discarded the way `load_for_connection` always does.
    model.update(cx, |model, cx| model.load_for_connection(None, cx));
    model.read_with(cx, |model, _app| {
        assert!(
            model.tab(first_id).is_none(),
            "the old schema tab must be gone"
        );
    });

    let second_id = model.update(cx, |model, cx| {
        model.open_or_reuse_schema("public", "orders", zsql_core::RelationKind::Table, cx)
    });

    assert_ne!(
        first_id, second_id,
        "reopening the same relation's schema after a connection switch must \
         create a fresh tab, not resolve to the stale, already-closed id"
    );
    model.read_with(cx, |model, _app| {
        assert!(model.tab(second_id).is_some());
        assert_eq!(model.active_id(), Some(second_id));
    });
}

/// An edit past a library-backed tab's first (e.g. one made after an
/// explicit save already cleared the dirty marker) must still eventually
/// notify, so its draft keeps tracking the live buffer instead of only ever
/// persisting on a structural event (tab/connection switch, close, quit).
#[gpui::test]
fn an_edit_after_the_first_on_a_library_tab_still_schedules_a_notify(cx: &mut TestAppContext) {
    let library_dir = TempDir::new("debounced-edit-notify");
    crate::session_store::LibraryDir::at(&library_dir.0)
        .save(
            &crate::session_store::LibraryName::new("orders").unwrap(),
            "select 1;",
        )
        .expect("seeding the library file must succeed");

    let model = build_model(cx);
    model.update(cx, |model, _cx| {
        model.set_library_dir(Some(library_dir.0.clone()));
        model.set_edit_debounce(std::time::Duration::from_millis(20));
    });
    let id = model
        .update(cx, |model, cx| model.open_or_focus_library("orders", cx))
        .expect("a valid library name must open a tab");
    let editor = model.read_with(cx, |model, _app| model.tab(id).unwrap().editor().clone());

    // First edit: notifies immediately (unchanged behavior).
    editor.update(cx, |editor, cx| {
        editor.insert_text_for_test("select 2;", cx);
    });
    cx.run_until_parked();
    let notify_count = std::rc::Rc::new(std::cell::Cell::new(0));
    let notify_count_for_sub = notify_count.clone();
    cx.update(|cx| {
        cx.observe(&model, move |_model, _cx| {
            notify_count_for_sub.set(notify_count_for_sub.get() + 1);
        })
        .detach();
    });

    // Second edit past the first: must not be silently dropped.
    editor.update(cx, |editor, cx| {
        editor.insert_text_for_test(" -- again", cx);
    });
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(100));
    cx.run_until_parked();

    assert!(
        notify_count.get() > 0,
        "an edit past the first must still eventually notify (debounced), \
         so a library/external tab's draft keeps tracking the live buffer"
    );
}

/// `open_or_focus_external`'s fresh-open branch creates a new
/// external-backed tab for a path not already open.
#[gpui::test]
fn open_or_focus_external_opens_a_fresh_tab_for_a_path_not_already_open(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let path = std::path::PathBuf::from("/home/t/work/migrate.sql");

    let id = model.update(cx, |model, cx| {
        model.open_or_focus_external(&path, "migrate.sql".to_owned(), "select 1;", cx)
    });

    model.read_with(cx, |model, app| {
        assert_eq!(model.tabs().len(), 1);
        let tab = model.tab(id).unwrap();
        assert!(matches!(
            tab.script_backing(),
            Some(ScriptBacking::External { .. })
        ));
        assert_eq!(tab.external_path(), Some(path.as_path()));
        assert_eq!(tab.editor().read(app).text(), "select 1;");
        assert_eq!(model.active_id(), Some(id));
    });
}

/// `open_or_focus_external` called twice for the same path must never
/// create a second tab: the second call focuses the tab the first opened.
#[gpui::test]
fn open_or_focus_external_focuses_the_existing_tab_instead_of_duplicating_it(
    cx: &mut TestAppContext,
) {
    let model = build_model(cx);
    let path = std::path::PathBuf::from("/home/t/work/migrate.sql");
    let first = model.update(cx, |model, cx| {
        model.open_or_focus_external(&path, "migrate.sql".to_owned(), "select 1;", cx)
    });
    // Open a second tab and activate it, so the dedupe path has to actively
    // re-focus the first tab rather than trivially finding it already active.
    model.update(cx, TabModel::new_script_tab);

    let second = model.update(cx, |model, cx| {
        model.open_or_focus_external(&path, "migrate.sql".to_owned(), "select 1;", cx)
    });

    assert_eq!(
        first, second,
        "opening an already-open external path must reuse its tab id"
    );
    model.read_with(cx, |model, _app| {
        assert_eq!(
            model.tabs().len(),
            2,
            "no duplicate tab must be created for a path already open"
        );
        assert_eq!(model.active_id(), Some(first));
    });
}

/// A symlink to an already-open external file must focus the existing tab
/// rather than open a duplicate racing saves against the same inode.
#[cfg(unix)]
#[gpui::test]
fn open_or_focus_external_dedupes_a_symlink_to_an_already_open_path(cx: &mut TestAppContext) {
    let dir = TempDir::new("open-or-focus-external-symlink");
    std::fs::create_dir_all(&dir.0).expect("must create dir");
    let real_path = dir.0.join("migrate.sql");
    std::fs::write(&real_path, "select 1;").expect("must write");
    let link_path = dir.0.join("migrate-link.sql");
    std::os::unix::fs::symlink(&real_path, &link_path).expect("must create symlink");

    let model = build_model(cx);
    let first = model.update(cx, |model, cx| {
        model.open_or_focus_external(&real_path, "migrate.sql".to_owned(), "select 1;", cx)
    });
    model.update(cx, TabModel::new_script_tab);

    let second = model.update(cx, |model, cx| {
        model.open_or_focus_external(&link_path, "migrate-link.sql".to_owned(), "select 1;", cx)
    });

    assert_eq!(
        first, second,
        "a symlink to an already-open path must resolve to the same tab"
    );
    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs().len(), 2, "no duplicate tab for the same inode");
    });
}

/// Subscribes to `model`'s [`SaveRequested`] events, returning a shared log
/// of every event emitted after this call.
fn subscribe_save_events(
    model: &Entity<TabModel>,
    cx: &mut TestAppContext,
) -> Rc<RefCell<Vec<SaveRequested>>> {
    let events = Rc::new(RefCell::new(Vec::new()));
    let events_for_sub = events.clone();
    cx.update(|cx| {
        cx.subscribe(model, move |_model, evt: &SaveRequested, _cx| {
            events_for_sub.borrow_mut().push(*evt);
        })
        .detach();
    });
    events
}
