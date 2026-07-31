//! [`Table`]: the per-render builder that assembles a two-pane virtualized
//! grid (a pinned gutter plus a horizontally scrolling data pane) by
//! composing [`crate::scrollable`]'s scroll/drag/wheel machinery rather than
//! reimplementing it.

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Context, Div, ElementId, Entity, FocusHandle, MouseDownEvent, Pixels, Render, Window, div,
    prelude::*, rgb,
};

use crate::scrollable::ScrollbarStyle;

use super::column::TableColumn;
use super::gutter::Gutter;
use super::resize::ColumnResizeConfig;
use super::row::TableRow;
use super::state::TableState;
use super::style::TableStyle;

pub(super) type RowRenderer<V> =
    Box<dyn Fn(&mut V, Range<usize>, &mut Window, &mut Context<V>) -> Vec<TableRow>>;

/// A caller's [`Table::on_cell_double_click`] callback, given the
/// double-clicked cell's `(row, col)`.
type CellDoubleClickHandler<V> = Rc<dyn Fn(&mut V, usize, usize, &mut Window, &mut Context<V>)>;
/// A caller's [`Table::on_cell_right_click`] callback, given the
/// right-clicked cell's `(row, col)` and the triggering event.
type CellRightClickHandler<V> =
    Rc<dyn Fn(&mut V, usize, usize, &MouseDownEvent, &mut Window, &mut Context<V>)>;
/// A caller's [`Table::on_cell_click`] callback, given the clicked cell's
/// `(row, col)`.
type CellSingleClickHandler<V> = Rc<dyn Fn(&mut V, usize, usize, &mut Window, &mut Context<V>)>;
/// A [`CellDoubleClickHandler`]/[`CellRightClickHandler`] once wrapped (via
/// `Context::listener`) into a plain mouse-down handler that reads its
/// target cell off [`TableState`] rather than closing over one -- see
/// [`Table::render`], where one instance of each is built and cloned onto
/// every selectable cell.
pub(super) type CellClickListener = Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

/// A two-pane virtualized table, built fresh every render. Owns no row or
/// column data of its own -- [`TableState`] (frame-persistent, held by the
/// caller) is the only piece of this abstraction that survives between
/// renders.
pub struct Table<V: Render> {
    pub(super) id: ElementId,
    pub(super) state: Entity<TableState>,
    pub(super) style: TableStyle,
    pub(super) scrollbar_style: ScrollbarStyle,
    pub(super) columns: Vec<TableColumn>,
    pub(super) row_count: usize,
    pub(super) gutter: Gutter<V>,
    pub(super) rows: Option<RowRenderer<V>>,
    pub(super) vertical_sizing: TableSizing,
    pub(super) focus_on_click: Option<FocusHandle>,
    pub(super) selectable: bool,
    pub(super) on_cell_click: Option<CellSingleClickHandler<V>>,
    pub(super) on_cell_double_click: Option<CellDoubleClickHandler<V>>,
    pub(super) on_cell_right_click: Option<CellRightClickHandler<V>>,
    pub(super) column_resize: Option<ColumnResizeConfig<V>>,
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
pub(super) fn build_single_click_listener<V: Render>(
    handler: Option<CellSingleClickHandler<V>>,
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

/// Wrap `handler` (if given) into a [`CellClickListener`]: reading its
/// target cell off `state` (already updated by the same click's own
/// selection) rather than closing over one, so this single built-once
/// listener can be cloned onto every selectable cell instead of
/// monomorphizing a fresh closure per cell.
pub(super) fn build_double_click_listener<V: Render>(
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
pub(super) fn build_right_click_listener<V: Render>(
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
            on_cell_click: None,
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

    /// Call `f` with the just-selected cell's `(row, col)` whenever a data
    /// cell is clicked. Has no effect unless [`Table::selectable`] is also
    /// called. `f` runs after that click has already updated `TableState`'s
    /// focused cell, so it always sees the cell that was actually clicked.
    #[must_use]
    pub fn on_cell_click(
        mut self,
        f: impl Fn(&mut V, usize, usize, &mut Window, &mut Context<V>) + 'static,
    ) -> Self {
        self.on_cell_click = Some(Rc::new(f));
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
}

/// A data cell's left mouse-down handler: selects `(row_index, cell_index)`
/// in `state`, focuses `focus_handle` if given (so a caller's own key
/// binding, e.g. a clipboard copy, captures the same click that just
/// selected the cell), then -- on the second mouse-down of a double click --
/// runs `double_click_listener`, set via [`Table::on_cell_double_click`].
pub(super) fn select_cell_on_click(
    state: &Entity<TableState>,
    row_index: usize,
    cell_index: usize,
    focus_handle: Option<FocusHandle>,
    single_click_listener: Option<CellClickListener>,
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
        tracing::debug!(
            click_count = event.click_count,
            "selected cell ({row_index}, {cell_index}) on click"
        );
        if event.click_count == 1
            && let Some(listener) = &single_click_listener
        {
            listener(event, window, cx);
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
pub(super) fn select_cell_on_right_click(
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
