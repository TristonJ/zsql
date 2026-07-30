//! Pure, driver- and UI-agnostic state for one generated preview's sort and
//! paging window: which column (if any) it sorts by, which page it shows,
//! how large a page is, and the relation's total row count once known.
//! Carries no gpui, driver, or connection types, so it is reusable by any
//! future preview-shaping ticket (e.g. filtering) and testable without a
//! window or a live database.

use serde::{Deserialize, Serialize};

use crate::RowCount;
use crate::filter::{FilterConditionId, FilterOperator, FilterState};
use crate::sql::SortDirection;

/// One generated preview's current sort column, direction, page, page size,
/// filter conditions, and (once known) the relation's total row count.
///
/// The sort, page, and filters define the query window and are persisted by
/// this type's `Serialize`/`Deserialize` impls; the row counts below are
/// volatile results of the last fetch against a live connection, so they are
/// never part of the persisted representation -- a deserialized value always
/// reports both as unknown (`None`), regardless of what a hand-edited or
/// stale source document might contain for them, ready for the count path
/// that produced them originally to refetch on the next run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewQueryState {
    sort_column: Option<String>,
    sort_direction: SortDirection,
    page: u64,
    page_size: u64,
    #[serde(skip)]
    total_rows: Option<RowCount>,
    /// The relation's total row count with no filter applied, last observed
    /// while [`PreviewQueryState::filters`] was empty. Kept distinct from
    /// [`PreviewQueryState::total_rows`] (which tracks whatever is currently
    /// active, filtered or not) so a renderer can show "N filtered of
    /// TOTAL" once a filter narrows the result.
    #[serde(skip)]
    base_total_rows: Option<RowCount>,
    filters: FilterState,
}

impl PreviewQueryState {
    /// A fresh, unsorted, unfiltered preview at page 1 of `page_size` rows,
    /// with no known total row count yet. `page_size` is clamped to at
    /// least 1: a page of zero rows can never be paged through.
    #[must_use]
    pub fn new(page_size: u64) -> Self {
        Self {
            sort_column: None,
            sort_direction: SortDirection::Asc,
            page: 1,
            page_size: page_size.max(1),
            total_rows: None,
            base_total_rows: None,
            filters: FilterState::new(),
        }
    }

    /// The active sort column, if any.
    #[must_use]
    pub fn sort_column(&self) -> Option<&str> {
        self.sort_column.as_deref()
    }

    /// The active sort direction. Meaningless (but always `Asc`) while
    /// [`PreviewQueryState::sort_column`] is `None`.
    #[must_use]
    pub fn sort_direction(&self) -> SortDirection {
        self.sort_direction
    }

    /// The active `(column, direction)` sort pair, ready to hand to a
    /// windowed query builder.
    #[must_use]
    pub fn sort_pair(&self) -> Option<(&str, SortDirection)> {
        self.sort_column
            .as_deref()
            .map(|column| (column, self.sort_direction))
    }

    /// The current 1-based page number.
    #[must_use]
    pub fn page(&self) -> u64 {
        self.page
    }

    /// The current page size (row count per page).
    #[must_use]
    pub fn page_size(&self) -> u64 {
        self.page_size
    }

    /// The relation's total row count, once known, keeping the exact-vs-
    /// estimated distinction so a renderer can mark an estimate.
    #[must_use]
    pub fn total_rows(&self) -> Option<RowCount> {
        self.total_rows
    }

    /// Whether the known total is a planner estimate rather than an exact
    /// count. `false` while the total is unknown.
    #[must_use]
    pub fn total_is_estimated(&self) -> bool {
        self.total_rows.is_some_and(RowCount::is_estimated)
    }

    /// The relation's total row count with no filter applied, last observed
    /// while unfiltered -- for a renderer to show alongside the current
    /// (possibly filtered) [`PreviewQueryState::total_rows`] as "N filtered
    /// of TOTAL". `None` until an unfiltered count has ever resolved.
    #[must_use]
    pub fn base_total_rows(&self) -> Option<RowCount> {
        self.base_total_rows
    }

