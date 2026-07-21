//! The results grid: a virtualized table view over a `Session`'s current
//! [`SessionState`] and accumulated result set

use std::ops::Range;

use gpui::{
    AnyElement, App, Context, Div, Entity, Pixels, Render, SharedString, Window, div, prelude::*,
    px, rgb,
};
use zsql_core::{ColumnMeta, ResultSet, RowCount};
use zsql_ui::grid;
use zsql_ui::table::{Gutter, RowNumberStyle, Table, TableColumn, TableRow, TableState, measure};
use zsql_ui::theme::{ActiveTheme, Theme};

use super::format::{ValueKind, format_value};
use super::theme;
use crate::session::{LivenessState, Session, SessionState};

/// A tab's captured query outcome: the label, lifecycle state, and result
/// set a [`ResultsView`] shows while that tab (rather than the live
/// `Session`) is what it is displaying. Captured once a tab's own run
/// reaches a terminal state, so switching back to that tab later restores
/// exactly what it last produced instead of whatever a different tab most
/// recently ran.
#[derive(Debug, Clone)]
pub struct ResultsSnapshot {
    pub source_label: SharedString,
    pub state: SessionState,
    pub result: ResultSet,
}

/// A virtualized results grid, driven by a `Session` entity.
pub struct ResultsView {
    session: Entity<Session>,
    source_label: SharedString,
    /// `Some` while this view is frozen to a specific tab's captured
    /// [`ResultsSnapshot`] instead of following `session` live -- e.g. the
    /// active tab is not the one `session` is currently running a query
    /// for. `None` (the default) means every render reads straight off
    /// `session`.
    frozen: Option<ResultsSnapshot>,
    column_widths: Vec<Pixels>,
    /// Per-column max formatted-text char count seen so far
    column_max_body_chars: Vec<usize>,
    /// How many of `session.result().rows` have already been folded into
    /// `column_max_body_chars`
    folded_row_count: usize,
    /// The grid's mechanical (scroll/drag) state, composed via
    /// `zsql_ui::table`.
    table_state: Entity<TableState>,
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
            frozen: None,
            column_widths: Vec::new(),
            column_max_body_chars: Vec::new(),
            folded_row_count: 0,
            table_state: cx.new(TableState::new),
        };
        view.sync_dimensions(cx);
        view
    }

    /// Follow `session`'s state/result live under `source_label`, e.g. for
    /// the tab that `session` is currently running a query for. Every
    /// render reads straight off `session` until the next
    /// [`ResultsView::show_snapshot`] or `show_live` call.
    pub fn show_live(&mut self, source_label: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.source_label = source_label.into();
        self.frozen = None;
        self.reset_dimension_cache();
        self.sync_dimensions(cx);
        cx.notify();
    }

    /// Freeze the grid to `snapshot` instead of following `session` live,
    /// e.g. when switching to a tab that is not the one `session` is
    /// currently running a query for.
    pub fn show_snapshot(&mut self, snapshot: ResultsSnapshot, cx: &mut Context<Self>) {
        self.source_label = snapshot.source_label.clone();
        self.frozen = Some(snapshot);
        self.reset_dimension_cache();
        self.sync_dimensions(cx);
        cx.notify();
    }

    /// Clear the incrementally-folded column-width cache, e.g. right before
    /// switching to a differently-shaped result set: the next
    /// `sync_dimensions` call must recompute widths from that result set's
    /// own columns/rows rather than folding onto stale per-column maxima
    /// left over from whatever this view was showing before.
    fn reset_dimension_cache(&mut self) {
        self.column_widths = Vec::new();
        self.column_max_body_chars = Vec::new();
        self.folded_row_count = 0;
    }

    /// The result set this view currently renders: `session`'s live result
    /// while [`ResultsView::frozen`] is `None`, else the frozen snapshot's.
    fn effective_result<'a>(&'a self, cx: &'a App) -> &'a ResultSet {
        match &self.frozen {
            Some(snapshot) => &snapshot.result,
            None => self.session.read(cx).result(),
        }
    }

    /// The lifecycle state this view currently renders: `session`'s live
    /// state while [`ResultsView::frozen`] is `None`, else the frozen
    /// snapshot's.
    fn effective_state<'a>(&'a self, cx: &'a App) -> &'a SessionState {
        match &self.frozen {
            Some(snapshot) => &snapshot.state,
            None => self.session.read(cx).state(),
        }
    }

    /// Bring `column_widths` up to date with the session's current result
    /// set, folding only the rows that streamed in since the last call.
    #[tracing::instrument(name = "results_sync_dimensions", skip_all)]
    fn sync_dimensions(&mut self, cx: &mut Context<Self>) {
        // Matched directly on the `frozen` field (rather than through the
        // `effective_result` method) so the borrow checker sees this as
        // borrowing only `self.frozen`, leaving `self.column_widths` and
        // the other fields assigned below free to borrow mutably in the
        // same call -- routing through a `&self` method would borrow all
        // of `self` and block those assignments.
        let result: &ResultSet = match &self.frozen {
            Some(snapshot) => &snapshot.result,
            None => self.session.read(cx).result(),
        };

        if result.columns.len() != self.column_max_body_chars.len() {
            self.column_max_body_chars = vec![0; result.columns.len()];
            self.folded_row_count = 0;
        }

        let rows_folded_this_call = result.rows.len().saturating_sub(self.folded_row_count);
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

        let table_style = Self::table_style(cx.theme());
        self.column_widths = result
            .columns
            .iter()
            .zip(self.column_max_body_chars.iter())
            .map(|(column, &max_body_chars)| {
                column_width_from_parts(column, max_body_chars, &table_style)
            })
            .collect();

        tracing::debug!(
            column_count = result.columns.len(),
            rows_folded_this_call,
            total_folded_row_count = self.folded_row_count,
            "remeasured results grid column widths"
        );
    }

    /// The results header bar: row count + source/relation label.
    fn render_bar(&self, cx: &Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let count_text = match self.effective_state(cx) {
            SessionState::Results(_) | SessionState::Running => {
                self.effective_result(cx).rows.len().to_string()
            }
            SessionState::Truncating { rows } | SessionState::Truncated { rows, .. } => format!(
                "{rows} (truncated at {})",
                self.effective_result(cx).rows.len()
            ),
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
            .bg(rgb(colors.bg_panel))
            .border_b_1()
            .border_color(rgb(colors.border_soft))
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
                            .text_color(rgb(colors.text_primary))
                            .child("Results"),
                    )
                    .child(
                        div()
                            .font_family(&cx.theme().fonts.data)
                            .text_color(rgb(colors.accent))
                            .child(count_text),
                    ),
            )
            .child(
                div()
                    .font_family(&cx.theme().fonts.data)
                    .text_size(px(theme::RESULTS_META_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(self.source_label.clone()),
            )
    }

    /// The main content area: the virtualized grid when results are
    /// available, or a centered prompt/status message otherwise
    fn render_body(&mut self, cx: &mut Context<Self>) -> Div {
        let active_theme = cx.theme();
        let state = self.effective_state(cx).clone();
        let has_columns = !self.effective_result(cx).columns.is_empty();

        match state {
            SessionState::Results(_)
            | SessionState::Truncating { .. }
            | SessionState::Truncated { .. } => self.render_grid(cx),
            // Once the streaming query's `Columns` event has arrived there
            // is a real (if partial) result set to paint, so switch to the
            // grid immediately rather than waiting for `Done`
            SessionState::Running if has_columns => self.render_grid(cx),
            SessionState::Empty => Self::render_placeholder(
                active_theme.colors.text_tertiary,
                "No connection configured",
                "Set DATABASE_URL or connection.default_url in your zsql config, then restart.",
                active_theme,
            ),
            SessionState::Connecting => Self::render_placeholder(
                active_theme.colors.text_tertiary,
                "Connecting…",
                "Establishing a connection to the configured database.",
                active_theme,
            ),
            SessionState::Connected => Self::render_placeholder(
                active_theme.colors.text_tertiary,
                "Connected",
                "Run a query to see results here.",
                active_theme,
            ),
            SessionState::Running => Self::render_placeholder(
                active_theme.colors.text_tertiary,
                "Running query…",
                "Streaming results from the database.",
                active_theme,
            ),
            SessionState::Error(message) => Self::render_placeholder(
                active_theme.colors.status_error,
                "Query failed",
                &message,
                active_theme,
            ),
        }
    }

    /// A centered title + detail message shown in place of the grid for any
    /// non-`Results` state.
    fn render_placeholder(
        title_color: u32,
        title: &str,
        detail: &str,
        active_theme: &Theme,
    ) -> Div {
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
                    .text_color(rgb(active_theme.colors.text_tertiary))
                    .child(detail.to_owned()),
            )
    }

    /// The two-pane virtualized grid (pinned row numbers + horizontally
    /// scrolling data columns), built by composing `zsql_ui::table::Table`.
    fn render_grid(&mut self, cx: &mut Context<Self>) -> Div {
        let row_count = self.effective_result(cx).rows.len();
        let active_theme = cx.theme();
        let columns = self.build_columns(cx);

        Table::new("results-grid", &self.table_state)
            .style(Self::table_style(active_theme))
            .columns(columns)
            .row_count(row_count)
            .gutter(Gutter::RowNumbers(RowNumberStyle {
                char_width: theme::CELL_CHAR_WIDTH,
                min_width: theme::ROW_NUMBER_MIN_WIDTH,
            }))
            .rows(Self::render_data_row_cells)
            .render(cx)
    }

    /// The one `TableStyle` both `render_grid`'s live `Table` and
    /// `column_width_from_parts`'s width estimate use, so a column's
    /// measured width can never drift from the padding it is actually
    /// rendered with.
    fn table_style(active_theme: &Theme) -> zsql_ui::table::TableStyle {
        zsql_ui::table::TableStyle::themed(active_theme)
    }

    /// The data pane's columns: each column's cached width plus its header
    /// content (name + type-tag badge).
    fn build_columns(&self, cx: &Context<Self>) -> Vec<TableColumn> {
        let active_theme = cx.theme();
        let columns: &[ColumnMeta] = &self.effective_result(cx).columns;
        columns
            .iter()
            .zip(self.column_widths.iter())
            .map(|(column, &width)| TableColumn::new(width, column_header(column, active_theme)))
            .collect()
    }

    /// Render the data-cell rows in `range` for the data pane's virtualized
    /// list
    fn render_data_row_cells(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<TableRow> {
        let active_theme = cx.theme();
        let rows: &[zsql_core::Row] = &self.effective_result(cx).rows;

        range
            .map(|ix| {
                let cells = rows
                    .get(ix)
                    .map(|row| {
                        row.0
                            .iter()
                            .map(|value| {
                                let formatted = format_value(value);
                                let is_null = formatted.kind == ValueKind::Null;
                                div()
                                    .text_color(rgb(kind_color(formatted.kind, active_theme)))
                                    .when(is_null, gpui::prelude::Styled::italic)
                                    .child(formatted.text)
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                TableRow::new(cells)
            })
            .collect()
    }

    /// The bottom connection/status bar: connection state + label, row
    /// count, and elapsed query time
    fn render_status_bar(&self, cx: &Context<Self>) -> Div {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let session = self.session.read(cx);
        // Liveness is the connection's real-time health, independent of
        // which tab is displayed, so it is read straight off `session`
        // rather than through `effective_state`: a dead connection must
        // show as disconnected regardless of whether the active tab is
        // frozen to an older, still-successful snapshot.
        let liveness = session.liveness().clone();
        let state = self.effective_state(cx);
        let (dot_color, label) = status_indicator(state, &liveness, active_theme);

        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .gap_4()
            .h(theme::STATUS_BAR_HEIGHT)
            .px_3()
            .bg(rgb(colors.bg_panel))
            .border_t_1()
            .border_color(rgb(colors.border))
            .font_family(&cx.theme().fonts.data)
            .text_size(px(theme::STATUS_BAR_TEXT_SIZE))
            .text_color(rgb(colors.text_secondary))
            .child(grid::status_dot(dot_color))
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(colors.text_primary))
                    .child(label),
            );

        if let Some((rows_text, elapsed_text)) =
            status_metrics(state, self.effective_result(cx).rows.len())
        {
            bar = bar.child(rows_text).child(elapsed_text);
        }

        if let Some(total_row_count_text) = format_total_row_count(session.row_count()) {
            bar = bar.child(total_row_count_text);
        }

        if let SessionState::Error(message) = state {
            bar = bar.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(colors.status_error))
                    .child(message.clone()),
            );
        }

        bar
    }
}

