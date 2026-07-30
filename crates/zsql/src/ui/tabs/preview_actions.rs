//! Routes a sort/pager/filter click ([`PreviewAction`]) to the active tab's
//! [`PreviewQueryState`] and, when that actually changes the window, reruns
//! the tab via the exact same windowed SQL builder that rewrites its
//! buffer -- so the executed query and the displayed buffer text can never
//! diverge. A no-op whenever the active tab is not a live, unedited
//! generated preview.

use gpui::Context;
use zsql_core::preview_state::PreviewQueryState;

use super::{PreviewControlsChanged, TabKind, TabModel};
use crate::ui::results::pager::{PreviewAction, PreviewControls};

impl TabModel {
    /// Route `action` to whichever tab is currently active, per
    /// [`PreviewAction`]'s own variants. The single body every control
    /// [`TabModel::preview_dispatch`] reaches ultimately calls into.
    pub(super) fn dispatch_preview_action(
        &mut self,
        action: PreviewAction,
        cx: &mut Context<Self>,
    ) {
        match action {
            PreviewAction::Sort(column) => self.sort_active_tab_by(&column, cx),
            PreviewAction::FirstPage => self.first_page_active_tab(cx),
            PreviewAction::PrevPage => self.prev_page_active_tab(cx),
            PreviewAction::NextPage => self.next_page_active_tab(cx),
            PreviewAction::LastPage => self.last_page_active_tab(cx),
            PreviewAction::CyclePageSize => self.cycle_page_size_active_tab(cx),
            PreviewAction::AddFilter {
                column,
                type_name,
                operator,
                value,
            } => self.add_filter_active_tab(column, type_name, operator, value, cx),
            PreviewAction::RemoveFilter(id) => self.remove_filter_active_tab(id, cx),
            PreviewAction::UpdateFilter {
                id,
                operator,
                value,
            } => self.update_filter_active_tab(id, operator, value, cx),
            PreviewAction::ToggleFilterConnector(index) => {
                self.toggle_filter_connector_active_tab(index, cx);
            }
            PreviewAction::ClearFilters => self.clear_filters_active_tab(cx),
        }
    }

    /// Toggle the active tab's sort by `column` (see
    /// [`PreviewQueryState::toggle_sort`]) and re-run it. A no-op while the
    /// active tab is not a live, unedited generated preview.
    fn sort_active_tab_by(&mut self, column: &str, cx: &mut Context<Self>) {
        let column = column.to_owned();
        self.rerun_active_generated_tab(cx, false, move |state| {
            state.toggle_sort(&column);
            true
        });
    }

    /// Step the active tab's pager back one page. A no-op at page 1, or
    /// while the active tab is not a live, unedited generated preview.
    fn prev_page_active_tab(&mut self, cx: &mut Context<Self>) {
        self.rerun_active_generated_tab(cx, false, PreviewQueryState::prev_page);
    }

    /// Step the active tab's pager forward one page. A no-op at the last
    /// known page, or while the active tab is not a live, unedited
    /// generated preview.
    fn next_page_active_tab(&mut self, cx: &mut Context<Self>) {
        self.rerun_active_generated_tab(cx, false, PreviewQueryState::next_page);
    }

    /// Jump the active tab's pager to page 1. A no-op already on page 1, or
    /// while the active tab is not a live, unedited generated preview.
    fn first_page_active_tab(&mut self, cx: &mut Context<Self>) {
        self.rerun_active_generated_tab(cx, false, PreviewQueryState::first_page);
    }

    /// Jump the active tab's pager to the last known page. A no-op while
    /// the total row count is unknown, already on the last page, or the
    /// active tab is not a live, unedited generated preview.
    fn last_page_active_tab(&mut self, cx: &mut Context<Self>) {
        self.rerun_active_generated_tab(cx, false, PreviewQueryState::last_page);
    }

    /// Advance the active tab's page size to the next configured choice
    /// (from [`crate::session::Session::preview_page_sizes`]), wrapping
    /// around, and re-anchor per [`PreviewQueryState::set_page_size`]. A
    /// no-op while the active tab is not a live, unedited generated
    /// preview.
    fn cycle_page_size_active_tab(&mut self, cx: &mut Context<Self>) {
        let page_sizes = self.session.read(cx).preview_page_sizes().to_vec();
        if page_sizes.is_empty() {
            return;
        }
        self.rerun_active_generated_tab(cx, false, move |state| {
            let current = page_sizes
                .iter()
                .position(|&size| size == state.page_size());
            let next = current.map_or(0, |index| (index + 1) % page_sizes.len());
            state.set_page_size(page_sizes[next]);
            true
        });
    }

    /// Commit a new filter condition on the active tab (see
    /// [`PreviewQueryState::add_filter`]) and re-run it, refetching the
    /// filtered total. A no-op while the active tab is not a live, unedited
    /// generated preview.
    fn add_filter_active_tab(
        &mut self,
        column: String,
        type_name: String,
        operator: zsql_core::FilterOperator,
        value: String,
        cx: &mut Context<Self>,
    ) {
        self.rerun_active_generated_tab(cx, true, move |state| {
            state.add_filter(column, type_name, operator, value);
            true
        });
    }