    /// Record the relation's total row count (or clear it back to unknown).
    /// While [`PreviewQueryState::filters`] is empty this also updates
    /// [`PreviewQueryState::base_total_rows`], since an unfiltered count is
    /// by definition the base total; a filtered count leaves the last known
    /// base total untouched. An exact total can retroactively invalidate a
    /// page reached optimistically while the count was still unknown, so the
    /// current page is clamped back to the final page rather than left
    /// stranded past it over an empty grid. An estimate never clamps:
    /// forward paging stays optimistic past a possibly-low estimate.
    pub fn set_total_rows(&mut self, total: Option<RowCount>) {
        self.total_rows = total;
        if self.filters.is_empty() {
            self.base_total_rows = total;
        }
        if let Some(RowCount::Exact(_)) = self.total_rows
            && let Some(last) = self.last_page_number()
            && self.page > last
        {
            self.page = last;
        }
    }

    /// The `OFFSET` this page implies: `(page - 1) * page_size`.
    #[must_use]
    pub fn offset(&self) -> u64 {
        (self.page - 1) * self.page_size
    }

    /// Clear both row-count fields back to unknown outright, independent of
    /// [`PreviewQueryState::filters`] and of whichever value either field
    /// currently holds. A tab restored from a snapshot -- whether read back
    /// from disk (where `#[serde(skip)]` already keeps these fields out of
    /// the file) or handed an in-memory cached snapshot directly (where no
    /// serialization ever runs) -- calls this so both paths leave identical,
    /// unresolved totals for the next run's count path to refill.
    pub fn clear_resolved_totals(&mut self) {
        self.total_rows = None;
        self.base_total_rows = None;
    }

    /// The last page number, derived from [`PreviewQueryState::total_rows`]
    /// and the current page size. `None` while the total is unknown. A
    /// relation with zero rows still has one (empty) page. When the total is
    /// an estimate this page number is itself approximate -- callers that
    /// render it should mark it (see [`PreviewQueryState::total_is_estimated`]).
    #[must_use]
    pub fn last_page_number(&self) -> Option<u64> {
        self.total_rows.map(|total| {
            let total = total.value();
            if total == 0 {
                1
            } else {
                (total - 1) / self.page_size + 1
            }
        })
    }

    /// Whether the current page is the first page.
    #[must_use]
    pub fn is_first_page(&self) -> bool {
        self.page <= 1
    }

    /// Whether the current page is the last page. Only an exact total can
    /// answer this: a planner estimate is not authoritative enough to disable
    /// paging on, since clamping `Next`/`Last` to a low estimate would strand
    /// real rows past it. So this stays `false` for an estimated or unknown
    /// total, keeping forward paging optimistic until an exact count arrives.
    #[must_use]
    pub fn is_last_page(&self) -> bool {
        match self.total_rows {
            Some(RowCount::Exact(_)) => self
                .last_page_number()
                .is_some_and(|last| self.page >= last),
            _ => false,
        }
    }

    /// Step to the next page. Returns whether the page actually moved
    /// (`false` at the last known page, a no-op).
    pub fn next_page(&mut self) -> bool {
        if self.is_last_page() {
            return false;
        }
        self.page += 1;
        true
    }

    /// Step to the previous page. Returns whether the page actually moved
    /// (`false` at page 1, a no-op).
    pub fn prev_page(&mut self) -> bool {
        if self.is_first_page() {
            return false;
        }
        self.page -= 1;
        true
    }

    /// Jump to page 1. Returns whether the page actually moved.
    pub fn first_page(&mut self) -> bool {
        if self.is_first_page() {
            return false;
        }
        self.page = 1;
        true
    }

    /// Jump to the last known page. A no-op (returns `false`) while the
    /// total row count is unknown, or already on the last page.
    pub fn last_page(&mut self) -> bool {
        let Some(last) = self.last_page_number() else {
            return false;
        };
        if self.page == last {
            return false;
        }
        self.page = last;
        true
    }

