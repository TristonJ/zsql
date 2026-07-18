//! The results grid: a virtualized table view over a `Session`'s current
//! [`SessionState`] and accumulated result set

use std::ops::Range;

use gpui::{
    Context, Div, Entity, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Render, SharedString, UniformListScrollHandle, Window, div, point, prelude::*, px, rgb, rgba,
    uniform_list,
};
use zsql_core::ColumnMeta;
use zsql_ui::{colors, grid, scrollbar};

use super::format::{ValueKind, format_value};
use super::theme;
use crate::session::{LivenessState, Session, SessionState};

/// A virtualized results grid, driven by a `Session` entity.
pub struct ResultsView {
    session: Entity<Session>,
    source_label: SharedString,
    column_widths: Vec<Pixels>,
    /// Per-column max formatted-text char count seen so far
    column_max_body_chars: Vec<usize>,
    /// How many of `session.result().rows` have already been folded into
    /// `column_max_body_chars`
    folded_row_count: usize,
    row_number_width: Pixels,
    /// Shared vertical scroll state between the row-number pane's list and
    /// the data pane's list, so the two stay in lockstep
    row_scroll_handle: UniformListScrollHandle,
    /// Set while the user is dragging the vertical scrollbar's thumb, so
    /// mouse-move events know to translate pointer movement into a new
    /// scroll offset instead of being ignored.
    vscrollbar_drag: Option<VscrollbarDrag>,
}

/// The pointer position and scroll offset captured when a vertical
/// scrollbar thumb-drag starts, used to translate subsequent pointer
/// movement into a new scroll offset.
#[derive(Debug, Clone, Copy)]
struct VscrollbarDrag {
    pointer_start_y: Pixels,
    offset_start_y: Pixels,
}

impl ResultsView {
    /// Build a view over `session`. `source_label` names where the rows came
    /// from (a relation like `public.orders`, or a query kind) and is shown
    /// in the results header bar next to the row count.
    #[must_use]
    pub fn new(
        session: Entity<Session>,
        source_label: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&session, |view: &mut Self, _session, cx| {
            view.sync_dimensions(cx);
            cx.notify();
        })
        .detach();

