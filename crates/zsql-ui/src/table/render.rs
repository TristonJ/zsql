//! [`Table::render`]'s per-frame assembly: the gutter pane, header row, and
//! virtualized data list a render pass builds from a consumed [`Table`].

use std::ops::Range;

use gpui::{
    Context, Div, ElementId, Entity, FocusHandle, MouseButton, Pixels, Render, SharedString,
    UniformList, UniformListScrollHandle, div, prelude::*, rgb, uniform_list,
};

use crate::scrollable::restrict_wheel_to_own_axis;
use crate::scrollable::{ScrollableState, WithScrollbars};
use crate::table::builder::build_single_click_listener;
use crate::theme::ActiveTheme;

use super::builder::{
    CellClickListener, RowRenderer, Table, TableSizing, build_double_click_listener,
    build_right_click_listener, cell_shell, select_cell_on_click, select_cell_on_right_click,
};
use super::debug::{tag_first_body_cell, tag_first_gutter_cell};
use super::gutter::{
    Gutter, gutter_cell_shell, gutter_header_shell, row_number_cell_shell, row_number_header_shell,
};
use super::layout::{
    ColumnGeometry, ColumnLayout, column_geometry, read_scroll_handles, sync_scroll_axes,
};
use super::measure;
use super::resize;
use super::row::TableRow;
use super::state::TableState;
use super::style::TableStyle;

impl<V: Render> Table<V> {
    /// Build this table's element for the current render. Consumes `self`:
    /// a `Table` exists only for the duration of one render pass.
    #[must_use = "dropping the returned element renders no table"]
    pub fn render(self, cx: &mut Context<V>) -> Div {
        let Table {
            id,
            state,
            style,
            scrollbar_style,
            columns,
            row_count,
            gutter,
            rows,
            vertical_sizing: table_height,
            focus_on_click,
            selectable,
            on_cell_click,
            on_cell_double_click,
            on_cell_right_click,
            column_resize,
        } = self;
        let rows = rows.unwrap_or_else(|| -> RowRenderer<V> {
            Box::new(|_v, range, _window, _cx| range.map(|_| TableRow::new(Vec::new())).collect())
        });

        let single_click_listener = build_single_click_listener(on_cell_click, &state, cx);
        let double_click_listener = build_double_click_listener(on_cell_double_click, &state, cx);
        let right_click_listener = build_right_click_listener(on_cell_right_click, &state, cx);

        let ColumnGeometry {
            layouts,
            column_count,
            content_extent,
            fill_width,
        } = column_geometry(&columns);

        let (handles, focused_cell) = read_scroll_handles(&state, cx);

        sync_scroll_axes(
            &handles,
            row_count,
            column_count,
            content_extent,
            style,
            table_height,
            cx,
        );

        let gutter_pane = build_gutter_pane(
            gutter,
            row_count,
            style,
            handles.row_scroll_handle.clone(),
            &id,
            &state,
            cx,
        );
        let header_row = resize::build_header_row(
            columns,
            &style,
            content_extent,
            fill_width,
            column_resize.is_some(),
            &state,
        );

        let data_list = build_data_list(
            &id,
            row_count,
            layouts,
            fill_width,
            style,
            content_extent,
            table_height,
            focus_on_click,
            selectable,
            single_click_listener,
            double_click_listener,
            right_click_listener,
            focused_cell,
            &state,
            handles.row_scroll_handle.clone(),
            rows,
            cx,
        );

        let h_scroll_id = SharedString::from(format!("{id}-h-scroll"));
        // `.id()` so `.track_scroll` is available: the horizontal axis
        // scrolls this container, not a list.
        let data_pane = div()
            .id(h_scroll_id)
            .flex()
            .flex_col()
            .flex_1()
            .w_full()
            .min_w_0()
            .min_h_0()
            .h_full()
            .overflow_x_hidden()
            .track_scroll(&handles.col_scroll_handle)
            .on_scroll_wheel(ScrollableState::wheel_handler(&handles.scroll))
            .font_family(&cx.theme().fonts.data)
            .child(header_row)
            .child(data_list);

        let scrollable_data_pane = data_pane.with_scrollbars(&handles.scroll, scrollbar_style, cx);

        let mut root = div().flex().flex_row().flex_1().min_h_0().w_full();
        root = resize::wire_root(root, column_resize.as_ref(), &state, cx);
        if let Some(pane) = gutter_pane {
            root = root.child(pane);
        }
        root.child(scrollable_data_pane)
    }
}

