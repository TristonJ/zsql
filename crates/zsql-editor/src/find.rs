//! Plain-text substring search over a multiline document, case-insensitive
//! unless case-sensitive matching is armed: ordered match spans plus
//! wrapping next/previous navigation. Knows nothing about `gpui` or
//! `EditorView` -- a caller renders the highlights and drives the buffer's
//! cursor from it.

/// One matching span: `line` is the zero-based line index, `start`/`end`
/// the zero-based `char` column range within that line (end-exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchSpan {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// Live match state for a find query over a caller-supplied document's
/// lines: the query text, every matching span in document order, and which
/// one is current.
#[derive(Debug, Clone, Default)]
pub struct EditorFind {
    query: String,
    matches: Vec<MatchSpan>,
    current: Option<usize>,
    case_sensitive: bool,
}

impl EditorFind {
    /// A fresh find session: an empty query and no matches.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every matching span, in document order.
    #[must_use]
    pub fn matches(&self) -> &[MatchSpan] {
        &self.matches
    }

    /// How many spans currently match.
    #[must_use]
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// The current match's 1-based position among [`EditorFind::matches`],
    /// or `None` with no matches.
    #[must_use]
    pub fn current_number(&self) -> Option<usize> {
        self.current.map(|index| index + 1)
    }

    /// The current match's span, or `None` with no matches.
    #[must_use]
    pub fn current_match(&self) -> Option<MatchSpan> {
        self.current
            .and_then(|index| self.matches.get(index).copied())
    }

    /// Set the query text and recompute matches against `lines`, landing on
    /// the first match (if any) as current.
    pub fn set_query(&mut self, query: impl Into<String>, lines: &[String]) {
        self.query = query.into();
        self.recompute(lines, None);
    }

    /// Recompute matches against `lines` for the current query, without
    /// changing what the user was searching for: use this after the
    /// document's text changed out from under an open session. The current
    /// match stays on the same span if it is still a match; otherwise it
    /// falls back to the first match, or `None`.
    pub fn sync(&mut self, lines: &[String]) {
        let previous_current = self.current_match();
        self.recompute(lines, previous_current);
    }

    /// Whether case-sensitive matching is armed.
    #[must_use]
    pub fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    /// Arm or disarm case-sensitive matching and recompute against `lines`.
    /// The current match stays on the same span if it still matches;
    /// otherwise it falls back to the first match, or `None`.
    pub fn set_case_sensitive(&mut self, case_sensitive: bool, lines: &[String]) {
        self.case_sensitive = case_sensitive;
        let previous_current = self.current_match();
        self.recompute(lines, previous_current);
    }