        let mut view = Self {
            session,
            source_label: source_label.into(),
            column_widths: Vec::new(),
            column_max_body_chars: Vec::new(),
            folded_row_count: 0,
            row_number_width: row_number_column_width(0),
            row_scroll_handle: UniformListScrollHandle::new(),
            vscrollbar_drag: None,
        };
        view.sync_dimensions(cx);
        view
    }

    /// The vertical scrollbar's visibility and size are computed from the
    /// scroll viewport's laid-out height, which reads back as zero during the
    /// render that first lays the grid out (a scroll container's bounds are
    /// only known after that render). The grid itself only appears once a
    /// query returns rows, so the first grid frame always starts unmeasured.
    /// When that state is detected - the grid is shown but its viewport has
    /// not been measured yet - schedule exactly one re-render so the scrollbar
    /// appears on the next frame instead of staying hidden until unrelated
    /// input forces a repaint. This settles immediately: once the viewport is
    /// measured (non-zero) the condition is false, so no further nudges fire.
    /// `request_animation_frame` cannot do this - it only queues a callback
    /// without forcing a draw, so on an otherwise idle window it never fires.
    fn nudge_scrollbar_when_grid_unmeasured(&mut self, cx: &mut Context<Self>) {
        let grid_shown = {
            let session = self.session.read(cx);
            matches!(session.state(), SessionState::Results(_))
                || (matches!(session.state(), SessionState::Running)
                    && !session.result().columns.is_empty())
        };
        let viewport_unmeasured = self
            .row_scroll_handle
            .0
            .borrow()
            .base_handle
            .bounds()
            .size
            .height
            == Pixels::ZERO;
        if grid_shown && viewport_unmeasured {
            cx.spawn(async move |this, cx| {
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }
    }

    /// Update the results header's source/relation label, e.g. after the
    /// schema sidebar previews a different relation.
    pub fn set_source_label(&mut self, label: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.source_label = label.into();
        cx.notify();
    }

    /// Bring `column_widths`/`row_number_width` up to date with the
    /// session's current result set
    fn sync_dimensions(&mut self, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        let result = session.result();

        if result.columns.len() != self.column_max_body_chars.len() {
            self.column_max_body_chars = vec![0; result.columns.len()];
            self.folded_row_count = 0;
        }

        for row in result.rows.iter().skip(self.folded_row_count) {
            for (index, max_chars) in self.column_max_body_chars.iter_mut().enumerate() {
                if let Some(value) = row.0.get(index) {
                    let chars = format_value(value).text.chars().count();
                    if chars > *max_chars {
                        *max_chars = chars;
                    }
                }
            }
        }
        self.folded_row_count = result.rows.len();

        self.column_widths = result
            .columns
            .iter()
            .zip(self.column_max_body_chars.iter())
            .map(|(column, &max_body_chars)| column_width_from_parts(column, max_body_chars))
            .collect();
        self.row_number_width = row_number_column_width(result.rows.len());
    }

    /// The results header bar: row count + source/relation label.
    fn render_bar(&self, cx: &Context<Self>) -> Div {
        let session = self.session.read(cx);
        let count_text = match session.state() {
            SessionState::Results(_) | SessionState::Running => {
                session.result().rows.len().to_string()
            }
            SessionState::Empty
            | SessionState::Connecting
            | SessionState::Connected
            | SessionState::Error(_) => "–".to_owned(),
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .gap_3()
            .h(theme::RESULTS_BAR_HEIGHT)
            .px_3()
            .bg(rgb(colors::PANEL))
            .border_b_1()
            .border_color(rgb(colors::LINE_SOFT))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap_2()
                    .text_size(px(theme::RESULTS_TAB_TEXT_SIZE))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(colors::TEXT))
                            .child("Results"),
                    )
                    .child(
                        div()
                            .font_family("monospace")
                            .text_color(rgb(colors::TEAL))
                            .child(count_text),
                    ),
            )
            .child(
                div()
                    .font_family("monospace")
                    .text_size(px(theme::RESULTS_META_TEXT_SIZE))
                    .text_color(rgb(colors::FAINT))
                    .child(self.source_label.clone()),
            )
    }

    /// The main content area: the virtualized grid when results are
    /// available, or a centered prompt/status message otherwise
    fn render_body(&mut self, cx: &mut Context<Self>) -> Div {
        let session = self.session.read(cx);
        let state = session.state().clone();
        let has_columns = !session.result().columns.is_empty();

        match state {
            SessionState::Results(_) => self.render_grid(cx),
            // Once the streaming query's `Columns` event has arrived there
            // is a real (if partial) result set to paint, so switch to the
            // grid immediately rather than waiting for `Done`
            SessionState::Running if has_columns => self.render_grid(cx),
            SessionState::Empty => Self::render_placeholder(
                colors::FAINT,
                "No connection configured",
                "Set DATABASE_URL or connection.default_url in your zsql config, then restart.",
            ),
            SessionState::Connecting => Self::render_placeholder(
                colors::FAINT,
                "Connecting…",
                "Establishing a connection to the configured database.",
            ),
            SessionState::Connected => Self::render_placeholder(
                colors::FAINT,
                "Connected",
                "Run a query to see results here.",
            ),
            SessionState::Running => Self::render_placeholder(
                colors::FAINT,
                "Running query…",
                "Streaming results from the database.",
            ),
            SessionState::Error(message) => {
                Self::render_placeholder(theme::STATUS_ERROR, "Query failed", &message)
            }
        }
    }

    /// A centered title + detail message shown in place of the grid for any
    /// non-`Results` state.
    fn render_placeholder(title_color: u32, title: &str, detail: &str) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_2()
            .px_6()
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(title_color))
                    .child(title.to_owned()),
            )
            .child(
                div()
                    .text_size(px(theme::RESULTS_META_TEXT_SIZE))
                    .text_color(rgb(colors::FAINT))
                    .child(detail.to_owned()),
            )
    }

    /// The two-pane virtualized grid (pinned row numbers + horizontally
    /// scrolling data columns)
    fn render_grid(&mut self, cx: &mut Context<Self>) -> Div {
        let row_count = self.session.read(cx).result().rows.len();
        let row_number_width = self.row_number_width;

        div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h_0()
            .w_full()
            // The row-number pane is pinned outside the horizontally
            // scrolling data pane below, rather than using CSS-style
            // `position: sticky` as the mockup's `.rownum` does: gpui 0.2.2
            // has no sticky-positioning primitive, so a fixed-width left
            // pane plus a horizontally-scrolling right pane is used instead
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_shrink_0()
                    .w(row_number_width)
                    .h_full()
                    .border_r_1()
                    .border_color(rgb(colors::LINE_SOFT))
                    .child(Self::render_row_number_header())
                    .child(
                        uniform_list(
                            "results-rownums",
                            row_count,
                            cx.processor(|this, range, window, cx| {
                                this.render_row_number_cells(range, window, cx)
                            }),
                        )
                        .flex_1()
                        .track_scroll(self.row_scroll_handle.clone()),
                    ),
            )
            .child(
                // The scrollbar is a sibling of the horizontally scrolling
                // "results-h-scroll" pane below, not a descendant of it:
                // gpui translates every descendant of a scroll container by
                // its scroll offset during prepaint (including absolutely
                // positioned ones), so nesting the scrollbar inside the
                // overflow_x_scroll pane would drag it left off the
                // viewport's right edge whenever the grid is scrolled right.
                // This outer div carries the `.relative()` anchor instead,
                // so the scrollbar's `.absolute()` positioning is relative
                // to a container unaffected by horizontal scrolling. It must
                // NOT set `min_w_full`: as a `flex_1` child sharing this
                // flex-row with the fixed-width row-number pane, a 100%
                // min-width would force it wider than the row and push its
                // right edge (where the scrollbar is pinned) off-screen. The
                // inner pane below fills the width instead.
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .h_full()
                    .child(
                        div()
                            .id("results-h-scroll")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_full()
                            .min_h_0()
                            .h_full()
                            .overflow_x_scroll()
                            .child(self.render_column_headers(cx))
                            .child(
                                uniform_list(
                                    "results-grid",
                                    row_count,
                                    cx.processor(|this, range, window, cx| {
                                        this.render_data_row_cells(range, window, cx)
                                    }),
                                )
                                .flex_1()
                                .track_scroll(self.row_scroll_handle.clone()),
                            ),
                    )
                    .children(self.render_vertical_scrollbar(row_count, cx)),
            )
    }

    /// The vertical scrollbar's current geometry, read fresh from
    /// `row_scroll_handle`'s live offset and bounds (never cached), so it
    /// stays in sync with wheel scrolling, keyboard scrolling, and rows
    /// streaming in across renders.
    fn vertical_scrollbar_geometry(&self, row_count: usize) -> scrollbar::ScrollbarGeometry {
        let (viewport_extent, scroll_offset) = {
            let state = self.row_scroll_handle.0.borrow();
            (
                f32::from(state.base_handle.bounds().size.height),
                f32::from(-state.base_handle.offset().y),
            )
        };
        scrollbar::ScrollbarGeometry::compute(
            content_extent_for_row_count(row_count),
            viewport_extent,
            scroll_offset,
            viewport_extent,
            scrollbar::MIN_THUMB_LENGTH,
        )
    }

    /// A thin track + draggable thumb overlaid on the right edge of the
    /// data pane, or `None` once `row_count` rows already fit inside the
    /// viewport and there is nothing to scroll.
    fn render_vertical_scrollbar(&self, row_count: usize, cx: &Context<Self>) -> Option<Div> {
        let geometry = self.vertical_scrollbar_geometry(row_count);
        if !geometry.visible {
            return None;
        }

        let track_length = f32::from(
            self.row_scroll_handle
                .0
                .borrow()
                .base_handle
                .bounds()
                .size
                .height,
        );
        let thumb_top = geometry.thumb_offset(track_length);

        Some(
            div()
                .absolute()
                .top(theme::HEADER_ROW_HEIGHT)
                .right(px(0.0))
                .bottom(px(0.0))
                .w(px(scrollbar::TRACK_WIDTH))
                .bg(rgba(scrollbar::TRACK_COLOR))
                .child(
                    div()
                        .absolute()
                        .top(px(thumb_top))
                        .right(px(0.0))
                        .w(px(scrollbar::TRACK_WIDTH))
                        .h(px(geometry.thumb_length))
                        .rounded(px(scrollbar::TRACK_WIDTH / 2.0))
                        .bg(rgba(scrollbar::THUMB_COLOR))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(Self::on_vscrollbar_mouse_down),
                        ),
                ),
        )
    }

    /// Start a vertical scrollbar thumb-drag, capturing the pointer's
    /// starting position and the grid's current scroll offset.
    fn on_vscrollbar_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offset_start_y = self.row_scroll_handle.0.borrow().base_handle.offset().y;
        self.vscrollbar_drag = Some(VscrollbarDrag {
            pointer_start_y: event.position.y,
            offset_start_y,
        });
        cx.notify();
    }

    /// While a thumb-drag is in progress, translate pointer movement into a
    /// new grid scroll offset via [`scrollbar::ScrollbarGeometry::scroll_offset_for_drag`].
    /// A no-op when no drag is in progress, or when the left button is no
    /// longer held: if the button was released outside the window mid-drag,
    /// neither `on_mouse_up` nor `on_mouse_up_out` fires, so this handler
    /// must independently notice the button is gone and end the drag itself
    /// rather than leaving the thumb stuck to a button-less pointer.
    fn on_vscrollbar_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            if self.vscrollbar_drag.take().is_some() {
                cx.notify();
            }
            return;
        }

        let Some(drag) = self.vscrollbar_drag else {
            return;
        };

        let row_count = self.session.read(cx).result().rows.len();
        let content_extent = content_extent_for_row_count(row_count);
        let viewport_extent = f32::from(
            self.row_scroll_handle
                .0
                .borrow()
                .base_handle
                .bounds()
                .size
                .height,
        );
        let pointer_delta = f32::from(event.position.y - drag.pointer_start_y);
        let new_offset_y = scrollbar::ScrollbarGeometry::scroll_offset_for_drag(
            f32::from(-drag.offset_start_y),
            pointer_delta,
            content_extent,
            viewport_extent,
            viewport_extent,
            scrollbar::MIN_THUMB_LENGTH,
        );

        let current_offset_x = self.row_scroll_handle.0.borrow().base_handle.offset().x;
        self.row_scroll_handle
            .0
            .borrow()
            .base_handle
            .set_offset(point(current_offset_x, px(-new_offset_y)));
        cx.notify();
    }

    /// End a vertical scrollbar thumb-drag, on both a mouse-up over the
    /// thumb and a mouse-up anywhere else in the window (the pointer often
    /// leaves the thumb's small hit region mid-drag).
    fn on_vscrollbar_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.vscrollbar_drag.take().is_some() {
            cx.notify();
        }
    }

    /// The sticky header cell for the pinned row-number pane
    fn render_row_number_header() -> Div {
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_end()
            .h(theme::HEADER_ROW_HEIGHT)
            .px(px(grid::CELL_PADDING_X))
            .bg(rgb(colors::RAISE))
            .border_b_1()
            .border_color(rgb(colors::LINE))
            .text_color(rgb(colors::FAINT))
            .child("#")
    }

    /// The sticky column-header row for the data pane
    fn render_column_headers(&self, cx: &Context<Self>) -> Div {
        let mut row = div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .h(theme::HEADER_ROW_HEIGHT)
            .bg(rgb(colors::RAISE))
            .border_b_1()
            .border_color(rgb(colors::LINE));

        let session = self.session.read(cx);
        let columns: &[ColumnMeta] = &session.result().columns;

        for (column, width) in columns.iter().zip(self.column_widths.iter()) {
            row = row.child(
                grid::header_cell_shell(*width)
                    .flex_row()
                    .items_baseline()
                    .gap_2()
                    .child(
                        div()
                            .text_color(rgb(colors::TEXT))
                            .child(column.name.clone()),
                    )
                    .child(grid::type_tag(&column.type_name)),
            );
        }

        row
    }

    /// Render the row-number cells in `range` for the pinned pane's
    /// virtualized list
    fn render_row_number_cells(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let width = self.row_number_width;

        range
            .map(|ix| {
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_end()
                    .w(width)
                    .h(theme::BODY_ROW_HEIGHT)
                    .px(px(grid::CELL_PADDING_X))
                    .bg(rgb(colors::PANEL))
                    .border_b_1()
                    .border_color(rgb(colors::LINE_SOFT))
                    .text_color(rgb(colors::FAINT))
                    .child((ix + 1).to_string())
            })
            .collect()
    }

    /// Render the data-cell rows in `range` for the data pane's virtualized
    /// list
    fn render_data_row_cells(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let column_widths = &self.column_widths;
        let session = self.session.read(cx);
        let rows: &[zsql_core::Row] = &session.result().rows;

        range
            .map(|ix| {
                let mut row_div = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(theme::BODY_ROW_HEIGHT)
                    .border_b_1()
                    .border_color(rgb(colors::LINE_SOFT));

                if let Some(row) = rows.get(ix) {
                    for (value, width) in row.0.iter().zip(column_widths.iter()) {
                        let formatted = format_value(value);
                        let is_null = formatted.kind == ValueKind::Null;
                        row_div = row_div.child(
                            grid::body_cell_shell(*width)
                                .text_color(rgb(kind_color(formatted.kind)))
                                .when(is_null, gpui::prelude::Styled::italic)
                                .child(formatted.text),
                        );
                    }
                }

                row_div
            })
            .collect()
    }

    /// The bottom connection/status bar: connection state + label, row
    /// count, and elapsed query time
    fn render_status_bar(&self, cx: &Context<Self>) -> Div {
        let session = self.session.read(cx);
        let state = session.state();
        let (dot_color, label) = status_indicator(state, session.liveness());

        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .gap_4()
            .h(theme::STATUS_BAR_HEIGHT)
            .px_3()
            .bg(rgb(colors::PANEL))
            .border_t_1()
            .border_color(rgb(colors::LINE))
            .font_family("monospace")
            .text_size(px(theme::STATUS_BAR_TEXT_SIZE))
            .text_color(rgb(colors::MUTED))
            .child(grid::status_dot(dot_color))
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(colors::TEXT))
                    .child(label),
            );

        if let Some((rows_text, elapsed_text)) = status_metrics(state, session.result().rows.len())
        {
            bar = bar.child(rows_text).child(elapsed_text);
        }

        if let SessionState::Error(message) = state {
            bar = bar.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(theme::STATUS_ERROR))
                    .child(message.clone()),
            );
        }

        bar
    }
}

