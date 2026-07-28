//! Opt-in draggable column-width resize handles for [`super::Table`],
//! enabled via [`super::Table::resizable_columns`]. Mirrors the drag
//! mechanics already used for a value-panel or sidebar divider: a mouse-down
//! on the handle captures the pointer's x position and the column's current
//! width, every mouse-move while the drag is in progress applies and clamps
//! the delta, and mouse-up clears the drag.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, CursorStyle, Div, Entity, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Render, SharedString, Stateful, Window, div, prelude::*,
    px, rgb, rgba,
};

use super::builder::{cell_content, cell_frame, cell_shell};
use super::column::TableColumn;
use super::debug::tag_header_cell;
use super::state::TableState;
use super::style::{COLUMN_RESIZE_HANDLE_WIDTH, TableStyle};

/// A column resize drag in progress: which column it targets, the pointer's
/// x position when the drag began, and that column's width at that moment.
#[derive(Debug, Clone, Copy)]
pub(super) struct ColumnResizeDrag {
    pub(super) column: usize,
    pub(super) origin_x: Pixels,
    pub(super) start_width: Pixels,
}

/// A [`super::Table::resizable_columns`] caller's own width-storage
/// callback, run with the resized column's index and new width on every
/// pointer move while a drag is in progress.
type OnColumnResize<V> = Rc<dyn Fn(&mut V, usize, Pixels, &mut Window, &mut Context<V>)>;

/// Configuration behind [`super::Table::resizable_columns`]: the floor a
/// column is never dragged narrower than, plus the caller's own callback for
/// storing the live width as the pointer moves.
pub(super) struct ColumnResizeConfig<V: Render> {
    min_width: Pixels,
    on_resize: OnColumnResize<V>,
}

impl<V: Render> ColumnResizeConfig<V> {
    pub(super) fn new(
        min_width: Pixels,
        on_resize: impl Fn(&mut V, usize, Pixels, &mut Window, &mut Context<V>) + 'static,
    ) -> Self {
        Self {
            min_width,
            on_resize: Rc::new(on_resize),
        }
    }
}

/// The width a column drags to once its handle has moved from `origin_x` to
/// `current_x`, floored at `min_width` so a drag can never shrink a column
/// to zero or a negative width.
pub(super) fn resized_width(
    start_width: Pixels,
    origin_x: Pixels,
    current_x: Pixels,
    min_width: Pixels,
) -> Pixels {
    (start_width + (current_x - origin_x)).max(min_width)
}

/// The data pane's sticky column-header row, with a draggable resize handle
/// on every column's trailing border when `resizable` is set. Identical to a
/// plain header row otherwise.
pub(super) fn build_header_row(
    columns: Vec<TableColumn>,
    style: &TableStyle,
    content_extent: Pixels,
    fill_width: bool,
    resizable: bool,
    state: &Entity<TableState>,
) -> Div {
    let mut row = div()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .min_w(content_extent)
        .h(style.header_height)
        .bg(rgb(style.header_bg));
    if fill_width {
        row = row.w_full();
    }
    if style.borders.row {
        row = row.border_b_1().border_color(rgb(style.header_border));
    }
    for (index, column) in columns.into_iter().enumerate() {
        let width = column.width;
        let grow = column.grow;
        let cell = if resizable {
            build_resizable_cell(width, grow, style, column.header, index, state)
        } else {
            cell_shell(width, grow, style).child(column.header)
        };
        row = row.child(tag_header_cell(cell, index, state));
    }
    row
}

/// A header cell whose trailing border carries a draggable resize handle:
/// a non-clipping [`cell_frame`] holding the padded, truncating
/// [`cell_content`] plus the handle as siblings, rather than nesting the
/// handle inside the truncating content itself -- doing so would clip the
/// half of the handle that straddles past the cell's own border out of both
/// painting and hit-testing.
fn build_resizable_cell(
    width: Pixels,
    grow: bool,
    style: &TableStyle,
    header: AnyElement,
    index: usize,
    state: &Entity<TableState>,
) -> Div {
    let content = cell_content(style).child(header);
    let handle = build_resize_handle(index, width, style, state);
    cell_frame(width, grow, style).child(content).child(handle)
}

/// A hoverable, draggable hit target straddling a header cell's trailing
/// border, with the horizontal-resize cursor, wired to start a resize drag
/// for `index` on left mouse-down.
fn build_resize_handle(
    index: usize,
    width: Pixels,
    style: &TableStyle,
    state: &Entity<TableState>,
) -> Stateful<Div> {
    let handle = div()
        .id(resize_handle_id(index, state))
        .absolute()
        .top_0()
        .bottom_0()
        .right(px(-(f32::from(COLUMN_RESIZE_HANDLE_WIDTH) / 2.0)))
        .w(COLUMN_RESIZE_HANDLE_WIDTH)
        .cursor(CursorStyle::ResizeLeftRight)
        .hover(|el| el.bg(rgba(style.selection_ring)))
        .on_mouse_down(
            MouseButton::Left,
            begin_resize_listener(index, width, state),
        );
    tag_resize_handle(handle, index, state)
}

