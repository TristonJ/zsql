//! The results grid's staged-changes queue: staging/restoring a row from the
//! cell context menu, discarding, expanding the ledger, and running Apply.

use gpui::{AnyElement, App, Context, Entity, EventEmitter, Render, Window, div, prelude::*};
use zsql_core::schema_detail::RelationSchema;
use zsql_editor::{Highlighter as _, SqlHighlighter, StyleSpan};

use super::ledger::{LedgerLine, StagedLedger};
use super::staging_bar::StagingBar;
use super::{ApplyStagedChanges, ResultsView};
use crate::session::Session;
use crate::staging::{RowIdentity, StagedChangeId, StagedChangeQueue};

/// Fetch state of the active relation's [`RelationSchema`].
enum RelationSchemaFetch {
    Idle,
    Loading,
    Ready(RelationSchema),
    Failed,
}

/// Where the staged-changes queue's most recent Apply stands.
enum ApplyState {
    Idle,
    Applying,
    /// The batch failed on the statement for `entry` (`None` if the failure
    /// happened opening/closing the transaction itself rather than running
    /// one of the queue's own statements), leaving the queue fully staged.
    Failed {
        entry: Option<StagedChangeId>,
        message: String,
    },
}

/// Emitted by [`StagingState`] whenever an Apply commits, so its host can
/// reload whatever result is on screen.
pub(super) struct StagedChangesApplied;

/// The staged-changes side of a [`ResultsView`], as its own entity: the
/// queue, the ledger's expanded state, the active relation's schema fetch,
/// and the most recent Apply's state, plus the async work against `session`
/// all of it drives.
pub(super) struct StagingState {
    session: Entity<Session>,
    queue: StagedChangeQueue,
    ledger_open: bool,
    /// The `(schema, relation)` `relation_schema` currently belongs to.
    relation_schema_key: Option<(String, String)>,
    /// The active relation's full structural detail.
    relation_schema: RelationSchemaFetch,
    apply_state: ApplyState,
    /// Highlights the ledger's statements. Owned here so the tree-sitter
    /// parser and query are built once, not per render.
    highlighter: SqlHighlighter,
}

impl EventEmitter<StagedChangesApplied> for StagingState {}

impl StagingState {
    pub fn new(session: Entity<Session>) -> Self {
        Self {
            session,
            queue: StagedChangeQueue::new(),
            ledger_open: false,
            relation_schema_key: None,
            relation_schema: RelationSchemaFetch::Idle,
            apply_state: ApplyState::Idle,
            highlighter: SqlHighlighter::new(),
        }
    }