/// Test-only accessor used by `ui::sidebar`'s render tests
#[cfg(test)]
impl ResultsView {
    pub(crate) fn source_label_for_test(&self) -> &str {
        &self.source_label
    }
}

impl Render for ResultsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.nudge_scrollbar_when_grid_unmeasured(cx);
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(colors::INK))
            // Attached here, above the grid, so a vertical scrollbar
            // thumb-drag keeps tracking the pointer even once it leaves the
            // thumb's own small hit region.
            .on_mouse_move(cx.listener(Self::on_vscrollbar_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_vscrollbar_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_vscrollbar_mouse_up))
            .child(self.render_bar(cx))
            .child(self.render_body(cx))
            .child(self.render_status_bar(cx))
    }
}

/// Total pixel height of `row_count` body rows, i.e. the vertical
/// scrollbar's content extent.
// Row counts here are always far below `f32`'s exact-integer range, so this
// conversion cannot lose meaningful precision.
#[allow(clippy::cast_precision_loss)]
fn content_extent_for_row_count(row_count: usize) -> f32 {
    row_count as f32 * f32::from(theme::BODY_ROW_HEIGHT)
}

/// The bottom status bar's dot color and label for `state`. A `liveness` of
/// [`LivenessState::Unreachable`] overrides every state's normal indicator
/// with a distinct "Disconnected" one, since the probe result is
/// independent of (and can contradict) whatever `state` currently holds -
/// for instance a query can still be `Running` against a connection the
/// probe has just found unreachable.
fn status_indicator(state: &SessionState, liveness: &LivenessState) -> (u32, &'static str) {
    if matches!(liveness, LivenessState::Unreachable(_)) {
        return (theme::STATUS_DISCONNECTED, "Disconnected");
    }
    match state {
        SessionState::Empty => (colors::FAINT, "Not connected"),
        SessionState::Connecting => (theme::STATUS_CONNECTING, "Connecting…"),
        SessionState::Connected | SessionState::Results(_) => (colors::TEAL, "Connected"),
        SessionState::Running => (colors::TEAL, "Running…"),
        SessionState::Error(_) => (theme::STATUS_ERROR, "Error"),
    }
}

