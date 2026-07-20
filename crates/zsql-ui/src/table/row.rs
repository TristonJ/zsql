//! A batch-rendered body row's cell content, as the caller's `rows`
//! callback returns it to [`super::Table`].

use gpui::AnyElement;

/// One body row's cells, in column order.
pub struct TableRow {
    pub cells: Vec<AnyElement>,
}

impl TableRow {
    /// A row built from `cells`, one per data column, in column order.
    #[must_use]
    pub fn new(cells: Vec<AnyElement>) -> Self {
        Self { cells }
    }
}

impl From<Vec<AnyElement>> for TableRow {
    fn from(cells: Vec<AnyElement>) -> Self {
        Self::new(cells)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Empty, IntoElement};

    use super::TableRow;

    #[test]
    fn new_carries_the_given_cell_count() {
        let row = TableRow::new(vec![Empty.into_any_element(), Empty.into_any_element()]);
        assert_eq!(row.cells.len(), 2);
    }

    #[test]
    fn from_vec_is_equivalent_to_new() {
        let row: TableRow = vec![Empty.into_any_element()].into();
        assert_eq!(row.cells.len(), 1);
    }
}