/// A resize handle's left mouse-down handler: captures `column`'s width at
/// this moment and the pointer's x position as the drag's origin. Never
/// touches the table's focused/selected cell or keyboard focus, unlike a
/// data cell's own click handling.
fn begin_resize_listener(
    column: usize,
    start_width: Pixels,
    state: &Entity<TableState>,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    let state = state.clone();
    move |event: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
        let _span = tracing::trace_span!("zsql_ui::table::begin_column_resize", column).entered();
        state.update(cx, |table_state, cx| {
            table_state.begin_column_resize(column, event.position.x, start_width);
            cx.notify();
        });
    }
}

/// [`super::Table::render`]'s root-level mouse-move handler while
/// `config` is set: applies and clamps the drag delta, then hands the
/// resized column's index and new width to the caller's own
/// [`super::Table::resizable_columns`] callback. A no-op whenever no resize
/// drag is currently in progress.
pub(super) fn move_listener<V: Render>(
    config: &ColumnResizeConfig<V>,
    state: &Entity<TableState>,
    cx: &mut Context<V>,
) -> impl Fn(&MouseMoveEvent, &mut Window, &mut App) + 'static {
    let min_width = config.min_width;
    let on_resize = config.on_resize.clone();
    let state = state.clone();
    cx.listener(
        move |view: &mut V, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<V>| {
            let Some(drag) = state.read(cx).column_resize() else {
                return;
            };
            let width = resized_width(drag.start_width, drag.origin_x, event.position.x, min_width);
            on_resize(view, drag.column, width, window, cx);
        },
    )
}

/// Wires `root` up to drive a column resize drag while `config` is set:
/// every pointer move anywhere within the table applies and clamps the
/// drag, and release (inside or outside the table) ends it. Returns `root`
/// unchanged for a table that never opted into
/// [`super::Table::resizable_columns`].
pub(super) fn wire_root<V: Render>(
    root: Div,
    config: Option<&ColumnResizeConfig<V>>,
    state: &Entity<TableState>,
    cx: &mut Context<V>,
) -> Div {
    let Some(config) = config else {
        return root;
    };
    root.on_mouse_move(move_listener(config, state, cx))
        .on_mouse_up(MouseButton::Left, end_listener(state))
        .on_mouse_up_out(MouseButton::Left, end_listener(state))
}

/// [`super::Table::render`]'s root-level mouse-up/mouse-up-out handler
/// while a resize is configured: clears any in-progress resize drag.
pub(super) fn end_listener(
    state: &Entity<TableState>,
) -> impl Fn(&MouseUpEvent, &mut Window, &mut App) + 'static {
    let state = state.clone();
    move |_event: &MouseUpEvent, _window: &mut Window, cx: &mut App| {
        state.update(cx, |table_state, cx| {
            if table_state.end_column_resize() {
                cx.notify();
            }
        });
    }
}

/// A resize handle's stable element id, distinct per column and per table
/// instance (via `state`'s entity id) so two tables on screen at once never
/// collide.
fn resize_handle_id(index: usize, state: &Entity<TableState>) -> SharedString {
    SharedString::from(format!(
        "zsql-ui-table-resize-handle-{index}-{}",
        state.entity_id()
    ))
}

/// Tags `handle` with a lookup key for `VisualTestContext::debug_bounds`, so
/// a render test can find and drag a specific column's resize handle.
#[cfg(any(test, feature = "test-support"))]
fn tag_resize_handle(
    handle: Stateful<Div>,
    index: usize,
    state: &Entity<TableState>,
) -> Stateful<Div> {
    let selector = resize_handle_id(index, state).to_string();
    handle.debug_selector(move || selector.clone())
}

/// A no-op outside test builds.
#[cfg(not(any(test, feature = "test-support")))]
fn tag_resize_handle(
    handle: Stateful<Div>,
    _index: usize,
    _state: &Entity<TableState>,
) -> Stateful<Div> {
    handle
}