/// Test-only accessors used by `ui::sidebar`'s and `ui::tabs`'s tests
#[cfg(test)]
impl ResultsView {
    pub(crate) fn source_label_for_test(&self) -> &str {
        &self.source_label
    }

    /// Whether this view is currently frozen to a captured
    /// [`ResultsSnapshot`] (see [`ResultsView::show_snapshot`]) rather than
    /// following `session` live.
    pub(crate) fn is_frozen_for_test(&self) -> bool {
        self.frozen.is_some()
    }
}

impl Render for ResultsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(cx.theme().colors.bg_app))
            .child(self.render_bar(cx))
            .child(self.render_body(cx))
            .child(self.render_status_bar(cx))
    }
}

/// A data column's header content: its name plus a type-name badge.
fn column_header(column: &ColumnMeta, active_theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_baseline()
        .gap_2()
        .child(
            div()
                .text_color(rgb(active_theme.colors.text_primary))
                .child(column.name.clone()),
        )
        .child(grid::type_tag(&column.type_name, active_theme))
        .into_any_element()
}

/// The bottom status bar's dot color and label for `state`. A `liveness` of
/// [`LivenessState::Unreachable`] overrides every state's normal indicator
/// with a distinct "Disconnected" one, since the probe result is
/// independent of (and can contradict) whatever `state` currently holds -
/// for instance a query can still be `Running` against a connection the
/// probe has just found unreachable.
fn status_indicator(
    state: &SessionState,
    liveness: &LivenessState,
    active_theme: &Theme,
) -> (u32, &'static str) {
    let colors = active_theme.colors;
    if matches!(liveness, LivenessState::Unreachable(_)) {
        return (theme::status_disconnected(active_theme), "Disconnected");
    }
    match state {
        SessionState::Empty => (colors.text_tertiary, "Not connected"),
        SessionState::Connecting => (colors.status_warn, "Connecting…"),
        SessionState::Connected | SessionState::Results(_) => (colors.accent, "Connected"),
        SessionState::Running => (colors.accent, "Running…"),
        SessionState::Truncating { .. } => (colors.status_limited, "Running… (truncated)"),
        SessionState::Truncated { .. } => (colors.status_limited, "Truncated"),
        SessionState::Error(_) => (colors.status_error, "Error"),
    }
}

