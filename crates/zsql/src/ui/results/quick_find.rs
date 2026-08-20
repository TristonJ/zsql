//! The results grid's quick-find state: `zsql_ui::quick_find::QuickFind`'s
//! pure match/navigation core joined to a `zsql_ui::quick_find_bar` overlay
//! and a cache of the loaded rows' formatted cell text. [`ResultsQuickFind`]
//! owns everything the open bar needs; the hosting view holds one as a
//! field and reacts to the bar's events.

use gpui::{AnyElement, App, Context, Entity, Subscription, Window, div, prelude::*};
use zsql_ui::quick_find::QuickFind;
use zsql_ui::quick_find_bar::{QuickFindBar, QuickFindBarEvent};

use super::{
    OpenQuickFind, QuickFindClose, QuickFindNext, QuickFindPrev, ResultsView, ViewMode, theme,
};
use crate::ui::format::format_value;

/// The key context quick-find key bindings are scoped to, active whenever
/// the bar (or its query input) holds window focus.
pub const KEY_CONTEXT: &str = "QuickFind";

/// A results grid cell's quick-find highlight, as [`super::grid`] paints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QuickFindHighlight {
    /// Not a match.
    None,
    /// A match, but not the current one.
    Match,
    /// The current match.
    Current,
}

/// Every loaded row's cells, formatted exactly as the grid displays them.
/// [`FormattedRows::sync`] extends this incrementally rather than
/// reformatting every cell on every keystroke: a full loaded result can run
/// into the millions of rows, so only the rows that streamed in since the
/// last sync are ever formatted.
#[derive(Debug, Clone, Default)]
struct FormattedRows {
    rows: Vec<Vec<String>>,
}

impl FormattedRows {
    /// Bring the cache up to date with `result`. Rows already cached are
    /// left untouched; only rows beyond the cached count are formatted and
    /// appended. If `result` has fewer rows than the cache, meaning the
    /// loaded result was replaced by a smaller one, the cache is rebuilt
    /// from scratch instead.
    fn sync(&mut self, result: &zsql_core::ResultSet) -> &[Vec<String>] {
        if result.rows.len() < self.rows.len() {
            self.rows.clear();
        }
        self.rows.extend(
            result.rows[self.rows.len()..]
                .iter()
                .map(|row| row.0.iter().map(|value| format_value(value).text).collect()),
        );
        &self.rows
    }
}

/// The open quick-find session: the pure match/navigation core, the bar
/// overlay entity, and the formatted-row cache the core matches against.
/// Dropping it closes the session and its event subscription with it.
pub(super) struct ResultsQuickFind {
    core: QuickFind,
    bar: Entity<QuickFindBar>,
    formatted_rows: FormattedRows,
    _bar_events: Subscription,
}

impl ResultsQuickFind {
    /// Open a fresh session: an empty core plus a new bar whose events are
    /// routed to the hosting view.
    pub(super) fn open(window: &gpui::Window, cx: &mut Context<ResultsView>) -> Self {
        let bar =
            cx.new(|cx| QuickFindBar::new("results-quick-find-bar", "Find in results...", cx));
        let bar_events = cx.subscribe_in(&bar, window, ResultsView::handle_quick_find_event);
        Self {
            core: QuickFind::new(),
            bar,
            formatted_rows: FormattedRows::default(),
            _bar_events: bar_events,
        }
    }

    /// The bar overlay entity, for rendering and input focus.
    pub(super) fn bar(&self) -> &Entity<QuickFindBar> {
        &self.bar
    }

    /// This cell's highlight.
    pub(super) fn highlight(&self, row: usize, col: usize) -> QuickFindHighlight {
        if self.core.is_current(row, col) {
            QuickFindHighlight::Current
        } else if self.core.is_match(row, col) {
            QuickFindHighlight::Match
        } else {
            QuickFindHighlight::None
        }
    }

