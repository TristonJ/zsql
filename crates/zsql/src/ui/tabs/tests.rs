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

use super::{PreviewControlsChanged, ResultsChanged, ResultsSnapshot, Tab, TabKind, TabModel};
use crate::session::Session;
use crate::tab_session::{TabEntryKind, TabEntrySnapshot, TabSessionSnapshot};
use crate::ui::results::ResultsView;
use crate::ui::results::pager::PreviewAction;

/// Test-only accessors: a tab's captured run, and its sort/page window (see
/// [`Tab::preview_state`]'s field doc for when the latter is meaningful).
impl Tab {
    pub(crate) fn last_run_for_test(&self) -> Option<&ResultsSnapshot> {
        self.last_run.as_ref()
    }

    pub(crate) fn preview_state(&self) -> &zsql_core::preview_state::PreviewQueryState {
        &self.preview_state
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
        assert_eq!(
            tab.kind(),
            &TabKind::Generated {
                schema: "public".to_owned(),
                relation: "orders".to_owned()
            }
        );
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
        assert_eq!(tab.kind(), &TabKind::Script);
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
        assert_eq!(
            first_tab.kind(),
            &TabKind::Script,
            "the old, edited tab is left untouched as a script"
        );
        let second_tab = model
            .tabs()
            .iter()
            .find(|tab| tab.id() == second_id)
            .unwrap();
        assert_eq!(
            second_tab.kind(),
            &TabKind::Generated {
                schema: "public".to_owned(),
                relation: "orders".to_owned()
            }
        );
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
        assert_eq!(
            tab.kind(),
            &TabKind::Script,
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
        assert_eq!(tab.kind(), &TabKind::Script);
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
                buffer_text: "select 1;\n".to_owned(),
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
        TabEntryKind::Generated {
            schema: "public".to_owned(),
            relation: "orders".to_owned(),
            edited: false,
        }
    );
    assert_eq!(
        snapshot.tabs[0].buffer_text,
        "SELECT * FROM \"public\".\"orders\" LIMIT 200"
    );
    assert_eq!(snapshot.tabs[0].title, "orders");
    assert_eq!(snapshot.tabs[1].kind, TabEntryKind::Script);
    assert_eq!(snapshot.tabs[1].buffer_text, "select 1;");
    assert_eq!(snapshot.tabs[1].title, "query-1.sql");
    assert_eq!(
        snapshot.active_index,
        Some(1),
        "the active tab is the script tab, at index 1"
    );
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
        assert_eq!(
            model.tabs()[0].kind(),
            &TabKind::Generated {
                schema: "public".to_owned(),
                relation: "orders".to_owned(),
            }
        );
        assert_eq!(model.tabs()[0].title(), "orders");
        assert_eq!(
            model.tabs()[0].editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200"
        );
        assert!(!model.tabs()[0].dirty());

        assert_eq!(model.tabs()[1].kind(), &TabKind::Script);
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
            kind: TabEntryKind::Script,
            title: "query-1.sql".to_owned(),
            buffer_text: String::new(),
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
            kind: TabEntryKind::Script,
            title: "query-5.sql".to_owned(),
            buffer_text: String::new(),
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

/// `TabModel::snapshot` can never actually produce a `Generated` entry
/// with `edited: true` -- `mark_edited` converts a tab to
/// `TabKind::Script` in the same call that would otherwise dirty it, so
/// a live tab is never simultaneously `Generated` and dirty. This test
/// constructs that combination by hand to pin `restore_tabs`'s defensive
/// handling of it, in case a future change to the persisted shape (or a
/// hand-edited store file) ever produces it.
#[gpui::test]
fn a_restored_generated_tab_marked_edited_comes_back_as_a_script(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let snapshot = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabEntryKind::Generated {
                schema: "public".to_owned(),
                relation: "orders".to_owned(),
                edited: true,
            },
            title: "orders".to_owned(),
            buffer_text: "SELECT * FROM \"public\".\"orders\" LIMIT 200 -- edited".to_owned(),
        }],
        active_index: Some(0),
    };

    model.update(cx, |model, cx| {
        model.load_for_connection(Some(&snapshot), cx);
    });

    model.read_with(cx, |model, app| {
        let tab = &model.tabs()[0];
        assert_eq!(
            tab.kind(),
            &TabKind::Script,
            "an edited generated entry must restore as a script"
        );
        assert!(tab.dirty());
        assert_eq!(
            tab.editor().read(app).text(),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200 -- edited"
        );
    });

    // A restored, edited tab's relation must not have been registered
    // for live reuse: reopening it must create a fresh generated tab.
    let new_id = model.update(cx, |model, cx| {
        model.open_or_reuse_generated("public", "orders", cx)
    });
    model.read_with(cx, |model, _app| {
        assert_eq!(model.tabs().len(), 2);
        assert_ne!(new_id, model.tabs()[0].id());
    });
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
        assert_eq!(tab.kind(), &TabKind::Script);
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
        assert_eq!(model.tabs()[0].kind(), &TabKind::Script);
        assert_eq!(model.tabs()[0].editor().read(app).text(), "");
    });
}

#[gpui::test]
fn switching_between_two_connections_snapshots_swaps_the_whole_tab_set(cx: &mut TestAppContext) {
    let model = build_model(cx);
    let snapshot_a = two_tab_snapshot();
    let snapshot_b = TabSessionSnapshot {
        tabs: vec![TabEntrySnapshot {
            kind: TabEntryKind::Script,
            title: "b-query.sql".to_owned(),
            buffer_text: "select 'b';".to_owned(),
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
        assert!(matches!(model.tab(id).unwrap().kind(), TabKind::Script));
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
    assert!(
        model.read_with(cx, |model, _app| model.tabs()[0]
            .preview_state()
            .filters()
            .is_empty()),
        "a filter action on a detached tab must not commit a filter"
    );
    assert_eq!(
        sinks.lock().expect("sinks lock poisoned").len(),
        runs_before,
        "a filter action on an edited tab must not dispatch a run"
    );
}
