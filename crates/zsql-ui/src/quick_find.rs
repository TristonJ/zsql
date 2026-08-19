//! Substring match/navigation core for a quick-find overlay: given rows of
//! already-formatted cell text, tracks which cells match a query and which
//! one is current, with wrapping next/previous navigation. Knows nothing
//! about `gpui`, sessions, or any specific view. A caller renders the
//! highlights and wires up its own text input and buttons on top of this.

/// A `(row, column)` position into the caller's row data, identifying one
/// matching cell.
pub type MatchPosition = (usize, usize);

/// Live match state for a quick-find query over a caller-supplied grid of
/// cell text: the query string, whether matching is case-sensitive, every
/// matching cell in row-then-column order, and which one is current.
#[derive(Debug, Clone, Default)]
pub struct QuickFind {
    query: String,
    case_sensitive: bool,
    matches: Vec<MatchPosition>,
    current: Option<usize>,
}

impl QuickFind {
    /// A fresh quick-find: an empty query, case-insensitive matching, and no
    /// matches.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current query text.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Whether matching is currently case-sensitive.
    #[must_use]
    pub fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    /// Every matching cell, in row-then-column order.
    #[must_use]
    pub fn matches(&self) -> &[MatchPosition] {
        &self.matches
    }

    /// How many cells currently match.
    #[must_use]
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// The current match's 1-based position among [`QuickFind::matches`], or
    /// `None` with no matches.
    #[must_use]
    pub fn current_number(&self) -> Option<usize> {
        self.current.map(|index| index + 1)
    }

    /// The current match's position, or `None` with no matches.
    #[must_use]
    pub fn current_match(&self) -> Option<MatchPosition> {
        self.current
            .and_then(|index| self.matches.get(index).copied())
    }

    /// Whether `(row, col)` is any match, current or not. `O(log n)`:
    /// [`QuickFind::matches`] is always kept in row-then-column order.
    #[must_use]
    pub fn is_match(&self, row: usize, col: usize) -> bool {
        self.matches.binary_search(&(row, col)).is_ok()
    }

    /// Whether `(row, col)` is the current match.
    #[must_use]
    pub fn is_current(&self, row: usize, col: usize) -> bool {
        self.current_match() == Some((row, col))
    }

    /// Set the query text and recompute matches against `rows`, landing on
    /// the first match (if any) as current.
    pub fn set_query(&mut self, query: impl Into<String>, rows: &[Vec<String>]) {
        self.query = query.into();
        self.recompute(rows, None);
    }

    /// Set case-sensitivity and recompute matches against `rows` for the
    /// current query, landing on the first match (if any) as current.
    pub fn set_case_sensitive(&mut self, case_sensitive: bool, rows: &[Vec<String>]) {
        self.case_sensitive = case_sensitive;
        self.recompute(rows, None);
    }

    /// Recompute matches against `rows` for the current query and
    /// case-sensitivity, without changing what the user was searching for:
    /// use this after the loaded rows themselves changed (a page swap, more
    /// rows streaming in). The current match stays on the same cell if it is
    /// still a match; otherwise it falls back to the first match, or `None`.
    pub fn sync(&mut self, rows: &[Vec<String>]) {
        let previous_current = self.current_match();
        self.recompute(rows, previous_current);
    }

    /// Clear the query and every match.
    pub fn clear(&mut self) {
        self.query.clear();
        self.matches.clear();
        self.current = None;
    }

    fn recompute(&mut self, rows: &[Vec<String>], keep_current: Option<MatchPosition>) {
        self.matches.clear();
        if self.query.is_empty() {
            self.current = None;
            return;
        }
        let needle = self.normalize_case(&self.query);
        for (row_index, row) in rows.iter().enumerate() {
            for (col_index, cell) in row.iter().enumerate() {
                if self.normalize_case(cell).contains(&needle) {
                    self.matches.push((row_index, col_index));
                }
            }
        }
        self.current = match keep_current {
            Some(position) => self
                .matches
                .iter()
                .position(|&candidate| candidate == position)
                .or(if self.matches.is_empty() {
                    None
                } else {
                    Some(0)
                }),
            None if self.matches.is_empty() => None,
            None => Some(0),
        };
    }

    fn normalize_case(&self, text: &str) -> String {
        if self.case_sensitive {
            text.to_owned()
        } else {
            text.to_lowercase()
        }
    }

    /// Advance to the next match, wrapping from the last back to the first.
    /// A no-op returning `None` with no matches.
    pub fn next_match(&mut self) -> Option<MatchPosition> {
        if self.matches.is_empty() {
            return None;
        }
        let next = self
            .current
            .map_or(0, |index| (index + 1) % self.matches.len());
        self.current = Some(next);
        self.current_match()
    }

    /// Step to the previous match, wrapping from the first back to the last.
    /// A no-op returning `None` with no matches.
    pub fn prev_match(&mut self) -> Option<MatchPosition> {
        if self.matches.is_empty() {
            return None;
        }
        let prev = self.current.map_or(self.matches.len() - 1, |index| {
            if index == 0 {
                self.matches.len() - 1
            } else {
                index - 1
            }
        });
        self.current = Some(prev);
        self.current_match()
    }
}

#[cfg(test)]
mod tests {
    use super::QuickFind;

    fn rows(cells: &[&[&str]]) -> Vec<Vec<String>> {
        cells
            .iter()
            .map(|row| row.iter().map(|cell| (*cell).to_owned()).collect())
            .collect()
    }

    #[test]
    fn an_empty_query_has_no_matches() {
        let mut qf = QuickFind::new();
        let data = rows(&[&["paid", "refund"]]);
        qf.set_query("", &data);
        assert_eq!(qf.match_count(), 0);
        assert_eq!(qf.current_match(), None);
        assert_eq!(qf.current_number(), None);
    }

