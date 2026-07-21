//! [`Table`]: the per-render builder that assembles a two-pane virtualized
//! grid (a pinned gutter plus a horizontally scrolling data pane) by
//! composing [`crate::scrollable`]'s scroll/drag/wheel machinery rather than
//! reimplementing it.

use std::ops::Range;

use gpui::{
    App, Context, Div, ElementId, Entity, FocusHandle, MouseButton, MouseDownEvent, Pixels, Render,
    SharedString, UniformList, UniformListScrollHandle, Window, div, prelude::*, px, rgb, rgba,
    uniform_list,
};

use crate::scrollable::restrict_wheel_to_own_axis;
use crate::scrollable::{Axis, ScrollSource, ScrollableState, ScrollbarStyle, WithScrollbars};
use crate::theme::ActiveTheme;

use super::column::TableColumn;
use super::gutter::{
    Gutter, gutter_cell_shell, gutter_header_shell, row_number_cell_shell, row_number_header_shell,
};
use super::measure;
use super::row::TableRow;
use super::state::TableState;
use super::style::TableStyle;

type RowRenderer<V> =
    Box<dyn Fn(&mut V, Range<usize>, &mut Window, &mut Context<V>) -> Vec<TableRow>>;

/// A two-pane virtualized table, built fresh every render. Owns no row or
/// column data of its own -- [`TableState`] (frame-persistent, held by the
/// caller) is the only piece of this abstraction that survives between
/// renders.
pub struct Table<V: Render> {
    id: ElementId,
    state: Entity<TableState>,
    style: TableStyle,
    scrollbar_style: ScrollbarStyle,
    columns: Vec<TableColumn>,
    row_count: usize,
    gutter: Gutter<V>,
    rows: Option<RowRenderer<V>>,
    vertical_sizing: TableSizing,
    focus_on_click: Option<FocusHandle>,
    selectable: bool,
}

/// How to size the table's vertical extent in its parent. Defaults to [`TableSizing::Fill`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSizing {
    /// Fit the table's height to its parent, showing scrollbars if the table's rows overflow.
    Fill,
    /// Let the table's height grow to fit all its rows, showing no vertical scrollbar.
    Fit,
}

impl<V: Render> Table<V> {
    /// A table over `state`'s scroll machinery, initially with no columns,
    /// no gutter, and zero rows.
    #[must_use]
    pub fn new(id: impl Into<ElementId>, state: &Entity<TableState>) -> Self {
        Self {
            id: id.into(),
            state: state.clone(),
            style: TableStyle::default(),
            scrollbar_style: ScrollbarStyle::default(),
            columns: Vec::new(),
            row_count: 0,
            gutter: Gutter::None,
            rows: None,
            vertical_sizing: TableSizing::Fill,
            focus_on_click: None,
            selectable: false,
        }
    }

    /// Override the default visual chrome.
    #[must_use]
    pub fn style(mut self, style: TableStyle) -> Self {
        self.style = style;
        self
    }

    /// Override the default chrome of the scrollbars this table composes
    /// via `with_scrollbars`.
    #[must_use]
    pub fn scrollbar_style(mut self, style: ScrollbarStyle) -> Self {
        self.scrollbar_style = style;
        self
    }

    /// The data columns, in display order.
    #[must_use]
    pub fn columns(mut self, columns: Vec<TableColumn>) -> Self {
        self.columns = columns;
        self
    }

    /// How many rows the data pane's virtualized list has.
    #[must_use]
    pub fn row_count(mut self, row_count: usize) -> Self {
        self.row_count = row_count;
        self
    }

    /// The pinned left pane. Defaults to [`Gutter::None`].
    #[must_use]
    pub fn gutter(mut self, gutter: Gutter<V>) -> Self {
        self.gutter = gutter;
        self
    }

    /// How to size the table's vertical extent in its parent. Defaults to [`TableSizing::Fill`].
    #[must_use]
    pub fn vertical_sizing(mut self, sizing: TableSizing) -> Self {
        self.vertical_sizing = sizing;
        self
    }

    /// Focus `handle` whenever a data cell is clicked, so a caller's own key
    /// binding (e.g. a clipboard copy) captures the same click that just
    /// selected the cell. Has no effect unless [`Table::selectable`] is
    /// also called: with no cell selection wired up, there is no click to
    /// focus on.
    #[must_use]
    pub fn focus_on_cell_click(mut self, handle: FocusHandle) -> Self {
        self.focus_on_click = Some(handle);
        self
    }

    /// Enables click-to-select on this table's data cells: a click sets
    /// `TableState`'s focused cell and the matching cell paints a themed
    /// highlight. Off by default, so a table with no use for cell selection
    /// (e.g. a plain read-only browser) renders its body cells as inert
    /// content with no per-cell element id, click handler, or highlight.
    #[must_use]
    pub fn selectable(mut self) -> Self {
        self.selectable = true;
        self
    }