    /// Sort by `column`: a new column sorts ascending, and clicking the
    /// already-active column flips its direction. Either way the window
    /// re-anchors to page 1 (offset drops to 0), since a different order
    /// makes the previous page's row set meaningless.
    pub fn toggle_sort(&mut self, column: &str) {
        if self.sort_column.as_deref() == Some(column) {
            self.sort_direction = self.sort_direction.flipped();
        } else {
            self.sort_column = Some(column.to_owned());
            self.sort_direction = SortDirection::Asc;
        }
        self.page = 1;
    }

    /// The active filter conditions and their AND/OR connectors.
    #[must_use]
    pub fn filters(&self) -> &FilterState {
        &self.filters
    }

    /// Commit a new filter condition against `column` (whose backend type is
    /// `type_name`) and re-anchor to page 1: an added filter changes which
    /// rows exist at all, making the previous page's row set meaningless.
    /// Returns the new condition's id.
    pub fn add_filter(
        &mut self,
        column: impl Into<String>,
        type_name: impl Into<String>,
        operator: FilterOperator,
        value: impl Into<String>,
    ) -> FilterConditionId {
        let id = self
            .filters
            .add_condition(column, type_name, operator, value);
        self.page = 1;
        id
    }

    /// Remove the filter condition with `id` and, if one was actually
    /// removed, re-anchor to page 1. Returns whether a condition was
    /// removed.
    pub fn remove_filter(&mut self, id: FilterConditionId) -> bool {
        let changed = self.filters.remove_condition(id);
        if changed {
            self.page = 1;
        }
        changed
    }

    /// Replace the operator/value of the filter condition with `id` and, if
    /// a matching condition was found, re-anchor to page 1. Returns whether
    /// a condition was updated.
    pub fn update_filter(
        &mut self,
        id: FilterConditionId,
        operator: FilterOperator,
        value: impl Into<String>,
    ) -> bool {
        let changed = self.filters.update_condition(id, operator, value);
        if changed {
            self.page = 1;
        }
        changed
    }

    /// Toggle the AND/OR connector at `index` and, if it was in bounds,
    /// re-anchor to page 1. Returns whether a connector was toggled.
    pub fn toggle_filter_connector(&mut self, index: usize) -> bool {
        let changed = self.filters.toggle_connector(index);
        if changed {
            self.page = 1;
        }
        changed
    }

    /// Remove every filter condition and, if any existed, re-anchor to page
    /// 1. Returns whether anything was actually cleared.
    pub fn clear_filters(&mut self) -> bool {
        let changed = self.filters.clear();
        if changed {
            self.page = 1;
        }
        changed
    }

