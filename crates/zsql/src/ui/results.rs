//! The results grid: a virtualized table view over a `Session`'s current
//! [`SessionState`] and accumulated result set

use std::ops::Range;

use gpui::{
    Context, Div, Entity, Pixels, Render, SharedString, UniformListScrollHandle, Window, div,
    prelude::*, px, rgb, rgba, uniform_list,
};
use zsql_core::ColumnMeta;

use super::format::{ValueKind, format_value};
use super::theme;
use crate::session::{Session, SessionState};

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
        };
        view.sync_dimensions(cx);
        view
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
                            .child(count_text),
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
                theme::FAINT,
                "No connection configured",
                "Set DATABASE_URL or connection.default_url in your zsql config, then restart.",
            ),
            SessionState::Connecting => Self::render_placeholder(
                theme::FAINT,
                "Connecting…",
                "Establishing a connection to the configured database.",
            ),
            SessionState::Connected => Self::render_placeholder(
                theme::FAINT,
                "Connected",
                "Run a query to see results here.",
            ),
            SessionState::Running => Self::render_placeholder(
                theme::FAINT,
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
                    .text_color(rgb(theme::FAINT))
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
    }

    /// The sticky header cell for the pinned row-number pane
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

    /// The sticky column-header row for the data pane
    fn render_column_headers(&self, cx: &Context<Self>) -> Div {
        let mut row = div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .h(theme::HEADER_ROW_HEIGHT)
            .bg(rgb(theme::RAISE))
            .border_b_1()
            .border_color(rgb(theme::LINE));

        let session = self.session.read(cx);
        let columns: &[ColumnMeta] = &session.result().columns;

        for (column, width) in columns.iter().zip(self.column_widths.iter()) {
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
                    .border_color(rgb(theme::LINE_SOFT));

                if let Some(row) = rows.get(ix) {
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

    /// The bottom connection/status bar: connection state + label, row
    /// count, and elapsed query time
    fn render_status_bar(&self, cx: &Context<Self>) -> Div {
        let session = self.session.read(cx);
        let state = session.state();
        let (dot_color, label) = status_indicator(state);

        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .gap_4()
            .h(theme::STATUS_BAR_HEIGHT)
            .px_3()
            .bg(rgb(theme::PANEL))
            .border_t_1()
            .border_color(rgb(theme::LINE))
            .font_family("monospace")
            .text_size(px(theme::STATUS_BAR_TEXT_SIZE))
            .text_color(rgb(theme::MUTED))
            .child(status_dot(dot_color))
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(theme::TEXT))
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
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(theme::INK))
            .child(self.render_bar(cx))
            .child(self.render_body(cx))
            .child(self.render_status_bar(cx))
    }
}

/// Shared chrome for a header cell
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

/// Shared chrome for a body cell
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

/// The bottom status bar's dot color and label for `state`
fn status_indicator(state: &SessionState) -> (u32, &'static str) {
    match state {
        SessionState::Empty => (theme::FAINT, "Not connected"),
        SessionState::Connecting => (theme::STATUS_CONNECTING, "Connecting…"),
        SessionState::Connected | SessionState::Results(_) => (theme::TEAL, "Connected"),
        SessionState::Running => (theme::TEAL, "Running…"),
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

/// The small round indicator dot in the bottom status bar
fn status_dot(color: u32) -> Div {
    div()
        .flex_shrink_0()
        .w(px(theme::STATUS_DOT_SIZE))
        .h(px(theme::STATUS_DOT_SIZE))
        .rounded(px(theme::STATUS_DOT_SIZE / 2.0))
        .bg(rgb(color))
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

/// Estimate a column's pixel width from its header (name + type tag) and
/// `max_body_chars`.
// Cell content lengths here are always small (column names, formatted
// scalar values), so the `usize -> f32` conversions below cannot lose
// meaningful precision.
#[allow(clippy::cast_precision_loss)]
fn column_width_from_parts(column: &ColumnMeta, max_body_chars: usize) -> Pixels {
    let header_chars = column.name.chars().count() + column.type_name.chars().count();
    let header_width = theme::CELL_PADDING_X * 2.0
        + header_chars as f32 * theme::CELL_CHAR_WIDTH
        + theme::TYPE_TAG_EXTRA_WIDTH;

    let body_width = theme::CELL_PADDING_X * 2.0 + max_body_chars as f32 * theme::CELL_CHAR_WIDTH;

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
    use std::time::Duration;

    use gpui::AppContext as _;
    use zsql_core::{ColumnMeta, ResultSet, Row, Value};

    use super::{
        SessionState, column_width_from_parts, row_number_column_width, status_indicator,
        status_metrics,
    };
    use crate::session::Session;
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
    fn row_number_width_grows_with_digit_count() {
        let small = row_number_column_width(9);
        let large = row_number_column_width(1_000_000);
        assert!(f32::from(large) > f32::from(small));
    }

    #[test]
    fn status_indicator_maps_each_state_to_its_dot_color_and_label() {
        assert_eq!(
            status_indicator(&SessionState::Empty),
            (theme::FAINT, "Not connected")
        );
        assert_eq!(
            status_indicator(&SessionState::Connecting),
            (theme::STATUS_CONNECTING, "Connecting…")
        );
        assert_eq!(
            status_indicator(&SessionState::Connected),
            (theme::TEAL, "Connected")
        );
        assert_eq!(
            status_indicator(&SessionState::Running),
            (theme::TEAL, "Running…")
        );
        assert_eq!(
            status_indicator(&SessionState::Results(Duration::from_millis(1))),
            (theme::TEAL, "Connected")
        );
        assert_eq!(
            status_indicator(&SessionState::Error("boom".to_owned())),
            (theme::STATUS_ERROR, "Error")
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
