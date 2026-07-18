//! A relation's total row count, distinguishing an exact count from a
//! cheap planner-derived estimate

use std::fmt;

/// Prefixes an [`RowCount::Estimated`] value wherever it is rendered as
/// text, visually distinguishing it from an exact count. Shared here so
/// every renderer (this crate's own [`fmt::Display`] impl below, and any
/// downstream UI formatting) marks estimates the same way instead of each
/// picking its own symbol.
pub const ESTIMATE_MARKER: &str = "~";

/// A relation's total row count, as reported by a [`crate::Connection`].
/// Which variant a query returns is entirely up to the driver: some
/// backends can only ever produce one of the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowCount {
    /// A precise count, typically from `SELECT COUNT(*)`.
    Exact(u64),
    /// A cheap approximation drawn from the backend's own planner
    /// statistics (e.g. Postgres's `pg_class.reltuples`), not guaranteed to
    /// match the table's live row count.
    Estimated(u64),
}

impl RowCount {
    /// The row count value, regardless of whether it is exact or estimated.
    #[must_use]
    pub fn value(self) -> u64 {
        match self {
            RowCount::Exact(n) | RowCount::Estimated(n) => n,
        }
    }

    /// Whether this count is a planner estimate rather than an exact count.
    #[must_use]
    pub fn is_estimated(self) -> bool {
        matches!(self, RowCount::Estimated(_))
    }
}

impl fmt::Display for RowCount {
    /// Exact counts render as bare digits; estimates are prefixed with
    /// [`ESTIMATE_MARKER`]. This is the plain, UI-agnostic rendering -- a
    /// downstream UI is free to layer its own formatting (thousands
    /// separators, an explanatory suffix) on top of [`RowCount::value`] and
    /// [`RowCount::is_estimated`] instead.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RowCount::Exact(n) => write!(f, "{n}"),
            RowCount::Estimated(n) => write!(f, "{ESTIMATE_MARKER}{n}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RowCount;

    #[test]
    fn exact_renders_without_an_estimate_marker() {
        assert_eq!(RowCount::Exact(1234).to_string(), "1234");
    }

    #[test]
    fn estimated_renders_with_an_estimate_marker() {
        assert_eq!(RowCount::Estimated(1234).to_string(), "~1234");
    }

    #[test]
    fn value_extracts_the_count_regardless_of_variant() {
        assert_eq!(RowCount::Exact(7).value(), 7);
        assert_eq!(RowCount::Estimated(9).value(), 9);
    }

    #[test]
    fn is_estimated_distinguishes_the_two_variants() {
        assert!(!RowCount::Exact(0).is_estimated());
        assert!(RowCount::Estimated(0).is_estimated());
    }
}
