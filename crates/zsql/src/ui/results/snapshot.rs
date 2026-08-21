//! Switching what a [`ResultsView`] currently renders: following `session`
//! live, freezing to a captured snapshot, or following live under a
//! same-relation window change (a page/sort/filter navigation) that
//! preserves the staged-changes queue rather than clearing it.

use gpui::{App, Context, SharedString};
use zsql_core::ResultSet;

use super::cell_edit::CellEditor;
use super::staging::StagingState;
use super::{ResultsSnapshot, ResultsView, ViewMode};
use crate::session::SessionState;

impl ResultsView {
    /// Follow `session`'s state/result live under `source_label`, e.g. for
    /// the tab that `session` is currently running a query for. Clears the
    /// staged-changes queue.
    pub fn show_live(&mut self, source_label: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.source_label = source_label.into();
        self.frozen = None;
        self.reset_for_new_result(cx);
        self.staging.update(cx, StagingState::discard_all);
        self.sync_dimensions(cx);
        cx.notify();
    }

    /// Like [`ResultsView::show_live`], for a same-relation window change
    /// (a page, sort, or filter navigation): the staged-changes queue
    /// survives.
    pub fn show_live_window(
        &mut self,
        source_label: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.source_label = source_label.into();
        self.frozen = None;
        self.reset_for_new_result(cx);
        self.sync_dimensions(cx);
        cx.notify();
    }

    /// Show the "Waiting for parameters..." placeholder, naming `count`
    /// detected parameters, in place of the grid or any stale prior result.
    /// Shown while the "Run with parameters" modal is open for the active
    /// tab's run.
    pub fn show_waiting_for_params(&mut self, count: usize, cx: &mut Context<Self>) {
        self.waiting_for_params = Some(count);
        cx.notify();
    }

    /// Clear the waiting-for-parameters placeholder, if shown, restoring
    /// whatever this view already held underneath (its live or frozen
    /// state is untouched by showing/clearing the placeholder).
    pub fn clear_waiting_for_params(&mut self, cx: &mut Context<Self>) {
        if self.waiting_for_params.take().is_some() {
            cx.notify();
        }
    }

    /// Freeze the grid to `snapshot` instead of following `session` live,
    /// e.g. when switching to a tab that is not the one `session` is
    /// currently running a query for. Clears the staged-changes queue.
    pub fn show_snapshot(&mut self, snapshot: ResultsSnapshot, cx: &mut Context<Self>) {
        self.source_label = snapshot.source_label.clone();
        self.frozen = Some(snapshot);
        self.reset_for_new_result(cx);
        self.staging.update(cx, StagingState::discard_all);
        self.sync_dimensions(cx);
        cx.notify();
    }

    /// Clear every piece of state derived from the previous result.
    fn reset_for_new_result(&mut self, cx: &mut Context<Self>) {
        self.column_widths = Vec::new();
        self.column_width_overrides = Vec::new();
        self.column_max_body_chars = Vec::new();
        self.folded_row_count = 0;
        self.view_mode = ViewMode::Grid;
        self.view_mode_defaulted = false;
        self.filter_editor = None;
        self.filter_column_picker_open = false;
        self.cell_editor.update(cx, CellEditor::close);
        self.quick_find = None;
        self.text_view.update(cx, |tv, _c| tv.reset());
    }

    /// The result set this view currently renders: `session`'s live result
    /// while [`ResultsView::frozen`] is `None`, else the frozen snapshot's.
    pub(super) fn effective_result<'a>(&'a self, cx: &'a App) -> &'a ResultSet {
        match &self.frozen {
            Some(snapshot) => &snapshot.result,
            None => self.session.read(cx).result(),
        }
    }

    /// The lifecycle state this view currently renders: `session`'s live
    /// state while [`ResultsView::frozen`] is `None`, else the frozen
    /// snapshot's.
    pub(super) fn effective_state<'a>(&'a self, cx: &'a App) -> &'a SessionState {
        match &self.frozen {
            Some(snapshot) => &snapshot.state,
            None => self.session.read(cx).state(),
        }
    }
}

/// The waiting-for-parameters placeholder's detail line, naming `count`
/// with correct singular/plural.
pub(super) fn waiting_for_params_detail(count: usize) -> String {
    format!(
        "{count} parameter{} found. Fill them in to run this query.",
        if count == 1 { "" } else { "s" }
    )
}