/// The bottom status bar's "N rows" / "N ms" text for `state`, given
/// `row_count`. `None` for any state with no completed query to
/// report timing/row-count for
fn status_metrics(state: &SessionState, row_count: usize) -> Option<(String, String)> {
    if let SessionState::Results(elapsed) = state {
        Some((
            format!("{row_count} rows"),
            format!("{} ms", elapsed.as_millis()),
        ))
    } else {
        None
    }
}

/// The text color for a formatted cell's semantic kind.
fn kind_color(kind: ValueKind) -> u32 {
    match kind {
        ValueKind::Null => colors::FAINT,
        ValueKind::Bool => colors::BOOL,
        ValueKind::Number => colors::NUMBER,
        ValueKind::Text => colors::TEXT,
        ValueKind::Json => colors::JSON,
        ValueKind::Timestamp => colors::MUTED,
        ValueKind::Bytes => colors::BYTES,
        ValueKind::Unknown => colors::UNKNOWN,
    }
}

/// Estimate a column's pixel width from its header (name + type tag) and
/// `max_body_chars`.
// Cell content lengths here are always small (column names, formatted
// scalar values), so the `usize -> f32` conversions below cannot lose
// meaningful precision.
#[allow(clippy::cast_precision_loss)]
fn column_width_from_parts(column: &ColumnMeta, max_body_chars: usize) -> Pixels {
    let header_chars = column.name.chars().count() + column.type_name.chars().count();
    let header_width = grid::CELL_PADDING_X * 2.0
        + header_chars as f32 * theme::CELL_CHAR_WIDTH
        + theme::TYPE_TAG_EXTRA_WIDTH;

    let body_width = grid::CELL_PADDING_X * 2.0 + max_body_chars as f32 * theme::CELL_CHAR_WIDTH;

    px(header_width
        .max(body_width)
        .clamp(theme::MIN_COLUMN_WIDTH, theme::MAX_COLUMN_WIDTH))
}

