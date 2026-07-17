//! The results grid: a virtualized table view over a `zsql_core::ResultSet`.
//! Renders a results header bar (row count + a source/relation label) above
//! a sticky column-header row and a `gpui::uniform_list` body, so only the
//! rows currently on screen are ever built regardless of result size.

use std::ops::Range;

use gpui::{
    Context, Div, Pixels, Render, SharedString, UniformListScrollHandle, Window, div, prelude::*,
    px, rgb, rgba, uniform_list,
};
use zsql_core::{ColumnMeta, ResultSet};

use super::format::{ValueKind, format_value};
use super::theme;

/// A virtualized results grid over a fully materialized `ResultSet`.
///
/// The visual spec's bottom connection/status strip reports live session
/// state (connection, elapsed query time, cursor position) that has no
/// source of truth yet in this hardcoded, editor-less view, so it is
/// intentionally not rendered here rather than showing fabricated status.
pub struct ResultsView {
    result: ResultSet,
    source_label: SharedString,
    column_widths: Vec<Pixels>,
    row_number_width: Pixels,
    /// Shared vertical scroll state between the row-number pane's list and
    /// the data pane's list, so the two stay in lockstep. See the comment in
    /// `Render::render` for why the grid is split into two panes at all.
    row_scroll_handle: UniformListScrollHandle,
}

impl ResultsView {
    /// Build a view over `result`. `source_label` names where the rows came
    /// from (a relation like `public.orders`, or a query kind) and is shown
    /// in the results header bar next to the row count.
    #[must_use]
    pub fn new(result: ResultSet, source_label: impl Into<SharedString>) -> Self {
        let column_widths = compute_column_widths(&result);
        let row_number_width = row_number_column_width(result.rows.len());
        Self {
            result,
            source_label: source_label.into(),
            column_widths,
            row_number_width,
            row_scroll_handle: UniformListScrollHandle::new(),
        }
    }