/// The `VisualTestContext::debug_bounds` lookup key for `state`'s table's
/// resize handle on column `index`, for a consumer crate's own render tests.
/// Requires this crate's `test-support` feature (or building this crate's
/// own tests).
///
/// The returned `&'static str` is deliberately leaked: `debug_bounds` takes
/// `&'static str`, and the key is per-table-instance and per-column so it
/// cannot be a literal. Test-support builds only, and one small leak per
/// call.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn column_resize_handle_debug_selector(
    state: &Entity<TableState>,
    index: usize,
) -> &'static str {
    Box::leak(resize_handle_id(index, state).to_string().into_boxed_str())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{
        AppContext as _, Context, Entity, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
        MouseUpEvent, Pixels, Render, TestAppContext, Window, div, point, prelude::*, px,
    };

    use super::{column_resize_handle_debug_selector, resized_width};
    use crate::table::{Table, TableColumn, TableState};

    #[test]
    fn resized_width_applies_the_pointer_delta_in_either_direction() {
        let grown = resized_width(px(150.0), px(200.0), px(230.0), px(50.0));
        assert_eq!(grown, px(180.0));
        let shrunk = resized_width(px(150.0), px(200.0), px(180.0), px(50.0));
        assert_eq!(shrunk, px(130.0));
    }

    #[test]
    fn resized_width_clamps_exactly_at_the_minimum() {
        let width = resized_width(px(150.0), px(200.0), px(0.0), px(50.0));
        assert_eq!(
            width,
            px(50.0),
            "a drag far past the minimum must clamp exactly at it, not go to zero or negative"
        );
    }

    /// A minimal view: two fixed-width columns, resizable down to a 40px
    /// floor, storing every live-resized width back into `widths` -- the
    /// same caller-owned-storage shape `ResultsView` uses for its own
    /// `column_widths`.
    struct Probe {
        state: Entity<TableState>,
        widths: Rc<RefCell<Vec<Pixels>>>,
    }

    impl Render for Probe {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let widths = self.widths.borrow().clone();
            let columns = widths
                .iter()
                .enumerate()
                .map(|(index, &width)| TableColumn::new(width, div().child(format!("c{index}"))))
                .collect::<Vec<_>>();
            let store = self.widths.clone();
            let table = Table::new("resize-probe", &self.state)
                .columns(columns)
                .row_count(0)
                .resizable_columns(px(40.0), move |_this: &mut Self, column, width, _w, _cx| {
                    store.borrow_mut()[column] = width;
                })
                .render(cx);

            div().size_full().child(
                div()
                    .w(px(400.0))
                    .h(px(200.0))
                    .flex()
                    .flex_col()
                    .child(table),
            )
        }
    }

    fn setup(
        cx: &mut TestAppContext,
    ) -> (
        Entity<Probe>,
        &mut gpui::VisualTestContext,
        Rc<RefCell<Vec<Pixels>>>,
    ) {
        let widths = Rc::new(RefCell::new(vec![px(100.0), px(120.0)]));
        let probe_widths = widths.clone();
        let (probe, vcx) = cx.add_window_view(|_window, cx| Probe {
            state: cx.new(TableState::new),
            widths: probe_widths,
        });
        vcx.run_until_parked();
        (probe, vcx, widths)
    }

    #[gpui::test]
    fn dragging_a_handle_resizes_only_its_own_column(cx: &mut TestAppContext) {
        let (probe, vcx, widths) = setup(cx);
        let state = probe.read_with(vcx, |p, _app| p.state.clone());
        let handle_bounds = vcx
            .debug_bounds(column_resize_handle_debug_selector(&state, 0))
            .expect("column 0's resize handle must be painted");
        let origin = handle_bounds.origin;

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: origin,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        assert_eq!(
            widths.borrow()[0],
            px(100.0),
            "pressing down on the handle must not itself resize the column"
        );

        vcx.simulate_event(MouseMoveEvent {
            position: point(origin.x + px(30.0), origin.y),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: point(origin.x + px(30.0), origin.y),
            modifiers: Modifiers::default(),
            click_count: 1,
        });

        let final_widths = widths.borrow().clone();
        assert_eq!(
            final_widths[0],
            px(130.0),
            "dragging column 0's handle by +30px must widen it by exactly 30px"
        );
        assert_eq!(
            final_widths[1],
            px(120.0),
            "a different column's width must be untouched by another column's drag"
        );

        vcx.simulate_event(MouseMoveEvent {
            position: point(origin.x + px(60.0), origin.y),
            pressed_button: None,
            modifiers: Modifiers::default(),
        });
        assert_eq!(
            widths.borrow()[0],
            px(130.0),
            "moving the mouse after mouse-up must not resume the drag"
        );
    }

    #[gpui::test]
    fn dragging_past_the_minimum_clamps_exactly_at_it(cx: &mut TestAppContext) {
        let (probe, vcx, widths) = setup(cx);
        let state = probe.read_with(vcx, |p, _app| p.state.clone());
        let handle_bounds = vcx
            .debug_bounds(column_resize_handle_debug_selector(&state, 0))
            .expect("column 0's resize handle must be painted");
        let origin = handle_bounds.origin;

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: origin,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(MouseMoveEvent {
            position: point(px(0.0), origin.y),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });

        assert_eq!(
            widths.borrow()[0],
            px(40.0),
            "dragging far past the configured minimum must clamp exactly at it"
        );
    }
}
