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
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, SharedString, Task};
use zsql_core::ResultSet;
use zsql_core::sql::params::Parameter;
use zsql_editor::{EditorView, QueryRunner};

use super::editor_adapter;
use super::results::pager::{PreviewControls, PreviewDispatch};
use super::schema_view::SchemaTabView;
use crate::session::{Session, SessionState};
use crate::session_store::{ScriptBacking, TabEntrySnapshot, TabKind, TabSessionSnapshot};

mod constructors;
mod open_requests;
mod preview_actions;
mod save_requests;

pub use open_requests::OpenRequested;
pub use save_requests::{SaveRequested, ScriptOpenFailed};

/// Identifies one open tab, stable for its lifetime and never reused within
/// a single `TabModel`.
pub type TabId = u64;

/// A tab's captured query outcome: the label, lifecycle state, and result
/// set a [`ResultsView`] shows while that tab (rather than the live
/// `Session`) is what it is displaying. Captured once a tab's own run
/// reaches a terminal state, so switching back to that tab later restores
/// exactly what it last produced instead of whatever a different tab most
/// recently ran.
#[derive(Debug, Clone)]
pub struct ResultsSnapshot {
    pub source_label: SharedString,
    pub state: SessionState,
    pub result: Rc<ResultSet>,
}

/// Leading text of every title [`TabModel::new_script_tab`] mints, before
/// the number.
const SCRIPT_TITLE_PREFIX: &str = "query-";
/// Trailing text of every title [`TabModel::new_script_tab`] mints, after
/// the number.
const SCRIPT_TITLE_SUFFIX: &str = crate::session_store::SCRIPT_FILE_EXTENSION;

/// The title [`TabModel::new_script_tab`] gives its `n`th script tab.
fn script_title(n: u64) -> String {
    format!("{SCRIPT_TITLE_PREFIX}{n}{SCRIPT_TITLE_SUFFIX}")
}

/// The number a title matching [`script_title`]'s pattern was minted with,
/// or `None` for any other title (a `Generated` tab's relation name, or a
/// script tab renamed by the user).
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

    /// Whether the buffer has ever received a manual edit. Superseded by
    /// [`Self::diverged`] for the tab-bar marker
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
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

    /// This tab's pure, gpui-free backing (see [`ScriptBacking`]), for a
    /// `Script` tab. `None` for every other kind.
    #[must_use]
    pub fn script_backing(&self) -> Option<&ScriptBacking> {
        match &self.kind {
            TabKind::Script { backing } => Some(backing),
            TabKind::Generated { .. } | TabKind::Schema { .. } => None,
        }
    }

    /// This tab's library file's bare name, for a library-backed tab.
    #[must_use]
    pub fn library_name(&self) -> Option<&str> {
        self.script_backing().and_then(ScriptBacking::library_name)
    }

    /// This tab's absolute backing path, for an external-backed tab.
    #[must_use]
    pub fn external_path(&self) -> Option<&std::path::Path> {
        self.script_backing().and_then(ScriptBacking::external_path)
    }

    /// Whether this tab's own sibling file currently lives under the
    /// session directory's `scratch/` subdirectory, for a session-owned
    /// `Script` tab.
    #[must_use]
    fn is_session_scratch(&self) -> bool {
        matches!(
            self.script_backing(),
            Some(ScriptBacking::SessionScratch { .. })
        )
    }

    /// Whether this tab's live buffer currently diverges from its backing's
    /// last explicitly-saved content. Always `false` for a non-`Script` tab
    /// or a session-owned one
    #[must_use]
    pub fn diverged(&self, cx: &App) -> bool {
        self.script_backing()
            .is_some_and(|backing| backing.diverged(&self.editor.read(cx).text()))
    }
}