    #[test]
    fn a_query_with_no_matches_leaves_current_none() {
        let mut qf = QuickFind::new();
        let data = rows(&[&["paid", "pending"]]);
        qf.set_query("refund", &data);
        assert_eq!(qf.match_count(), 0);
        assert_eq!(qf.current_match(), None);
    }

    #[test]
    fn a_single_match_becomes_current() {
        let mut qf = QuickFind::new();
        let data = rows(&[&["paid", "refunded"]]);
        qf.set_query("refund", &data);
        assert_eq!(qf.match_count(), 1);
        assert_eq!(qf.current_match(), Some((0, 1)));
        assert_eq!(qf.current_number(), Some(1));
        assert!(qf.is_match(0, 1));
        assert!(qf.is_current(0, 1));
        assert!(!qf.is_match(0, 0));
    }

    #[test]
    fn multiple_matches_are_found_in_row_then_column_order() {
        let mut qf = QuickFind::new();
        let data = rows(&[&["refund", "paid"], &["paid", "refunded"]]);
        qf.set_query("refund", &data);
        assert_eq!(qf.matches(), &[(0, 0), (1, 1)]);
        assert_eq!(qf.current_match(), Some((0, 0)));
    }

    #[test]
    fn next_advances_and_wraps_from_the_last_match_to_the_first() {
        let mut qf = QuickFind::new();
        let data = rows(&[&["refund"], &["refunded"], &["a refund"]]);
        qf.set_query("refund", &data);
        assert_eq!(qf.current_match(), Some((0, 0)));
        assert_eq!(qf.next_match(), Some((1, 0)));
        assert_eq!(qf.next_match(), Some((2, 0)));
        assert_eq!(
            qf.next_match(),
            Some((0, 0)),
            "next from the last match must wrap to the first"
        );
    }

    #[test]
    fn prev_steps_back_and_wraps_from_the_first_match_to_the_last() {
        let mut qf = QuickFind::new();
        let data = rows(&[&["refund"], &["refunded"], &["a refund"]]);
        qf.set_query("refund", &data);
        assert_eq!(qf.current_match(), Some((0, 0)));
        assert_eq!(
            qf.prev_match(),
            Some((2, 0)),
            "prev from the first match must wrap to the last"
        );
        assert_eq!(qf.prev_match(), Some((1, 0)));
        assert_eq!(qf.prev_match(), Some((0, 0)));
    }

    #[test]
    fn next_and_prev_are_no_ops_returning_none_with_no_matches() {
        let mut qf = QuickFind::new();
        let data = rows(&[&["paid"]]);
        qf.set_query("refund", &data);
        assert_eq!(qf.next_match(), None);
        assert_eq!(qf.prev_match(), None);
    }

    #[test]
    fn matching_is_case_insensitive_by_default() {
        let mut qf = QuickFind::new();
        let data = rows(&[&["REFUND"]]);
        qf.set_query("refund", &data);
        assert_eq!(qf.match_count(), 1);
    }

    #[test]
    fn case_sensitive_matching_excludes_a_different_case_hit() {
        let mut qf = QuickFind::new();
        let data = rows(&[&["REFUND"], &["refund"]]);
        qf.set_case_sensitive(true, &data);
        qf.set_query("refund", &data);
        assert_eq!(qf.matches(), &[(1, 0)]);
    }

    #[test]
    fn toggling_case_sensitivity_recomputes_the_current_query() {
        let mut qf = QuickFind::new();
        let data = rows(&[&["REFUND"], &["refund"]]);
        qf.set_query("refund", &data);
        assert_eq!(qf.match_count(), 2, "case-insensitive matches both rows");

        qf.set_case_sensitive(true, &data);
        assert_eq!(qf.matches(), &[(1, 0)]);
    }

    #[test]
    fn sync_preserves_the_current_match_when_it_is_still_present() {
        let mut qf = QuickFind::new();
        let data = rows(&[&["refund"], &["refunded"]]);
        qf.set_query("refund", &data);
        qf.next_match();
        assert_eq!(qf.current_match(), Some((1, 0)));

        // A third matching row streams in; the previous current match must
        // stay current rather than resetting to the first.
        let grown = rows(&[&["refund"], &["refunded"], &["a refund"]]);
        qf.sync(&grown);
        assert_eq!(qf.matches(), &[(0, 0), (1, 0), (2, 0)]);
        assert_eq!(qf.current_match(), Some((1, 0)));
    }

    #[test]
    fn sync_falls_back_to_the_first_match_once_the_current_one_is_gone() {
        let mut qf = QuickFind::new();
        let data = rows(&[&["refund"], &["refunded"]]);
        qf.set_query("refund", &data);
        qf.next_match();
        assert_eq!(qf.current_match(), Some((1, 0)));

        // The page swaps out from under the current match.
        let swapped = rows(&[&["a refund"]]);
        qf.sync(&swapped);
        assert_eq!(qf.current_match(), Some((0, 0)));
    }

    #[test]
    fn sync_clears_current_once_no_rows_match_anymore() {
        let mut qf = QuickFind::new();
        let data = rows(&[&["refund"]]);
        qf.set_query("refund", &data);
        assert_eq!(qf.match_count(), 1);

        let swapped = rows(&[&["paid"]]);
        qf.sync(&swapped);
        assert_eq!(qf.match_count(), 0);
        assert_eq!(qf.current_match(), None);
    }

    #[test]
    fn clear_resets_the_query_and_every_match() {
        let mut qf = QuickFind::new();
        let data = rows(&[&["refund"]]);
        qf.set_query("refund", &data);
        qf.clear();
        assert_eq!(qf.query(), "");
        assert_eq!(qf.match_count(), 0);
        assert_eq!(qf.current_match(), None);
    }
}