    /// Remove the active tab's filter condition with `id` (see
    /// [`PreviewQueryState::remove_filter`]) and re-run it, refetching the
    /// filtered total. A no-op if `id` is not one of the active tab's
    /// filters, or the active tab is not a live, unedited generated preview.
    fn remove_filter_active_tab(
        &mut self,
        id: zsql_core::FilterConditionId,
        cx: &mut Context<Self>,
    ) {
        self.rerun_active_generated_tab(cx, true, move |state| state.remove_filter(id));
    }

    /// Replace the operator/value of the active tab's filter condition with
    /// `id` (see [`PreviewQueryState::update_filter`]) and re-run it,
    /// refetching the filtered total. A no-op if `id` is not one of the
    /// active tab's filters, or the active tab is not a live, unedited
    /// generated preview.
    fn update_filter_active_tab(
        &mut self,
        id: zsql_core::FilterConditionId,
        operator: zsql_core::FilterOperator,
        value: String,
        cx: &mut Context<Self>,
    ) {
        self.rerun_active_generated_tab(cx, true, move |state| {
            state.update_filter(id, operator, value)
        });
    }

    /// Toggle the active tab's AND/OR connector at `index` (see
    /// [`PreviewQueryState::toggle_filter_connector`]) and re-run it,
    /// refetching the filtered total. A no-op if `index` is out of bounds,
    /// or the active tab is not a live, unedited generated preview.
    fn toggle_filter_connector_active_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        self.rerun_active_generated_tab(cx, true, move |state| {
            state.toggle_filter_connector(index)
        });
    }

    /// Remove every filter condition from the active tab (see
    /// [`PreviewQueryState::clear_filters`]) and re-run it, refetching the
    /// unfiltered total. A no-op if the active tab has no filters, or is not
    /// a live, unedited generated preview.
    fn clear_filters_active_tab(&mut self, cx: &mut Context<Self>) {
        self.rerun_active_generated_tab(cx, true, PreviewQueryState::clear_filters);
    }

    /// Apply `mutate` to the active tab's [`PreviewQueryState`] (only while
    /// it is a live, unedited generated preview), and, only when `mutate`
    /// reports it actually changed the window (e.g. `prev_page` at page 1
    /// returns `false`), re-run the tab's query via the exact same windowed
    /// builder call used to rewrite its buffer -- so buffer text and
    /// executed SQL can never diverge, and a pager control already at its
    /// boundary is a true no-op rather than an redundant identical rerun.
    /// `refetch_count` is forwarded to
    /// [`crate::session::Session::preview_relation_windowed`]: `true` for a
    /// filter change, which alters which rows exist at all, `false` for a
    /// sort/page change, which does not.
    fn rerun_active_generated_tab(
        &mut self,
        cx: &mut Context<Self>,
        refetch_count: bool,
        mutate: impl FnOnce(&mut PreviewQueryState) -> bool,
    ) {
        let Some(id) = self.active else {
            return;
        };
        let changed = self
            .tabs
            .iter_mut()
            .find(|tab| tab.id == id)
            .is_some_and(|tab| {
                if tab.dirty || !matches!(tab.kind, TabKind::Generated { .. }) {
                    return false;
                }
                mutate(&mut tab.preview_state)
            });
        if !changed {
            return;
        }

        let Some(tab) = self.tab(id) else {
            return;
        };
        let TabKind::Generated { schema, relation } = tab.kind.clone() else {
            return;
        };
        let sort = tab
            .preview_state
            .sort_pair()
            .map(|(column, direction)| (column.to_owned(), direction));
        let limit = tab.preview_state.page_size();
        let offset = tab.preview_state.offset();
        let filters = tab.preview_state.filters().clone();
        let editor = tab.editor.clone();

        let _span = tracing::info_span!(
            "tab_preview_requery",
            tab_id = id,
            schema = %schema,
            relation = %relation,
            limit,
            offset,
            filtered = !filters.is_empty()
        )
        .entered();

        let sort_ref = sort
            .as_ref()
            .map(|(column, direction)| (column.as_str(), *direction));
        let sql = self
            .session
            .read(cx)
            .preview_sql_windowed(&schema, &relation, sort_ref, limit, offset, &filters);
        editor.update(cx, |editor, cx| editor.set_text(&sql, cx));

        tracing::info!("rewriting generated preview query for a sort/page/filter change");
        let task = self.session.update(cx, |session, cx| {
            session.preview_relation_windowed(
                &schema,
                &relation,
                sort_ref,
                limit,
                offset,
                &filters,
                refetch_count,
                cx,
            )
        });
        self.dispatch_run(id, format!("{schema}.{relation}"), task, cx);
        self.sync_preview_controls(cx);
    }

    /// Rebuild `results`'s [`PreviewControls`] from the active tab: `Some`
    /// while it is a live, unedited generated preview, else `None`, which
    /// is what renders the grid's sort headers and the results bar's pager
    /// inert without hiding the grid itself.
    pub(super) fn sync_preview_controls(&self, cx: &mut Context<Self>) {
        let controls = self.active.and_then(|id| self.tab(id)).and_then(|tab| {
            let TabKind::Generated { .. } = tab.kind else {
                return None;
            };
            if tab.dirty {
                return None;
            }
            Some(PreviewControls {
                state: tab.preview_state.clone(),
                dispatch: self.preview_dispatch.clone(),
            })
        });
        cx.emit(PreviewControlsChanged(controls));
    }
}
