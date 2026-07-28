//! [`Table`]: the per-render builder that assembles a two-pane virtualized
//! grid (a pinned gutter plus a horizontally scrolling data pane) by
//! composing [`crate::scrollable`]'s scroll/drag/wheel machinery rather than
//! reimplementing it.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Context, Div, ElementId, Entity, FocusHandle, MouseButton, MouseDownEvent, Pixels, Render,
    SharedString, UniformList, UniformListScrollHandle, Window, div, prelude::*, rgb, rgba,
    uniform_list,
};

use crate::scrollable::restrict_wheel_to_own_axis;
use crate::scrollable::{ScrollableState, ScrollbarStyle, WithScrollbars};
use crate::theme::ActiveTheme;

use super::column::TableColumn;
use super::gutter::{
    Gutter, gutter_cell_shell, gutter_header_shell, row_number_cell_shell, row_number_header_shell,
};
use super::layout::{
    ColumnGeometry, ColumnLayout, column_geometry, read_scroll_handles, sync_scroll_axes,
};
use super::measure;
use super::resize::{self, ColumnResizeConfig};
use super::row::TableRow;
use super::state::TableState;
use super::style::TableStyle;

type RowRenderer<V> =
    Box<dyn Fn(&mut V, Range<usize>, &mut Window, &mut Context<V>) -> Vec<TableRow>>;

/// A caller's [`Table::on_cell_double_click`] callback, given the
/// double-clicked cell's `(row, col)`.
type CellDoubleClickHandler<V> = Rc<dyn Fn(&mut V, usize, usize, &mut Window, &mut Context<V>)>;
/// A caller's [`Table::on_cell_right_click`] callback, given the
/// right-clicked cell's `(row, col)` and the triggering event.
type CellRightClickHandler<V> =
    Rc<dyn Fn(&mut V, usize, usize, &MouseDownEvent, &mut Window, &mut Context<V>)>;
/// A [`CellDoubleClickHandler`]/[`CellRightClickHandler`] once wrapped (via
/// `Context::listener`) into a plain mouse-down handler that reads its
/// target cell off [`TableState`] rather than closing over one -- see
/// [`Table::render`], where one instance of each is built and cloned onto
/// every selectable cell.
type CellClickListener = Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

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
    on_cell_double_click: Option<CellDoubleClickHandler<V>>,
    on_cell_right_click: Option<CellRightClickHandler<V>>,
    column_resize: Option<ColumnResizeConfig<V>>,
}

/// How to size the table's vertical extent in its parent. Defaults to [`TableSizing::Fill`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableSizing {
    /// Fit the table's height to its parent, showing scrollbars if the table's rows overflow.
    Fill,
    /// Let the table's height grow to fit all its rows, showing no vertical scrollbar.
    Fit,
}

/// Wrap `handler` (if given) into a [`CellClickListener`]: reading its
/// target cell off `state` (already updated by the same click's own
/// selection) rather than closing over one, so this single built-once
/// listener can be cloned onto every selectable cell instead of
/// monomorphizing a fresh closure per cell.
fn build_double_click_listener<V: Render>(
    handler: Option<CellDoubleClickHandler<V>>,
    state: &Entity<TableState>,
    cx: &mut Context<V>,
) -> Option<CellClickListener> {
    handler.map(|f| {
        let state = state.clone();
        let wrapped = cx.listener(
            move |view: &mut V,
                  _event: &MouseDownEvent,
                  window: &mut Window,
                  cx: &mut Context<V>| {
                if let Some((row, col)) = state.read(cx).focused_cell() {
                    f(view, row, col, window, cx);
                }
            },
        );
        Rc::new(wrapped) as CellClickListener
    })
}

