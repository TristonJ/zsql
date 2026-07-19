//! The editor tab model: an ordered set of tabs, each owning its own
//! `zsql_editor::EditorView` buffer independently of any other tab's. A tab
//! is `Generated` (auto-preview SQL for a clicked relation, reused on
//! reopen until its buffer is manually edited), `Script` (a normal,
//! freely-editable buffer), or `Schema` (a read-only structural view of a
//! relation, never editable). Opening a generated tab, reusing one, and
//! converting one to a script all drive `Session`/`ResultsView`, which is
//! why this lives in the binary's `ui` module rather than in `zsql-editor`
//! (framework-agnostic) or `zsql-core` (driver-agnostic).

use std::collections::HashMap;

use gpui::{App, AppContext as _, Context, Entity, SharedString, Task};
use zsql_core::RelationKind;
use zsql_editor::{EditorView, QueryRunner};

use super::editor_adapter;
use super::results::{ResultsSnapshot, ResultsView};
use super::schema_view::SchemaTabView;
use crate::session::{Session, SessionState};
use crate::tab_session::{TabEntryKind, TabEntrySnapshot, TabSessionSnapshot};

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
    /// A read-only structural view of `schema.relation`'s columns, indexes,
    /// and constraints (see [`TabModel::open_or_reuse_schema`]). Never
    /// editable and never converts to `Script`.
    Schema { schema: String, relation: String },
}

/// Leading text of every title [`TabModel::new_script_tab`] mints, before
/// the number.
const SCRIPT_TITLE_PREFIX: &str = "query-";
/// Trailing text of every title [`TabModel::new_script_tab`] mints, after
/// the number.
const SCRIPT_TITLE_SUFFIX: &str = ".sql";

/// The title [`TabModel::new_script_tab`] gives its `n`th script tab.
fn script_title(n: u64) -> String {
    format!("{SCRIPT_TITLE_PREFIX}{n}{SCRIPT_TITLE_SUFFIX}")
}

