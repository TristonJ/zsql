//! A reusable, virtualized two-pane table: a pinned gutter (row numbers by
//! default) plus a horizontally scrolling data pane, built by composing
//! [`crate::scrollable`] rather than reimplementing scrollbar geometry, drag
//! tracking, or wheel handling.
//!
//! [`Table`] is a per-render builder -- it owns no row or column data, only
//! the element tree it assembles. [`TableState`] is the frame-persistent
//! counterpart the caller holds as `Entity<TableState>`: the scroll handles
//! both panes share, plus the [`crate::scrollable::ScrollableState`] that
//! turns them into scrollbars. See [`TableState`]'s own docs for the
//! scrollbar thumb-drag obligation every caller must fulfill.

mod builder;
mod column;
mod gutter;
mod layout;
pub mod measure;
mod resize;
mod row;
mod state;
mod style;

pub use builder::{Table, TableSizing};
pub use column::TableColumn;
pub use gutter::{Gutter, RowNumberStyle, row_number_cell_shell};
pub use row::TableRow;
pub use state::TableState;
pub use style::{TableBorders, TableStyle};

#[cfg(any(test, feature = "test-support"))]
pub use builder::{
    body_first_cell_debug_selector, gutter_first_cell_debug_selector,
    header_first_cell_debug_selector,
};
#[cfg(any(test, feature = "test-support"))]
pub use resize::column_resize_handle_debug_selector;