/// The bottom status bar's "N rows" / "N ms" text for `state`, given
/// `row_count`. `None` for any state with no completed query to
/// report timing/row-count for
fn status_metrics(state: &SessionState, row_count: usize) -> Option<(String, String)> {
    match state {
        SessionState::Results(elapsed) => Some((
            format!("{row_count} rows"),
            format!("{} ms", elapsed.as_millis()),
        )),
        SessionState::Truncated { elapsed, rows } => Some((
            format!("Result limited to {row_count} rows ({rows} total)"),
            format!("{} ms", elapsed.as_millis()),
        )),
        _ => None,
    }
}

/// Groups digits every three places when rendering a total row count in the
/// status bar.
pub(crate) const THOUSANDS_SEPARATOR: char = ',';

/// Appended after the number when a total row count is a planner estimate
/// rather than an exact count, so the distinction reads clearly even without
/// color.
const ESTIMATED_ROW_COUNT_SUFFIX: &str = " (estimated)";

/// Labels the whole-relation total so it never reads as the streamed-rows
/// metric beside it. That metric renders as `"200 rows"` (capped at the
/// preview limit), so the total drops the bare `"rows"` word for `"total"`
/// and the two can no longer be confused.
const TOTAL_ROW_COUNT_LABEL: &str = " total";