/// The number a title matching [`script_title`]'s pattern was minted with,
/// or `None` for any other title (a `Generated` tab's relation name, or a
/// script tab renamed by the user, once renaming exists).
fn parse_script_number(title: &str) -> Option<u64> {
    title
        .strip_prefix(SCRIPT_TITLE_PREFIX)?
        .strip_suffix(SCRIPT_TITLE_SUFFIX)?
        .parse()
        .ok()
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
    /// The read-only schema view for a `Schema` tab; `None` for every other
    /// kind.
    schema_view: Option<Entity<SchemaTabView>>,
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

    #[must_use]
    pub fn is_schema(&self) -> bool {
        matches!(self.kind, TabKind::Schema { .. })
    }

    /// This tab's schema view, for a `Schema` tab. `None` for every other
    /// kind.
    #[must_use]
    pub fn schema_view(&self) -> Option<&Entity<SchemaTabView>> {
        self.schema_view.as_ref()
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
        TabKind::Generated { schema, relation } | TabKind::Schema { schema, relation } => {
            format!("{schema}.{relation}")
        }
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
    /// Maps a relation to its already-open `Schema` tab. An entry is
    /// removed only when that tab closes: a `Schema` tab has no edit path,
    /// so (unlike `generated_by_relation`) nothing else ever invalidates it.
    schema_by_relation: HashMap<(String, String), TabId>,
    next_id: TabId,
    /// Numbers successive `query-N.sql` titles for tabs opened via
    /// [`TabModel::new_script_tab`], starting at 1 and never reused.
    /// [`TabModel::restore_tabs`] advances this past the highest number
    /// among the titles it restores, so a tab opened right after a restore
    /// can never collide with a restored title.
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
            schema_by_relation: HashMap::new(),
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
    /// when no tab is active (every tab has been closed) or the active tab
    /// is a `Schema` tab: a schema tab renders its own view in place of the
    /// shared results grid entirely (see `ui::workspace`), so `results`
    /// is simply left showing whatever it last held, restored correctly the
    /// next time a non-`Schema` tab becomes active.
    fn sync_results_to_active(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active else {
            return;
        };
        let Some(tab) = self.tab(id) else {
            return;
        };
        if tab.is_schema() {
            return;
        }
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
        let Some(tab) = self.tab(id) else {
            return;
        };
        let label = display_label(tab);
        let kind = tab.kind.clone();
        // A live generated tab is a preview of its relation, so re-running it
        // must refresh that relation's total row count the same way opening it
        // did. preview_relation fetches the count; a plain run_query would
        // leave it cleared. Once edited (kind Script) the tab is an ordinary
        // query and runs its own text verbatim.
        let task = match kind {
            TabKind::Generated { schema, relation } => self.session.update(cx, |session, cx| {
                session.preview_relation(&schema, &relation, cx)
            }),
            TabKind::Script => self
                .session
                .update(cx, |session, cx| session.run_query(sql, cx)),
            // A schema tab is read-only and has no query to run.
            TabKind::Schema { .. } => return,
        };
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
    /// showing exactly the SQL text `Session::preview_relation` itself
    /// executes for it. Reuses the relation's existing live (never-edited)
    /// generated tab instead of creating a duplicate, if one exists --
    /// re-focusing it with whatever it last showed rather than re-running
    /// the query, since a live generated tab's buffer (and thus its SQL)
    /// cannot have changed since that run.
    pub fn open_or_reuse_generated(
        &mut self,
        schema: &str,
        relation: &str,
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
        // Read the exact text `preview_relation` (dispatched below) is about
        // to execute, so the buffer a user sees can never drift from what
        // actually runs.
        let sql = self.session.read(cx).preview_sql(schema, relation);
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
            schema_view: None,
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
        let title = script_title(self.next_script_number);
        self.next_script_number += 1;

        tracing::info!(tab_id = id, title = %title, "opened new script tab");
        self.tabs.push(Tab {
            id,
            kind: TabKind::Script,
            title,
            editor,
            dirty: false,
            last_run: None,
            schema_view: None,
        });
        self.active = Some(id);
        self.sync_results_to_active(cx);
        cx.notify();
        id
    }

    /// Open (or, if `schema.relation` already has one open, reuse/activate)
    /// a read-only `Schema` tab for `schema.relation` and make it active.
    /// `kind` is the relation's [`RelationKind`], shown in the tab's header
    /// kind pill. The tab dispatches its own `describe_relation` (and
    /// row-count) fetches independently of `session`'s shared
    /// query-lifecycle state -- see [`SchemaTabView::new`].
    pub fn open_or_reuse_schema(
        &mut self,
        schema: &str,
        relation: &str,
        kind: RelationKind,
        cx: &mut Context<Self>,
    ) -> TabId {
        let key = (schema.to_owned(), relation.to_owned());
        if let Some(&id) = self.schema_by_relation.get(&key) {
            tracing::info!(tab_id = id, schema, relation, "reusing open schema tab");
            self.set_active(id, cx);
            return id;
        }

        let id = self.allocate_id();
        // A schema tab has no editable buffer: this editor is built for
        // uniformity with every other tab kind (e.g. `Tab::editor` stays
        // infallible) but is never rendered or focused for a `Schema` tab,
        // so its `RunQuery`/edit wiring is simply never triggered.
        let editor = Self::build_editor(id, cx);
        let session = self.session.clone();
        let schema_view = cx.new(|cx| {
            SchemaTabView::new(&session, schema.to_owned(), relation.to_owned(), kind, cx)
        });

        self.tabs.push(Tab {
            id,
            kind: TabKind::Schema {
                schema: schema.to_owned(),
                relation: relation.to_owned(),
            },
            title: relation.to_owned(),
            editor,
            dirty: false,
            last_run: None,
            schema_view: Some(schema_view),
        });
        self.schema_by_relation.insert(key, id);
        self.active = Some(id);
        self.sync_results_to_active(cx);

        tracing::info!(tab_id = id, schema, relation, "opened schema tab");
        cx.notify();
        id
    }

    /// Close `id`, dropping its editor. Updates the active tab to a
    /// neighboring tab if `id` was active (or clears it if `id` was the last
    /// tab), and, if `id` was a live generated tab or an open schema tab,
    /// removes it from the corresponding relation reuse map.
    pub fn close_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let closed = self.tabs.remove(index);
        match &closed.kind {
            TabKind::Generated { schema, relation } => {
                let key = (schema.clone(), relation.clone());
                if self.generated_by_relation.get(&key) == Some(&id) {
                    self.generated_by_relation.remove(&key);
                }
            }
            TabKind::Schema { schema, relation } => {
                let key = (schema.clone(), relation.clone());
                if self.schema_by_relation.get(&key) == Some(&id) {
                    self.schema_by_relation.remove(&key);
                }
            }
            TabKind::Script => {}
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

    /// This model's entire tab state as a persistable, window-independent
    /// snapshot: every tab's kind/title/buffer text, in order, plus which
    /// one is active.
    #[must_use]
    pub fn snapshot(&self, cx: &App) -> TabSessionSnapshot {
        let tabs = self
            .tabs
            .iter()
            .filter_map(|tab| {
                let kind = match &tab.kind {
                    TabKind::Script => TabEntryKind::Script,
                    // `tab.dirty` is always `false` here: `mark_edited`
                    // converts a `Generated` tab to `TabKind::Script` in the
                    // same call that would otherwise set it, so a live tab
                    // can never be both still-`Generated` and dirty. The
                    // field exists so the persisted shape has somewhere to
                    // carry that conversion if the live behavior ever
                    // changes, and so `restore_tabs` has a single rule
                    // (check `edited`) instead of two.
                    TabKind::Generated { schema, relation } => TabEntryKind::Generated {
                        schema: schema.clone(),
                        relation: relation.clone(),
                        edited: tab.dirty,
                    },
                    // A schema tab is a read-only view re-openable from the
                    // sidebar at any time and carries no editable buffer, so
                    // it is intentionally not persisted.
                    TabKind::Schema { .. } => return None,
                };
                Some(TabEntrySnapshot {
                    kind,
                    title: tab.title.clone(),
                    buffer_text: tab.editor.read(cx).text(),
                })
            })
            .collect();
        // Index into the persisted (non-schema) tabs, so it lines up with
        // what `restore_tabs` rebuilds; an active schema tab leaves no active
        // index and restore falls back to the first tab.
        let active_index = self.active.and_then(|id| {
            self.tabs
                .iter()
                .filter(|tab| !matches!(tab.kind, TabKind::Schema { .. }))
                .position(|tab| tab.id == id)
        });
        TabSessionSnapshot { tabs, active_index }
    }

    /// Replace every open tab with `snapshot`'s tabs (or, if `snapshot` is
    /// `None` or holds no tabs, the same single-empty-script default a
    /// brand new workspace opens with), for a connection that was just
    /// (re)connected. Never merges with whatever tabs were already open --
    /// switching the connection a tab set belongs to always swaps the whole
    /// set rather than folding one into the other.
    pub fn load_for_connection(
        &mut self,
        snapshot: Option<&TabSessionSnapshot>,
        cx: &mut Context<Self>,
    ) {
        self.tabs.clear();
        self.generated_by_relation.clear();
        self.active = None;
        self.live_owner = None;

        match snapshot {
            Some(snapshot) if !snapshot.tabs.is_empty() => self.restore_tabs(snapshot, cx),
            _ => {
                // No restored titles to stay clear of, so a connection with
                // no snapshot always starts back at the same "query-1.sql"
                // a brand new workspace opens with, rather than carrying
                // over whatever number a previous connection's tabs left
                // behind.
                self.next_script_number = 1;
                self.new_script_tab(cx);
            }
        }

        tracing::info!(
            tab_count = self.tabs.len(),
            "tab session loaded for connection"
        );
        self.sync_results_to_active(cx);
        cx.notify();
    }

    /// Rebuild `self.tabs` from `snapshot`'s entries: same order, kind,
    /// title, and buffer text. A `Generated` entry whose persisted `edited`
    /// flag is set restores as [`TabKind::Script`] instead, consistent with
    /// the live conversion [`TabModel::mark_edited`] performs on a
    /// generated tab's first edit.
    ///
    /// Never triggers a query: [`EditorView::set_text`] does not invoke the
    /// on-edit listener, so restoring a buffer's text neither marks a
    /// restored tab dirty nor dispatches anything through `session`.
    fn restore_tabs(&mut self, snapshot: &TabSessionSnapshot, cx: &mut Context<Self>) {
        self.next_script_number = snapshot
            .tabs
            .iter()
            .filter_map(|entry| parse_script_number(&entry.title))
            .max()
            .map_or(1, |highest| highest + 1);

        for entry in &snapshot.tabs {
            let id = self.allocate_id();
            let editor = Self::build_editor(id, cx);
            editor.update(cx, |editor, cx| editor.set_text(&entry.buffer_text, cx));

            let (kind, dirty) = match &entry.kind {
                TabEntryKind::Script => (TabKind::Script, false),
                TabEntryKind::Generated { edited: true, .. } => (TabKind::Script, true),
                TabEntryKind::Generated {
                    schema,
                    relation,
                    edited: false,
                } => (
                    TabKind::Generated {
                        schema: schema.clone(),
                        relation: relation.clone(),
                    },
                    false,
                ),
            };
            if let TabKind::Generated { schema, relation } = &kind {
                self.generated_by_relation
                    .insert((schema.clone(), relation.clone()), id);
                editor.update(cx, |editor, _cx| editor.set_compact(true));
            }

            self.tabs.push(Tab {
                id,
                kind,
                title: entry.title.clone(),
                editor,
                dirty,
                last_run: None,
                schema_view: None,
            });
        }

        self.active = snapshot
            .active_index
            .and_then(|index| self.tabs.get(index))
            .or_else(|| self.tabs.first())
            .map(Tab::id);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use gpui::{AppContext as _, Entity, SharedString, TestAppContext};
    use zsql_core::{
        BatchSink, ColumnMeta, Connection, CoreError, QueryEvent, QueryHandle, RowCount, SchemaTree,
    };

    use super::{TabKind, TabModel};
    use crate::session::Session;
    use crate::tab_session::{TabEntryKind, TabEntrySnapshot, TabSessionSnapshot};
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
    fn re_running_a_generated_preview_tab_refreshes_the_relation_row_count(
        cx: &mut TestAppContext,
    ) {
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
    fn a_generated_tabs_displayed_sql_matches_what_preview_relation_executes(
        cx: &mut TestAppContext,
    ) {
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
    fn reopening_a_relation_whose_tab_was_edited_creates_a_new_generated_tab(
        cx: &mut TestAppContext,
    ) {
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
    fn opening_a_generated_tab_shows_it_live_then_captures_its_finished_run(
        cx: &mut TestAppContext,
    ) {
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
    fn snapshot_captures_every_tabs_kind_title_buffer_and_the_active_index(
        cx: &mut TestAppContext,
    ) {
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
    fn restoring_a_snapshot_rebuilds_the_expected_tabs_order_and_active_tab(
        cx: &mut TestAppContext,
    ) {
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
    fn connecting_with_no_snapshot_yields_the_default_single_empty_script_tab(
        cx: &mut TestAppContext,
    ) {
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
    fn switching_between_two_connections_snapshots_swaps_the_whole_tab_set(
        cx: &mut TestAppContext,
    ) {
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
}