/// The data pane's virtualized body: `row_count` rows batch-rendered
/// through `rows` and shaped into cells via [`build_body_row`], restricted
/// to its own scroll axis and sized per `table_height`.
#[allow(clippy::too_many_arguments)]
fn build_data_list<V: Render>(
    id: &ElementId,
    row_count: usize,
    layouts: Vec<ColumnLayout>,
    fill_width: bool,
    style: TableStyle,
    content_extent: Pixels,
    table_height: TableSizing,
    focus_on_click: Option<FocusHandle>,
    selectable: bool,
    single_click_listener: Option<CellClickListener>,
    double_click_listener: Option<CellClickListener>,
    right_click_listener: Option<CellClickListener>,
    focused_cell: Option<(usize, usize)>,
    state: &Entity<TableState>,
    row_scroll_handle: UniformListScrollHandle,
    rows: RowRenderer<V>,
    cx: &mut Context<V>,
) -> UniformList {
    let data_list_id = SharedString::from(format!("{id}-data"));
    let body_tag_state = state.clone();
    restrict_wheel_to_own_axis(
        uniform_list(
            data_list_id,
            row_count,
            cx.processor(move |this, range: Range<usize>, window, cx| {
                let top_of_viewport = range.start;
                let indices = range.clone();
                let row_ctx = BodyRowContext {
                    layouts: &layouts,
                    fill_width,
                    content_extent,
                    style: &style,
                    top_of_viewport,
                    focused_cell,
                    state: &body_tag_state,
                    focus_on_click: focus_on_click.as_ref(),
                    selectable,
                    single_click_listener: single_click_listener.clone(),
                    double_click_listener: double_click_listener.clone(),
                    right_click_listener: right_click_listener.clone(),
                };
                rows(this, range, window, cx)
                    .into_iter()
                    .zip(indices)
                    .map(|(row, row_index)| build_body_row(row, row_index, &row_ctx))
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .min_w(content_extent)
        .with_sizing_behavior(match table_height {
            TableSizing::Fill => gpui::ListSizingBehavior::Auto,
            TableSizing::Fit => gpui::ListSizingBehavior::Infer,
        })
        .track_scroll(row_scroll_handle),
    )
}

/// The pinned left pane, or `None` for [`Gutter::None`].
fn build_gutter_pane<V: Render>(
    gutter: Gutter<V>,
    row_count: usize,
    style: TableStyle,
    row_scroll_handle: gpui::UniformListScrollHandle,
    table_id: &ElementId,
    state: &Entity<TableState>,
    cx: &mut Context<V>,
) -> Option<Div> {
    match gutter {
        Gutter::None => None,
        Gutter::RowNumbers(row_number_style) => {
            let width = measure::row_number_column_width(
                row_count,
                &style,
                row_number_style.char_width,
                row_number_style.min_width,
            );
            let list_id = SharedString::from(format!("{table_id}-gutter"));
            let header = row_number_header_shell(&style).child("#");
            let state = state.clone();
            let list = restrict_wheel_to_own_axis(
                uniform_list(
                    list_id,
                    row_count,
                    cx.processor(move |_this, range: Range<usize>, _window, _cx| {
                        let top_of_viewport = range.start;
                        range
                            .map(|ix| {
                                let cell = row_number_cell_shell(width, &style)
                                    .child((ix + 1).to_string());
                                tag_first_gutter_cell(cell, ix, top_of_viewport, &state)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .track_scroll(row_scroll_handle),
            );
            Some(assemble_gutter_pane(width, &style, header, list))
        }
        Gutter::Custom {
            width,
            header,
            render,
        } => {
            let list_id = SharedString::from(format!("{table_id}-gutter"));
            let header_cell = gutter_header_shell(&style).child(header);
            let list = restrict_wheel_to_own_axis(
                uniform_list(
                    list_id,
                    row_count,
                    cx.processor(move |this, range: Range<usize>, window, cx| {
                        let expected = range.len();
                        let cells = render(this, range, window, cx);
                        debug_assert_eq!(
                            cells.len(),
                            expected,
                            "Gutter::Custom's renderer returned {} element(s) for a range of {} \
                             index(es); it must return exactly one element per requested index \
                             or the pinned gutter falls out of alignment with the data rows",
                            cells.len(),
                            expected,
                        );
                        cells
                            .into_iter()
                            .map(|cell| gutter_cell_shell(width, &style).child(cell))
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .track_scroll(row_scroll_handle),
            );
            Some(assemble_gutter_pane(width, &style, header_cell, list))
        }
    }
}

fn assemble_gutter_pane(
    width: Pixels,
    style: &TableStyle,
    header: Div,
    list: gpui::UniformList,
) -> Div {
    let mut pane = div().flex().flex_col().flex_shrink_0().w(width).h_full();
    if style.borders.outer {
        pane = pane.border_r_1().border_color(rgb(style.row_border));
    }
    pane.child(header).child(list)
}

/// One data-pane body row, its cells wrapped in `style`'s chrome; see
/// [`Table::rows`] for the cell-count contract this enforces in debug
/// builds.
///
/// `row_index` is this row's position in the data source; `ctx.top_of_viewport`
/// is the row index currently at the top of the data pane's visible range.
fn build_body_row(row: TableRow, row_index: usize, ctx: &BodyRowContext<'_>) -> Div {
    debug_assert!(
        row.cells.len() <= ctx.layouts.len(),
        "table row was given {} cells but the table only has {} columns; a release build \
         truncates the extra {} cell(s) instead of panicking",
        row.cells.len(),
        ctx.layouts.len(),
        row.cells.len() - ctx.layouts.len(),
    );
    build_body_row_cells(row, row_index, ctx)
}

/// Everything one data-pane body row needs beyond its own [`TableRow`] and
/// position: column sizing/chrome, the current selection, the table's
/// mechanical state, and an optional focus target for a cell click.
/// Grouped into one struct so row-building functions take a single context
/// argument instead of a long, easily-transposed positional parameter list.
struct BodyRowContext<'a> {
    layouts: &'a [ColumnLayout],
    /// Whether the table fills its container's width (any column grows), in
    /// which case each body row stretches to the full pane width so its
    /// growable cells have slack to expand into.
    fill_width: bool,
    /// The summed column width, i.e. the row's minimum width. Used only in
    /// `fill_width` mode, where each body row floors at this (matching the
    /// header) so a pane narrower than the columns scrolls both in lockstep
    /// instead of letting the body shrink out of alignment with the header.
    content_extent: Pixels,
    style: &'a TableStyle,
    /// The row index currently at the top of the data pane's visible range.
    top_of_viewport: usize,
    focused_cell: Option<(usize, usize)>,
    state: &'a Entity<TableState>,
    focus_on_click: Option<&'a FocusHandle>,
    /// Whether this table opted into click-to-select and the matching
    /// highlight via [`Table::selectable`]. Off by default, so a table with
    /// no use for cell selection renders inert, unclickable body cells.
    selectable: bool,
    /// Set via [`Table::on_cell_click`].
    single_click_listener: Option<CellClickListener>,
    /// Set via [`Table::on_cell_double_click`].
    double_click_listener: Option<CellClickListener>,
    /// Set via [`Table::on_cell_right_click`].
    right_click_listener: Option<CellClickListener>,
}

/// [`build_body_row`] without its debug assertion: zips `row.cells` against
/// `ctx.column_widths`, truncating to the shorter of the two.
fn build_body_row_cells(row: TableRow, row_index: usize, ctx: &BodyRowContext<'_>) -> Div {
    let style = ctx.style;
    let mut row_div = div().flex().flex_row().items_center().h(style.row_height);
    if let Some(background) = row.background {
        row_div = row_div.bg(background);
    }
    if ctx.fill_width {
        // Mirror the header row: fill the pane so growable cells have slack,
        // but never shrink below the summed column width, so a narrow pane
        // scrolls the header and body together instead of misaligning them.
        row_div = row_div.w_full().min_w(ctx.content_extent);
    }
    if style.borders.row {
        row_div = row_div.border_b_1().border_color(rgb(style.row_border));
    }
    for (cell_index, (cell, layout)) in row.cells.into_iter().zip(ctx.layouts.iter()).enumerate() {
        let mut shell = cell_shell(layout.width, layout.grow, style).child(cell);
        if ctx.selectable {
            if ctx.focused_cell == Some((row_index, cell_index)) {
                shell = shell
                    .bg(style.selection_wash)
                    .border_1()
                    .border_color(style.selection_ring);
            }
            let cell_id = SharedString::from(format!(
                "zsql-ui-table-cell-{row_index}-{cell_index}-{}",
                ctx.state.entity_id()
            ));
            let mut interactive = shell.id(cell_id).on_mouse_down(
                MouseButton::Left,
                select_cell_on_click(
                    ctx.state,
                    row_index,
                    cell_index,
                    ctx.focus_on_click.cloned(),
                    ctx.single_click_listener.clone(),
                    ctx.double_click_listener.clone(),
                ),
            );
            if let Some(right_click_listener) = ctx.right_click_listener.clone() {
                interactive = interactive.on_mouse_down(
                    MouseButton::Right,
                    select_cell_on_right_click(
                        ctx.state,
                        row_index,
                        cell_index,
                        right_click_listener,
                    ),
                );
            }
            let tagged = tag_first_body_cell(
                interactive,
                cell_index,
                row_index,
                ctx.top_of_viewport,
                ctx.state,
            );
            row_div = row_div.child(tagged);
        } else {
            let tagged =
                tag_first_body_cell(shell, cell_index, row_index, ctx.top_of_viewport, ctx.state);
            row_div = row_div.child(tagged);
        }
    }
    row_div
}

#[cfg(test)]
mod tests;