    /// Recompute matches for `query` against `result`'s loaded rows,
    /// returning the new current match to land on, if any.
    pub(super) fn set_query(
        &mut self,
        query: String,
        result: &zsql_core::ResultSet,
    ) -> Option<(usize, usize)> {
        let rows = self.formatted_rows.sync(result);
        self.core.set_query(query, rows);
        self.core.current_match()
    }

    /// Step the current match forward (`forward`) or backward, wrapping at
    /// either end, and return the match landed on, if any.
    pub(super) fn step(&mut self, forward: bool) -> Option<(usize, usize)> {
        if forward {
            self.core.next_match()
        } else {
            self.core.prev_match()
        }
    }

    /// Flip case-sensitive matching, recompute the current query against
    /// `result`, and return the new current match, if any.
    pub(super) fn toggle_case(&mut self, result: &zsql_core::ResultSet) -> Option<(usize, usize)> {
        let case_sensitive = !self.core.case_sensitive();
        let rows = self.formatted_rows.sync(result);
        self.core.set_case_sensitive(case_sensitive, rows);
        self.core.current_match()
    }

    /// Recompute matches against `result`'s loaded rows, keeping the
    /// current match on the same cell where it is still one. Call after a
    /// page swap or more rows streaming in.
    pub(super) fn sync(&mut self, result: &zsql_core::ResultSet) {
        let rows = self.formatted_rows.sync(result);
        self.core.sync(rows);
    }

    /// Push the core's current position and case mode into the bar's
    /// display. Call after any mutation the bar should reflect.
    pub(super) fn push_status(&self, cx: &mut App) {
        let current = self.core.current_number().unwrap_or(0);
        let total = self.core.match_count();
        let case_on = self.core.case_sensitive();
        self.bar.update(cx, |bar, cx| {
            bar.set_status(current, total, case_on, cx);
        });
    }

    /// The total match count.
    #[cfg(test)]
    pub(super) fn match_count(&self) -> usize {
        self.core.match_count()
    }

    /// The current match's 1-based position, `None` with no matches.
    #[cfg(test)]
    pub(super) fn current_number(&self) -> Option<usize> {
        self.core.current_number()
    }

    /// Whether case-sensitive matching is armed.
    #[cfg(test)]
    pub(super) fn case_sensitive(&self) -> bool {
        self.core.case_sensitive()
    }
}

// ---- the hosting view's own quick-find wiring -----------------------------

impl ResultsView {
    /// This cell's quick-find highlight, `None` while the bar is closed or
    /// the cell has no match.
    pub(super) fn quick_find_highlight(&self, row: usize, col: usize) -> QuickFindHighlight {
        self.quick_find
            .as_ref()
            .map_or(QuickFindHighlight::None, |state| state.highlight(row, col))
    }

    /// [`OpenQuickFind`]'s handler: open the bar over the results grid, or
    /// refocus its input if already open. Switches to the Grid view first
    /// if the Text document view is active, so the highlights the bar
    /// drives are actually visible.
    #[tracing::instrument(name = "results_open_quick_find", skip_all)]
    pub(super) fn open_quick_find(
        &mut self,
        _: &OpenQuickFind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = &self.quick_find {
            state.bar().read(cx).focus_input(window, cx);
            return;
        }
        self.set_view_mode(ViewMode::Grid, cx);
        let state = ResultsQuickFind::open(window, cx);
        state.bar().read(cx).focus_input(window, cx);
        self.quick_find = Some(state);
        tracing::debug!("opened the results quick-find bar");
        cx.notify();
    }

    /// [`QuickFindClose`]'s handler: closes the bar and clears every
    /// highlight, leaving the grid's own focused cell (the last current
    /// match, if any) untouched, so quick-find doubles as jump-to.
    pub(super) fn quick_find_close(
        &mut self,
        _: &QuickFindClose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quick_find.take().is_none() {
            return;
        }
        window.focus(&self.focus_handle);
        tracing::debug!("closed the results quick-find bar");
        cx.notify();
    }

