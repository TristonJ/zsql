//! The editor tab model: an ordered set of tabs, each owning its own
//! `zsql_editor::EditorView` buffer independently of any other tab's. A tab
//! is either `Generated` (auto-preview SQL for a clicked relation, reused on
//! reopen until its buffer is manually edited) or `Script` (a normal,
//! freely-editable buffer). Opening a generated tab, reusing one, and
//! converting one to a script all drive `Session`/`ResultsView` and reuse
//! `crate::sql::preview_sql`, which is why this lives in the binary's `ui`
//! module rather than in `zsql-editor` (framework-agnostic) or `zsql-core`
//! (driver-agnostic).

use std::collections::HashMap;

use gpui::{AppContext as _, Context, Entity, SharedString, Task};
use zsql_editor::{EditorView, QueryRunner};

use super::editor_adapter;
use super::results::{ResultsSnapshot, ResultsView};
use crate::session::{Session, SessionState};
use crate::sql::preview_sql;

/// Identifies one open tab, stable for its lifetime and never reused within
/// a single `TabModel`.
pub type TabId = u64;

/// What kind of buffer a tab holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabKind {
    /// Auto-generated preview SQL for `schema.relation`, live for reuse
    /// (see [`TabModel::open_or_reuse_generated`]) until the buffer receives
    /// a manual edit.
    Generated { schema: String, relation: String },
    /// A normal, freely-editable script buffer.
    Script,
}

/// The SQL text a `Generated` tab shows for `schema.relation`: exactly the
/// text `Session::preview_relation` executes, so what a generated tab
/// displays always matches what actually ran.
#[must_use]
pub fn generated_tab_sql(schema: &str, relation: &str, preview_limit: u64) -> String {
    preview_sql(schema, relation, preview_limit)
}

/// One open editor tab: its kind, display title, own independent editor
/// buffer, and whether it has unsaved edits.
pub struct Tab {
    id: TabId,
    kind: TabKind,
    /// The relation name for a `Generated` tab; a `query-N.sql`-style name
    /// for a `Script` tab opened via [`TabModel::new_script_tab`]. Unchanged
    /// by [`TabModel`]'s conversion of a generated tab to a script, so an
    /// edited "orders" tab stays titled "orders".
    title: String,
    editor: Entity<EditorView>,
    /// Set once the buffer receives any manual edit. For a `Generated` tab
    /// this coincides with (and triggers) its permanent conversion to
    /// `Script`; for a `Script` tab it marks the tab's title with a
    /// trailing `*`.
    dirty: bool,
    /// This tab's own most recently completed run, captured from `Session`
    /// once that run reaches a terminal state. Restored into the shared
    /// `ResultsView` whenever this tab becomes active but is not the one
    /// `Session` is currently running a query for, so switching tabs shows
    /// each tab's own last results rather than whichever tab ran most
    /// recently. `None` for a tab that has never run.
    last_run: Option<ResultsSnapshot>,
}

impl Tab {
    #[must_use]
    pub fn id(&self) -> TabId {
        self.id
    }