/// The results-header label a tab's runs are shown under: `schema.relation`
/// for a `Generated` tab, or the generic `"query"` label every `Script` tab
/// shares (matching `editor_adapter`'s label for a plain, ungenerated run).
fn display_label(tab: &Tab) -> String {
    match &tab.kind {
        TabKind::Generated {
            schema, relation, ..
        }
        | TabKind::Schema { schema, relation } => {
            format!("{schema}.{relation}")
        }
        TabKind::Script { .. } => "query".to_owned(),
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
    /// The single callback every sort/pager control routes its clicks
    /// through, built once and shared by every rendered control (see
    /// [`TabModel::preview_controls_for_active_tab`]) rather than a fresh
    /// closure per render.
    preview_dispatch: PreviewDispatch,
    /// Root directory the shared library's flat pool of `.sql` files lives
    /// under (typically [`crate::config::Config::library_dir`]). `None`
    /// disables library restore/lookup entirely
    library_dir: Option<PathBuf>,
    /// The active connection's own session directory, for opening a named
    /// session script by file name that is not already open as a tab
    session_dir: Option<PathBuf>,
    /// The shared claim factory [`Self::open_or_focus_session_script`].
    /// `None` in a test that drives this model directly without wiring one up.
    claims: Option<crate::session_store::SaveClaimFactory>,
    /// Debounce interval [`Self::mark_edited`] waits after any edit past a
    /// tab's first before notifying
    edit_debounce: std::time::Duration,
    /// Generation counter [`Self::mark_edited`]'s debounce timer checks
    /// before notifying
    edit_debounce_generation: Rc<std::cell::Cell<u64>>,
    /// External-backed entries the last restore could not open (the file
    /// was temporarily unavailable, e.g. an unmounted volume, with no draft
    /// to fall back to) but must not silently drop from `tabs.toml`
    carried_forward_entries: Vec<TabEntrySnapshot>,
}

pub enum ResultsChanged {
    /// Results are live - fetch them from the session
    Live(SharedString),
    /// The active live preview's window changed (a page, sort, or filter
    /// navigation) but its relation did not.
    LiveWindowChanged(SharedString),
    /// Results are loaded from a snapshot
    Snapshot(ResultsSnapshot),
}
pub struct PreviewControlsChanged(pub Option<PreviewControls>);

/// A Script tab's run was intercepted because its SQL contains one or more
/// detected parameters, emitted instead of dispatching so a host can open
/// the "Run with parameters" modal in place of reaching `Session::run_query`
/// directly.
#[derive(Debug, Clone)]
pub struct ParametersRequested {
    pub tab_id: TabId,
    /// The tab's own title, for the modal's eyebrow.
    pub script_label: String,
    /// The SQL the run was requested with, every parameter token intact.
    pub sql: String,
    pub parameters: Vec<Parameter>,
    /// Scopes where confirming this run remembers its values; see
    /// `crate::session_store::ScriptBacking::param_history_key`.
    pub history_key: String,
    /// The active connection's driver id, deciding both which native
    /// parameter syntax `parameters` was detected with and how the modal's
    /// own substitution escapes each value on confirm.
    pub driver_id: &'static str,
}

impl EventEmitter<ResultsChanged> for TabModel {}
impl EventEmitter<PreviewControlsChanged> for TabModel {}
impl EventEmitter<ParametersRequested> for TabModel {}

impl TabModel {
    /// Build an empty tab model over `session`/`results`, the same pair
    /// every tab's editor runs its queries through. Starts with no tabs;
    /// callers that always want an initial tab (e.g. the workspace, on
    /// startup) call [`TabModel::new_script_tab`] right after construction.
    #[must_use]
    pub fn new(session: Entity<Session>, cx: &mut Context<Self>) -> Self {
        let model = cx.entity();
        // Reused, not re-fetched: a sort or page change previews the same
        // relation the tab's initial `preview_relation` call already
        // counted, so its total is still valid (see
        // `Session::preview_relation_windowed`'s own doc comment). Syncing
        // it here on every `session` notification -- rather than only once
        // right after a run finishes -- also picks up the initial preview's
        // own count fetch, which resolves on its own schedule alongside
        // (not before) the query that renders the first page.
        cx.observe(&session, |model: &mut Self, session, cx| {
            let Some(id) = model.live_owner else {
                return;
            };
            let total = session.read(cx).row_count();
            let mut changed = false;
            if let Some(tab) = model.tabs.iter_mut().find(|tab| tab.id == id)
                && let TabKind::Generated { preview, .. } = &mut tab.kind
                && preview.total_rows() != total
            {
                preview.set_total_rows(total);
                changed = true;
            }
            // The pager and page readout render from a snapshot pushed by
            // `sync_preview_controls`, not from the tab directly. A session
            // notification fires for every streaming batch, but the count
            // lands just once, so refresh that snapshot only when the active
            // tab's total actually changes -- reflecting it right away without
            // re-syncing on every unrelated notification.
            if changed && model.active == Some(id) {
                model.sync_preview_controls(cx);
            }
        })
        .detach();

        Self {
            tabs: Vec::new(),
            active: None,
            generated_by_relation: HashMap::new(),
            schema_by_relation: HashMap::new(),
            next_id: 0,
            next_script_number: 1,
            live_owner: None,
            session,
            preview_dispatch: Rc::new(move |action, cx| {
                model.update(cx, |model, cx| model.dispatch_preview_action(action, cx));
            }),
            library_dir: None,
            session_dir: None,
            claims: None,
            edit_debounce: crate::config::AutosaveConfig::default().edit_debounce(),
            edit_debounce_generation: Rc::new(std::cell::Cell::new(0)),
            carried_forward_entries: Vec::new(),
        }
    }

    /// Set the root directory the shared library lives under
    pub fn set_library_dir(&mut self, library_dir: Option<PathBuf>) {
        self.library_dir = library_dir;
    }

    /// Update the active connection's own session directory, for opening a
    /// named session script by file name that is not already open as a tab
    pub fn set_session_dir(&mut self, session_dir: Option<PathBuf>) {
        self.session_dir = session_dir;
    }

    /// Set the shared claim factory
    pub fn set_claim_factory(&mut self, claims: crate::session_store::SaveClaimFactory) {
        self.claims = Some(claims);
    }

    /// Override the debounce interval [`Self::mark_edited`] uses past a
    /// tab's first edit
    pub(crate) fn set_edit_debounce(&mut self, duration: std::time::Duration) {
        self.edit_debounce = duration;
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
        let edit_model = model.clone();
        let save_model = model.clone();
        let save_as_model = model.clone();
        editor.update(cx, |editor, _cx| {
            editor.set_on_edit(Box::new(move |cx| {
                edit_model.update(cx, |model, cx| model.mark_edited(id, cx));
            }));
            editor.set_save_requester(Box::new(move |cx| {
                save_model.update(cx, |model, cx| model.request_save(id, cx));
            }));
            editor.set_save_as_requester(Box::new(move |cx| {
                save_as_model.update(cx, |model, cx| model.request_save_as(id, cx));
            }));
        });
        let open_model = model.clone();
        let browse_model = model;
        editor.update(cx, |editor, _cx| {
            editor.set_open_requester(Box::new(move |cx| {
                open_model.update(cx, TabModel::request_open);
            }));
            editor.set_browse_requester(Box::new(move |cx| {
                browse_model.update(cx, TabModel::request_browse);
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
            self.sync_preview_controls(cx);
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
            cx.emit(ResultsChanged::Live(label));
            return;
        }

        if let Some(snapshot) = tab.last_run.clone() {
            cx.emit(ResultsChanged::Snapshot(snapshot));
            return;
        }

        let state = self.session.read(cx).state().clone();
        if matches!(state, SessionState::Empty | SessionState::Connecting) {
            cx.emit(ResultsChanged::Live(label));
            return;
        }

        cx.emit(ResultsChanged::Snapshot(ResultsSnapshot {
            source_label: label,
            state: SessionState::Connected,
            result: Rc::new(ResultSet::default()),
        }));
    }

    /// Dispatch `task` (a run just started for `id`, labeled `label`) as
    /// this model's live run: `results` follows `session` live under
    /// `label` until this tab's run completes, at which point its final
    /// state/result are captured into [`Tab::last_run`] for any later
    /// switch back to it. `window_change` marks a same-relation page/sort/
    /// filter navigation rather than a fresh query run.
    fn dispatch_run(
        &mut self,
        id: TabId,
        label: String,
        task: Task<()>,
        window_change: bool,
        cx: &mut Context<Self>,
    ) {
        let label = SharedString::from(label);
        self.live_owner = Some(id);

        cx.emit(if window_change {
            ResultsChanged::LiveWindowChanged(label.clone())
        } else {
            ResultsChanged::Live(label.clone())
        });

        cx.spawn(async move |this, cx| {
            task.await;
            let _ = this.update(cx, |this, cx| this.finish_run(id, label, cx));
        })
        .detach();
    }

    /// Run `sql` for tab `id` through `session`, the `RunQuery`/Run-button
    /// seam every tab's editor is wired to (see [`TabModel::build_editor`]).
    /// A no-op while `session` holds no live connection, so a keystroke or
    /// click reaching here without a connection never dispatches.
    ///
    /// A `Script` tab's `sql` containing one or more parameters detected
    /// for the active connection's driver id (see
    /// `zsql_core::sql::params::detect_parameters`) emits
    /// [`ParametersRequested`] instead of dispatching: the caller opens the
    /// "Run with parameters" modal, then routes the user's filled-in run
    /// back through [`Self::run_confirmed_params`].
    #[tracing::instrument(name = "tab_model_run_for_tab", skip(self, sql, cx), fields(tab_id = id))]
    fn run_for_tab(&mut self, id: TabId, sql: String, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        if !self.session.read(cx).is_connected() {
            tracing::debug!(tab_id = id, "run rejected: not connected");
            return;
        }
        let label = display_label(tab);
        let kind = tab.kind.clone();
        // A live generated tab is a preview of its relation, so re-running
        // it must replay its current sort/page/filter window and refresh the
        // relation's total row count, exactly like the pager's own Reload; a
        // plain run_query would drop the window and clear the count. Once
        // edited (kind Script) the tab is an ordinary query and runs its own
        // text verbatim.
        let (task, window_change) = match kind {
            TabKind::Generated {
                schema,
                relation,
                preview,
            } => (
                self.session.update(cx, |session, cx| {
                    session.preview_relation_windowed(
                        &schema,
                        &relation,
                        preview.sort_pair(),
                        preview.page_size(),
                        preview.offset(),
                        preview.filters(),
                        true,
                        cx,
                    )
                }),
                true,
            ),
            TabKind::Script { backing } => {
                let driver_id = self
                    .session
                    .read(cx)
                    .driver_id()
                    .unwrap_or(crate::drivers::UNKNOWN_DRIVER_ID);
                let parameters = zsql_core::sql::params::detect_parameters(&sql, driver_id);
                if parameters.is_empty() {
                    (
                        self.session
                            .update(cx, |session, cx| session.run_query(sql, cx)),
                        false,
                    )
                } else {
                    tracing::debug!(
                        tab_id = id,
                        parameter_count = parameters.len(),
                        driver_id,
                        "run intercepted: opening the parameters modal"
                    );
                    cx.emit(ParametersRequested {
                        tab_id: id,
                        script_label: tab.title().to_owned(),
                        sql,
                        parameters,
                        history_key: backing.param_history_key(),
                        driver_id,
                    });
                    return;
                }
            }
            // A schema tab is read-only and has no query to run.
            TabKind::Schema { .. } => return,
        };
        self.dispatch_run(id, label, task, window_change, cx);
    }

    /// Run `sql` (already parameter-substituted by the "Run with
    /// parameters" modal) for tab `id`, exactly like a parameter-free
    /// [`Self::run_for_tab`] would have dispatched it.
    #[tracing::instrument(name = "tab_model_run_confirmed_params", skip(self, sql, cx), fields(tab_id = id))]
    pub fn run_confirmed_params(&mut self, id: TabId, sql: String, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        if !self.session.read(cx).is_connected() {
            tracing::debug!(
                tab_id = id,
                "confirmed-parameters run rejected: not connected"
            );
            return;
        }
        let label = display_label(tab);
        let task = self
            .session
            .update(cx, |session, cx| session.run_query(sql, cx));
        self.dispatch_run(id, label, task, false, cx);
    }

    /// Re-point `results` at the active tab's current state, without
    /// dispatching a run.
    pub fn resync_results(&mut self, cx: &mut Context<Self>) {
        self.sync_results_to_active(cx);
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
                result: Rc::new(session.result().clone()),
            }
        };
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.last_run = Some(snapshot);
        }
        cx.notify();
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
            TabKind::Generated {
                schema, relation, ..
            } => {
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
            TabKind::Script { .. } => {}
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
            self.sync_preview_controls(cx);
        }

        tracing::info!(tab_id = id, "closed tab");
        cx.notify();
    }

    /// Record that tab `id`'s buffer just received a manual edit. The first
    /// time this fires for a given tab, a `Generated` tab permanently
    /// converts to `Script` (dropping the generated flag and its
    /// relation-reuse entry), any tab's dirty flag flips on, and this
    /// notifies immediately so the conversion (and, for a library/external
    /// tab, the dirty marker) is visible at once. Every edit past the first
    /// -- including one after an explicit save cleared the dirty marker on a
    /// library/external tab -- still needs to eventually flush to disk (a
    /// session-owned tab autosaves continuously; a library/external tab's
    /// draft must keep tracking the live buffer), so it schedules a
    /// debounced notify instead of firing on every keystroke: the workspace
    /// saves the whole tab session on every `TabModel` notify, and would
    /// otherwise re-snapshot and rewrite the entire session directory once
    /// per keystroke.
    fn mark_edited(&mut self, id: TabId, cx: &mut Context<Self>) {
        // Computed up front, before taking the mutable borrow below: two
        // live tabs can share a title (e.g. two relations both named
        // `orders` in different schemas), so the scratch file this
        // conversion is about to assign must be disambiguated against every
        // other currently-open scratch-backed tab's own file, never derived
        // from `title` alone at the point a rename later needs it back.
        let new_scratch_file = self
            .tabs
            .iter()
            .find(|tab| tab.id == id)
            .filter(|tab| matches!(tab.kind, TabKind::Generated { .. }))
            .map(|tab| {
                let used: std::collections::HashSet<String> = self
                    .tabs
                    .iter()
                    .filter(|other| other.id != id && other.is_session_scratch())
                    .filter_map(|other| {
                        other
                            .script_backing()
                            .and_then(ScriptBacking::session_file)
                            .map(|file| file.as_str().to_owned())
                    })
                    .collect();
                crate::session_store::unique_script_file_name(&tab.title, &used)
            });

        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            return;
        };
        let first_edit = !tab.dirty;
        tab.dirty = true;

        if let TabKind::Generated {
            schema, relation, ..
        } = tab.kind.clone()
        {
            tracing::info!(
                tab_id = id,
                schema = %schema,
                relation = %relation,
                "generated tab converted to script on first edit"
            );
            self.generated_by_relation.remove(&(schema, relation));
            // The user never named this script by editing a live preview
            // into one: it keeps the bare relation name as its title, but
            // its sibling file is unnamed the same as a fresh query-N tab's.
            let file_name = new_scratch_file
                .unwrap_or_else(|| crate::session_store::script_file_name(&tab.title));
            let file = crate::session_store::ScriptFileName::new(file_name)
                .expect("a disambiguated scratch candidate is always a valid ScriptFileName");
            tab.kind = TabKind::Script {
                backing: ScriptBacking::SessionScratch { file },
            };

            // `mark_edited` runs from inside this same editor's own
            // `EditListener` (see `build_editor`), i.e. while its entity is
            // already mid-update, so dropping compact mode has to happen
            // after that update finishes rather than re-entering it here.
            let editor = tab.editor.clone();
            cx.defer(move |cx| {
                editor.update(cx, |editor, _cx| editor.set_compact(false));
            });
        }

        self.sync_preview_controls(cx);
        if first_edit {
            cx.notify();
        } else {
            self.schedule_debounced_edit_notify(cx);
        }
    }

    /// Notify after [`Self::edit_debounce`] elapses, unless a later edit's
    /// own call to this method has already superseded it -- so a burst of
    /// keystrokes collapses into exactly one notify (and thus one
    /// downstream autosave/draft write) after typing pauses, rather than
    /// one per keystroke.
    fn schedule_debounced_edit_notify(&mut self, cx: &mut Context<Self>) {
        let generation = self.edit_debounce_generation.get() + 1;
        self.edit_debounce_generation.set(generation);
        let generation_cell = self.edit_debounce_generation.clone();
        let duration = self.edit_debounce;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(duration).await;
            if generation_cell.get() != generation {
                return;
            }
            let _ = this.update(cx, |_this, cx| cx.notify());
        })
        .detach();
    }

    /// This model's entire tab state as a persistable, window-independent
    /// snapshot: every tab's kind/title and kind-specific payload, in order,
    /// plus which one is active. A `Generated` tab's SQL is never itself
    /// part of the payload -- it is always machine-written from its own
    /// `TabKind::Generated::preview`, so only that state is captured; a
    /// `Script` tab's text is user-authored, so its full buffer is captured
    /// instead.
    #[must_use]
    pub fn snapshot(&self, cx: &App) -> TabSessionSnapshot {
        let mut tabs: Vec<TabEntrySnapshot> = self
            .tabs
            .iter()
            .filter_map(|tab| {
                let buffer_text = match &tab.kind {
                    TabKind::Script { backing } => match backing {
                        ScriptBacking::SessionScratch { .. }
                        | ScriptBacking::SessionNamed { .. } => Some(tab.editor.read(cx).text()),
                        ScriptBacking::Library { .. } | ScriptBacking::External { .. } => {
                            let text = tab.editor.read(cx).text();
                            tab.diverged(cx).then_some(text)
                        }
                    },
                    // `tab.dirty` is always `false` here: `mark_edited`
                    // converts a `Generated` tab to `TabKind::Script` in the
                    // same call that would otherwise set it, so a live tab
                    // can never be both still-`Generated` and dirty.
                    TabKind::Generated { .. } => None,
                    // A schema tab is a read-only view re-openable from the
                    // sidebar at any time and carries no editable buffer, so
                    // it is intentionally not persisted.
                    TabKind::Schema { .. } => return None,
                };
                Some(TabEntrySnapshot {
                    kind: tab.kind.clone(),
                    title: tab.title.clone(),
                    buffer_text,
                })
            })
            .collect();
        // Index into the persisted (non-schema) tabs, so it lines up with
        // what `restore_tabs` rebuilds; an active schema tab leaves no active
        // index and restore falls back to the first tab. Computed before
        // appending carried-forward entries, which have no live tab (and
        // thus no position) of their own.
        let active_index = self.active.and_then(|id| {
            self.tabs
                .iter()
                .filter(|tab| !matches!(tab.kind, TabKind::Schema { .. }))
                .position(|tab| tab.id == id)
        });
        tabs.extend(self.carried_forward_entries.iter().cloned());
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
        self.schema_by_relation.clear();
        self.active = None;
        self.live_owner = None;
        self.carried_forward_entries.clear();

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
        self.sync_preview_controls(cx);
        cx.notify();
    }

    /// Drop any [`Self::carried_forward_entries`] item whose external path
    /// canonicalizes to `path`, since a live tab now exists for it again via
    /// a fresh [`Self::open_or_focus_external`] call (the sole caller: a
    /// [`Self::restore_tabs`] pass always starts from a freshly cleared
    /// [`Self::carried_forward_entries`] -- see `Self::load_for_connection`
    /// -- so it never has a stale record of its own left to retire). Without
    /// this, a file that was briefly unreachable (an unmounted volume, say)
    /// keeps a stale carried-forward record forever once it becomes
    /// reachable again: the next save would then persist both the live
    /// tab's own entry and the stale one side by side, and the save after
    /// that would restore two tabs racing each other over the same file.
    fn retire_carried_forward_external_entry(&mut self, path: &std::path::Path) {
        let canonical = canonicalize_or_self(path);
        self.carried_forward_entries.retain(|entry| {
            !matches!(
                &entry.kind,
                TabKind::Script {
                    backing: ScriptBacking::External { path, .. }
                } if canonicalize_or_self(path) == canonical
            )
        });
    }
}

/// `path` canonicalized (resolving symlinks and `.`/`..` components), or
/// `path` itself unchanged if it cannot be canonicalized (e.g. it no longer
/// exists)
fn canonicalize_or_self(path: &std::path::Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

mod restore;
#[cfg(test)]
mod tests;