/// The previewed relation's total row count, for the status bar: e.g.
/// `"1,234 total"` for an exact count, or `"~1,234,567 total (estimated)"`
/// when the driver could only provide a planner estimate. `None` when no
/// count has been fetched (no preview yet, still fetching, or the fetch
/// failed), so the caller can omit the segment entirely.
fn format_total_row_count(row_count: Option<RowCount>) -> Option<String> {
    let row_count = row_count?;
    let grouped = group_thousands(row_count.value());
    Some(if row_count.is_estimated() {
        format!(
            "{}{grouped}{TOTAL_ROW_COUNT_LABEL}{ESTIMATED_ROW_COUNT_SUFFIX}",
            zsql_core::ESTIMATE_MARKER
        )
    } else {
        format!("{grouped}{TOTAL_ROW_COUNT_LABEL}")
    })
}

/// Render `n` with [`THOUSANDS_SEPARATOR`] inserted every three digits from
/// the right, e.g. `1234567` -> `"1,234,567"`.
pub(crate) fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push(THOUSANDS_SEPARATOR);
        }
        grouped.push(ch);
    }
    grouped.chars().rev().collect()
}

/// The text color for a formatted cell's semantic kind.
fn kind_color(kind: ValueKind, active_theme: &Theme) -> u32 {
    let colors = active_theme.colors;
    match kind {
        ValueKind::Null => colors.value_null,
        ValueKind::Bool => colors.value_bool,
        ValueKind::Number => colors.value_number,
        ValueKind::Text => colors.value_text,
        ValueKind::Json => colors.value_json,
        ValueKind::Timestamp => colors.value_timestamp,
        ValueKind::Bytes => colors.value_bytes,
        ValueKind::Unknown => colors.value_unknown,
    }
}