    #[must_use]
    pub fn kind(&self) -> &TabKind {
        &self.kind
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn editor(&self) -> &Entity<EditorView> {
        &self.editor
    }

    #[must_use]
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    #[must_use]
    pub fn is_generated(&self) -> bool {
        matches!(self.kind, TabKind::Generated { .. })
    }
}

/// Test-only accessor for asserting on a tab's captured run.
#[cfg(test)]
impl Tab {
    pub(crate) fn last_run_for_test(&self) -> Option<&ResultsSnapshot> {
        self.last_run.as_ref()
    }
}

/// The results-header label a tab's runs are shown under: `schema.relation`
/// for a `Generated` tab, or the generic `"query"` label every `Script` tab
/// shares (matching `editor_adapter`'s label for a plain, ungenerated run).
fn display_label(tab: &Tab) -> String {
    match &tab.kind {
        TabKind::Generated { schema, relation } => format!("{schema}.{relation}"),
        TabKind::Script => "query".to_owned(),
    }
}

/// Owns the workspace's open editor tabs: their order, which one is active,
/// and the relation -> tab reuse mapping for live generated tabs.
pub struct TabModel {
    tabs: Vec<Tab>,
    active: Option<TabId>,
    /// Maps a relation to its live (never-edited) `Generated` tab. An entry
    /// is removed as soon as that tab converts to `Script` or closes, so a
    /// later click on the same relation always opens a fresh tab instead of
    /// re-focusing stale, already-edited state.
    generated_by_relation: HashMap<(String, String), TabId>,
    next_id: TabId,
    /// Numbers successive `query-N.sql` titles for tabs opened via
    /// [`TabModel::new_script_tab`], starting at 1 and never reused.
    next_script_number: u64,
    /// The tab whose run `session` is currently tracking live (streaming or
    /// just completed): [`TabModel::set_active`] shows `results` live for
    /// this tab, and shows every other tab's own captured
    /// [`Tab::last_run`] instead. Set whenever a run is dispatched; never
    /// cleared on completion, since `session`'s state stays valid for this
    /// tab until a different tab's run replaces it.
    live_owner: Option<TabId>,
    session: Entity<Session>,
    results: Entity<ResultsView>,
}

impl TabModel {
    /// Build an empty tab model over `session`/`results`, the same pair
    /// every tab's editor runs its queries through. Starts with no tabs;
    /// callers that always want an initial tab (e.g. the workspace, on
    /// startup) call [`TabModel::new_script_tab`] right after construction.
    #[must_use]
    pub fn new(
        session: Entity<Session>,
        results: Entity<ResultsView>,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            generated_by_relation: HashMap::new(),
            next_id: 0,
            next_script_number: 1,
            live_owner: None,
            session,
            results,
        }
    }

    #[must_use]
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    #[must_use]
    pub fn active_id(&self) -> Option<TabId> {
        self.active
    }