    /// Resize the page, re-anchoring the window to keep showing the same
    /// first row rather than resetting to page 1: the row at 0-based index
    /// `(page - 1) * old_page_size` lands on whichever new page contains
    /// that index under `new_size`. `new_size` is clamped to at least 1.
    pub fn set_page_size(&mut self, new_size: u64) {
        let new_size = new_size.max(1);
        let first_row = self.offset();
        self.page_size = new_size;
        self.page = first_row / new_size + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::PreviewQueryState;
    use crate::RowCount;
    use crate::filter::FilterOperator;
    use crate::sql::SortDirection;

    #[test]
    fn a_fresh_state_starts_unsorted_at_page_one_with_no_known_total() {
        let state = PreviewQueryState::new(200);
        assert_eq!(state.sort_column(), None);
        assert_eq!(state.page(), 1);
        assert_eq!(state.page_size(), 200);
        assert_eq!(state.total_rows(), None);
        assert_eq!(state.offset(), 0);
    }

    #[test]
    fn a_zero_page_size_is_clamped_to_one() {
        let state = PreviewQueryState::new(0);
        assert_eq!(state.page_size(), 1);
    }

    #[test]
    fn offset_math_for_page_one_is_zero() {
        let state = PreviewQueryState::new(200);
        assert_eq!(state.offset(), 0);
    }

    #[test]
    fn offset_math_for_a_later_page() {
        let mut state = PreviewQueryState::new(200);
        state.next_page();
        state.next_page();
        assert_eq!(state.page(), 3);
        assert_eq!(state.offset(), 400);
    }

    #[test]
    fn toggle_sort_on_a_new_column_sorts_ascending() {
        let mut state = PreviewQueryState::new(200);
        state.toggle_sort("total_cents");
        assert_eq!(state.sort_column(), Some("total_cents"));
        assert_eq!(state.sort_direction(), SortDirection::Asc);
        assert_eq!(state.sort_pair(), Some(("total_cents", SortDirection::Asc)));
    }

    #[test]
    fn toggle_sort_on_the_active_column_flips_direction() {
        let mut state = PreviewQueryState::new(200);
        state.toggle_sort("total_cents");
        state.toggle_sort("total_cents");
        assert_eq!(state.sort_direction(), SortDirection::Desc);
        state.toggle_sort("total_cents");
        assert_eq!(state.sort_direction(), SortDirection::Asc);
    }

    #[test]
    fn toggle_sort_on_a_different_column_switches_and_resets_to_ascending() {
        let mut state = PreviewQueryState::new(200);
        state.toggle_sort("total_cents");
        state.toggle_sort("total_cents");
        assert_eq!(state.sort_direction(), SortDirection::Desc);
        state.toggle_sort("user_id");
        assert_eq!(state.sort_column(), Some("user_id"));
        assert_eq!(state.sort_direction(), SortDirection::Asc);
    }

    #[test]
    fn toggle_sort_re_anchors_to_page_one() {
        let mut state = PreviewQueryState::new(200);
        state.next_page();
        state.next_page();
        assert_eq!(state.page(), 3);
        state.toggle_sort("total_cents");
        assert_eq!(state.page(), 1);
        assert_eq!(state.offset(), 0);
    }

    #[test]
    fn next_page_is_optimistic_while_the_total_is_unknown() {
        let mut state = PreviewQueryState::new(200);
        assert!(state.next_page());
        assert_eq!(state.page(), 2);
    }

    #[test]
    fn next_page_clamps_at_the_last_known_page() {
        let mut state = PreviewQueryState::new(200);
        state.set_total_rows(Some(RowCount::Exact(450))); // 3 pages: 200, 200, 50.
        assert!(state.next_page());
        assert_eq!(state.page(), 2);
        assert!(state.next_page());
        assert_eq!(state.page(), 3);
        assert!(!state.next_page(), "page 3 is the last page");
        assert_eq!(state.page(), 3);
    }

    #[test]
    fn prev_page_clamps_at_page_one() {
        let mut state = PreviewQueryState::new(200);
        assert!(!state.prev_page());
        assert_eq!(state.page(), 1);
        state.next_page();
        assert!(state.prev_page());
        assert_eq!(state.page(), 1);
    }

    #[test]
    fn first_page_jumps_directly_to_page_one() {
        let mut state = PreviewQueryState::new(200);
        state.next_page();
        state.next_page();
        state.next_page();
        assert!(state.first_page());
        assert_eq!(state.page(), 1);
        assert!(!state.first_page(), "already on page 1");
    }

    #[test]
    fn last_page_jumps_directly_to_the_final_page() {
        let mut state = PreviewQueryState::new(200);
        state.set_total_rows(Some(RowCount::Exact(450)));
        assert!(state.last_page());
        assert_eq!(state.page(), 3);
        assert!(!state.last_page(), "already on the last page");
    }

    #[test]
    fn last_page_is_a_no_op_while_the_total_is_unknown() {
        let mut state = PreviewQueryState::new(200);
        assert!(!state.last_page());
        assert_eq!(state.page(), 1);
    }

    #[test]
    fn last_page_number_accounts_for_a_partial_final_page() {
        let mut state = PreviewQueryState::new(200);
        state.set_total_rows(Some(RowCount::Exact(450)));
        assert_eq!(state.last_page_number(), Some(3));
    }

    #[test]
    fn last_page_number_for_an_exact_multiple_of_page_size() {
        let mut state = PreviewQueryState::new(200);
        state.set_total_rows(Some(RowCount::Exact(400)));
        assert_eq!(state.last_page_number(), Some(2));
    }

    #[test]
    fn last_page_number_for_zero_rows_is_still_one_page() {
        let mut state = PreviewQueryState::new(200);
        state.set_total_rows(Some(RowCount::Exact(0)));
        assert_eq!(state.last_page_number(), Some(1));
        assert!(state.is_last_page());
    }

    #[test]
    fn is_first_and_is_last_page_flags() {
        let mut state = PreviewQueryState::new(200);
        state.set_total_rows(Some(RowCount::Exact(450)));
        assert!(state.is_first_page());
        assert!(!state.is_last_page());
        state.last_page();
        assert!(!state.is_first_page());
        assert!(state.is_last_page());
    }

    #[test]
    fn set_page_size_re_anchors_to_the_same_first_visible_row() {
        // Page 2 at 200/page starts at row 200 (0-based). Resizing to 500
        // must land on the page that still contains row 200: page 1.
        let mut state = PreviewQueryState::new(200);
        state.next_page();
        assert_eq!(state.offset(), 200);
        state.set_page_size(500);
        assert_eq!(state.page(), 1);
        assert_eq!(state.page_size(), 500);
    }

    #[test]
    fn set_page_size_can_move_forward_when_shrinking() {
        // Page 2 at 1000/page starts at row 1000. Resizing down to 100 must
        // land on the page containing row 1000: page 11.
        let mut state = PreviewQueryState::new(1000);
        state.next_page();
        assert_eq!(state.offset(), 1000);
        state.set_page_size(100);
        assert_eq!(state.page(), 11);
        assert_eq!(state.offset(), 1000);
    }

    #[test]
    fn set_page_size_supports_every_configured_choice() {
        let mut state = PreviewQueryState::new(200);
        for size in [100_u64, 200, 500, 1000] {
            state.set_page_size(size);
            assert_eq!(state.page_size(), size);
        }
    }

    #[test]
    fn set_page_size_clamps_to_at_least_one() {
        let mut state = PreviewQueryState::new(200);
        state.set_page_size(0);
        assert_eq!(state.page_size(), 1);
    }

    #[test]
    fn set_total_rows_can_clear_back_to_unknown() {
        let mut state = PreviewQueryState::new(200);
        state.set_total_rows(Some(RowCount::Exact(10)));
        state.set_total_rows(None);
        assert_eq!(state.total_rows(), None);
        assert_eq!(state.last_page_number(), None);
    }

    #[test]
    fn an_estimated_total_never_reports_the_last_page_so_next_stays_optimistic() {
        // An estimate of 450 rows would imply 3 pages, but the real table may
        // hold more; forward paging must not be clamped to the estimate or the
        // extra rows become unreachable.
        let mut state = PreviewQueryState::new(200);
        state.set_total_rows(Some(RowCount::Estimated(450)));
        assert!(state.total_is_estimated());
        // Even standing on the estimated final page, Next is still available.
        state.set_page_size(200);
        state.next_page();
        state.next_page();
        assert_eq!(state.page(), 3);
        assert!(!state.is_last_page(), "an estimate never disables Next");
        assert!(
            state.next_page(),
            "Next steps past the estimated final page"
        );
        assert_eq!(state.page(), 4);
    }

    #[test]
    fn last_page_jumps_to_the_approximate_final_page_for_an_estimate() {
        // `Last` is still useful on an estimate: it jumps to the best-effort
        // final page, and (because the total is not exact) Next remains live.
        let mut state = PreviewQueryState::new(200);
        state.set_total_rows(Some(RowCount::Estimated(450)));
        assert_eq!(state.last_page_number(), Some(3));
        assert!(state.last_page());
        assert_eq!(state.page(), 3);
        assert!(!state.is_last_page());
    }

    #[test]
    fn set_total_rows_clamps_a_page_advanced_past_a_later_exact_total() {
        // Paged forward optimistically while the total was still unknown...
        let mut state = PreviewQueryState::new(200);
        state.next_page();
        state.next_page();
        state.next_page();
        assert_eq!(state.page(), 4);
        // ...then an exact count lands with only 2 pages (300 rows): the page
        // must clamp back to the final page, not sit past it over empty rows.
        state.set_total_rows(Some(RowCount::Exact(300)));
        assert_eq!(state.page(), 2);
        assert!(state.is_last_page());
    }

    #[test]
    fn set_total_rows_does_not_clamp_the_page_for_an_estimate() {
        let mut state = PreviewQueryState::new(200);
        state.next_page();
        state.next_page();
        state.next_page();
        assert_eq!(state.page(), 4);
        // An estimate must never strand rows by clamping the page down.
        state.set_total_rows(Some(RowCount::Estimated(300)));
        assert_eq!(state.page(), 4);
    }

    // -- filters --------------------------------------------------------

    #[test]
    fn a_fresh_state_has_no_filters() {
        let state = PreviewQueryState::new(200);
        assert!(state.filters().is_empty());
    }

    #[test]
    fn add_filter_re_anchors_to_page_one() {
        let mut state = PreviewQueryState::new(200);
        state.next_page();
        state.next_page();
        assert_eq!(state.page(), 3);
        state.add_filter("status", "text", FilterOperator::Eq, "paid");
        assert_eq!(state.page(), 1);
        assert_eq!(state.filters().len(), 1);
    }

    #[test]
    fn remove_filter_re_anchors_to_page_one() {
        let mut state = PreviewQueryState::new(200);
        let id = state.add_filter("status", "text", FilterOperator::Eq, "paid");
        state.next_page();
        assert_eq!(state.page(), 2);
        assert!(state.remove_filter(id));
        assert_eq!(state.page(), 1);
        assert!(state.filters().is_empty());
    }

    #[test]
    fn remove_filter_is_a_no_op_for_an_unknown_id_and_does_not_move_the_page() {
        let mut state = PreviewQueryState::new(200);
        state.add_filter("status", "text", FilterOperator::Eq, "paid");
        state.next_page();
        assert_eq!(state.page(), 2);
        assert!(!state.remove_filter(9999));
        assert_eq!(state.page(), 2, "an unknown id must not re-anchor the page");
    }

    #[test]
    fn update_filter_re_anchors_to_page_one() {
        let mut state = PreviewQueryState::new(200);
        let id = state.add_filter("total_cents", "int4", FilterOperator::Eq, "100");
        state.next_page();
        assert_eq!(state.page(), 2);
        assert!(state.update_filter(id, FilterOperator::Ge, "500"));
        assert_eq!(state.page(), 1);
        assert_eq!(state.filters().conditions()[0].value(), "500");
    }

    #[test]
    fn toggle_filter_connector_re_anchors_to_page_one() {
        let mut state = PreviewQueryState::new(200);
        state.add_filter("status", "text", FilterOperator::Eq, "paid");
        state.add_filter("status", "text", FilterOperator::Eq, "pending");
        state.next_page();
        assert_eq!(state.page(), 2);
        assert!(state.toggle_filter_connector(0));
        assert_eq!(state.page(), 1);
    }

    #[test]
    fn clear_filters_re_anchors_to_page_one() {
        let mut state = PreviewQueryState::new(200);
        state.add_filter("status", "text", FilterOperator::Eq, "paid");
        state.next_page();
        assert_eq!(state.page(), 2);
        assert!(state.clear_filters());
        assert_eq!(state.page(), 1);
        assert!(state.filters().is_empty());
    }

    #[test]
    fn clear_filters_on_an_already_empty_state_does_not_move_the_page() {
        let mut state = PreviewQueryState::new(200);
        state.next_page();
        assert_eq!(state.page(), 2);
        assert!(!state.clear_filters());
        assert_eq!(state.page(), 2);
    }

    #[test]
    fn base_total_rows_tracks_the_total_while_unfiltered() {
        let mut state = PreviewQueryState::new(200);
        assert_eq!(state.base_total_rows(), None);
        state.set_total_rows(Some(RowCount::Exact(12_480)));
        assert_eq!(state.base_total_rows(), Some(RowCount::Exact(12_480)));
    }

    #[test]
    fn base_total_rows_is_untouched_by_a_filtered_count() {
        let mut state = PreviewQueryState::new(200);
        state.set_total_rows(Some(RowCount::Exact(12_480)));
        state.add_filter("status", "text", FilterOperator::Eq, "paid");
        state.set_total_rows(Some(RowCount::Exact(3_102)));
        assert_eq!(state.total_rows(), Some(RowCount::Exact(3_102)));
        assert_eq!(
            state.base_total_rows(),
            Some(RowCount::Exact(12_480)),
            "the base total must survive a filtered recount"
        );
    }

    #[test]
    fn base_total_rows_updates_again_once_filters_are_cleared() {
        let mut state = PreviewQueryState::new(200);
        state.set_total_rows(Some(RowCount::Exact(12_480)));
        state.add_filter("status", "text", FilterOperator::Eq, "paid");
        state.set_total_rows(Some(RowCount::Exact(3_102)));
        state.clear_filters();
        state.set_total_rows(Some(RowCount::Exact(12_480)));
        assert_eq!(state.base_total_rows(), Some(RowCount::Exact(12_480)));
    }

    #[test]
    fn total_is_estimated_reflects_the_row_count_variant() {
        let mut state = PreviewQueryState::new(200);
        assert!(!state.total_is_estimated());
        state.set_total_rows(Some(RowCount::Exact(10)));
        assert!(!state.total_is_estimated());
        state.set_total_rows(Some(RowCount::Estimated(10)));
        assert!(state.total_is_estimated());
    }

    // -- serde round-trips ----------------------------------------------------

    #[test]
    fn sort_page_and_filters_round_trip_through_json() {
        let mut state = PreviewQueryState::new(500);
        state.toggle_sort("total_cents");
        state.add_filter("status", "text", FilterOperator::Eq, "paid");
        state.next_page();
        state.next_page();

        let text = serde_json::to_string(&state).expect("must serialize");
        let parsed: PreviewQueryState = serde_json::from_str(&text).expect("must parse back");

        assert_eq!(parsed, state);
    }

    #[test]
    fn total_rows_and_base_total_rows_never_survive_a_round_trip() {
        let mut state = PreviewQueryState::new(200);
        state.set_total_rows(Some(RowCount::Exact(12_480)));
        assert_eq!(state.total_rows(), Some(RowCount::Exact(12_480)));

        let text = serde_json::to_string(&state).expect("must serialize");
        assert!(
            !text.contains("12480") && !text.contains("total_rows"),
            "row counts must never appear in the persisted representation: {text}"
        );

        let parsed: PreviewQueryState = serde_json::from_str(&text).expect("must parse back");
        assert_eq!(parsed.total_rows(), None);
        assert_eq!(parsed.base_total_rows(), None);
    }

    #[test]
    fn a_hand_edited_document_carrying_total_rows_is_still_ignored_on_load() {
        // Even if a source document (e.g. hand-edited, or from a future
        // format) carries a total_rows-shaped field, it must never resurface
        // on the parsed value: the count path that resolved it originally is
        // the only source of truth, refetched on the next run.
        let text = r#"{
            "sort_column": null,
            "sort_direction": "Asc",
            "page": 1,
            "page_size": 200,
            "total_rows": {"Exact": 999},
            "base_total_rows": {"Exact": 999},
            "filters": {"conditions": [], "connectors": [], "next_id": 0}
        }"#;

        let parsed: PreviewQueryState = serde_json::from_str(text).expect("must parse back");

        assert_eq!(parsed.total_rows(), None);
        assert_eq!(parsed.base_total_rows(), None);
    }
}