    fn recompute(&mut self, lines: &[String], keep_current: Option<MatchSpan>) {
        self.matches.clear();
        if self.query.is_empty() {
            self.current = None;
            return;
        }
        let needle: Vec<char> = self.query.chars().collect();
        for (line_index, line) in lines.iter().enumerate() {
            for (start, end) in matches_in_line(line, &needle, self.case_sensitive) {
                self.matches.push(MatchSpan {
                    line: line_index,
                    start,
                    end,
                });
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

    /// Advance to the next match, wrapping from the last back to the first.
    /// A no-op returning `None` with no matches.
    pub fn next_match(&mut self) -> Option<MatchSpan> {
        if self.matches.is_empty() {
            return None;
        }
        let next = self
            .current
            .map_or(0, |index| (index + 1) % self.matches.len());
        self.current = Some(next);
        self.current_match()
    }

    /// Step to the previous match, wrapping from the first back to the
    /// last. A no-op returning `None` with no matches.
    pub fn prev_match(&mut self) -> Option<MatchSpan> {
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

/// Every non-overlapping occurrence of `needle` in `line`, as
/// `(start, end)` char-column ranges. Scanning resumes right after each
/// match, so a needle of `"aa"` against `"aaa"` yields one match, not two
/// overlapping ones. Case-insensitive comparison folds case per character
/// (rather than lowercasing the whole line up front), keeping every
/// returned column aligned to `line`'s own `char` indices even where
/// Unicode case-folding would otherwise change a string's length.
fn matches_in_line(line: &str, needle: &[char], case_sensitive: bool) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut index = 0;
    while index + needle.len() <= chars.len() {
        let is_match =
            chars[index..index + needle.len()]
                .iter()
                .zip(needle)
                .all(|(haystack, needle)| {
                    if case_sensitive {
                        haystack == needle
                    } else {
                        haystack.to_lowercase().eq(needle.to_lowercase())
                    }
                });
        if is_match {
            spans.push((index, index + needle.len()));
            index += needle.len();
        } else {
            index += 1;
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::{EditorFind, MatchSpan};

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    fn span(line: usize, start: usize, end: usize) -> MatchSpan {
        MatchSpan { line, start, end }
    }

    #[test]
    fn an_empty_query_has_no_matches() {
        let mut find = EditorFind::new();
        let doc = lines(&["select * from orders"]);
        find.set_query("", &doc);
        assert_eq!(find.match_count(), 0);
        assert_eq!(find.current_match(), None);
        assert_eq!(find.current_number(), None);
    }

    #[test]
    fn a_query_with_no_matches_leaves_current_none() {
        let mut find = EditorFind::new();
        let doc = lines(&["select * from orders"]);
        find.set_query("customers", &doc);
        assert_eq!(find.match_count(), 0);
        assert_eq!(find.current_match(), None);
    }

    #[test]
    fn a_single_match_becomes_current() {
        let mut find = EditorFind::new();
        let doc = lines(&["select * from orders"]);
        find.set_query("orders", &doc);
        assert_eq!(find.match_count(), 1);
        assert_eq!(find.current_match(), Some(span(0, 14, 20)));
        assert_eq!(find.current_number(), Some(1));
    }

    #[test]
    fn matching_is_case_insensitive() {
        let mut find = EditorFind::new();
        let doc = lines(&["SELECT * FROM Orders"]);
        find.set_query("orders", &doc);
        assert_eq!(find.match_count(), 1);
        assert_eq!(find.current_match(), Some(span(0, 14, 20)));
    }

    #[test]
    fn arming_case_sensitivity_filters_matches_to_the_exact_case() {
        let mut find = EditorFind::new();
        let doc = lines(&["Orders", "ORDERS", "orders"]);
        find.set_query("orders", &doc);
        assert_eq!(find.match_count(), 3);

        find.set_case_sensitive(true, &doc);
        assert_eq!(
            find.matches(),
            &[span(2, 0, 6)],
            "case-sensitive \"orders\" must only match the literal lowercase line"
        );
        assert_eq!(find.current_match(), Some(span(2, 0, 6)));
    }

    #[test]
    fn disarming_case_sensitivity_restores_case_insensitive_matches() {
        let mut find = EditorFind::new();
        let doc = lines(&["Orders", "orders"]);
        find.set_query("orders", &doc);
        find.set_case_sensitive(true, &doc);
        assert_eq!(find.match_count(), 1);

        find.set_case_sensitive(false, &doc);
        assert_eq!(find.matches(), &[span(0, 0, 6), span(1, 0, 6)]);
    }

    #[test]
    fn arming_case_sensitivity_keeps_the_current_match_when_it_survives() {
        let mut find = EditorFind::new();
        let doc = lines(&["Orders", "orders", "orders again"]);
        find.set_query("orders", &doc);
        find.next_match();
        assert_eq!(find.current_match(), Some(span(1, 0, 6)));

        find.set_case_sensitive(true, &doc);
        assert_eq!(
            find.current_match(),
            Some(span(1, 0, 6)),
            "the current match must stay put when it survives the case change"
        );
        assert_eq!(find.match_count(), 2);
    }

    #[test]
    fn multiple_matches_across_multiple_lines_are_found_in_document_order() {
        let mut find = EditorFind::new();
        let doc = lines(&["select id from orders", "where orders.id > 1"]);
        find.set_query("orders", &doc);
        assert_eq!(
            find.matches(),
            &[span(0, 15, 21), span(1, 6, 12)],
            "matches must be ordered by line, then column"
        );
        assert_eq!(find.current_match(), Some(span(0, 15, 21)));
    }

    #[test]
    fn matches_within_one_line_are_non_overlapping() {
        let mut find = EditorFind::new();
        let doc = lines(&["aaaa"]);
        find.set_query("aa", &doc);
        assert_eq!(find.matches(), &[span(0, 0, 2), span(0, 2, 4)]);
    }

    #[test]
    fn next_advances_and_wraps_from_the_last_match_to_the_first() {
        let mut find = EditorFind::new();
        let doc = lines(&["orders", "order_items", "customer_orders"]);
        find.set_query("order", &doc);
        assert_eq!(find.current_match(), Some(span(0, 0, 5)));
        assert_eq!(find.next_match(), Some(span(1, 0, 5)));
        assert_eq!(find.next_match(), Some(span(2, 9, 14)));
        assert_eq!(
            find.next_match(),
            Some(span(0, 0, 5)),
            "next from the last match must wrap to the first"
        );
    }

    #[test]
    fn prev_steps_back_and_wraps_from_the_first_match_to_the_last() {
        let mut find = EditorFind::new();
        let doc = lines(&["orders", "order_items", "customer_orders"]);
        find.set_query("order", &doc);
        assert_eq!(find.current_match(), Some(span(0, 0, 5)));
        assert_eq!(
            find.prev_match(),
            Some(span(2, 9, 14)),
            "prev from the first match must wrap to the last"
        );
        assert_eq!(find.prev_match(), Some(span(1, 0, 5)));
        assert_eq!(find.prev_match(), Some(span(0, 0, 5)));
    }

    #[test]
    fn next_and_prev_are_no_ops_returning_none_with_no_matches() {
        let mut find = EditorFind::new();
        let doc = lines(&["select 1"]);
        find.set_query("orders", &doc);
        assert_eq!(find.next_match(), None);
        assert_eq!(find.prev_match(), None);
    }

    #[test]
    fn sync_preserves_the_current_match_when_it_is_still_present() {
        let mut find = EditorFind::new();
        let doc = lines(&["orders", "order_items"]);
        find.set_query("order", &doc);
        find.next_match();
        assert_eq!(find.current_match(), Some(span(1, 0, 5)));

        let grown = lines(&["orders", "order_items", "customer_orders"]);
        find.sync(&grown);
        assert_eq!(
            find.matches(),
            &[span(0, 0, 5), span(1, 0, 5), span(2, 9, 14)]
        );
        assert_eq!(
            find.current_match(),
            Some(span(1, 0, 5)),
            "the current match must stay on the same span after a text change"
        );
    }

    #[test]
    fn sync_falls_back_to_the_first_match_once_the_current_one_is_gone() {
        let mut find = EditorFind::new();
        let doc = lines(&["orders", "order_items"]);
        find.set_query("order", &doc);
        find.next_match();
        assert_eq!(find.current_match(), Some(span(1, 0, 5)));

        let edited = lines(&["orders"]);
        find.sync(&edited);
        assert_eq!(find.current_match(), Some(span(0, 0, 5)));
    }

    #[test]
    fn sync_clears_current_once_no_lines_match_anymore() {
        let mut find = EditorFind::new();
        let doc = lines(&["orders"]);
        find.set_query("orders", &doc);
        assert_eq!(find.match_count(), 1);

        let edited = lines(&["customers"]);
        find.sync(&edited);
        assert_eq!(find.match_count(), 0);
        assert_eq!(find.current_match(), None);
    }
}