    /// The results header bar: row count + source/relation label.
    fn render_bar(&self) -> Div {
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .gap_3()
            .h(theme::RESULTS_BAR_HEIGHT)
            .px_3()
            .bg(rgb(theme::PANEL))
            .border_b_1()
            .border_color(rgb(theme::LINE_SOFT))
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
                            .text_color(rgb(theme::TEXT))
                            .child("Results"),
                    )
                    .child(
                        div()
                            .font_family("monospace")
                            .text_color(rgb(theme::TEAL))
                            .child(self.result.rows.len().to_string()),
                    ),
            )
            .child(
                div()
                    .font_family("monospace")
                    .text_size(px(theme::RESULTS_META_TEXT_SIZE))
                    .text_color(rgb(theme::FAINT))
                    .child(self.source_label.clone()),
            )
    }

    /// The sticky header cell for the pinned row-number pane: just the `#`
    /// label, matching the column-header row's height and background.
    fn render_row_number_header() -> Div {
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_end()
            .h(theme::HEADER_ROW_HEIGHT)
            .px(px(theme::CELL_PADDING_X))
            .bg(rgb(theme::RAISE))
            .border_b_1()
            .border_color(rgb(theme::LINE))
            .text_color(rgb(theme::FAINT))
            .child("#")
    }

    /// The sticky column-header row for the data pane: each column's name
    /// plus its backend type tag. Does not include the row-number cell,
    /// which lives in the separate pinned pane (see `render_row_number_header`).
    fn render_column_headers(&self) -> Div {
        let mut row = div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .h(theme::HEADER_ROW_HEIGHT)
            .bg(rgb(theme::RAISE))
            .border_b_1()
            .border_color(rgb(theme::LINE));

        for (column, width) in self.result.columns.iter().zip(self.column_widths.iter()) {
            row = row.child(
                header_cell_shell(*width)
                    .flex_row()
                    .items_baseline()
                    .gap_2()
                    .child(
                        div()
                            .text_color(rgb(theme::TEXT))
                            .child(column.name.clone()),
                    )
                    .child(type_tag(&column.type_name)),
            );
        }

        row
    }

    /// Render the row-number cells in `range` for the pinned pane's
    /// virtualized list. Only the rows currently scrolled into view are ever
    /// built.
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
                    .px(px(theme::CELL_PADDING_X))
                    .bg(rgb(theme::PANEL))
                    .border_b_1()
                    .border_color(rgb(theme::LINE_SOFT))
                    .text_color(rgb(theme::FAINT))
                    .child((ix + 1).to_string())
            })
            .collect()
    }

    /// Render the data-cell rows in `range` for the data pane's virtualized
    /// list. Only the rows currently scrolled into view are ever built.
    fn render_data_row_cells(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Vec<Div> {
        // A shared borrow: every use below only reads column widths, and sits
        // alongside the (disjoint) `self.result.rows` read in the loop, so
        // there is no need to clone the vector on every render/scroll frame.
        let column_widths = &self.column_widths;

        range
            .map(|ix| {
                let mut row_div = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(theme::BODY_ROW_HEIGHT)
                    .border_b_1()
                    .border_color(rgb(theme::LINE_SOFT));

                if let Some(row) = self.result.rows.get(ix) {
                    for (value, width) in row.0.iter().zip(column_widths.iter()) {
                        let formatted = format_value(value);
                        let is_null = formatted.kind == ValueKind::Null;
                        row_div = row_div.child(
                            body_cell_shell(*width)
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
}

impl Render for ResultsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let row_count = self.result.rows.len();
        let row_number_width = self.row_number_width;

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(theme::INK))
            .child(self.render_bar())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    // The row-number pane is pinned outside the horizontally
                    // scrolling data pane below, rather than using CSS-style
                    // `position: sticky` as the mockup's `.rownum` does:
                    // gpui 0.2.2 has no sticky-positioning primitive, so a
                    // fixed-width left pane plus a horizontally-scrolling
                    // right pane is used instead. Both panes' uniform_lists
                    // are given clones of the same `UniformListScrollHandle`
                    // (an `Rc<RefCell<_>>`), so they track one shared
                    // vertical offset and scroll in lockstep, while only the
                    // right pane's own horizontal scroll state moves its
                    // content underneath the pinned row numbers.
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_shrink_0()
                            .w(row_number_width)
                            .h_full()
                            .border_r_1()
                            .border_color(rgb(theme::LINE_SOFT))
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
                        div()
                            .id("results-h-scroll")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .h_full()
                            .overflow_x_scroll()
                            .child(self.render_column_headers())
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
                    ),
            )
    }
}

/// Shared chrome for a header cell: fixed width, right hairline, padding,
/// clipped to its column so overlong content never bleeds into the next
/// column.
fn header_cell_shell(width: Pixels) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .w(width)
        .h_full()
        .px(px(theme::CELL_PADDING_X))
        .truncate()
        .border_r_1()
        .border_color(rgb(theme::LINE_SOFT))
}

/// Shared chrome for a body cell: fixed width matching the header cell in
/// the same column, right hairline, padding, single line (wide tables
/// scroll horizontally rather than wrapping).
fn body_cell_shell(width: Pixels) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .w(width)
        .h_full()
        .px(px(theme::CELL_PADDING_X))
        .truncate()
        .border_r_1()
        .border_color(rgb(theme::LINE_SOFT))
}

/// The small type-name badge shown next to a column's name in the header.
fn type_tag(type_name: &str) -> Div {
    div()
        .text_size(px(theme::TYPE_TAG_TEXT_SIZE))
        .text_color(rgb(theme::TEAL))
        .px(px(theme::TYPE_TAG_PADDING_X))
        .border_1()
        .border_color(rgba(theme::TYPE_TAG_BORDER))
        .rounded(px(theme::TYPE_TAG_RADIUS))
        .child(type_name.to_owned())
}

/// The text color for a formatted cell's semantic kind.
fn kind_color(kind: ValueKind) -> u32 {
    match kind {
        ValueKind::Null => theme::FAINT,
        ValueKind::Bool => theme::BOOL,
        ValueKind::Number => theme::NUMBER,
        ValueKind::Text => theme::TEXT,
        ValueKind::Json => theme::JSON,
        ValueKind::Timestamp => theme::MUTED,
        ValueKind::Bytes => theme::BYTES,
        ValueKind::Unknown => theme::UNKNOWN,
    }
}