/// Width of the leading row-number column, wide enough for the largest row
/// number in the result set.
#[allow(clippy::cast_precision_loss)]
fn row_number_column_width(row_count: usize) -> Pixels {
    let digits = row_count.to_string().chars().count().max(1);
    let width = grid::CELL_PADDING_X * 2.0 + digits as f32 * theme::CELL_CHAR_WIDTH;
    px(width.max(theme::ROW_NUMBER_MIN_WIDTH))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::AppContext as _;
    use zsql_core::{ColumnMeta, ResultSet, Row, Value};

    use super::{
        SessionState, column_width_from_parts, row_number_column_width, status_indicator,
        status_metrics,
    };
    use zsql_ui::colors;

    use crate::session::{LivenessState, Session};
    use crate::ui::format::format_value;
    use crate::ui::theme;

    fn sample_result() -> ResultSet {
        ResultSet {
            columns: vec![
                ColumnMeta {
                    name: "id".to_owned(),
                    type_name: "int8".to_owned(),
                    nullable: false,
                },
                ColumnMeta {
                    name: "status".to_owned(),
                    type_name: "text".to_owned(),
                    nullable: true,
                },
            ],
            rows: vec![
                Row(vec![Value::Int(1), Value::Text("paid".to_owned())]),
                Row(vec![
                    Value::Int(2),
                    Value::Text("a-very-long-status-value".to_owned()),
                ]),
            ],
            affected: None,
            notices: Vec::new(),
        }
    }

    fn max_body_chars(result: &ResultSet, index: usize) -> usize {
        result
            .rows
            .iter()
            .filter_map(|row| row.0.get(index))
            .map(|value| format_value(value).text.chars().count())
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn column_width_grows_with_the_longest_cell() {
        let result = sample_result();
        let narrow = column_width_from_parts(&result.columns[0], max_body_chars(&result, 0));
        let wide = column_width_from_parts(&result.columns[1], max_body_chars(&result, 1));
        assert!(f32::from(wide) > f32::from(narrow));
    }

    #[test]
    fn column_width_never_shrinks_below_the_minimum() {
        let result = sample_result();
        let width = column_width_from_parts(&result.columns[0], max_body_chars(&result, 0));
        assert!(f32::from(width) >= super::theme::MIN_COLUMN_WIDTH);
    }

    #[test]
    // `clamp`'s ceiling arm returns `MAX_COLUMN_WIDTH` verbatim (no further
    // arithmetic on it), so an exact comparison here is intentional
    #[allow(clippy::float_cmp)]
    fn column_width_clamps_to_the_maximum() {
        let mut result = sample_result();
        result.rows[1].0[1] = Value::Text("x".repeat(500));
        let width = column_width_from_parts(&result.columns[1], max_body_chars(&result, 1));
        assert_eq!(f32::from(width), super::theme::MAX_COLUMN_WIDTH);
    }

    #[test]
    // `.max()` returns `ROW_NUMBER_MIN_WIDTH` verbatim when it wins (no
    // further arithmetic on it), so an exact comparison here is intentional.
    #[allow(clippy::float_cmp)]
    fn row_number_width_clamps_to_the_minimum_for_small_row_counts() {
        // Both counts have few enough digits that the computed width sits
        // below `ROW_NUMBER_MIN_WIDTH`, so both clamp to the same floor.
        let single_digit = row_number_column_width(9);
        let seven_digit = row_number_column_width(1_000_000);
        assert_eq!(f32::from(single_digit), super::theme::ROW_NUMBER_MIN_WIDTH);
        assert_eq!(f32::from(seven_digit), super::theme::ROW_NUMBER_MIN_WIDTH);
    }

    #[test]
    fn row_number_width_grows_with_digit_count() {
        // `row_number_column_width` clamps to `ROW_NUMBER_MIN_WIDTH`, so the
        // "large" side needs enough digits to actually clear that floor
        // (1_000_000 alone still clamps to the minimum).
        let small = row_number_column_width(9);
        // 100_000_000 is the smallest 9-digit row count; nine digits is the
        // first digit count whose computed width clears `ROW_NUMBER_MIN_WIDTH`'s
        // floor, so this is the smallest row count that actually demonstrates
        // growth over `small`.
        let large = row_number_column_width(100_000_000);
        assert!(f32::from(large) > f32::from(small));
    }

    #[test]
    fn status_indicator_maps_each_state_to_its_dot_color_and_label() {
        assert_eq!(
            status_indicator(&SessionState::Empty, &LivenessState::Unknown),
            (colors::FAINT, "Not connected")
        );
        assert_eq!(
            status_indicator(&SessionState::Connecting, &LivenessState::Unknown),
            (theme::STATUS_CONNECTING, "Connecting…")
        );
        assert_eq!(
            status_indicator(&SessionState::Connected, &LivenessState::Healthy),
            (colors::TEAL, "Connected")
        );
        assert_eq!(
            status_indicator(&SessionState::Running, &LivenessState::Healthy),
            (colors::TEAL, "Running…")
        );
        assert_eq!(
            status_indicator(
                &SessionState::Results(Duration::from_millis(1)),
                &LivenessState::Healthy
            ),
            (colors::TEAL, "Connected")
        );
        assert_eq!(
            status_indicator(
                &SessionState::Error("boom".to_owned()),
                &LivenessState::Unknown
            ),
            (theme::STATUS_ERROR, "Error")
        );
    }

    #[test]
    fn status_indicator_shows_disconnected_regardless_of_session_state_when_liveness_is_unreachable()
     {
        let unreachable = LivenessState::Unreachable("connection reset".to_owned());
        for state in [
            SessionState::Connected,
            SessionState::Running,
            SessionState::Results(Duration::from_millis(1)),
        ] {
            assert_eq!(
                status_indicator(&state, &unreachable),
                (theme::STATUS_DISCONNECTED, "Disconnected"),
                "expected a Disconnected indicator regardless of state {state:?}"
            );
        }
    }

    #[test]
    fn status_indicator_treats_a_healthy_or_unknown_liveness_as_no_override() {
        assert_eq!(
            status_indicator(&SessionState::Connected, &LivenessState::Healthy),
            status_indicator(&SessionState::Connected, &LivenessState::Unknown),
            "Healthy and Unknown liveness must not change a state's own indicator"
        );
    }

    #[test]
    fn status_metrics_reports_rows_and_elapsed_ms_only_for_results() {
        let state = SessionState::Results(Duration::from_millis(42));
        assert_eq!(
            status_metrics(&state, 1),
            Some(("1 rows".to_owned(), "42 ms".to_owned()))
        );

        for state in [
            SessionState::Empty,
            SessionState::Connecting,
            SessionState::Connected,
            SessionState::Running,
            SessionState::Error("boom".to_owned()),
        ] {
            assert_eq!(
                status_metrics(&state, 5),
                None,
                "expected no fabricated rows/ms text for {state:?}"
            );
        }
    }

    #[gpui::test]
    fn renders_one_frame_without_panicking(cx: &mut gpui::TestAppContext) {
        let mut result = sample_result();
        result.rows.push(Row(vec![Value::Int(3), Value::Null]));
        result
            .rows
            .push(Row(vec![Value::Bool(true), Value::Float(42.5)]));
        result.rows.push(Row(vec![
            Value::Numeric("123456789.12345".to_owned()),
            Value::Bytes(vec![0xAB, 0xCD, 0xEF]),
        ]));
        result.rows.push(Row(vec![
            Value::Uuid("550e8400-e29b-41d4-a716-446655440000".to_owned()),
            Value::Timestamp("2026-07-14T09:12:31+00:00".to_owned()),
        ]));
        result.rows.push(Row(vec![
            Value::Json(r#"{"key":"value"}"#.to_owned()),
            Value::Array(vec![
                Value::Int(1),
                Value::Text("two".to_owned()),
                Value::Null,
            ]),
        ]));
        result.rows.push(Row(vec![
            Value::Unknown("custom_type".to_owned()),
            Value::Bool(false),
        ]));

        let state = SessionState::Results(Duration::from_millis(8));
        let session = cx.new(|_cx| Session::new_for_render_test(state, result));
        cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
    }

    #[gpui::test]
    fn renders_every_non_results_state_without_panicking(cx: &mut gpui::TestAppContext) {
        for state in [
            SessionState::Empty,
            SessionState::Connecting,
            SessionState::Connected,
            // No `Columns` event has arrived yet: the placeholder path,
            // not the grid.
            SessionState::Running,
            SessionState::Error("connection refused".to_owned()),
        ] {
            let session = cx.new(|_cx| Session::new_for_render_test(state, ResultSet::default()));
            cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
        }
    }

    #[gpui::test]
    fn renders_the_grid_for_a_running_query_with_partial_results(cx: &mut gpui::TestAppContext) {
        let mut result = sample_result();
        result.rows.truncate(1);
        let session = cx.new(|_cx| Session::new_for_render_test(SessionState::Running, result));

        cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
    }

    /// A result set with enough rows to overflow any reasonable viewport must
    /// show its vertical scrollbar without user interaction. This guards the
    /// first-frame regression where the scrollbar stayed hidden because the
    /// scroll viewport's bounds are zero during the first render and nothing
    /// forced the follow-up re-render once they became known.
    #[gpui::test]
    fn vertical_scrollbar_is_shown_after_the_first_frame_when_rows_overflow(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut result = sample_result();
        let template = result.rows[0].clone();
        result.rows = (0..400).map(|_| template.clone()).collect();
        let row_count = result.rows.len();
        let session = cx.new(|_cx| {
            Session::new_for_render_test(
                SessionState::Results(std::time::Duration::from_millis(1)),
                result,
            )
        });
        let (view, vcx) =
            cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
        vcx.run_until_parked();

        view.update(vcx, |v, cx| {
            let geometry = v.vertical_scrollbar_geometry(row_count);
            assert!(
                geometry.visible,
                "the scrollbar geometry must be visible for 400 overflowing rows"
            );
            assert!(
                v.render_vertical_scrollbar(row_count, cx).is_some(),
                "the scrollbar overlay must be rendered once the viewport is laid out"
            );
        });
    }

    #[gpui::test]
    fn column_widths_grow_incrementally_as_rows_stream_in(cx: &mut gpui::TestAppContext) {
        let columns = vec![ColumnMeta {
            name: "v".to_owned(),
            type_name: "text".to_owned(),
            nullable: true,
        }];
        let first_batch = ResultSet {
            columns: columns.clone(),
            rows: vec![Row(vec![Value::Text("ab".to_owned())])],
            affected: None,
            notices: Vec::new(),
        };

        let session =
            cx.new(|_cx| Session::new_for_render_test(SessionState::Running, first_batch));
        let session_for_view = session.clone();
        let (view, vcx) =
            cx.add_window_view(|_window, cx| super::ResultsView::new(session_for_view, "t", cx));

        let width_after_first_batch = view.update(vcx, |view, _cx| {
            assert_eq!(
                view.folded_row_count, 1,
                "the one row present at construction should already be folded"
            );
            view.column_widths[0]
        });

        // A second batch arrives with a much longer cell in the same
        // column.
        session.update(vcx, |session, _cx| {
            session.set_result_for_test(ResultSet {
                columns,
                rows: vec![
                    Row(vec![Value::Text("ab".to_owned())]),
                    Row(vec![Value::Text(
                        "a much longer value than before".to_owned(),
                    )]),
                ],
                affected: None,
                notices: Vec::new(),
            });
        });
        // `Session::set_result_for_test` bypasses `cx.notify()`, so the view
        // is synced explicitly here rather than relying on the observer
        view.update(vcx, super::ResultsView::sync_dimensions);

        view.update(vcx, |view, _cx| {
            assert_eq!(
                view.folded_row_count, 2,
                "folded_row_count should catch up to the new total row count"
            );
            assert!(
                f32::from(view.column_widths[0]) > f32::from(width_after_first_batch),
                "width should grow once a longer cell streams in"
            );
        });
    }
}