    /// The data pane's batch cell renderer, wired through `cx.processor` so
    /// it keeps `&mut V` access for building each visible range's cells.
    /// Each returned [`TableRow`] is expected to carry at most one cell per
    /// column: a debug build panics if a row carries more cells than the
    /// table has columns, while a release build truncates to the shorter of
    /// the two (zip-style) so a malformed batch degrades visually instead
    /// of crashing a shipped app. A row with fewer cells than columns is
    /// always tolerated and leaves the remaining columns blank for that
    /// row.
    #[must_use]
    pub fn rows(
        mut self,
        f: impl Fn(&mut V, Range<usize>, &mut Window, &mut Context<V>) -> Vec<TableRow> + 'static,
    ) -> Self {
        self.rows = Some(Box::new(f));
        self
    }

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
        } = self;
        let rows = rows.unwrap_or_else(|| -> RowRenderer<V> {
            Box::new(|_v, range, _window, _cx| range.map(|_| TableRow::new(Vec::new())).collect())
        });

        let column_widths: Vec<Pixels> = columns.iter().map(|column| column.width).collect();
        let content_extent = px(content_extent_for_columns(&column_widths));

        let (handles, focused_cell) = {
            let table_state = state.read(cx);
            (
                TableScrollHandles {
                    scroll: table_state.scroll.clone(),
                    row_scroll_handle: table_state.row_scroll_handle.clone(),
                    col_scroll_handle: table_state.col_scroll_handle.clone(),
                },
                table_state.focused_cell(),
            )
        };

        sync_scroll_axes(
            &handles,
            row_count,
            column_widths.len(),
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
        let header_row = build_header_row(columns, &style, content_extent, &state);

        let data_list = build_data_list(
            &id,
            row_count,
            column_widths,
            style,
            content_extent,
            table_height,
            focus_on_click,
            selectable,
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
    column_widths: Vec<Pixels>,
    style: TableStyle,
    content_extent: Pixels,
    table_height: TableSizing,
    focus_on_click: Option<FocusHandle>,
    selectable: bool,
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
                    column_widths: &column_widths,
                    style: &style,
                    top_of_viewport,
                    focused_cell,
                    state: &body_tag_state,
                    focus_on_click: focus_on_click.as_ref(),
                    selectable,
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

/// The three scroll handles a table drives, grouped so they travel together
/// rather than as three positional arguments that can be transposed.
struct TableScrollHandles {
    scroll: Entity<ScrollableState>,
    row_scroll_handle: gpui::UniformListScrollHandle,
    col_scroll_handle: gpui::ScrollHandle,
}

/// Recompute both scroll axes' content extent for this render and push them
/// into the scrollable state, so its scrollbars and drag geometry stay current
/// with the table's latest row/column counts.
fn sync_scroll_axes<V: Render>(
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

/// The data pane's sticky column-header row.
fn build_header_row(
    columns: Vec<TableColumn>,
    style: &TableStyle,
    content_extent: Pixels,
    state: &Entity<TableState>,
) -> Div {
    let mut row = div()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .min_w(content_extent)
        .h(style.header_height)
        .bg(rgb(style.header_bg));
    if style.borders.row {
        row = row.border_b_1().border_color(rgb(style.header_border));
    }
    for (index, column) in columns.into_iter().enumerate() {
        let cell = cell_shell(column.width, style).child(column.header);
        row = row.child(tag_header_cell(cell, index, state));
    }
    row
}

/// One data-pane body row, its cells wrapped in `style`'s chrome; see
/// [`Table::rows`] for the cell-count contract this enforces in debug
/// builds.
///
/// `row_index` is this row's position in the data source; `ctx.top_of_viewport`
/// is the row index currently at the top of the data pane's visible range.
fn build_body_row(row: TableRow, row_index: usize, ctx: &BodyRowContext<'_>) -> Div {
    debug_assert!(
        row.cells.len() <= ctx.column_widths.len(),
        "table row was given {} cells but the table only has {} columns; a release build \
         truncates the extra {} cell(s) instead of panicking",
        row.cells.len(),
        ctx.column_widths.len(),
        row.cells.len() - ctx.column_widths.len(),
    );
    build_body_row_cells(row, row_index, ctx)
}

/// Everything one data-pane body row needs beyond its own [`TableRow`] and
/// position: column sizing/chrome, the current selection, the table's
/// mechanical state, and an optional focus target for a cell click.
/// Grouped into one struct so row-building functions take a single context
/// argument instead of a long, easily-transposed positional parameter list.
struct BodyRowContext<'a> {
    column_widths: &'a [Pixels],
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
}

/// [`build_body_row`] without its debug assertion: zips `row.cells` against
/// `ctx.column_widths`, truncating to the shorter of the two.
fn build_body_row_cells(row: TableRow, row_index: usize, ctx: &BodyRowContext<'_>) -> Div {
    let style = ctx.style;
    let mut row_div = div().flex().flex_row().items_center().h(style.row_height);
    if style.borders.row {
        row_div = row_div.border_b_1().border_color(rgb(style.row_border));
    }
    for (cell_index, (cell, &width)) in row
        .cells
        .into_iter()
        .zip(ctx.column_widths.iter())
        .enumerate()
    {
        let mut shell = cell_shell(width, style).child(cell);
        if ctx.selectable {
            if ctx.focused_cell == Some((row_index, cell_index)) {
                shell = shell
                    .bg(rgba(style.selection_wash))
                    .border_1()
                    .border_color(rgba(style.selection_ring));
            }
            let cell_id = SharedString::from(format!(
                "zsql-ui-table-cell-{row_index}-{cell_index}-{}",
                ctx.state.entity_id()
            ));
            let interactive = shell.id(cell_id).on_mouse_down(
                MouseButton::Left,
                select_cell_on_click(
                    ctx.state,
                    row_index,
                    cell_index,
                    ctx.focus_on_click.cloned(),
                ),
            );
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

/// A data cell's mouse-down handler: selects `(row_index, cell_index)` in
/// `state` and, if given, focuses `focus_handle` so a caller's own key
/// binding (e.g. a clipboard copy) captures the same click that just
/// selected the cell.
fn select_cell_on_click(
    state: &Entity<TableState>,
    row_index: usize,
    cell_index: usize,
    focus_handle: Option<FocusHandle>,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    let state = state.clone();
    move |_event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
        let _span =
            tracing::trace_span!("zsql_ui::table::select_cell", row_index, cell_index).entered();
        state.update(cx, |table_state, cx| {
            table_state.set_focused_cell(row_index, cell_index);
            cx.notify();
        });
        if let Some(handle) = &focus_handle {
            window.focus(handle);
        }
    }
}

/// A data-pane cell's chrome, shared by header and body.
fn cell_shell(width: Pixels, style: &TableStyle) -> Div {
    let mut cell = div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .w(width)
        .h_full()
        .px(style.cell_padding_x)
        .truncate();
    if style.borders.column {
        cell = cell.border_r_1().border_color(rgb(style.row_border));
    }
    cell
}

/// Total pixel height of `row_count` body rows, i.e. the vertical
/// scrollbar's content extent.
// Row counts here are always far below `f32`'s exact-integer range, so this
// conversion cannot lose meaningful precision.
#[allow(clippy::cast_precision_loss)]
fn content_extent_for_row_count(row_count: usize, row_height: Pixels) -> f32 {
    row_count as f32 * f32::from(row_height)
}

/// Total pixel width of the data pane's columns, i.e. the horizontal
/// scrollbar's content extent. Excludes the pinned gutter pane, which never
/// scrolls horizontally.
fn content_extent_for_columns(column_widths: &[Pixels]) -> f32 {
    column_widths.iter().copied().map(f32::from).sum()
}

#[cfg(any(test, feature = "test-support"))]
fn gutter_first_cell_id(state: &Entity<TableState>) -> SharedString {
    SharedString::from(format!("zsql-ui-table-gutter-cell0-{}", state.entity_id()))
}

#[cfg(any(test, feature = "test-support"))]
fn header_first_cell_id(state: &Entity<TableState>) -> SharedString {
    SharedString::from(format!("zsql-ui-table-header-cell0-{}", state.entity_id()))
}

#[cfg(any(test, feature = "test-support"))]
fn body_first_cell_id(state: &Entity<TableState>) -> SharedString {
    SharedString::from(format!("zsql-ui-table-body-cell0-{}", state.entity_id()))
}

/// Tags `cell` if `ix` is the row currently at the top of the visible
/// range (`top_of_viewport`) with a lookup key for
/// `VisualTestContext::debug_bounds`, i.e. whichever gutter row a render
/// test can reliably find painted every frame regardless of how far the
/// list has scrolled, so render tests can confirm the gutter actually
/// moves in step with a vertical scroll/drag rather than only checking
/// that the shared scroll handle's offset changed. Every other cell passes
/// through unchanged.
#[cfg(any(test, feature = "test-support"))]
fn tag_first_gutter_cell(
    cell: Div,
    ix: usize,
    top_of_viewport: usize,
    state: &Entity<TableState>,
) -> Div {
    if ix == top_of_viewport {
        let selector = gutter_first_cell_id(state).to_string();
        cell.debug_selector(move || selector.clone())
    } else {
        cell
    }
}

/// A no-op outside test builds.
#[cfg(not(any(test, feature = "test-support")))]
fn tag_first_gutter_cell(
    cell: Div,
    _ix: usize,
    _top_of_viewport: usize,
    _state: &Entity<TableState>,
) -> Div {
    cell
}

/// Tags the header row's first cell with a lookup key for
/// `VisualTestContext::debug_bounds`, so render tests can confirm the header
/// actually moves in step with a horizontal scroll/drag rather than only
/// checking that the shared scroll handle's offset changed.
#[cfg(any(test, feature = "test-support"))]
fn tag_header_cell(cell: Div, index: usize, state: &Entity<TableState>) -> Div {
    if index == 0 {
        let selector = header_first_cell_id(state).to_string();
        cell.debug_selector(move || selector.clone())
    } else {
        cell
    }
}

/// A no-op outside test builds.
#[cfg(not(any(test, feature = "test-support")))]
fn tag_header_cell(cell: Div, _index: usize, _state: &Entity<TableState>) -> Div {
    cell
}

/// Tags the top-of-viewport body row's first cell (`cell_index == 0` and
/// `row_index == top_of_viewport`) with a lookup key for
/// `VisualTestContext::debug_bounds`, so render tests can confirm the data
/// body actually moves in step with the header/gutter rather than only
/// checking that a shared scroll handle's offset changed. Every other cell
/// passes through unchanged.
#[cfg(any(test, feature = "test-support"))]
fn tag_first_body_cell<E: InteractiveElement>(
    cell: E,
    cell_index: usize,
    row_index: usize,
    top_of_viewport: usize,
    state: &Entity<TableState>,
) -> E {
    if cell_index == 0 && row_index == top_of_viewport {
        let selector = body_first_cell_id(state).to_string();
        cell.debug_selector(move || selector.clone())
    } else {
        cell
    }
}

/// A no-op outside test builds.
#[cfg(not(any(test, feature = "test-support")))]
fn tag_first_body_cell<E>(
    cell: E,
    _cell_index: usize,
    _row_index: usize,
    _top_of_viewport: usize,
    _state: &Entity<TableState>,
) -> E {
    cell
}

/// The `VisualTestContext::debug_bounds` lookup key for `state`'s table's
/// gutter's first visible row-number cell, for a consumer crate's own render
/// tests. Requires this crate's `test-support` feature (or building this
/// crate's own tests).
///
/// The returned `&'static str` is deliberately leaked:
/// `VisualTestContext::debug_bounds` takes `&'static str`, and the key is
/// per-entity so it cannot be a literal. Test-support builds only, and one
/// small leak per call.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn gutter_first_cell_debug_selector(state: &Entity<TableState>) -> &'static str {
    Box::leak(gutter_first_cell_id(state).to_string().into_boxed_str())
}

/// The `VisualTestContext::debug_bounds` lookup key for `state`'s table's
/// header row's first cell, for a consumer crate's own render tests.
/// Requires this crate's `test-support` feature (or building this crate's
/// own tests).
///
/// The returned `&'static str` is deliberately leaked: see
/// [`gutter_first_cell_debug_selector`] for why.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn header_first_cell_debug_selector(state: &Entity<TableState>) -> &'static str {
    Box::leak(header_first_cell_id(state).to_string().into_boxed_str())
}

/// The `VisualTestContext::debug_bounds` lookup key for `state`'s table's
/// top-of-viewport body row's first cell, for a consumer crate's own render
/// tests. Requires this crate's `test-support` feature (or building this
/// crate's own tests).
///
/// The returned `&'static str` is deliberately leaked: see
/// [`gutter_first_cell_debug_selector`] for why.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn body_first_cell_debug_selector(state: &Entity<TableState>) -> &'static str {
    Box::leak(body_first_cell_id(state).to_string().into_boxed_str())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{
        AnyElement, AppContext as _, Context, Entity, Modifiers, MouseButton, MouseDownEvent,
        MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollDelta, ScrollWheelEvent,
        TestAppContext, TouchPhase, Window, div, point, prelude::*, px,
    };

    use super::{
        Table, body_first_cell_debug_selector, content_extent_for_columns,
        content_extent_for_row_count, gutter_first_cell_debug_selector,
        header_first_cell_debug_selector,
    };
    use crate::scrollable::{
        ScrollbarStyle, horizontal_thumb_debug_selector, vertical_thumb_debug_selector,
    };
    use crate::table::{
        Gutter, RowNumberStyle, TableBorders, TableColumn, TableRow, TableState, TableStyle,
    };

    const COLUMN_WIDTH: f32 = 300.0;
    const PANE_SIZE: f32 = 220.0;
    /// Passed for `top_of_viewport` when a test has no interest in the
    /// first-cell tagging path, guaranteeing it never matches `row_index`.
    const NO_TAG: usize = usize::MAX;

    /// Mounts `build_body_row_cells(row, widths, ...)` in a minimal window
    /// and returns how many cells it actually painted, so a test can
    /// observe truncation/short-row behavior directly rather than only
    /// "did not panic".
    fn painted_body_row_cell_count(
        cx: &mut TestAppContext,
        row: TableRow,
        widths: Vec<gpui::Pixels>,
    ) -> usize {
        struct Probe {
            table_state: Entity<TableState>,
            row: Option<TableRow>,
            widths: Vec<gpui::Pixels>,
            painted_count: Rc<Cell<Option<usize>>>,
        }
        impl Render for Probe {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                let Some(row) = self.row.take() else {
                    return div();
                };
                let style = TableStyle::default();
                let count = self.painted_count.clone();
                let row_ctx = super::BodyRowContext {
                    column_widths: &self.widths,
                    style: &style,
                    top_of_viewport: NO_TAG,
                    focused_cell: None,
                    state: &self.table_state,
                    focus_on_click: None,
                    selectable: false,
                };
                super::build_body_row_cells(row, 0, &row_ctx).on_children_prepainted(
                    move |bounds, _window, _app| {
                        count.set(Some(bounds.len()));
                    },
                )
            }
        }

        let painted_count = Rc::new(Cell::new(None));
        let probe_count = painted_count.clone();
        let (_probe, vcx) = cx.add_window_view(|_window, cx| Probe {
            table_state: cx.new(TableState::new),
            row: Some(row),
            widths,
            painted_count: probe_count,
        });
        vcx.run_until_parked();
        painted_count
            .get()
            .expect("build_body_row_cells's row must paint at least once")
    }

    #[gpui::test]
    fn build_body_row_leaves_missing_columns_unrendered_for_a_short_row(cx: &mut TestAppContext) {
        let widths = vec![px(100.0), px(100.0), px(100.0)];
        let row = TableRow::new(vec![gpui::Empty.into_any_element()]);
        let painted = painted_body_row_cell_count(cx, row, widths);
        assert_eq!(
            painted, 1,
            "a row with 1 cell but 3 columns must paint only the supplied cell, leaving the \
             remaining columns unrendered rather than padded out to the column count"
        );
    }

    #[gpui::test]
    #[should_panic(expected = "but the table only has")]
    fn build_body_row_flags_a_row_with_more_cells_than_columns(cx: &mut TestAppContext) {
        let state = cx.new(TableState::new);
        let style = TableStyle::default();
        let widths = vec![px(100.0)];
        let row = TableRow::new(vec![
            gpui::Empty.into_any_element(),
            gpui::Empty.into_any_element(),
        ]);
        // A row with more cells than the table has columns is a caller bug
        // (data that can never reach the screen), so the debug assertion
        // must fire in this (debug-assertions-enabled) test build.
        let row_ctx = super::BodyRowContext {
            column_widths: &widths,
            style: &style,
            top_of_viewport: NO_TAG,
            focused_cell: None,
            state: &state,
            focus_on_click: None,
            selectable: false,
        };
        let _div = super::build_body_row(row, 0, &row_ctx);
    }

    #[gpui::test]
    fn build_body_row_cells_truncates_a_row_with_more_cells_than_columns(cx: &mut TestAppContext) {
        let widths = vec![px(100.0)];
        let row = TableRow::new(vec![
            gpui::Empty.into_any_element(),
            gpui::Empty.into_any_element(),
        ]);
        // Exercises the same truncating path a release build's
        // `build_body_row` takes once its debug assertion compiles out.
        let painted = painted_body_row_cell_count(cx, row, widths);
        assert_eq!(
            painted, 1,
            "a row with 2 cells but only 1 column must paint exactly 1 cell (zip-style \
             truncation), not one per supplied cell"
        );
    }

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

    /// Which pinned-gutter arrangement [`Harness`] builds for its table.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum GutterPreset {
        None,
        RowNumbers,
        Custom,
    }

    /// A minimal view exercising `Table` with a configurable row/column
    /// count, generated content, and a pane small enough that both axes
    /// overflow by default.
    struct Harness {
        table_state: Entity<TableState>,
        row_count: usize,
        column_count: usize,
        column_width: gpui::Pixels,
        style: TableStyle,
        scrollbar_style: ScrollbarStyle,
        gutter: GutterPreset,
        omit_rows_callback: bool,
        selectable: bool,
    }

    impl Harness {
        fn new(row_count: usize, column_count: usize, cx: &mut Context<Self>) -> Self {
            Self {
                table_state: cx.new(TableState::new),
                row_count,
                column_count,
                column_width: px(COLUMN_WIDTH),
                style: TableStyle::default(),
                scrollbar_style: ScrollbarStyle::default(),
                gutter: GutterPreset::RowNumbers,
                omit_rows_callback: false,
                selectable: true,
            }
        }

        fn with_column_width(mut self, width: gpui::Pixels) -> Self {
            self.column_width = width;
            self
        }

        fn with_style(mut self, style: TableStyle) -> Self {
            self.style = style;
            self
        }

        fn with_scrollbar_style(mut self, style: ScrollbarStyle) -> Self {
            self.scrollbar_style = style;
            self
        }

        fn with_gutter(mut self, gutter: GutterPreset) -> Self {
            self.gutter = gutter;
            self
        }

        fn without_rows_callback(mut self) -> Self {
            self.omit_rows_callback = true;
            self
        }

        fn without_selectable(mut self) -> Self {
            self.selectable = false;
            self
        }
    }

    impl Render for Harness {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let columns: Vec<TableColumn> = (0..self.column_count)
                .map(|ix| TableColumn::new(self.column_width, format!("col{ix}")))
                .collect();
            let column_count = self.column_count;

            let gutter = match self.gutter {
                GutterPreset::None => Gutter::None,
                GutterPreset::RowNumbers => Gutter::RowNumbers(RowNumberStyle::default()),
                GutterPreset::Custom => Gutter::Custom {
                    width: px(48.0),
                    header: div().child("g").into_any_element(),
                    render: Box::new(|_this: &mut Self, range, _window, _cx| {
                        range
                            .map(|ix| div().child(format!("g{ix}")).into_any_element())
                            .collect::<Vec<AnyElement>>()
                    }),
                },
            };

            let mut table = Table::new("harness-table", &self.table_state)
                .style(self.style)
                .scrollbar_style(self.scrollbar_style)
                .columns(columns)
                .row_count(self.row_count)
                .gutter(gutter);
            if self.selectable {
                table = table.selectable();
            }
            if !self.omit_rows_callback {
                table = table.rows(move |_this: &mut Self, range, _window, _cx| {
                    range
                        .map(|ix| {
                            let cells = (0..column_count)
                                .map(|col| div().child(format!("r{ix}c{col}")).into_any_element())
                                .collect::<Vec<_>>();
                            TableRow::new(cells)
                        })
                        .collect::<Vec<_>>()
                });
            }
            let table = table.render(cx);

            div().size_full().child(
                div()
                    .w(px(PANE_SIZE))
                    .h(px(PANE_SIZE))
                    .flex()
                    .flex_col()
                    .child(table),
            )
        }
    }

    #[gpui::test]
    fn renders_one_frame_without_panicking(cx: &mut TestAppContext) {
        let (_harness, vcx) = cx.add_window_view(|_window, cx| Harness::new(200, 10, cx));
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn a_small_table_renders_no_scrollbars(cx: &mut TestAppContext) {
        // One column narrow enough that, together with the row-number
        // gutter's own width, it still fits inside `PANE_SIZE`: the
        // "no scrollbar" assertions below therefore depend on the real
        // fits-vs-overflows comparison rather than a trivially empty pane.
        const FITTING_COLUMN_WIDTH: f32 = 100.0;
        let (harness, vcx) = cx.add_window_view(|_window, cx| {
            Harness::new(3, 1, cx).with_column_width(px(FITTING_COLUMN_WIDTH))
        });
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, app| h.table_state.read(app).scroll().clone());
        assert!(
            vcx.debug_bounds(vertical_thumb_debug_selector(&scroll))
                .is_none(),
            "a table whose rows fit the pane must show no vertical scrollbar"
        );
        assert!(
            vcx.debug_bounds(horizontal_thumb_debug_selector(&scroll))
                .is_none(),
            "a table whose columns fit the pane must show no horizontal scrollbar"
        );
    }

    #[gpui::test]
    fn an_overflowing_table_shows_both_scrollbars(cx: &mut TestAppContext) {
        let (harness, vcx) = cx.add_window_view(|_window, cx| Harness::new(400, 20, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, app| h.table_state.read(app).scroll().clone());
        assert!(
            vcx.debug_bounds(vertical_thumb_debug_selector(&scroll))
                .is_some(),
            "400 overflowing rows must show a vertical scrollbar"
        );
        assert!(
            vcx.debug_bounds(horizontal_thumb_debug_selector(&scroll))
                .is_some(),
            "20 wide columns must show a horizontal scrollbar"
        );
    }

    #[gpui::test]
    fn dragging_the_vertical_thumb_moves_the_handle_shared_by_the_gutter_and_body_panes(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) = cx.add_window_view(|_window, cx| Harness::new(400, 2, cx));
        vcx.run_until_parked();

        let table_state = harness.read_with(vcx, |h, _app| h.table_state.clone());
        let scroll = harness.read_with(vcx, |h, app| h.table_state.read(app).scroll().clone());
        let selector = vertical_thumb_debug_selector(&scroll);
        let thumb_center = vcx
            .debug_bounds(selector)
            .expect("the vertical thumb must be painted once measured")
            .center();
        let gutter_cell_y_before = vcx
            .debug_bounds(gutter_first_cell_debug_selector(&table_state))
            .expect("the gutter's top-of-viewport cell must be painted before the drag")
            .origin
            .y;
        let body_cell_y_before = vcx
            .debug_bounds(body_first_cell_debug_selector(&table_state))
            .expect("the data body's top-of-viewport cell must be painted before the drag")
            .origin
            .y;

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: thumb_center,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(MouseMoveEvent {
            position: point(thumb_center.x, thumb_center.y + px(40.0)),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        vcx.run_until_parked();

        let offset = harness.read_with(vcx, |h, app| {
            h.table_state
                .read(app)
                .row_scroll_handle
                .0
                .borrow()
                .base_handle
                .offset()
                .y
        });
        assert!(
            offset < px(0.0),
            "dragging the vertical thumb must move the row scroll handle both the gutter and \
             data panes are built from, keeping them in lockstep"
        );

        // The gutter's own painted content must have actually re-rendered in
        // response to the drag, not just the shared handle's offset value:
        // this is what would catch the gutter's `uniform_list` silently not
        // being wired to `row_scroll_handle` at all.
        let gutter_cell_y_after = vcx
            .debug_bounds(gutter_first_cell_debug_selector(&table_state))
            .expect("the gutter's top-of-viewport cell must still be painted after the drag")
            .origin
            .y;
        assert_ne!(
            gutter_cell_y_after, gutter_cell_y_before,
            "the gutter pane's painted content must move when the shared vertical handle scrolls"
        );

        // The data body must move too, not just the gutter: this is what
        // would catch the body's own `uniform_list` silently not being
        // wired to `row_scroll_handle`, or the body living outside the
        // element the drag actually scrolls.
        let body_cell_y_after = vcx
            .debug_bounds(body_first_cell_debug_selector(&table_state))
            .expect("the data body's top-of-viewport cell must still be painted after the drag")
            .origin
            .y;
        assert_ne!(
            body_cell_y_after, body_cell_y_before,
            "the data body's painted content must move when the shared vertical handle scrolls"
        );
        assert_eq!(
            (body_cell_y_after < body_cell_y_before),
            (gutter_cell_y_after < gutter_cell_y_before),
            "the data body must shift in the same direction as the gutter when the shared \
             vertical handle scrolls, keeping them in lockstep"
        );

        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: thumb_center,
            modifiers: Modifiers::default(),
            click_count: 1,
        });
    }

    #[gpui::test]
    fn dragging_the_horizontal_thumb_moves_the_handle_shared_by_the_header_and_body(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) = cx.add_window_view(|_window, cx| Harness::new(400, 20, cx));
        vcx.run_until_parked();

        let table_state = harness.read_with(vcx, |h, _app| h.table_state.clone());
        let scroll = harness.read_with(vcx, |h, app| h.table_state.read(app).scroll().clone());
        let selector = horizontal_thumb_debug_selector(&scroll);
        let thumb_center = vcx
            .debug_bounds(selector)
            .expect("the horizontal thumb must be painted once measured")
            .center();
        let header_cell_x_before = vcx
            .debug_bounds(header_first_cell_debug_selector(&table_state))
            .expect("the header's first cell must be painted before the drag")
            .origin
            .x;
        let body_cell_x_before = vcx
            .debug_bounds(body_first_cell_debug_selector(&table_state))
            .expect("the data body's top-of-viewport cell must be painted before the drag")
            .origin
            .x;

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: thumb_center,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(MouseMoveEvent {
            position: point(thumb_center.x + px(60.0), thumb_center.y),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        vcx.run_until_parked();

        let offset = harness.read_with(vcx, |h, app| {
            h.table_state.read(app).col_scroll_handle.offset().x
        });
        assert!(
            offset < px(0.0),
            "dragging the horizontal thumb must move the scroll handle the header row and data \
             body are both children of, moving them together"
        );

        // The header's own painted content must have actually shifted, not
        // just the shared handle's offset value: this is what would catch
        // the header living outside the container `col_scroll_handle`
        // actually tracks.
        let header_cell_x_after = vcx
            .debug_bounds(header_first_cell_debug_selector(&table_state))
            .expect("the header's first cell must still be painted after the drag")
            .origin
            .x;
        assert!(
            header_cell_x_after < header_cell_x_before,
            "the header row's painted content must shift left when the shared horizontal \
             handle scrolls right"
        );

        // The data body must shift too, not just the header: this is what
        // would catch the data body living outside the `col_scroll_handle`
        // container the horizontal drag actually moves.
        let body_cell_x_after = vcx
            .debug_bounds(body_first_cell_debug_selector(&table_state))
            .expect("the data body's top-of-viewport cell must still be painted after the drag")
            .origin
            .x;
        assert!(
            body_cell_x_after < body_cell_x_before,
            "the data body's painted content must shift left when the shared horizontal handle \
             scrolls right, in lockstep with the header"
        );

        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: thumb_center,
            modifiers: Modifiers::default(),
            click_count: 1,
        });
    }

    #[gpui::test]
    fn shift_held_wheel_over_a_table_with_extra_rows_and_columns_moves_only_the_horizontal_axis(
        cx: &mut TestAppContext,
    ) {
        // Both axes populated with real content: the regression this guards
        // against only reproduces when the vertical axis is backed by a
        // real uniform_list with rows to (incorrectly) scroll.
        let (harness, vcx) = cx.add_window_view(|_window, cx| Harness::new(400, 20, cx));
        vcx.run_until_parked();

        let vertical_before = harness.read_with(vcx, |h, app| {
            h.table_state
                .read(app)
                .row_scroll_handle
                .0
                .borrow()
                .base_handle
                .offset()
                .y
        });

        // x=150 lands inside the data pane, past the row-number gutter's own
        // width (~80px) that would otherwise absorb the event.
        vcx.simulate_event(ScrollWheelEvent {
            position: point(px(150.0), px(50.0)),
            delta: ScrollDelta::Pixels(point(px(-40.0), px(0.0))),
            modifiers: Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            touch_phase: TouchPhase::Moved,
        });
        vcx.run_until_parked();

        let (horizontal_after, vertical_after) = harness.read_with(vcx, |h, app| {
            let state = h.table_state.read(app);
            (
                state.col_scroll_handle.offset().x,
                state.row_scroll_handle.0.borrow().base_handle.offset().y,
            )
        });
        assert!(
            horizontal_after < px(0.0),
            "a shift-held wheel event must move the horizontal axis"
        );
        assert_eq!(
            vertical_after, vertical_before,
            "a shift-held wheel event must leave the vertical axis untouched: both the gutter \
             and data uniform_lists must restrict their own wheel scrolling to their own axis, \
             or the platform's swapped gesture magnitude scrolls both axes at once"
        );
    }

    #[gpui::test]
    fn shift_held_wheel_over_the_row_number_gutter_leaves_both_axes_untouched(
        cx: &mut TestAppContext,
    ) {
        // The gutter's `uniform_list` is the other place
        // `restrict_wheel_to_own_axis` must be applied, and the data-pane
        // regression test above (which dispatches past the gutter's own
        // width) cannot catch a gutter left unrestricted. The gutter has no
        // horizontal scroll surface of its own, so nothing should ever move
        // its horizontal offset; without the restriction, gpui's own
        // fallback reads the platform's swapped gesture magnitude as a
        // *vertical* delta and scrolls the row handle the gutter shares with
        // the data pane, using a gesture that never signalled "scroll down".
        let (harness, vcx) = cx.add_window_view(|_window, cx| Harness::new(400, 20, cx));
        vcx.run_until_parked();

        let (vertical_before, horizontal_before) = harness.read_with(vcx, |h, app| {
            let state = h.table_state.read(app);
            (
                state.row_scroll_handle.0.borrow().base_handle.offset().y,
                state.col_scroll_handle.offset().x,
            )
        });

        // x=40 lands inside the row-number gutter: 400 rows need 3 digits,
        // clamping the gutter to its configured minimum width of 80px.
        vcx.simulate_event(ScrollWheelEvent {
            position: point(px(40.0), px(50.0)),
            delta: ScrollDelta::Pixels(point(px(-40.0), px(0.0))),
            modifiers: Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            touch_phase: TouchPhase::Moved,
        });
        vcx.run_until_parked();

        let (vertical_after, horizontal_after) = harness.read_with(vcx, |h, app| {
            let state = h.table_state.read(app);
            (
                state.row_scroll_handle.0.borrow().base_handle.offset().y,
                state.col_scroll_handle.offset().x,
            )
        });
        assert_eq!(
            vertical_after, vertical_before,
            "a shift-held wheel event over the gutter must leave the vertical axis (shared with \
             the data pane) untouched: the gutter's own uniform_list must restrict its wheel \
             scrolling to its own axis rather than falling back to reading the swapped delta"
        );
        assert_eq!(
            horizontal_after, horizontal_before,
            "the gutter pane has no horizontal scroll surface of its own, so a wheel event over \
             it must never move the horizontal axis either"
        );
    }

    #[gpui::test]
    fn a_plain_wheel_over_the_data_pane_scrolls_the_gutter_in_lockstep(cx: &mut TestAppContext) {
        // Every other lockstep assertion in this file drives the shared
        // handle via a scrollbar thumb drag. A plain (non-shift) wheel over
        // the data pane instead exercises `uniform_list`'s own native
        // vertical scrolling, the path users hit most: this must still land
        // on `row_scroll_handle` and move the gutter along with it.
        let (harness, vcx) = cx.add_window_view(|_window, cx| Harness::new(400, 20, cx));
        vcx.run_until_parked();

        let table_state = harness.read_with(vcx, |h, _app| h.table_state.clone());
        let gutter_cell_y_before = vcx
            .debug_bounds(gutter_first_cell_debug_selector(&table_state))
            .expect("the gutter's top-of-viewport cell must be painted before the wheel event")
            .origin
            .y;

        // x=150 lands inside the data pane, past the row-number gutter's own
        // width (~80px) that would otherwise absorb the event.
        vcx.simulate_event(ScrollWheelEvent {
            position: point(px(150.0), px(50.0)),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-40.0))),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        });
        vcx.run_until_parked();

        let offset = harness.read_with(vcx, |h, app| {
            h.table_state
                .read(app)
                .row_scroll_handle
                .0
                .borrow()
                .base_handle
                .offset()
                .y
        });
        assert!(
            offset < px(0.0),
            "a plain wheel event over the data pane must move the shared row scroll handle"
        );

        let gutter_cell_y_after = vcx
            .debug_bounds(gutter_first_cell_debug_selector(&table_state))
            .expect("the gutter's top-of-viewport cell must still be painted after the wheel event")
            .origin
            .y;
        assert_ne!(
            gutter_cell_y_after, gutter_cell_y_before,
            "the gutter pane's painted content must move when a plain wheel event scrolls the \
             shared vertical handle through the data pane's own uniform_list"
        );
    }

    #[gpui::test]
    fn a_custom_row_height_sizes_the_painted_body_cells(cx: &mut TestAppContext) {
        // Compared against the default rather than asserted absolutely: a
        // row's bottom border is drawn inside its height, so a painted cell
        // is one pixel shorter than row_height. The difference between two
        // heights cancels that out and still fails if row_height is ignored.
        const TALLER_ROW_HEIGHT: f32 = 40.0;
        let taller = TableStyle {
            row_height: px(TALLER_ROW_HEIGHT),
            ..TableStyle::default()
        };
        let default_height = TableStyle::default().row_height;

        let (default_harness, default_vcx) =
            cx.add_window_view(|_window, cx| Harness::new(400, 2, cx));
        default_vcx.run_until_parked();
        let default_state = default_harness.read_with(default_vcx, |h, _app| h.table_state.clone());
        let default_cell = default_vcx
            .debug_bounds(body_first_cell_debug_selector(&default_state))
            .expect("the default-height body cell must be painted");

        let (tall_harness, tall_vcx) =
            cx.add_window_view(|_window, cx| Harness::new(400, 2, cx).with_style(taller));
        tall_vcx.run_until_parked();
        let tall_state = tall_harness.read_with(tall_vcx, |h, _app| h.table_state.clone());
        let tall_cell = tall_vcx
            .debug_bounds(body_first_cell_debug_selector(&tall_state))
            .expect("the tall-row body cell must be painted");

        assert_eq!(
            tall_cell.size.height - default_cell.size.height,
            px(TALLER_ROW_HEIGHT) - default_height,
            "a body cell's painted height must track TableStyle::row_height, not a hardcoded \
             default"
        );
    }

    #[gpui::test]
    fn a_custom_header_height_pushes_the_vertical_thumbs_track_down_to_match(
        cx: &mut TestAppContext,
    ) {
        const CUSTOM_HEADER_HEIGHT: f32 = 48.0;
        let style = TableStyle {
            header_height: px(CUSTOM_HEADER_HEIGHT),
            ..TableStyle::default()
        };
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(400, 2, cx).with_style(style));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, app| h.table_state.read(app).scroll().clone());
        let bounds = vcx
            .debug_bounds(vertical_thumb_debug_selector(&scroll))
            .expect("the vertical thumb must be painted once measured");
        assert!(
            bounds.origin.y >= px(CUSTOM_HEADER_HEIGHT),
            "the vertical thumb's track must start at or below a non-default header height \
             ({CUSTOM_HEADER_HEIGHT}), not a hardcoded default: painted origin.y was {:?}",
            bounds.origin.y
        );
    }

    #[gpui::test]
    fn a_custom_scrollbar_style_reaches_the_painted_thumb(cx: &mut TestAppContext) {
        const CUSTOM_TRACK_WIDTH: f32 = 21.0;
        let scrollbar_style = ScrollbarStyle {
            track_width: CUSTOM_TRACK_WIDTH,
            ..ScrollbarStyle::default()
        };
        let (harness, vcx) = cx.add_window_view(|_window, cx| {
            Harness::new(400, 2, cx).with_scrollbar_style(scrollbar_style)
        });
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, app| h.table_state.read(app).scroll().clone());
        let bounds = vcx
            .debug_bounds(vertical_thumb_debug_selector(&scroll))
            .expect("the vertical thumb must be painted once measured");
        assert!(
            (f32::from(bounds.size.width) - CUSTOM_TRACK_WIDTH).abs() < 1.0,
            "Table::scrollbar_style's track_width ({CUSTOM_TRACK_WIDTH}) must reach the \
             painted thumb via with_scrollbars, not just the ScrollbarStyle struct: measured \
             {:?}",
            bounds.size.width
        );
    }

    #[gpui::test]
    fn a_table_with_no_gutter_renders_without_panicking(cx: &mut TestAppContext) {
        let (_harness, vcx) = cx
            .add_window_view(|_window, cx| Harness::new(50, 3, cx).with_gutter(GutterPreset::None));
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn a_table_with_no_gutter_starts_its_header_further_left_than_a_row_number_gutter_does(
        cx: &mut TestAppContext,
    ) {
        let (harness_none, vcx_none) = cx
            .add_window_view(|_window, cx| Harness::new(50, 3, cx).with_gutter(GutterPreset::None));
        vcx_none.run_until_parked();
        let table_state_none = harness_none.read_with(vcx_none, |h, _app| h.table_state.clone());
        let header_x_none = vcx_none
            .debug_bounds(header_first_cell_debug_selector(&table_state_none))
            .expect("the header's first cell must be painted with no gutter")
            .origin
            .x;

        let (harness_numbers, vcx_numbers) = cx.add_window_view(|_window, cx| {
            Harness::new(50, 3, cx).with_gutter(GutterPreset::RowNumbers)
        });
        vcx_numbers.run_until_parked();
        let table_state_numbers =
            harness_numbers.read_with(vcx_numbers, |h, _app| h.table_state.clone());
        let header_x_with_gutter = vcx_numbers
            .debug_bounds(header_first_cell_debug_selector(&table_state_numbers))
            .expect("the header's first cell must be painted with a row-number gutter")
            .origin
            .x;

        assert!(
            header_x_none < header_x_with_gutter,
            "a pinned row-number gutter must push the data pane's header to the right of where \
             it starts with no gutter at all"
        );
    }

    #[gpui::test]
    fn a_table_with_a_custom_gutter_renders_without_panicking(cx: &mut TestAppContext) {
        let (_harness, vcx) = cx.add_window_view(|_window, cx| {
            Harness::new(50, 3, cx).with_gutter(GutterPreset::Custom)
        });
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn a_custom_gutter_lays_out_at_its_declared_width_not_the_row_number_width(
        cx: &mut TestAppContext,
    ) {
        // `Harness` declares its custom gutter at this width -- see the
        // `GutterPreset::Custom` arm of `Harness::render`.
        const CUSTOM_GUTTER_WIDTH: f32 = 48.0;

        let (harness_none, vcx_none) = cx
            .add_window_view(|_window, cx| Harness::new(50, 3, cx).with_gutter(GutterPreset::None));
        vcx_none.run_until_parked();
        let table_state_none = harness_none.read_with(vcx_none, |h, _app| h.table_state.clone());
        let header_x_none = vcx_none
            .debug_bounds(header_first_cell_debug_selector(&table_state_none))
            .expect("the header's first cell must be painted with no gutter")
            .origin
            .x;

        let (harness_custom, vcx_custom) = cx.add_window_view(|_window, cx| {
            Harness::new(50, 3, cx).with_gutter(GutterPreset::Custom)
        });
        vcx_custom.run_until_parked();
        let table_state_custom =
            harness_custom.read_with(vcx_custom, |h, _app| h.table_state.clone());
        let header_x_custom = vcx_custom
            .debug_bounds(header_first_cell_debug_selector(&table_state_custom))
            .expect("the header's first cell must be painted with a custom gutter")
            .origin
            .x;

        let (harness_numbers, vcx_numbers) = cx.add_window_view(|_window, cx| {
            Harness::new(50, 3, cx).with_gutter(GutterPreset::RowNumbers)
        });
        vcx_numbers.run_until_parked();
        let table_state_numbers =
            harness_numbers.read_with(vcx_numbers, |h, _app| h.table_state.clone());
        let header_x_numbers = vcx_numbers
            .debug_bounds(header_first_cell_debug_selector(&table_state_numbers))
            .expect("the header's first cell must be painted with a row-number gutter")
            .origin
            .x;

        let custom_gutter_width = f32::from(header_x_custom) - f32::from(header_x_none);
        assert!(
            (custom_gutter_width - CUSTOM_GUTTER_WIDTH).abs() < 2.0,
            "a Gutter::Custom pane must push the header right by its declared {CUSTOM_GUTTER_WIDTH}px \
             width, not the row-number gutter's own computed width: measured {custom_gutter_width}px"
        );
        assert_ne!(
            header_x_custom, header_x_numbers,
            "the custom gutter's declared width must differ from the row-number gutter's own \
             computed width for this harness, or this test would not distinguish the two"
        );
    }

    #[gpui::test]
    fn a_custom_gutter_cell_imposes_no_forced_alignment_on_its_content(cx: &mut TestAppContext) {
        // `gutter_cell_shell` must contribute sizing/background chrome only,
        // never a forced justification or text color: a narrow child inside
        // the wide custom cell should sit near the cell's own left edge, not
        // pushed to its right edge the way `Gutter::RowNumbers`' own
        // right-aligned cells are.
        const GUTTER_WIDTH: f32 = 48.0;
        const CHILD_WIDTH: f32 = 4.0;
        const CHILD_SELECTOR: &str = "zsql-ui-table-test-custom-gutter-child";

        struct Probe {
            table_state: Entity<TableState>,
        }
        impl Render for Probe {
            fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let gutter = Gutter::Custom {
                    width: px(GUTTER_WIDTH),
                    header: div().child("g").into_any_element(),
                    render: Box::new(|_this: &mut Self, range, _window, _cx| {
                        range
                            .map(|ix| {
                                let child = div().w(px(CHILD_WIDTH)).h(px(CHILD_WIDTH));
                                if ix == 0 {
                                    child.debug_selector(|| CHILD_SELECTOR.to_string())
                                } else {
                                    child
                                }
                                .into_any_element()
                            })
                            .collect::<Vec<AnyElement>>()
                    }),
                };
                let table = Table::new("probe-table", &self.table_state)
                    .columns(vec![TableColumn::new(px(COLUMN_WIDTH), div().child("c"))])
                    .row_count(5)
                    .gutter(gutter)
                    .rows(|_this: &mut Self, range, _window, _cx| {
                        range.map(|_ix| TableRow::new(Vec::new())).collect()
                    })
                    .render(cx);
                div()
                    .w(px(PANE_SIZE))
                    .h(px(PANE_SIZE))
                    .flex()
                    .flex_col()
                    .child(table)
            }
        }

        let (probe, vcx) = cx.add_window_view(|_window, cx| Probe {
            table_state: cx.new(TableState::new),
        });
        vcx.run_until_parked();

        let table_state = probe.read_with(vcx, |p, _app| p.table_state.clone());
        let header_x = vcx
            .debug_bounds(header_first_cell_debug_selector(&table_state))
            .expect("the data pane's header cell must be painted")
            .origin
            .x;
        let cell_left = header_x - px(GUTTER_WIDTH);
        let child_x = vcx
            .debug_bounds(CHILD_SELECTOR)
            .expect("the tagged custom-gutter child must be painted")
            .origin
            .x;

        assert!(
            child_x - cell_left < px(GUTTER_WIDTH / 2.0),
            "a Gutter::Custom cell's content must render near the cell's own left edge, not \
             pushed toward its right edge by a forced justify_end: cell left was {cell_left:?}, \
             child left was {child_x:?}"
        );
    }

    #[gpui::test]
    #[should_panic(expected = "exactly one element per requested index")]
    fn a_custom_gutter_renderer_returning_too_few_cells_panics(cx: &mut TestAppContext) {
        struct Probe {
            table_state: Entity<TableState>,
        }
        impl Render for Probe {
            fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let gutter = Gutter::Custom {
                    width: px(48.0),
                    header: div().child("g").into_any_element(),
                    render: Box::new(|_this: &mut Self, range, _window, _cx| {
                        // One fewer element than the requested range, so the
                        // pinned gutter would fall out of alignment with the
                        // data rows were the mismatch not caught.
                        range
                            .skip(1)
                            .map(|ix| div().child(format!("g{ix}")).into_any_element())
                            .collect::<Vec<AnyElement>>()
                    }),
                };
                let table = Table::new("probe-table", &self.table_state)
                    .columns(vec![TableColumn::new(px(COLUMN_WIDTH), div().child("c"))])
                    .row_count(5)
                    .gutter(gutter)
                    .rows(|_this: &mut Self, range, _window, _cx| {
                        range.map(|_ix| TableRow::new(Vec::new())).collect()
                    })
                    .render(cx);
                div()
                    .w(px(PANE_SIZE))
                    .h(px(PANE_SIZE))
                    .flex()
                    .flex_col()
                    .child(table)
            }
        }

        let (_probe, vcx) = cx.add_window_view(|_window, cx| Probe {
            table_state: cx.new(TableState::new),
        });
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn a_table_with_borders_disabled_renders_without_panicking(cx: &mut TestAppContext) {
        let borderless = TableStyle {
            borders: TableBorders {
                row: false,
                column: false,
                outer: false,
            },
            ..TableStyle::default()
        };
        let (_harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(50, 3, cx).with_style(borderless));
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn a_borderless_table_paints_a_taller_body_cell_than_a_bordered_one(cx: &mut TestAppContext) {
        // A body row's bottom hairline (`border_b_1`) is painted inside the
        // row's own box rather than added on top of it, so it is the
        // row's *reported paint height* -- not any sibling's position --
        // that shrinks by the border's width while it is drawn. This is
        // the one dimension `TableBorders` actually changes: it proves the
        // flag is read at all, rather than a smoke test that would pass
        // even if `style.borders` were ignored entirely.
        let (harness_bordered, vcx_bordered) =
            cx.add_window_view(|_window, cx| Harness::new(50, 3, cx));
        vcx_bordered.run_until_parked();
        let state_bordered =
            harness_bordered.read_with(vcx_bordered, |h, _app| h.table_state.clone());
        let body_height_bordered = vcx_bordered
            .debug_bounds(body_first_cell_debug_selector(&state_bordered))
            .expect("the data body's top-of-viewport cell must be painted for a bordered table")
            .size
            .height;

        let borderless = TableStyle {
            borders: TableBorders {
                row: false,
                column: false,
                outer: false,
            },
            ..TableStyle::default()
        };
        let (harness_borderless, vcx_borderless) =
            cx.add_window_view(|_window, cx| Harness::new(50, 3, cx).with_style(borderless));
        vcx_borderless.run_until_parked();
        let state_borderless =
            harness_borderless.read_with(vcx_borderless, |h, _app| h.table_state.clone());
        let body_height_borderless = vcx_borderless
            .debug_bounds(body_first_cell_debug_selector(&state_borderless))
            .expect("the data body's top-of-viewport cell must be painted for a borderless table")
            .size
            .height;

        assert!(
            body_height_borderless > body_height_bordered,
            "disabling TableBorders.row must paint a taller body cell (no hairline eating into \
             its content height) than a bordered table's cell of the same configured \
             row_height, rather than TableStyle::borders being silently ignored: bordered \
             height was {body_height_bordered:?}, borderless was {body_height_borderless:?}"
        );
    }

    /// Mounts a single-column, gutterless probe table styled with `style`
    /// and returns the painted width of a full-width child inside its one
    /// body cell.
    ///
    /// A cell's own painted bounds are its declared width regardless of
    /// whether its own right border is drawn (the border paints inside the
    /// box rather than shrinking the box's own reported size, the same
    /// reason the row-border test below measures a full-*height* child
    /// rather than the row's own bounds). A full-*width* child of the cell,
    /// by contrast, is sized as a percentage of the cell's shrunken content
    /// box, so it is what actually exposes whether `TableBorders.column` is
    /// read at all.
    fn probe_column_border_child_width(style: TableStyle, cx: &mut TestAppContext) -> Pixels {
        const CHILD_SELECTOR: &str = "zsql-ui-table-test-column-border-child";

        struct Probe {
            table_state: Entity<TableState>,
            style: TableStyle,
        }
        impl Render for Probe {
            fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let table = Table::new("probe-table", &self.table_state)
                    .style(self.style)
                    .columns(vec![TableColumn::new(px(COLUMN_WIDTH), div().child("c"))])
                    .row_count(3)
                    .rows(|_this: &mut Self, range, _window, _cx| {
                        range
                            .map(|ix| {
                                let child = div()
                                    .w_full()
                                    .h(px(4.0))
                                    .debug_selector(|| CHILD_SELECTOR.to_string());
                                TableRow::new(vec![if ix == 0 {
                                    child.into_any_element()
                                } else {
                                    div().w_full().h(px(4.0)).into_any_element()
                                }])
                            })
                            .collect::<Vec<_>>()
                    })
                    .render(cx);
                div()
                    .w(px(PANE_SIZE))
                    .h(px(PANE_SIZE))
                    .flex()
                    .flex_col()
                    .child(table)
            }
        }

        let (_probe, vcx) = cx.add_window_view(|_window, cx| Probe {
            table_state: cx.new(TableState::new),
            style,
        });
        vcx.run_until_parked();
        vcx.debug_bounds(CHILD_SELECTOR)
            .expect("the tagged full-width child must be painted")
            .size
            .width
    }

    #[gpui::test]
    fn disabling_only_the_column_border_widens_a_full_width_child_of_the_body_cell(
        cx: &mut TestAppContext,
    ) {
        // `TableBorders`' edges are documented as independent, so flipping
        // `column` alone (leaving `row`/`outer` at their defaults) must
        // still be visible in painted geometry -- not just the all-off
        // combination `a_borderless_table_paints_a_taller_body_cell_than_a_bordered_one`
        // already covers.
        let width_bordered = probe_column_border_child_width(TableStyle::default(), cx);
        let column_borderless = TableStyle {
            borders: TableBorders {
                column: false,
                ..TableBorders::default()
            },
            ..TableStyle::default()
        };
        let width_borderless = probe_column_border_child_width(column_borderless, cx);

        assert!(
            width_borderless > width_bordered,
            "disabling TableBorders.column alone must widen a full-width child of the body \
             cell (the right hairline eats into the cell's content box, exactly as the row \
             hairline does for height), rather than TableStyle::borders.column being silently \
             ignored: bordered width was {width_bordered:?}, borderless was {width_borderless:?}"
        );
    }

    /// Mounts a probe table with a [`Gutter::Custom`] pane styled with
    /// `style` and returns the painted width of a full-width child inside
    /// the gutter's own header cell.
    ///
    /// The gutter pane's own painted bounds are its declared width
    /// regardless of whether its own right (outer) border is drawn, for the
    /// same box-model reason [`probe_column_border_child_width`] documents.
    /// The gutter header shell has no explicit width of its own, so it
    /// stretches to the pane's content box and a full-width child of it
    /// exposes whether `TableBorders.outer` is read at all.
    fn probe_outer_border_child_width(style: TableStyle, cx: &mut TestAppContext) -> Pixels {
        const CHILD_SELECTOR: &str = "zsql-ui-table-test-outer-border-child";

        struct Probe {
            table_state: Entity<TableState>,
            style: TableStyle,
        }
        impl Render for Probe {
            fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let header_child = div()
                    .w_full()
                    .h(px(4.0))
                    .debug_selector(|| CHILD_SELECTOR.to_string());
                let gutter = Gutter::Custom {
                    width: px(48.0),
                    header: header_child.into_any_element(),
                    render: Box::new(|_this: &mut Self, range, _window, _cx| {
                        range
                            .map(|_ix| gpui::Empty.into_any_element())
                            .collect::<Vec<AnyElement>>()
                    }),
                };
                let table = Table::new("probe-table", &self.table_state)
                    .style(self.style)
                    .columns(vec![TableColumn::new(px(COLUMN_WIDTH), div().child("c"))])
                    .row_count(3)
                    .gutter(gutter)
                    .rows(|_this: &mut Self, range, _window, _cx| {
                        range.map(|_ix| TableRow::new(Vec::new())).collect()
                    })
                    .render(cx);
                div()
                    .w(px(PANE_SIZE))
                    .h(px(PANE_SIZE))
                    .flex()
                    .flex_col()
                    .child(table)
            }
        }

        let (_probe, vcx) = cx.add_window_view(|_window, cx| Probe {
            table_state: cx.new(TableState::new),
            style,
        });
        vcx.run_until_parked();
        vcx.debug_bounds(CHILD_SELECTOR)
            .expect("the tagged full-width gutter header child must be painted")
            .size
            .width
    }

    #[gpui::test]
    fn disabling_only_the_outer_border_widens_a_full_width_child_of_the_gutter_header(
        cx: &mut TestAppContext,
    ) {
        // Mirrors `disabling_only_the_column_border_widens_a_full_width_child_of_the_body_cell`
        // for the gutter/data-pane divider: flipping `outer` alone must
        // move painted geometry, not just the all-off combination.
        let width_bordered = probe_outer_border_child_width(TableStyle::default(), cx);
        let outer_borderless = TableStyle {
            borders: TableBorders {
                outer: false,
                ..TableBorders::default()
            },
            ..TableStyle::default()
        };
        let width_borderless = probe_outer_border_child_width(outer_borderless, cx);

        assert!(
            width_borderless > width_bordered,
            "disabling TableBorders.outer alone must widen a full-width child of the gutter's \
             own header cell (the divider hairline eats into the gutter pane's content box), \
             rather than TableStyle::borders.outer being silently ignored: bordered width was \
             {width_bordered:?}, borderless was {width_borderless:?}"
        );
    }

    #[gpui::test]
    fn a_table_with_no_rows_callback_does_not_panic(cx: &mut TestAppContext) {
        let (_harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(50, 3, cx).without_rows_callback());
        vcx.run_until_parked();
    }

    // -- cell selection ------------------------------------------------

    fn click_at(vcx: &mut gpui::VisualTestContext, position: gpui::Point<Pixels>) {
        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn a_table_that_never_opted_into_selectable_ignores_clicks_on_its_cells(
        cx: &mut TestAppContext,
    ) {
        // A table that never calls `Table::selectable` must render its body
        // cells as inert content: no per-cell click handler and no
        // highlight, so a caller that only wants a read-only grid (e.g. a
        // schema browser) is unaffected by another caller's use of cell
        // selection.
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(50, 3, cx).without_selectable());
        vcx.run_until_parked();

        let table_state = harness.read_with(vcx, |h, _app| h.table_state.clone());
        let cell_bounds = vcx
            .debug_bounds(body_first_cell_debug_selector(&table_state))
            .expect("the top-of-viewport body cell must still be painted even when unselectable");
        click_at(
            vcx,
            gpui::point(
                cell_bounds.origin.x + px(5.0),
                cell_bounds.origin.y + px(5.0),
            ),
        );

        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            None,
            "clicking a cell in a non-selectable table must not select it"
        );
    }

    #[gpui::test]
    fn clicking_a_data_cell_selects_it_in_table_state(cx: &mut TestAppContext) {
        let (harness, vcx) = cx.add_window_view(|_window, cx| Harness::new(50, 3, cx));
        vcx.run_until_parked();

        let table_state = harness.read_with(vcx, |h, _app| h.table_state.clone());
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            None,
            "nothing must be selected before any click"
        );

        let cell_bounds = vcx
            .debug_bounds(body_first_cell_debug_selector(&table_state))
            .expect("the top-of-viewport body cell must be painted");
        // The cell's own layout box is wider than the pane (this harness's
        // columns overflow it by design, for the scrollbar tests above), so
        // its *center* can fall outside the clipped visible viewport; a
        // point near its top-left corner stays inside both.
        click_at(
            vcx,
            gpui::point(
                cell_bounds.origin.x + px(5.0),
                cell_bounds.origin.y + px(5.0),
            ),
        );

        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            Some((0, 0)),
            "clicking the top-of-viewport body cell must select row 0, column 0"
        );
    }

    #[gpui::test]
    fn gutter_and_header_cells_remain_unclickable_in_a_selectable_table(cx: &mut TestAppContext) {
        // AC #7: row-number gutter and header cells stay unclickable for
        // selection even when the table IS selectable (unlike body cells,
        // which do accept clicks to select when the table is selectable).
        let (harness, vcx) = cx.add_window_view(|_window, cx| Harness::new(50, 3, cx));
        vcx.run_until_parked();

        let table_state = harness.read_with(vcx, |h, _app| h.table_state.clone());
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            None,
            "nothing must be selected before any click"
        );

        // Click on the gutter cell (row-number gutter, first row).
        let gutter_bounds = vcx
            .debug_bounds(gutter_first_cell_debug_selector(&table_state))
            .expect("the row-number gutter's first cell must be painted");
        click_at(
            vcx,
            gpui::point(
                gutter_bounds.origin.x + px(5.0),
                gutter_bounds.origin.y + px(5.0),
            ),
        );

        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            None,
            "clicking the gutter cell must not select it"
        );

        // Click on the header cell.
        let header_bounds = vcx
            .debug_bounds(header_first_cell_debug_selector(&table_state))
            .expect("the header row's first cell must be painted");
        click_at(
            vcx,
            gpui::point(
                header_bounds.origin.x + px(5.0),
                header_bounds.origin.y + px(5.0),
            ),
        );

        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            None,
            "clicking the header cell must not select it"
        );
    }

    #[gpui::test]
    fn clicking_a_second_cell_moves_the_selection_away_from_the_first(cx: &mut TestAppContext) {
        // Narrow enough that both columns fit inside `PANE_SIZE` (unlike
        // `COLUMN_WIDTH`, used elsewhere to force horizontal overflow), so a
        // click at either cell's own bounds lands inside the pane's clipped
        // visible viewport rather than being clipped away.
        const CELL_WIDTH: f32 = 80.0;
        const CELL_00_SELECTOR: &str = "zsql-ui-table-test-select-cell-0-0";
        const CELL_11_SELECTOR: &str = "zsql-ui-table-test-select-cell-1-1";

        struct Probe {
            table_state: Entity<TableState>,
        }
        impl Render for Probe {
            fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let columns = vec![
                    TableColumn::new(px(CELL_WIDTH), div().child("c0")),
                    TableColumn::new(px(CELL_WIDTH), div().child("c1")),
                ];
                let table = Table::new("probe-table", &self.table_state)
                    .columns(columns)
                    .row_count(2)
                    .selectable()
                    .rows(|_this: &mut Self, range, _window, _cx| {
                        range
                            .map(|row| {
                                let cells: Vec<AnyElement> = (0..2)
                                    .map(|col| {
                                        let content =
                                            div().size_full().child(format!("r{row}c{col}"));
                                        if row == 0 && col == 0 {
                                            content
                                                .debug_selector(|| CELL_00_SELECTOR.to_string())
                                                .into_any_element()
                                        } else if row == 1 && col == 1 {
                                            content
                                                .debug_selector(|| CELL_11_SELECTOR.to_string())
                                                .into_any_element()
                                        } else {
                                            content.into_any_element()
                                        }
                                    })
                                    .collect();
                                TableRow::new(cells)
                            })
                            .collect()
                    })
                    .render(cx);
                div()
                    .w(px(PANE_SIZE))
                    .h(px(PANE_SIZE))
                    .flex()
                    .flex_col()
                    .child(table)
            }
        }

        let (probe, vcx) = cx.add_window_view(|_window, cx| Probe {
            table_state: cx.new(TableState::new),
        });
        vcx.run_until_parked();
        let table_state = probe.read_with(vcx, |p, _app| p.table_state.clone());

        let cell_00 = vcx
            .debug_bounds(CELL_00_SELECTOR)
            .expect("cell (0,0)'s content must be painted");
        click_at(vcx, cell_00.center());
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            Some((0, 0)),
            "clicking cell (0,0) must select it"
        );

        let cell_11 = vcx
            .debug_bounds(CELL_11_SELECTOR)
            .expect("cell (1,1)'s content must be painted");
        click_at(vcx, cell_11.center());
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            Some((1, 1)),
            "clicking a different cell must move the selection to it, away from (0, 0), so \
             exactly one cell is ever the selected cell at once"
        );
    }

    /// A focused cell's selection ring borders every edge, unlike an
    /// unfocused cell's own right-only column hairline, so a full-size
    /// child's painted content box shrinks -- the same geometry-based proof
    /// [`probe_column_border_child_width`] uses, since a painted color
    /// cannot be asserted from bounds alone.
    fn probe_focus_ring_child_size(select_it: bool, cx: &mut TestAppContext) -> (Pixels, Pixels) {
        const CHILD_SELECTOR: &str = "zsql-ui-table-test-focus-ring-child";

        struct Probe {
            table_state: Entity<TableState>,
        }
        impl Render for Probe {
            fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let table = Table::new("probe-table", &self.table_state)
                    .columns(vec![TableColumn::new(px(COLUMN_WIDTH), div().child("c"))])
                    .row_count(1)
                    .selectable()
                    .rows(|_this: &mut Self, range, _window, _cx| {
                        range
                            .map(|_ix| {
                                let child = div()
                                    .size_full()
                                    .debug_selector(|| CHILD_SELECTOR.to_string());
                                TableRow::new(vec![child.into_any_element()])
                            })
                            .collect()
                    })
                    .render(cx);
                div()
                    .w(px(PANE_SIZE))
                    .h(px(PANE_SIZE))
                    .flex()
                    .flex_col()
                    .child(table)
            }
        }

        let (_probe, vcx) = cx.add_window_view(|_window, cx| Probe {
            table_state: cx.new(|cx| {
                let mut state = TableState::new(cx);
                if select_it {
                    state.set_focused_cell(0, 0);
                }
                state
            }),
        });
        vcx.run_until_parked();
        let bounds = vcx
            .debug_bounds(CHILD_SELECTOR)
            .expect("the tagged child must be painted");
        (bounds.size.width, bounds.size.height)
    }

    #[gpui::test]
    fn a_focused_cell_paints_a_smaller_content_box_than_an_unfocused_cell(cx: &mut TestAppContext) {
        let (unfocused_width, unfocused_height) = probe_focus_ring_child_size(false, cx);
        let (focused_width, focused_height) = probe_focus_ring_child_size(true, cx);

        assert!(
            focused_width < unfocused_width,
            "a focused cell's ring must add a left border an unfocused cell lacks, shrinking a \
             full-width child: unfocused {unfocused_width:?}, focused {focused_width:?}"
        );
        assert!(
            focused_height < unfocused_height,
            "a focused cell's ring must add top/bottom borders an unfocused cell lacks, \
             shrinking a full-height child: unfocused {unfocused_height:?}, focused \
             {focused_height:?}"
        );
    }

    #[gpui::test]
    fn a_stale_selection_outside_a_smaller_table_highlights_nothing_and_does_not_panic(
        cx: &mut TestAppContext,
    ) {
        // A selection recorded against a larger result must not crash the
        // render path once the table shrinks underneath it -- `TableState`
        // only compares indices, so an out-of-range selection simply never
        // matches any painted cell rather than panicking or indexing past
        // the smaller table's own rows/columns.
        let (_harness, vcx) = cx.add_window_view(|_window, cx| {
            let harness = Harness::new(2, 1, cx);
            harness
                .table_state
                .update(cx, |state, _cx| state.set_focused_cell(50, 3));
            harness
        });
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn an_empty_table_renders_without_panicking_and_selects_nothing_on_click(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) = cx.add_window_view(|_window, cx| Harness::new(0, 0, cx));
        vcx.run_until_parked();

        // No column exists to click through to a data cell, so a click over
        // the empty pane must not select anything.
        click_at(vcx, gpui::point(px(10.0), px(10.0)));

        let table_state = harness.read_with(vcx, |h, _app| h.table_state.clone());
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            None,
            "an empty table has no cell to select"
        );
    }
}