/// [`build_double_click_listener`]'s right-click counterpart: `handler`
/// additionally receives the triggering [`MouseDownEvent`] (e.g. for a
/// context menu's anchor position).
fn build_right_click_listener<V: Render>(
    handler: Option<CellRightClickHandler<V>>,
    state: &Entity<TableState>,
    cx: &mut Context<V>,
) -> Option<CellClickListener> {
    handler.map(|f| {
        let state = state.clone();
        let wrapped = cx.listener(
            move |view: &mut V,
                  event: &MouseDownEvent,
                  window: &mut Window,
                  cx: &mut Context<V>| {
                if let Some((row, col)) = state.read(cx).focused_cell() {
                    f(view, row, col, event, window, cx);
                }
            },
        );
        Rc::new(wrapped) as CellClickListener
    })
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
            on_cell_double_click: None,
            on_cell_right_click: None,
            column_resize: None,
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

    /// Call `f` with the just-selected cell's `(row, col)` whenever the
    /// second mouse-down of a double click lands on a data cell. Has no
    /// effect unless [`Table::selectable`] is also called. `f` runs after
    /// that click has already updated `TableState`'s focused cell, so it
    /// always sees the cell that was actually double-clicked.
    #[must_use]
    pub fn on_cell_double_click(
        mut self,
        f: impl Fn(&mut V, usize, usize, &mut Window, &mut Context<V>) + 'static,
    ) -> Self {
        self.on_cell_double_click = Some(Rc::new(f));
        self
    }

    /// Call `f` with the just-selected cell's `(row, col)` and the
    /// triggering event whenever a data cell is right-clicked. Has no effect
    /// unless [`Table::selectable`] is also called. A right-click selects
    /// the cell first (mirroring a left click), so `f` always sees the cell
    /// that was actually right-clicked.
    #[must_use]
    pub fn on_cell_right_click(
        mut self,
        f: impl Fn(&mut V, usize, usize, &MouseDownEvent, &mut Window, &mut Context<V>) + 'static,
    ) -> Self {
        self.on_cell_right_click = Some(Rc::new(f));
        self
    }

    /// Opt this table into a draggable resize handle on every data column's
    /// header cell trailing border (never the pinned gutter). `min_width` is
    /// the floor a column is never dragged narrower than; `on_resize` is
    /// called with the resized column's index and its new width on every
    /// pointer move while a drag is in progress, so the caller can store the
    /// live width (e.g. into its own per-column width cache) and notify. Off
    /// by default, so an existing table opts in explicitly rather than
    /// gaining resize handles unannounced.
    #[must_use]
    pub fn resizable_columns(
        mut self,
        min_width: Pixels,
        on_resize: impl Fn(&mut V, usize, Pixels, &mut Window, &mut Context<V>) + 'static,
    ) -> Self {
        self.column_resize = Some(ColumnResizeConfig::new(min_width, on_resize));
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
            on_cell_double_click,
            on_cell_right_click,
            column_resize,
        } = self;
        let rows = rows.unwrap_or_else(|| -> RowRenderer<V> {
            Box::new(|_v, range, _window, _cx| range.map(|_| TableRow::new(Vec::new())).collect())
        });

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
                    .bg(rgba(style.selection_wash))
                    .border_1()
                    .border_color(rgba(style.selection_ring));
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

/// A data cell's left mouse-down handler: selects `(row_index, cell_index)`
/// in `state`, focuses `focus_handle` if given (so a caller's own key
/// binding, e.g. a clipboard copy, captures the same click that just
/// selected the cell), then -- on the second mouse-down of a double click --
/// runs `double_click_listener`, set via [`Table::on_cell_double_click`].
fn select_cell_on_click(
    state: &Entity<TableState>,
    row_index: usize,
    cell_index: usize,
    focus_handle: Option<FocusHandle>,
    double_click_listener: Option<CellClickListener>,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    let state = state.clone();
    move |event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
        let _span =
            tracing::trace_span!("zsql_ui::table::select_cell", row_index, cell_index).entered();
        state.update(cx, |table_state, cx| {
            table_state.set_focused_cell(row_index, cell_index);
            cx.notify();
        });
        if let Some(handle) = &focus_handle {
            window.focus(handle);
        }
        if event.click_count >= 2
            && let Some(listener) = &double_click_listener
        {
            listener(event, window, cx);
        }
    }
}

/// A data cell's right mouse-down handler: selects `(row_index, cell_index)`
/// in `state` (mirroring a left click) then runs `listener`, set via
/// [`Table::on_cell_right_click`].
fn select_cell_on_right_click(
    state: &Entity<TableState>,
    row_index: usize,
    cell_index: usize,
    listener: CellClickListener,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    let state = state.clone();
    move |event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
        let _span = tracing::trace_span!("zsql_ui::table::right_click_cell", row_index, cell_index)
            .entered();
        state.update(cx, |table_state, cx| {
            table_state.set_focused_cell(row_index, cell_index);
            cx.notify();
        });
        listener(event, window, cx);
    }
}

/// A data-pane cell's chrome, shared by header and body. A fixed column
/// (`grow == false`) is pinned to exactly `width`; a growable one treats
/// `width` as a floor (`flex_basis`/`min_w`) and expands to take a share of
/// any leftover row width. `min_w(width)` also pins a growable cell's flex
/// minimum to `width` rather than its content's min-content size, so a long
/// value truncates within the cell instead of forcing the whole row wider.
pub(super) fn cell_shell(width: Pixels, grow: bool, style: &TableStyle) -> Div {
    let mut cell = div()
        .flex()
        .items_center()
        .h_full()
        .px(style.cell_padding_x)
        .truncate();
    cell = if grow {
        cell.flex_grow().flex_basis(width).min_w(width)
    } else {
        cell.flex_shrink_0().w(width)
    };
    if style.borders.column {
        cell = cell.border_r_1().border_color(rgb(style.row_border));
    }
    cell
}

/// A header cell's sizing and border chrome, split out from
/// [`cell_shell`] and left non-clipping (no `.truncate()`) so a resize
/// handle positioned outside its trailing border is not masked out of
/// painting or hit-testing by an ancestor's `overflow_hidden`. Sizes
/// identically to [`cell_shell`]; pair with [`cell_content`] for the
/// padded, truncating content [`cell_shell`] would otherwise apply itself.
pub(super) fn cell_frame(width: Pixels, grow: bool, style: &TableStyle) -> Div {
    let mut frame = div().h_full().relative();
    frame = if grow {
        frame.flex_grow().flex_basis(width).min_w(width)
    } else {
        frame.flex_shrink_0().w(width)
    };
    if style.borders.column {
        frame = frame.border_r_1().border_color(rgb(style.row_border));
    }
    frame
}

/// The padded, truncating content wrapper a [`cell_frame`] holds full-size,
/// carrying the clipping behavior [`cell_shell`] otherwise applies to its
/// own sizing/border div.
pub(super) fn cell_content(style: &TableStyle) -> Div {
    div()
        .flex()
        .items_center()
        .h_full()
        .w_full()
        .px(style.cell_padding_x)
        .truncate()
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
pub(super) fn tag_header_cell(cell: Div, index: usize, state: &Entity<TableState>) -> Div {
    if index == 0 {
        let selector = header_first_cell_id(state).to_string();
        cell.debug_selector(move || selector.clone())
    } else {
        cell
    }
}

/// A no-op outside test builds.
#[cfg(not(any(test, feature = "test-support")))]
pub(super) fn tag_header_cell(cell: Div, _index: usize, _state: &Entity<TableState>) -> Div {
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
mod tests;
