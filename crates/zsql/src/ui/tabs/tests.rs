use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use gpui::{AppContext as _, Entity, SharedString, TestAppContext};
use zsql_core::{
    BatchSink, ColumnMeta, Connection, CoreError, QueryEvent, QueryHandle, RowCount, SchemaTree,
};

use super::{Tab, TabKind, TabModel};
use crate::session::Session;
use crate::tab_session::{TabEntryKind, TabEntrySnapshot, TabSessionSnapshot};
use crate::ui::results::{ResultsSnapshot, ResultsView};

/// Test-only accessor for asserting on a tab's captured run.
impl Tab {
    pub(crate) fn last_run_for_test(&self) -> Option<&ResultsSnapshot> {
        self.last_run.as_ref()
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

/// A `Connection` double that hands back every `stream_query` call's
/// sink, in call order, letting a test control exactly when (and
/// whether) a dispatched run's events arrive -- unlike `FakeConnection`,
/// whose sinks a test can never reach, so its runs never resolve.
struct RecordingConnection {
    sinks: Arc<Mutex<Vec<BatchSink>>>,
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

/// Like [`build_model_with_results`], but backed by a
/// [`RecordingConnection`] so a test can independently complete (or
/// leave in flight) each tab's own dispatched run, by sending directly
/// on the sink `stream_query` was called with.
fn build_model_with_recording_connection(
    cx: &mut TestAppContext,
) -> (Entity<TabModel>, Arc<Mutex<Vec<BatchSink>>>) {
    let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
    let connection: Arc<dyn Connection> = Arc::new(RecordingConnection {
        sinks: sinks.clone(),
    });
    let session = cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)));
    let session_for_results = session.clone();
    let results = cx.update(|cx| cx.new(|cx| ResultsView::new(session_for_results, "", cx)));
    let model = cx.update(|cx| cx.new(|cx| TabModel::new(session, results, cx)));
    (model, sinks)
}

#[gpui::test]
fn re_running_a_generated_preview_tab_refreshes_the_relation_row_count(cx: &mut TestAppContext) {
    let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
    let connection: Arc<dyn Connection> = Arc::new(RecordingConnection { sinks });
    let session = cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)));
    let results = cx.update(|cx| cx.new(|cx| ResultsView::new(session.clone(), "", cx)));
    let model = cx.update(|cx| cx.new(|cx| TabModel::new(session.clone(), results, cx)));

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
    let session_for_results = session.clone();
    let results = cx.update(|cx| cx.new(|cx| ResultsView::new(session_for_results, "", cx)));
    let results_for_model = results.clone();
    let model = cx.update(|cx| cx.new(|cx| TabModel::new(session, results_for_model, cx)));
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

    fn preview_query(&self, schema: &str, relation: &str, limit: u64) -> String {
        format!("SELECT TOP ({limit}) * FROM [{schema}].[{relation}]")
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
    let results = cx.update(|cx| cx.new(|cx| ResultsView::new(session.clone(), "", cx)));
    let model = cx.update(|cx| cx.new(|cx| TabModel::new(session, results, cx)));

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