    #[must_use]
    pub fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.tab(id))
    }

    #[must_use]
    fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    fn allocate_id(&mut self) -> TabId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Build a fresh `EditorView` whose `RunQuery`/Run-button seam dispatches
    /// through this model's [`TabModel::run_for_tab`] for `id` (rather than
    /// the generic `editor_adapter` seam every tab shared before per-tab run
    /// tracking existed), and whose `EditListener` reports a manual edit
    /// back to [`TabModel::mark_edited`] for `id`.
    fn build_editor(id: TabId, cx: &mut Context<Self>) -> Entity<EditorView> {
        let model = cx.entity();
        let run_query: QueryRunner = {
            let model = model.clone();
            Box::new(move |sql, cx| {
                model.update(cx, |model, cx| model.run_for_tab(id, sql, cx));
            })
        };
        let editor = cx.new(|cx| editor_adapter::new_tab_editor_view(run_query, cx));
        editor.update(cx, |editor, _cx| {
            editor.set_on_edit(Box::new(move |cx| {
                model.update(cx, |model, cx| model.mark_edited(id, cx));
            }));
        });
        editor
    }

    /// Make `id` the active tab, if it exists, and bring `results` up to
    /// date with it: live if `id` is the tab `session` is currently running
    /// a query for, else that tab's own captured [`Tab::last_run`] (or an
    /// empty placeholder if it has never run).
    pub fn set_active(&mut self, id: TabId, cx: &mut Context<Self>) {
        if self.tab(id).is_some() {
            self.active = Some(id);
            self.sync_results_to_active(cx);
            cx.notify();
        }
    }

    /// Point `results` at the active tab's own state: live if it is the
    /// tab `session` is currently running a query for, else its captured
    /// `last_run` snapshot, else an empty "never run" placeholder. A no-op
    /// when no tab is active (every tab has been closed).
    fn sync_results_to_active(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active else {
            return;
        };
        let Some(tab) = self.tab(id) else {
            return;
        };
        let label = SharedString::from(display_label(tab));

        if self.live_owner == Some(id) {
            self.results
                .update(cx, |results, cx| results.show_live(label, cx));
            return;
        }

        let snapshot = tab.last_run.clone().unwrap_or_else(|| ResultsSnapshot {
            source_label: label,
            state: SessionState::Connected,
            result: zsql_core::ResultSet::default(),
        });
        self.results
            .update(cx, |results, cx| results.show_snapshot(snapshot, cx));
    }

    /// Dispatch `task` (a run just started for `id`, labeled `label`) as
    /// this model's live run: `results` follows `session` live under
    /// `label` until this tab's run completes, at which point its final
    /// state/result are captured into [`Tab::last_run`] for any later
    /// switch back to it.
    fn dispatch_run(&mut self, id: TabId, label: String, task: Task<()>, cx: &mut Context<Self>) {
        let label = SharedString::from(label);
        self.live_owner = Some(id);
        self.results
            .update(cx, |results, cx| results.show_live(label.clone(), cx));

        cx.spawn(async move |this, cx| {
            task.await;
            let _ = this.update(cx, |this, cx| this.finish_run(id, label, cx));
        })
        .detach();
    }

    /// Run `sql` for tab `id` through `session`, the `RunQuery`/Run-button
    /// seam every tab's editor is wired to (see [`TabModel::build_editor`]).
    fn run_for_tab(&mut self, id: TabId, sql: String, cx: &mut Context<Self>) {
        let Some(label) = self.tab(id).map(display_label) else {
            return;
        };
        let task = self
            .session
            .update(cx, |session, cx| session.run_query(sql, cx));
        self.dispatch_run(id, label, task, cx);
    }

    /// Capture tab `id`'s just-finished run into its [`Tab::last_run`], from
    /// whatever `session` holds now that the run's task has resolved.
    ///
    /// Only captures while `id` is still `live_owner`: a run's task can
    /// resolve after a later run (for a different tab) has already taken
    /// over `session` -- a stale event draining out of the superseded run's
    /// own channel is enough to unblock its task, per `Session::run_query`'s
    /// own generation check -- and by then `session.state()`/`result()`
    /// belong to that other tab, not to `id`. Skipping the capture in that
    /// case leaves `id`'s `last_run` as whatever it was before this run
    /// (`None` for a tab's first run), rather than recording another tab's
    /// results under `id`'s label.
    fn finish_run(&mut self, id: TabId, label: SharedString, cx: &mut Context<Self>) {
        if self.live_owner != Some(id) {
            return;
        }
        let snapshot = {
            let session = self.session.read(cx);
            ResultsSnapshot {
                source_label: label,
                state: session.state().clone(),
                result: session.result().clone(),
            }
        };
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.last_run = Some(snapshot);
        }
        cx.notify();
    }

    /// Open a `Generated` tab for `schema.relation` and make it active,
    /// executing the same preview SQL text `Session::preview_relation`
    /// itself builds from `preview_limit`. Reuses the relation's existing
    /// live (never-edited) generated tab instead of creating a duplicate, if
    /// one exists -- re-focusing it with whatever it last showed rather than
    /// re-running the query, since a live generated tab's buffer (and thus
    /// its SQL) cannot have changed since that run.
    pub fn open_or_reuse_generated(
        &mut self,
        schema: &str,
        relation: &str,
        preview_limit: u64,
        cx: &mut Context<Self>,
    ) -> TabId {
        let key = (schema.to_owned(), relation.to_owned());
        if let Some(&id) = self.generated_by_relation.get(&key) {
            tracing::info!(tab_id = id, schema, relation, "reusing live generated tab");
            self.set_active(id, cx);
            return id;
        }

        let id = self.allocate_id();
        let editor = Self::build_editor(id, cx);
        let sql = generated_tab_sql(schema, relation, preview_limit);
        editor.update(cx, |editor, cx| {
            editor.set_text(&sql, cx);
            editor.set_compact(true);
        });

        self.tabs.push(Tab {
            id,
            kind: TabKind::Generated {
                schema: schema.to_owned(),
                relation: relation.to_owned(),
            },
            title: relation.to_owned(),
            editor,
            dirty: false,
            last_run: None,
        });
        self.generated_by_relation.insert(key, id);
        self.active = Some(id);

        let task = self.session.update(cx, |session, cx| {
            session.preview_relation(schema, relation, cx)
        });
        self.dispatch_run(id, format!("{schema}.{relation}"), task, cx);

        tracing::info!(tab_id = id, schema, relation, "opened generated tab");
        cx.notify();
        id
    }

    /// Open a new, empty `Script` tab titled `query-N.sql` and make it
    /// active. The `+` tab-bar affordance's action.
    pub fn new_script_tab(&mut self, cx: &mut Context<Self>) -> TabId {
        let id = self.allocate_id();
        let editor = Self::build_editor(id, cx);
        let title = format!("query-{}.sql", self.next_script_number);
        self.next_script_number += 1;

        tracing::info!(tab_id = id, title = %title, "opened new script tab");
        self.tabs.push(Tab {
            id,
            kind: TabKind::Script,
            title,
            editor,
            dirty: false,
            last_run: None,
        });
        self.active = Some(id);
        self.sync_results_to_active(cx);
        cx.notify();
        id
    }

    /// Close `id`, dropping its editor. Updates the active tab to a
    /// neighboring tab if `id` was active (or clears it if `id` was the last
    /// tab), and, if `id` was a live generated tab, removes it from the
    /// relation reuse map.
    pub fn close_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let closed = self.tabs.remove(index);
        if let TabKind::Generated { schema, relation } = &closed.kind {
            let key = (schema.clone(), relation.clone());
            if self.generated_by_relation.get(&key) == Some(&id) {
                self.generated_by_relation.remove(&key);
            }
        }
        if self.live_owner == Some(id) {
            self.live_owner = None;
        }

        if self.active == Some(id) {
            self.active = if self.tabs.is_empty() {
                None
            } else {
                Some(self.tabs[index.min(self.tabs.len() - 1)].id)
            };
            self.sync_results_to_active(cx);
        }

        tracing::info!(tab_id = id, "closed tab");
        cx.notify();
    }

    /// Record that tab `id`'s buffer just received a manual edit. The first
    /// time this fires for a given tab, a `Generated` tab permanently
    /// converts to `Script` (dropping the generated flag and its
    /// relation-reuse entry) and any tab's dirty flag flips on; later edits
    /// to an already-dirty tab are no-ops, so a generated tab can never
    /// revert even if further edits happen to recreate its original SQL
    /// text.
    fn mark_edited(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            return;
        };
        if tab.dirty {
            return;
        }
        tab.dirty = true;

        if let TabKind::Generated { schema, relation } = tab.kind.clone() {
            tracing::info!(
                tab_id = id,
                schema = %schema,
                relation = %relation,
                "generated tab converted to script on first edit"
            );
            self.generated_by_relation.remove(&(schema, relation));
            tab.kind = TabKind::Script;

            // `mark_edited` runs from inside this same editor's own
            // `EditListener` (see `build_editor`), i.e. while its entity is
            // already mid-update, so dropping compact mode has to happen
            // after that update finishes rather than re-entering it here.
            let editor = tab.editor.clone();
            cx.defer(move |cx| {
                editor.update(cx, |editor, _cx| editor.set_compact(false));
            });
        }

        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use gpui::{AppContext as _, Entity, SharedString, TestAppContext};
    use zsql_core::{
        BatchSink, ColumnMeta, Connection, CoreError, QueryEvent, QueryHandle, SchemaTree,
    };

    use super::{TabKind, TabModel, generated_tab_sql};
    use crate::session::Session;
    use crate::ui::results::ResultsView;

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

    fn build_model(cx: &mut TestAppContext) -> Entity<TabModel> {
        build_model_with_results(cx).0
    }

    /// Like [`build_model`], but also returns the shared `ResultsView`
    /// entity so a test can assert on what it is currently showing.
    fn build_model_with_results(
        cx: &mut TestAppContext,
    ) -> (Entity<TabModel>, Entity<ResultsView>) {
        let connection: Arc<dyn Connection> = Arc::new(FakeConnection);
        let session = cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)));
        let session_for_results = session.clone();
        let results = cx.update(|cx| cx.new(|cx| ResultsView::new(session_for_results, "", cx)));
        let results_for_model = results.clone();
        let model = cx.update(|cx| cx.new(|cx| TabModel::new(session, results_for_model, cx)));
        (model, results)
    }

    #[test]
    fn generated_tab_sql_matches_preview_sql_unchanged() {
        assert_eq!(
            generated_tab_sql("public", "orders", 200),
            "SELECT * FROM \"public\".\"orders\" LIMIT 200"
        );
    }

    #[gpui::test]
    fn opening_a_relation_creates_one_generated_tab_and_activates_it(cx: &mut TestAppContext) {
        let model = build_model(cx);
        model.update(cx, |model, cx| {
            model.open_or_reuse_generated("public", "orders", 200, cx);
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
            model.open_or_reuse_generated("public", "orders", 200, cx)
        });

        // Focus a different tab first so reopening has to actively
        // re-focus, not just happen to already be active.
        model.update(cx, |model, cx| {
            model.new_script_tab(cx);
        });
        let second_id = model.update(cx, |model, cx| {
            model.open_or_reuse_generated("public", "orders", 200, cx)
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
            model.open_or_reuse_generated("public", "orders", 200, cx)
        });
        let users_id = model.update(cx, |model, cx| {
            model.open_or_reuse_generated("public", "users", 200, cx)
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
            model.open_or_reuse_generated("public", "orders", 200, cx)
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
    fn reopening_a_relation_whose_tab_was_edited_creates_a_new_generated_tab(
        cx: &mut TestAppContext,
    ) {
        let model = build_model(cx);
        let first_id = model.update(cx, |model, cx| {
            model.open_or_reuse_generated("public", "orders", 200, cx)
        });
        let first_editor = model.read_with(cx, |model, _app| model.tabs()[0].editor().clone());
        first_editor.update(cx, |editor, cx| editor.insert_text_for_test("x", cx));

        let second_id = model.update(cx, |model, cx| {
            model.open_or_reuse_generated("public", "orders", 200, cx)
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
            model.open_or_reuse_generated("public", "orders", 200, cx)
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
            model.open_or_reuse_generated("public", "orders", 200, cx)
        });

        model.update(cx, |model, cx| model.close_tab(first_id, cx));
        let second_id = model.update(cx, |model, cx| {
            model.open_or_reuse_generated("public", "orders", 200, cx)
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
    fn switching_the_active_tab_does_not_touch_either_tabs_text_or_dirty_state(
        cx: &mut TestAppContext,
    ) {
        let model = build_model(cx);
        let generated_id = model.update(cx, |model, cx| {
            model.open_or_reuse_generated("public", "orders", 200, cx)
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
    fn opening_a_generated_tab_shows_it_live_then_captures_its_finished_run(
        cx: &mut TestAppContext,
    ) {
        let (model, results) = build_model_with_results(cx);
        let id = model.update(cx, |model, cx| {
            model.open_or_reuse_generated("public", "orders", 200, cx)
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
            model.open_or_reuse_generated("public", "orders", 200, cx)
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
            model.open_or_reuse_generated("public", "orders", 200, cx)
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
            model.open_or_reuse_generated("public", "orders", 200, cx)
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
            model.open_or_reuse_generated("public", "orders", 200, cx)
        });
        cx.run_until_parked();

        // Opening a second relation's generated tab dispatches its own run
        // before "orders"'s has reached a terminal state, superseding it.
        let users_id = model.update(cx, |model, cx| {
            model.open_or_reuse_generated("public", "users", 200, cx)
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
}
