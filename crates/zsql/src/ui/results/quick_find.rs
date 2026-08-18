//! The results grid's quick-find state: `zsql_ui::quick_find::QuickFind`'s
//! pure match/navigation core joined to a `zsql_ui::quick_find_bar` overlay
//! and a cache of the loaded rows' formatted cell text. [`ResultsQuickFind`]
//! owns everything the open bar needs; the hosting view holds one as a
//! field and reacts to the bar's events.

use gpui::{App, Context, Entity, Subscription, prelude::*};
use zsql_ui::quick_find::QuickFind;
use zsql_ui::quick_find_bar::QuickFindBar;

use super::ResultsView;
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