    pub(super) fn quick_find_next(
        &mut self,
        _: &QuickFindNext,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.quick_find_step(true, cx);
    }

    pub(super) fn quick_find_prev(
        &mut self,
        _: &QuickFindPrev,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.quick_find_step(false, cx);
    }

    /// The single reaction point for everything the bar asks for, whether
    /// requested by one of its buttons or by typing in its input.
    fn handle_quick_find_event(
        &mut self,
        _bar: &Entity<QuickFindBar>,
        event: &QuickFindBarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            QuickFindBarEvent::QueryChanged(query) => self.set_quick_find_query(query.clone(), cx),
            QuickFindBarEvent::StepRequested { forward } => self.quick_find_step(*forward, cx),
            QuickFindBarEvent::CaseToggleRequested => self.toggle_quick_find_case(cx),
            QuickFindBarEvent::DismissRequested => {
                self.quick_find_close(&QuickFindClose, window, cx);
            }
        }
    }

    /// Step the current match forward (`forward`) or backward, wrapping at
    /// either end, and move the grid's focus/scroll to it.
    fn quick_find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let landed = {
            let Some(state) = &mut self.quick_find else {
                return;
            };
            let landed = state.step(forward);
            state.push_status(cx);
            landed
        };
        if let Some((row, col)) = landed {
            self.focus_quick_find_match(row, col, cx);
        }
        cx.notify();
    }

    /// Recompute matches for `query` against the currently loaded rows and
    /// jump to the new current match, if any.
    fn set_quick_find_query(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(mut state) = self.quick_find.take() else {
            return;
        };
        let landed = state.set_query(query, self.effective_result(cx));
        state.push_status(cx);
        self.quick_find = Some(state);
        if let Some((row, col)) = landed {
            self.focus_quick_find_match(row, col, cx);
        }
        cx.notify();
    }

    /// Toggle case-sensitive matching and recompute the current query
    /// against it.
    pub(super) fn toggle_quick_find_case(&mut self, cx: &mut Context<Self>) {
        let Some(mut state) = self.quick_find.take() else {
            return;
        };
        let landed = state.toggle_case(self.effective_result(cx));
        state.push_status(cx);
        self.quick_find = Some(state);
        if let Some((row, col)) = landed {
            self.focus_quick_find_match(row, col, cx);
        }
        cx.notify();
    }

    /// Recompute the open bar's matches against the currently loaded rows,
    /// keeping the current match on the same cell where it is still one.
    /// Call this after a page swap or more rows streaming in. A no-op while
    /// the bar is closed.
    pub(super) fn sync_quick_find_matches(&mut self, cx: &mut Context<Self>) {
        let Some(mut state) = self.quick_find.take() else {
            return;
        };
        state.sync(self.effective_result(cx));
        state.push_status(cx);
        self.quick_find = Some(state);
    }

    /// Move the grid's focused cell to `(row, col)` and scroll its row into
    /// view, so keyboard cell navigation continues from a quick-find match.
    fn focus_quick_find_match(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        self.table_state.update(cx, |state, cx| {
            state.set_focused_cell(row, col);
            cx.notify();
        });
        self.table_state.read(cx).scroll_row_into_view(row);
    }

    /// The floating quick-find overlay over the grid's top-right, wrapping
    /// the bar with this view's key context and action handlers. `None`
    /// while the bar is closed.
    pub(super) fn render_quick_find_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = self.quick_find.as_ref()?;
        Some(
            div()
                .key_context(KEY_CONTEXT)
                .absolute()
                .top(theme::QUICK_FIND_BAR_TOP_OFFSET)
                .right(theme::QUICK_FIND_BAR_RIGHT_OFFSET)
                .on_action(cx.listener(Self::quick_find_next))
                .on_action(cx.listener(Self::quick_find_prev))
                .on_action(cx.listener(Self::quick_find_close))
                .child(state.bar().clone())
                .into_any_element(),
        )
    }
}