    /// The active relation's schema, once fetched.
    pub fn relation_schema(&self) -> Option<&RelationSchema> {
        match &self.relation_schema {
            RelationSchemaFetch::Ready(schema) => Some(schema),
            _ => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// The id of the queued change targeting `identity`, if any.
    pub fn find_staged(&self, identity: &RowIdentity) -> Option<StagedChangeId> {
        if self.queue.is_empty() {
            return None;
        }
        self.queue.find_staged(identity)
    }

    /// Whether an Apply is currently in flight, blocking queue mutation.
    fn applying(&self) -> bool {
        matches!(self.apply_state, ApplyState::Applying)
    }

    /// Stage a delete of `identity` (read from `source_row`) if it is not
    /// already staged, or unstage it if it is. A no-op while an Apply is in
    /// flight against the queue.
    pub fn stage_or_restore(
        &mut self,
        source_row: usize,
        identity: RowIdentity,
        cx: &mut Context<Self>,
    ) {
        if self.applying() {
            tracing::trace!("stage/restore requested while an apply is in flight; ignored");
            return;
        }
        if let Some(id) = self.queue.find_staged(&identity) {
            self.queue.unstage(id);
            tracing::info!(row = source_row, "restored a staged row delete");
        } else {
            self.queue.stage_delete(source_row, identity);
            tracing::info!(
                row = source_row,
                staged_count = self.queue.len(),
                "staged a row delete"
            );
        }
        self.settle_after_mutation();
        cx.notify();
    }

    /// The ledger's per-line unstage control. A no-op while an Apply is in
    /// flight against the queue.
    pub fn unstage(&mut self, id: StagedChangeId, cx: &mut Context<Self>) {
        if self.applying() {
            tracing::trace!("unstage requested while an apply is in flight; ignored");
            return;
        }
        if self.queue.unstage(id) {
            tracing::info!(remaining = self.queue.len(), "unstaged a queued change");
        }
        self.settle_after_mutation();
        cx.notify();
    }

    /// `Discard all`: clear the entire queue in one action. A quiet no-op
    /// when the queue is already empty or an Apply is in flight against it.
    pub fn discard_all(&mut self, cx: &mut Context<Self>) {
        if self.queue.is_empty() || self.applying() {
            return;
        }
        let discarded = self.queue.len();
        self.queue.discard_all();
        self.settle_after_mutation();
        tracing::info!(discarded, "discarded the staged-changes queue");
        cx.notify();
    }

    /// `review sql` / `hide sql`: toggle the ledger panel.
    pub fn toggle_ledger(&mut self, cx: &mut Context<Self>) {
        self.ledger_open = !self.ledger_open;
        cx.notify();
    }

    /// After any queue mutation: an empty queue has no ledger to show, and
    /// a stale Apply failure no longer describes the queue's contents.
    fn settle_after_mutation(&mut self) {
        if self.queue.is_empty() {
            self.ledger_open = false;
        }
        self.apply_state = ApplyState::Idle;
    }

    /// Send the entire queue to the active connection as one transaction. A
    /// no-op while the queue is empty or a previous Apply is still in flight
    /// (stage/unstage/discard are also blocked for the duration, so the
    /// queue submitted here is exactly what completes). On success, removes
    /// exactly the entries submitted and emits [`StagedChangesApplied`]; on
    /// failure, leaves the queue staged and attaches the database's error to
    /// the entry that failed.
    #[tracing::instrument(name = "staging_apply", skip(self, cx))]
    pub fn apply(&mut self, cx: &mut Context<Self>) {
        if self.queue.is_empty() || self.applying() {
            return;
        }
        let applying_ids: Vec<StagedChangeId> =
            self.queue.entries().iter().map(|entry| entry.id).collect();
        let statements = self.queue.statements();
        tracing::info!(
            statement_count = statements.len(),
            "applying staged changes"
        );
        self.apply_state = ApplyState::Applying;
        cx.notify();

        let task = self
            .session
            .update(cx, |session, cx| session.run_in_transaction(statements, cx));

        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            let _ = this.update(cx, |staging, cx| {
                match outcome {
                    Ok(()) => {
                        for id in &applying_ids {
                            staging.queue.unstage(*id);
                        }
                        if staging.queue.is_empty() {
                            staging.ledger_open = false;
                        }
                        staging.apply_state = ApplyState::Idle;
                        tracing::info!(applied = applying_ids.len(), "staged changes applied");
                        cx.emit(StagedChangesApplied);
                    }
                    Err(failure) => {
                        let failed_entry = failure
                            .statement_index
                            .and_then(|index| applying_ids.get(index).copied());
                        tracing::warn!(
                            entry = ?failed_entry,
                            error = %failure.message,
                            "staged changes apply failed"
                        );
                        staging.apply_state = ApplyState::Failed {
                            entry: failed_entry,
                            message: failure.message,
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Fetch `target`'s [`RelationSchema`] via `session` when it names a
    /// relation this entity has not already fetched (or is already fetching)
    /// for. A no-op while `target` is `None` (the active tab is not a live
    /// generated preview) or its relation is unchanged. While the session
    /// holds no live connection yet (a restored tab activating during
    /// startup's connect), the relation is left unmarked so the host's
    /// session-observer resync retries once the connection lands.
    pub fn sync_relation(&mut self, target: Option<(String, String)>, cx: &mut Context<Self>) {
        let Some(key) = target else {
            self.relation_schema_key = None;
            self.relation_schema = RelationSchemaFetch::Idle;
            return;
        };
        if !self.session.read(cx).is_connected() {
            self.relation_schema_key = None;
            self.relation_schema = RelationSchemaFetch::Idle;
            return;
        }
        if self.relation_schema_key.as_ref() == Some(&key) {
            return;
        }
        self.relation_schema_key = Some(key.clone());
        self.relation_schema = RelationSchemaFetch::Loading;

        let task = self.session.update(cx, |session, cx| {
            session.describe_relation(&key.0, &key.1, cx)
        });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            let _ = this.update(cx, |staging, cx| {
                if staging.relation_schema_key.as_ref() != Some(&key) {
                    // A different relation became active while this fetch
                    // was in flight; its result no longer belongs to what
                    // is currently on screen.
                    return;
                }
                match outcome {
                    Ok(relation_schema) => {
                        tracing::debug!(
                            schema = %key.0,
                            relation = %key.1,
                            "fetched relation schema for staged deletes"
                        );
                        staging.relation_schema = RelationSchemaFetch::Ready(relation_schema);
                    }
                    Err(err) => {
                        tracing::warn!(
                            schema = %key.0,
                            relation = %key.1,
                            error = %err,
                            "relation schema fetch for staged deletes failed"
                        );
                        staging.relation_schema = RelationSchemaFetch::Failed;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// One [`LedgerLine`] per queue entry, in Apply order, each statement
    /// syntax-highlighted.
    fn ledger_lines(&mut self) -> Vec<LedgerLine> {
        let failed_entry = match &self.apply_state {
            ApplyState::Failed {
                entry: Some(id), ..
            } => Some(*id),
            _ => None,
        };
        let error_message = match &self.apply_state {
            ApplyState::Failed { message, .. } => message.clone(),
            _ => String::new(),
        };
        self.queue
            .entries()
            .iter()
            .map(|entry| {
                let sql = crate::staging::statement_sql(&entry.change);
                self.highlighter.set_text(&sql);
                let spans: Vec<StyleSpan> = self.highlighter.spans_for_line(0).to_vec();
                LedgerLine {
                    id: entry.id,
                    source_row: entry.source_row,
                    sql,
                    spans,
                    error: (failed_entry == Some(entry.id)).then(|| error_message.clone()),
                }
            })
            .collect()
    }

    /// The staging bar: `None` while the queue is empty.
    fn render_bar(&self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.queue.is_empty() {
            return None;
        }
        let retrying = matches!(self.apply_state, ApplyState::Failed { .. });
        let general_error = match &self.apply_state {
            ApplyState::Failed {
                entry: None,
                message,
            } => Some(message.clone()),
            _ => None,
        };
        Some(
            StagingBar::new(self.queue.len(), self.ledger_open)
                .retrying(retrying)
                .applying(self.applying())
                .general_error(general_error)
                .on_toggle_ledger(
                    cx.listener(|staging, _event, _window, cx| staging.toggle_ledger(cx)),
                )
                .on_discard_all(cx.listener(|staging, _event, _window, cx| {
                    staging.discard_all(cx);
                }))
                .on_apply(cx.listener(|staging, _event, _window, cx| staging.apply(cx)))
                .into_any_element(),
        )
    }

    /// The expanded "review sql" ledger panel: `None` unless the queue is
    /// non-empty and the ledger is expanded.
    fn render_ledger(&mut self, cx: &Context<Self>) -> Option<AnyElement> {
        if self.queue.is_empty() || !self.ledger_open {
            return None;
        }
        Some(
            StagedLedger::new(self.ledger_lines())
                .on_unstage(cx.listener(|staging, id: &StagedChangeId, _window, cx| {
                    staging.unstage(*id, cx);
                }))
                .into_any_element(),
        )
    }
}

impl Render for StagingState {
    /// The ledger panel (while expanded) docked above the staging bar;
    /// renders nothing while the queue is empty.
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .children(self.render_ledger(cx))
            .children(self.render_bar(cx))
    }
}

impl ResultsView {
    /// Whether the active result maps to a relation whose primary key is
    /// known, so a row can be staged for delete at all.
    pub(super) fn staging_available(&self, cx: &App) -> bool {
        self.preview.is_some()
            && self
                .staging
                .read(cx)
                .relation_schema()
                .is_some_and(crate::staging::has_usable_primary_key)
    }

    /// The reason staging is unavailable, for the cell menu's disabled-item
    /// hint. `None` while staging is available.
    pub(super) fn staging_unavailable_hint(&self, cx: &App) -> Option<&'static str> {
        if self.staging_available(cx) {
            None
        } else {
            Some("needs a primary key")
        }
    }

    /// `row`'s [`RowIdentity`] against the active relation, or `None` when
    /// staging is unavailable or `row` is out of bounds. Resolved here (not
    /// on [`StagingState`]) since only this view knows the active preview
    /// and result set.
    pub(super) fn row_identity_for(&self, cx: &App, row: usize) -> Option<RowIdentity> {
        let preview = self.preview.as_ref()?;
        let staging = self.staging.read(cx);
        let relation_schema = staging.relation_schema()?;
        let result = self.effective_result(cx);
        let row_data = result.rows.get(row)?;
        crate::staging::row_identity(
            &preview.relation.schema,
            &preview.relation.relation,
            relation_schema,
            &result.columns,
            row_data,
        )
    }

    /// The id of `row`'s staged change, if it is currently staged.
    pub(super) fn staged_id_for_row(&self, cx: &App, row: usize) -> Option<StagedChangeId> {
        if self.staging.read(cx).is_empty() {
            return None;
        }
        let identity = self.row_identity_for(cx, row)?;
        self.staging.read(cx).find_staged(&identity)
    }

    /// The cell menu's `Delete row`/`Restore row` click: stage `row`'s
    /// delete if it is not already staged, or unstage it if it is. A no-op
    /// while staging is unavailable or `row` has no resolvable identity.
    pub(super) fn stage_or_restore_row(&mut self, row: usize, cx: &mut Context<Self>) {
        let Some(identity) = self.row_identity_for(cx, row) else {
            tracing::trace!("stage/restore requested for a row with no resolvable identity");
            return;
        };
        self.staging.update(cx, |staging, cx| {
            staging.stage_or_restore(row, identity, cx);
        });
    }

    /// [`ApplyStagedChanges`]'s handler: run the staged-changes Apply.
    pub(super) fn apply_staged_changes_action(
        &mut self,
        _: &ApplyStagedChanges,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.staging.update(cx, StagingState::apply);
    }

    /// Point [`StagingState`] at the active preview's relation (or at
    /// nothing, while the active tab is not a live generated preview).
    pub(super) fn sync_relation_schema(&mut self, cx: &mut Context<Self>) {
        let target = self.preview.as_ref().map(|preview| {
            (
                preview.relation.schema.clone(),
                preview.relation.relation.clone(),
            )
        });
        self.staging
            .update(cx, |staging, cx| staging.sync_relation(target, cx));
    }
}

#[cfg(test)]
impl ResultsView {
    pub(crate) fn staged_count_for_test(&self, cx: &App) -> usize {
        self.staging.read(cx).queue.len()
    }

    pub(crate) fn staged_ledger_open_for_test(&self, cx: &App) -> bool {
        self.staging.read(cx).ledger_open
    }

    pub(crate) fn staging_unavailable_hint_for_test(&self, cx: &App) -> Option<&'static str> {
        self.staging_unavailable_hint(cx)
    }

    pub(crate) fn staged_id_for_row_for_test(
        &self,
        cx: &App,
        row: usize,
    ) -> Option<StagedChangeId> {
        self.staged_id_for_row(cx, row)
    }

    pub(crate) fn stage_or_restore_row_for_test(&mut self, row: usize, cx: &mut Context<Self>) {
        self.stage_or_restore_row(row, cx);
    }

    pub(crate) fn discard_all_staged_for_test(&mut self, cx: &mut Context<Self>) {
        self.staging.update(cx, StagingState::discard_all);
    }

    pub(crate) fn apply_staged_for_test(&mut self, cx: &mut Context<Self>) {
        self.staging.update(cx, StagingState::apply);
    }

    pub(crate) fn toggle_ledger_for_test(&mut self, cx: &mut Context<Self>) {
        self.staging.update(cx, StagingState::toggle_ledger);
    }

    pub(crate) fn apply_is_retrying_for_test(&self, cx: &App) -> bool {
        matches!(self.staging.read(cx).apply_state, ApplyState::Failed { .. })
    }
}