/// Estimate a column's pixel width from its header (name + type tag) and
/// `max_body_chars`, using `style`'s cell padding -- the same `TableStyle`
/// the live grid renders with, so the estimate and the render never drift.
fn column_width_from_parts(
    column: &ColumnMeta,
    max_body_chars: usize,
    style: &zsql_ui::table::TableStyle,
) -> Pixels {
    let header_chars = column.name.chars().count() + column.type_name.chars().count();
    measure::column_width(
        header_chars,
        max_body_chars,
        style,
        measure::ColumnWidthLimits {
            char_width: theme::CELL_CHAR_WIDTH,
            header_extra_width: theme::TYPE_TAG_EXTRA_WIDTH,
            min_width: theme::MIN_COLUMN_WIDTH,
            max_width: theme::MAX_COLUMN_WIDTH,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::AppContext as _;
    use zsql_core::{ColumnMeta, ResultSet, Row, RowCount, Value};

    use super::{
        ResultsView, SessionState, ValueKind, column_width_from_parts, format_total_row_count,
        kind_color, status_indicator, status_metrics,
    };

    use crate::session::{LivenessState, Session};
    use crate::ui::theme;
    use zsql_ui::theme::Theme;

    fn column(name: &str, type_name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            nullable: false,
        }
    }

    #[test]
    fn column_width_from_parts_grows_for_a_longer_type_name() {
        let style = ResultsView::table_style(&Theme::default());
        let short_type = column("id", "int8");
        let long_type = column("id", "timestamp with time zone");

        let narrow = column_width_from_parts(&short_type, 0, &style);
        let wide = column_width_from_parts(&long_type, 0, &style);

        assert!(
            f32::from(wide) > f32::from(narrow),
            "a longer type_name must widen the column even with an identical name and no body \
             content: narrow={narrow:?} wide={wide:?}"
        );
    }

    #[test]
    fn column_width_from_parts_clamps_at_the_configured_minimum() {
        let style = ResultsView::table_style(&Theme::default());
        let width = column_width_from_parts(&column("a", "b"), 0, &style);
        assert!(f32::from(width) >= theme::MIN_COLUMN_WIDTH);
    }

    #[test]
    fn column_width_from_parts_clamps_at_the_configured_maximum() {
        let style = ResultsView::table_style(&Theme::default());
        let width =
            column_width_from_parts(&column(&"x".repeat(500), &"y".repeat(500)), 5_000, &style);
        assert!((f32::from(width) - theme::MAX_COLUMN_WIDTH).abs() < f32::EPSILON);
    }

    #[test]
    fn kind_color_maps_every_value_kind_to_its_named_color_role() {
        let theme = Theme::default();
        let colors = theme.colors;
        assert_eq!(kind_color(ValueKind::Null, &theme), colors.value_null);
        assert_eq!(kind_color(ValueKind::Bool, &theme), colors.value_bool);
        assert_eq!(kind_color(ValueKind::Number, &theme), colors.value_number);
        assert_eq!(kind_color(ValueKind::Text, &theme), colors.value_text);
        assert_eq!(kind_color(ValueKind::Json, &theme), colors.value_json);
        assert_eq!(
            kind_color(ValueKind::Timestamp, &theme),
            colors.value_timestamp
        );
        assert_eq!(kind_color(ValueKind::Bytes, &theme), colors.value_bytes);
        assert_eq!(kind_color(ValueKind::Unknown, &theme), colors.value_unknown);
    }

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
    fn status_indicator_maps_each_state_to_its_dot_color_and_label() {
        let active_theme = Theme::default();
        let colors = active_theme.colors;
        assert_eq!(
            status_indicator(&SessionState::Empty, &LivenessState::Unknown, &active_theme),
            (colors.text_tertiary, "Not connected")
        );
        assert_eq!(
            status_indicator(
                &SessionState::Connecting,
                &LivenessState::Unknown,
                &active_theme
            ),
            (colors.status_warn, "Connecting…")
        );
        assert_eq!(
            status_indicator(
                &SessionState::Connected,
                &LivenessState::Healthy,
                &active_theme
            ),
            (colors.accent, "Connected")
        );
        assert_eq!(
            status_indicator(
                &SessionState::Running,
                &LivenessState::Healthy,
                &active_theme
            ),
            (colors.accent, "Running…")
        );
        assert_eq!(
            status_indicator(
                &SessionState::Results(Duration::from_millis(1)),
                &LivenessState::Healthy,
                &active_theme
            ),
            (colors.accent, "Connected")
        );
        assert_eq!(
            status_indicator(
                &SessionState::Error("boom".to_owned()),
                &LivenessState::Unknown,
                &active_theme
            ),
            (colors.status_error, "Error")
        );
        let limited = status_indicator(
            &SessionState::Truncated {
                elapsed: Duration::from_millis(1),
                rows: 100,
            },
            &LivenessState::Healthy,
            &active_theme,
        );
        assert_eq!(limited, (colors.status_limited, "Truncated"));
        assert_ne!(
            limited,
            (colors.accent, "Connected"),
            "Limited must not be indistinguishable from a normal completed result"
        );
        assert_ne!(
            limited,
            (colors.status_error, "Error"),
            "Limited must not be indistinguishable from a query error"
        );
    }

    #[test]
    fn status_indicator_shows_disconnected_regardless_of_session_state_when_liveness_is_unreachable()
     {
        let active_theme = Theme::default();
        let unreachable = LivenessState::Unreachable("connection reset".to_owned());
        for state in [
            SessionState::Connected,
            SessionState::Running,
            SessionState::Results(Duration::from_millis(1)),
        ] {
            assert_eq!(
                status_indicator(&state, &unreachable, &active_theme),
                (theme::status_disconnected(&active_theme), "Disconnected"),
                "expected a Disconnected indicator regardless of state {state:?}"
            );
        }
    }

    #[test]
    fn status_indicator_treats_a_healthy_or_unknown_liveness_as_no_override() {
        let active_theme = Theme::default();
        assert_eq!(
            status_indicator(
                &SessionState::Connected,
                &LivenessState::Healthy,
                &active_theme
            ),
            status_indicator(
                &SessionState::Connected,
                &LivenessState::Unknown,
                &active_theme
            ),
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

    #[test]
    fn status_metrics_reads_as_truncated_for_a_limited_result() {
        let state = SessionState::Truncated {
            elapsed: Duration::from_millis(7),
            rows: 5_000,
        };
        assert_eq!(
            status_metrics(&state, 100),
            Some((
                "Result limited to 100 rows (5000 total)".to_owned(),
                "7 ms".to_owned()
            )),
            "the row count shown must be the actual number streamed, with the limit accurate"
        );
    }

    #[test]
    fn format_total_row_count_renders_nothing_when_absent() {
        assert_eq!(format_total_row_count(None), None);
    }

    #[test]
    fn format_total_row_count_renders_an_exact_count_with_thousands_separators() {
        assert_eq!(
            format_total_row_count(Some(RowCount::Exact(1_234))),
            Some("1,234 total".to_owned())
        );
    }

    #[test]
    fn format_total_row_count_renders_an_estimated_count_marked_distinctly() {
        assert_eq!(
            format_total_row_count(Some(RowCount::Estimated(1_234_567))),
            Some("~1,234,567 total (estimated)".to_owned())
        );
    }

    #[test]
    fn format_total_row_count_labels_the_total_distinctly_from_the_streamed_rows_metric() {
        // The streamed-rows metric reads "N rows"; the total must not, or the
        // two segments are indistinguishable in the status bar.
        let exact = format_total_row_count(Some(RowCount::Exact(1_234))).unwrap();
        let estimated = format_total_row_count(Some(RowCount::Estimated(1_234))).unwrap();
        assert!(!exact.ends_with(" rows"));
        assert!(exact.contains("total"));
        assert!(estimated.contains("total"));
    }

    #[test]
    fn format_total_row_count_handles_small_counts_with_no_separator_needed() {
        assert_eq!(
            format_total_row_count(Some(RowCount::Exact(7))),
            Some("7 total".to_owned())
        );
        assert_eq!(
            format_total_row_count(Some(RowCount::Exact(0))),
            Some("0 total".to_owned())
        );
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
    fn renders_with_every_row_count_variant_without_panicking(cx: &mut gpui::TestAppContext) {
        for row_count in [
            None,
            Some(RowCount::Exact(1_234)),
            Some(RowCount::Estimated(1_234_567)),
        ] {
            let state = SessionState::Results(Duration::from_millis(8));
            let session = cx.new(|_cx| {
                let mut session = Session::new_for_render_test(state, sample_result());
                session.set_row_count_for_test(row_count);
                session
            });
            cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
        }
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

    /// The rows streamed before a query was cancelled at the configured
    /// limit must stay visible: `Limited` renders the grid, not a
    /// placeholder, exactly like a normal completed result.
    #[gpui::test]
    fn renders_the_grid_for_a_limited_result_keeping_rows_visible(cx: &mut gpui::TestAppContext) {
        let mut result = sample_result();
        result.rows.truncate(1);
        let state = SessionState::Truncated {
            elapsed: Duration::from_millis(5),
            rows: 1,
        };
        let session = cx.new(|_cx| Session::new_for_render_test(state, result));

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
        let session = cx.new(|_cx| {
            Session::new_for_render_test(
                SessionState::Results(std::time::Duration::from_millis(1)),
                result,
            )
        });
        let (view, vcx) =
            cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
        vcx.run_until_parked();

        view.read_with(vcx, |v, app| {
            assert!(
                v.table_state
                    .read(app)
                    .scroll()
                    .read(app)
                    .vertical_visible(),
                "the vertical scrollbar must be visible for 400 overflowing rows"
            );
        });
    }

    /// A result set with enough wide columns to overflow any reasonable
    /// viewport must show its horizontal scrollbar without user
    /// interaction, mirroring the equivalent vertical-overflow test.
    #[gpui::test]
    fn horizontal_scrollbar_is_shown_after_the_first_frame_when_columns_overflow(
        cx: &mut gpui::TestAppContext,
    ) {
        let columns: Vec<ColumnMeta> = (0..40)
            .map(|index| ColumnMeta {
                name: format!("a_fairly_long_column_name_{index}"),
                type_name: "text".to_owned(),
                nullable: true,
            })
            .collect();
        let row = Row(columns
            .iter()
            .map(|_| Value::Text("a moderately long cell value".to_owned()))
            .collect());
        let result = ResultSet {
            columns,
            rows: vec![row],
            affected: None,
            notices: Vec::new(),
        };
        let session = cx.new(|_cx| {
            Session::new_for_render_test(
                SessionState::Results(std::time::Duration::from_millis(1)),
                result,
            )
        });
        let (view, vcx) =
            cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
        vcx.run_until_parked();

        view.read_with(vcx, |v, app| {
            assert!(
                v.table_state
                    .read(app)
                    .scroll()
                    .read(app)
                    .horizontal_visible(),
                "the horizontal scrollbar must be visible for 40 overflowing wide columns"
            );
        });
    }

    /// A result set whose columns already fit inside the viewport must not
    /// show a horizontal scrollbar, mirroring the vertical scrollbar's
    /// hidden contract when rows already fit.
    #[gpui::test]
    fn horizontal_scrollbar_is_absent_when_columns_fit_the_viewport(cx: &mut gpui::TestAppContext) {
        let session = cx.new(|_cx| {
            Session::new_for_render_test(
                SessionState::Results(std::time::Duration::from_millis(1)),
                sample_result(),
            )
        });
        let (view, vcx) =
            cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
        vcx.run_until_parked();

        view.read_with(vcx, |v, app| {
            assert!(
                !v.table_state
                    .read(app)
                    .scroll()
                    .read(app)
                    .horizontal_visible(),
                "the horizontal scrollbar must be absent when columns already fit the viewport"
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
