//! Per-render column and scroll-handle geometry for [`super::Table::render`]:
//! computed once from the caller's columns and [`super::TableState`] so the
//! header row, body rows, and both scroll axes never drift apart from each
//! other.

use gpui::{Context, Entity, Pixels, Render, ScrollHandle, UniformListScrollHandle, px};

use super::builder::TableSizing;
use super::column::TableColumn;
use super::state::TableState;
use super::style::TableStyle;
use crate::scrollable::{Axis, ScrollSource, ScrollableState};

/// A column's per-cell sizing, carried from its [`TableColumn`] to every body
/// cell so header and body stay aligned. See [`TableColumn::grow`].
#[derive(Debug, Clone, Copy)]
pub(super) struct ColumnLayout {
    pub(super) width: Pixels,
    pub(super) grow: bool,
}

/// A render pass's column layouts plus the two aggregate values derived from
/// them, computed once and shared by the header row, body rows, and both
/// scroll axes so they can never drift apart from each other.
pub(super) struct ColumnGeometry {
    pub(super) layouts: Vec<ColumnLayout>,
    pub(super) column_count: usize,
    /// Summed column width, i.e. the horizontal scrollbar's content extent.
    pub(super) content_extent: Pixels,
    /// Whether any column grows -- see [`TableColumn::grow`].
    pub(super) fill_width: bool,
}

pub(super) fn column_geometry(columns: &[TableColumn]) -> ColumnGeometry {
    let layouts: Vec<ColumnLayout> = columns
        .iter()
        .map(|column| ColumnLayout {
            width: column.width,
            grow: column.grow,
        })
        .collect();
    let column_widths: Vec<Pixels> = layouts.iter().map(|layout| layout.width).collect();
    let content_extent = px(content_extent_for_columns(&column_widths));
    // When any column grows, the table fills its container's width so the
    // growable columns have slack to expand into; otherwise rows stay at
    // their fixed content width and scroll horizontally when they overflow.
    let fill_width = layouts.iter().any(|layout| layout.grow);
    ColumnGeometry {
        column_count: layouts.len(),
        layouts,
        content_extent,
        fill_width,
    }
}

/// The three scroll handles a table drives, grouped so they travel together
/// rather than as three positional arguments that can be transposed.
pub(super) struct TableScrollHandles {
    pub(super) scroll: Entity<ScrollableState>,
    pub(super) row_scroll_handle: UniformListScrollHandle,
    pub(super) col_scroll_handle: ScrollHandle,
}

/// `state`'s scroll handles and currently focused cell, read together so
/// [`super::Table::render`] borrows `state` for this snapshot and no longer
/// than it.
pub(super) fn read_scroll_handles<V: Render>(
    state: &Entity<TableState>,
    cx: &Context<V>,
) -> (TableScrollHandles, Option<(usize, usize)>) {
    let table_state = state.read(cx);
    (
        TableScrollHandles {
            scroll: table_state.scroll.clone(),
            row_scroll_handle: table_state.row_scroll_handle.clone(),
            col_scroll_handle: table_state.col_scroll_handle.clone(),
        },
        table_state.focused_cell(),
    )
}

/// Recompute both scroll axes' content extent for this render and push them
/// into the scrollable state, so its scrollbars and drag geometry stay current
/// with the table's latest row/column counts.
pub(super) fn sync_scroll_axes<V: Render>(
    handles: &TableScrollHandles,
    row_count: usize,
    column_count: usize,
    content_extent: Pixels,
    style: TableStyle,
    table_height: TableSizing,
    cx: &mut Context<V>,
) {
    let TableScrollHandles {
        scroll,
        row_scroll_handle,
        col_scroll_handle,
    } = handles;
    let _span =
        tracing::trace_span!("zsql_ui::table::sync_scroll_axes", row_count, column_count).entered();
    let vertical_extent = content_extent_for_row_count(row_count, style.row_height);
    tracing::trace!(
        vertical_extent,
        horizontal_extent = f32::from(content_extent),
        "recomputed the table's scroll axes for this render"
    );
    scroll.update(cx, |scroll, _cx| {
        // A `Fit` table grows to all its rows and never scrolls vertically on
        // its own -- its parent page scrolls instead -- so it must not
        // configure a vertical axis, or it would paint a vertical scrollbar
        // over content the parent is responsible for scrolling. A `Fill`
        // table is bounded by its parent and scrolls its rows internally, so
        // it keeps its vertical axis.
        match table_height {
            TableSizing::Fill => {
                scroll.vertical(
                    Axis::new(
                        ScrollSource::UniformList(row_scroll_handle.clone()),
                        vertical_extent,
                    )
                    .track_start(f32::from(style.header_height)),
                );
            }
            TableSizing::Fit => {
                scroll.clear_vertical();
            }
        }
        scroll.horizontal(Axis::new(
            ScrollSource::Container(col_scroll_handle.clone()),
            f32::from(content_extent),
        ));
    });
}

/// Total pixel height of `row_count` body rows, i.e. the vertical
/// scrollbar's content extent.
// Row counts here are always far below `f32`'s exact-integer range, so this
// conversion cannot lose meaningful precision.
#[allow(clippy::cast_precision_loss)]
pub(super) fn content_extent_for_row_count(row_count: usize, row_height: Pixels) -> f32 {
    row_count as f32 * f32::from(row_height)
}

/// Total pixel width of the data pane's columns, i.e. the horizontal
/// scrollbar's content extent. Excludes the pinned gutter pane, which never
/// scrolls horizontally.
pub(super) fn content_extent_for_columns(column_widths: &[Pixels]) -> f32 {
    column_widths.iter().copied().map(f32::from).sum()
}

#[cfg(test)]
mod tests {
    use gpui::px;

    use super::{content_extent_for_columns, content_extent_for_row_count};

    #[test]
    fn content_extent_for_columns_sums_the_widths() {
        let widths = vec![px(100.0), px(150.0), px(80.0)];
        assert!((content_extent_for_columns(&widths) - 330.0).abs() < f32::EPSILON);
    }

    #[test]
    fn content_extent_for_columns_is_zero_for_no_columns() {
        assert!((content_extent_for_columns(&[]) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn content_extent_for_row_count_multiplies_by_row_height() {
        assert!((content_extent_for_row_count(10, px(24.0)) - 240.0).abs() < f32::EPSILON);
    }
}