/// Estimate each column's pixel width from its header (name + type tag) and
/// the longest formatted cell in that column, so the header and every
/// virtualized body row line up under the same column boundaries.
fn compute_column_widths(result: &ResultSet) -> Vec<Pixels> {
    result
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| column_width(column, index, result))
        .collect()
}

// Cell content lengths here are always small (column names, formatted
// scalar values), so the `usize -> f32` conversions below cannot lose
// meaningful precision.
#[allow(clippy::cast_precision_loss)]
fn column_width(column: &ColumnMeta, index: usize, result: &ResultSet) -> Pixels {
    let header_chars = column.name.chars().count() + column.type_name.chars().count();
    let header_width = theme::CELL_PADDING_X * 2.0
        + header_chars as f32 * theme::CELL_CHAR_WIDTH
        + theme::TYPE_TAG_EXTRA_WIDTH;

    let body_chars = result
        .rows
        .iter()
        .filter_map(|row| row.0.get(index))
        .map(|value| format_value(value).text.chars().count())
        .max()
        .unwrap_or(0);
    let body_width = theme::CELL_PADDING_X * 2.0 + body_chars as f32 * theme::CELL_CHAR_WIDTH;

    px(header_width
        .max(body_width)
        .clamp(theme::MIN_COLUMN_WIDTH, theme::MAX_COLUMN_WIDTH))
}

/// Width of the leading row-number column, wide enough for the largest row
/// number in the result set.
#[allow(clippy::cast_precision_loss)]
fn row_number_column_width(row_count: usize) -> Pixels {
    let digits = row_count.to_string().chars().count().max(1);
    let width = theme::CELL_PADDING_X * 2.0 + digits as f32 * theme::CELL_CHAR_WIDTH;
    px(width.max(theme::ROW_NUMBER_MIN_WIDTH))
}

#[cfg(test)]
mod tests {
    use zsql_core::{ColumnMeta, ResultSet, Row, Value};

    use super::{column_width, row_number_column_width};

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

    #[test]
    fn column_width_grows_with_the_longest_cell() {
        let result = sample_result();
        let narrow = column_width(&result.columns[0], 0, &result);
        let wide = column_width(&result.columns[1], 1, &result);
        assert!(f32::from(wide) > f32::from(narrow));
    }

    #[test]
    fn column_width_never_shrinks_below_the_minimum() {
        let result = sample_result();
        let width = column_width(&result.columns[0], 0, &result);
        assert!(f32::from(width) >= super::theme::MIN_COLUMN_WIDTH);
    }

    #[test]
    // `clamp`'s ceiling arm returns `MAX_COLUMN_WIDTH` verbatim (no further
    // arithmetic on it), so an exact comparison here is intentional, not a
    // fragile computed-float equality check.
    #[allow(clippy::float_cmp)]
    fn column_width_clamps_to_the_maximum() {
        let mut result = sample_result();
        // Comfortably longer than any header/body width formula could stay
        // under the clamp for, so this exercises the ceiling arm of the
        // `clamp` in `column_width` rather than the growth path.
        result.rows[1].0[1] = Value::Text("x".repeat(500));
        let width = column_width(&result.columns[1], 1, &result);
        assert_eq!(f32::from(width), super::theme::MAX_COLUMN_WIDTH);
    }

    #[test]
    fn row_number_width_grows_with_digit_count() {
        let small = row_number_column_width(9);
        let large = row_number_column_width(1_000_000);
        assert!(f32::from(large) > f32::from(small));
    }

    // gpui's `TestAppContext` runs on `TestPlatform`, a headless mock of the
    // platform layer (window system, display list) used purely for
    // deterministic tests, so this does not require a real display server.
    #[gpui::test]
    fn renders_one_frame_without_panicking(cx: &mut gpui::TestAppContext) {
        let mut result = sample_result();
        result.rows.push(Row(vec![Value::Int(3), Value::Null]));
        // Extend with comprehensive test coverage of all Value variants.
        // This ensures render_data_row_cells and every kind_color arm execute
        // in the one rendered frame, exercising Bool, Json, Timestamp, Bytes,
        // and Unknown (via Array) value types that are missing from the basic
        // sample_result.
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

        cx.add_window_view(|_window, _cx| super::ResultsView::new(result, "public.orders"));
    }
}
