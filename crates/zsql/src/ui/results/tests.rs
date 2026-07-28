use std::time::Duration;

use gpui::{
    AppContext as _, Context, Focusable as _, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, point, px,
};
use zsql_core::{ColumnMeta, ResultSet, Row, RowCount, Value};

use super::{
    CellDown, CellLeft, CellRight, CellUp, Copy, ResultsView, SessionState, ViewMode,
    column_width_from_parts, format_total_row_count, results_bar_count_text, status_indicator,
    status_metrics,
};

use crate::session::{LivenessState, Session};
use crate::ui::theme;
use zsql_ui::scrollable::horizontal_thumb_debug_selector;
use zsql_ui::table::{body_first_cell_debug_selector, column_resize_handle_debug_selector};
use zsql_ui::theme::Theme;

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

    pub(crate) fn view_mode_for_test(&self) -> ViewMode {
        self.view_mode
    }

    pub(crate) fn set_view_mode_for_test(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        self.set_view_mode(mode, cx);
    }
}

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
    let width = column_width_from_parts(&column(&"x".repeat(500), &"y".repeat(500)), 5_000, &style);
    assert!((f32::from(width) - theme::MAX_COLUMN_WIDTH).abs() < f32::EPSILON);
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
        (colors.status_warn, "Connecting...")
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
        (colors.accent, "Running...")
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
fn status_indicator_shows_disconnected_regardless_of_session_state_when_liveness_is_unreachable() {
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
        status_metrics(&state, 1, "rows"),
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
            status_metrics(&state, 5, "rows"),
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
        status_metrics(&state, 100, "rows"),
        Some((
            "Result limited to 100 rows (5000 total)".to_owned(),
            "7 ms".to_owned()
        )),
        "the row count shown must be the actual number streamed, with the limit accurate"
    );
}

#[test]
fn status_metrics_uses_the_given_unit_word_for_the_count_text() {
    let state = SessionState::Results(Duration::from_millis(3));
    assert_eq!(
        status_metrics(&state, 17, "lines"),
        Some(("17 lines".to_owned(), "3 ms".to_owned())),
        "the Text view's status metric must read lines, not rows"
    );
}

// ---- connection_status: the status bar's real-session-state wiring --

#[gpui::test]
fn connection_status_shows_connecting_for_a_frozen_tab_while_the_session_is_switching(
    cx: &mut gpui::TestAppContext,
) {
    let session =
        cx.new(|_cx| Session::new_for_render_test(SessionState::Connecting, ResultSet::default()));
    let (view, vcx) =
        cx.add_window_view(|_window, cx| ResultsView::new(session, "public.orders", cx));

    view.update(vcx, |view, cx| {
        view.show_snapshot(
            super::ResultsSnapshot {
                source_label: "public.orders".into(),
                state: SessionState::Results(Duration::from_millis(42)),
                result: sample_result(),
            },
            cx,
        );
    });

    view.update(vcx, |view, cx| {
        let (_dot_color, label, error) = view.connection_status(cx);
        assert_eq!(
            label, "Connecting...",
            "switching sessions must show Connecting, not the frozen tab's stale Connected"
        );
        assert!(error.is_none());

        let metrics = status_metrics(
            view.effective_state(cx),
            view.effective_result(cx).rows.len(),
            "rows",
        );
        assert_eq!(
            metrics,
            Some(("2 rows".to_owned(), "42 ms".to_owned())),
            "the frozen tab's own row/elapsed metrics must be unaffected by the switch"
        );
    });
}

#[gpui::test]
fn connection_status_never_shows_connected_for_a_failed_or_timed_out_connect(
    cx: &mut gpui::TestAppContext,
) {
    let session = cx.new(|_cx| {
        Session::new_for_render_test(
            SessionState::Error("liveness probe timed out after 3000ms".to_owned()),
            ResultSet::default(),
        )
    });
    let (view, vcx) =
        cx.add_window_view(|_window, cx| ResultsView::new(session, "public.orders", cx));

    // The tab is still frozen to a prior, successful run: the failed
    // connect must not be masked by it.
    view.update(vcx, |view, cx| {
        view.show_snapshot(
            super::ResultsSnapshot {
                source_label: "public.orders".into(),
                state: SessionState::Results(Duration::from_millis(9)),
                result: sample_result(),
            },
            cx,
        );
    });

    view.update(vcx, |view, cx| {
        let (_dot_color, label, error) = view.connection_status(cx);
        assert_ne!(label, "Connected");
        assert_eq!(label, "Error");
        assert_eq!(
            error,
            Some("liveness probe timed out after 3000ms".to_owned())
        );
    });
}

#[gpui::test]
fn connection_status_shows_connected_immediately_after_connect_without_waiting_for_a_probe(
    cx: &mut gpui::TestAppContext,
) {
    // A freshly successful connect: liveness has not had time to reach
    // Healthy yet (no probe has ticked), and there is no frozen tab.
    let session =
        cx.new(|_cx| Session::new_for_render_test(SessionState::Connected, ResultSet::default()));
    let (view, vcx) =
        cx.add_window_view(|_window, cx| ResultsView::new(session, "public.orders", cx));

    view.update(vcx, |view, cx| {
        let (_dot_color, label, error) = view.connection_status(cx);
        assert_eq!(label, "Connected");
        assert!(error.is_none());
    });
}

#[gpui::test]
fn connection_status_shows_disconnected_when_the_session_liveness_is_unreachable(
    cx: &mut gpui::TestAppContext,
) {
    // Query results were previously frozen into the tab while the
    // connection was healthy; the session's liveness has since gone
    // Unreachable and must override the stale frozen indicator.
    let session =
        cx.new(|_cx| Session::new_for_render_test(SessionState::Connected, ResultSet::default()));
    session.update(cx, |session, _cx| {
        session.set_liveness_for_test(LivenessState::Unreachable("connection reset".into()));
    });
    let (view, vcx) =
        cx.add_window_view(|_window, cx| ResultsView::new(session, "public.orders", cx));

    view.update(vcx, |view, cx| {
        view.show_snapshot(
            super::ResultsSnapshot {
                source_label: "public.orders".into(),
                state: SessionState::Results(Duration::from_millis(9)),
                result: sample_result(),
            },
            cx,
        );
    });

    view.update(vcx, |view, cx| {
        let (_dot_color, label, error) = view.connection_status(cx);
        assert_eq!(
            label, "Disconnected",
            "an unreachable liveness must override a frozen tab's stale Connected indicator"
        );
        assert!(error.is_none());
    });
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
    let (view, vcx) =
        cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
    assert_eq!(
        view.read_with(vcx, |v, _app| v.source_label_for_test().to_owned()),
        "public.orders"
    );
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
        let (view, vcx) =
            cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Grid
        );
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
        let (view, vcx) =
            cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
        assert_eq!(
            view.read_with(vcx, |v, _app| v.source_label_for_test().to_owned()),
            "public.orders"
        );
    }
}

#[gpui::test]
fn only_the_empty_state_paints_the_add_connection_control(cx: &mut gpui::TestAppContext) {
    for state in [
        SessionState::Connecting,
        SessionState::Connected,
        SessionState::Running,
        SessionState::Truncated {
            elapsed: Duration::from_millis(5),
            rows: 1,
        },
        SessionState::Error("connection refused".to_owned()),
    ] {
        let session = cx.new(|_cx| Session::new_for_render_test(state, sample_result()));
        let (_view, vcx) =
            cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
        vcx.run_until_parked();

        assert!(
            vcx.debug_bounds(super::empty_state::ADD_CONNECTION_ID)
                .is_none(),
            "only the Empty-state body should paint the Add connection control"
        );
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

    let session = cx.new(|_cx| Session::new_for_render_test(SessionState::Running, first_batch));
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

// -- column resize -----------------------------------------------------

#[gpui::test]
fn dragging_a_column_header_border_resizes_only_that_column_without_touching_selection(
    cx: &mut gpui::TestAppContext,
) {
    let (view, vcx) = view_with_results(cx, sample_result());
    vcx.run_until_parked();

    let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());
    let (start_width_0, start_width_1) =
        view.read_with(vcx, |v, _app| (v.column_widths[0], v.column_widths[1]));

    // Select a cell (and focus the grid via it) before the drag, so the
    // assertions below actually constrain the drag against a real prior
    // selection/focus rather than trivially observing that dragging did
    // not create one out of a `None` starting point.
    let cell_bounds = vcx
        .debug_bounds(body_first_cell_debug_selector(&table_state))
        .expect("the top-of-viewport body cell must be painted");
    vcx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position: gpui::point(
            cell_bounds.origin.x + px(5.0),
            cell_bounds.origin.y + px(5.0),
        ),
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    vcx.run_until_parked();
    assert_eq!(
        table_state.read_with(vcx, |s, _app| s.focused_cell()),
        Some((0, 0)),
        "the setup click must select row 0, column 0 before the drag begins"
    );
    let selected_cell = table_state.read_with(vcx, |s, _app| s.focused_cell());
    let focused_before = vcx.update(|window, cx| window.focused(cx));

    let handle_bounds = vcx
        .debug_bounds(column_resize_handle_debug_selector(&table_state, 0))
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
        view.read_with(vcx, |v, _app| v.column_widths[0]),
        start_width_0,
        "pressing down on the handle must not itself resize the column"
    );
    assert_eq!(
        table_state.read_with(vcx, |s, _app| s.focused_cell()),
        selected_cell,
        "pressing down on the resize handle must not change the grid's selection"
    );
    assert_eq!(
        vcx.update(|window, cx| window.focused(cx)),
        focused_before,
        "pressing down on the resize handle must not move keyboard focus"
    );

    vcx.simulate_event(MouseMoveEvent {
        position: point(origin.x + px(40.0), origin.y),
        pressed_button: Some(MouseButton::Left),
        modifiers: Modifiers::default(),
    });
    assert_eq!(
        view.read_with(vcx, |v, _app| v.column_widths[0]),
        start_width_0 + px(40.0),
        "dragging the handle by +40px must widen exactly that column by 40px"
    );
    assert_eq!(
        view.read_with(vcx, |v, _app| v.column_widths[1]),
        start_width_1,
        "a different column's width must be untouched by another column's drag"
    );
    assert_eq!(
        table_state.read_with(vcx, |s, _app| s.focused_cell()),
        selected_cell,
        "moving the pointer during the drag must not change the grid's selection"
    );
    assert_eq!(
        vcx.update(|window, cx| window.focused(cx)),
        focused_before,
        "moving the pointer during the drag must not move keyboard focus"
    );

    vcx.simulate_event(MouseUpEvent {
        button: MouseButton::Left,
        position: point(origin.x + px(40.0), origin.y),
        modifiers: Modifiers::default(),
        click_count: 1,
    });
    assert_eq!(
        view.read_with(vcx, |v, _app| v.column_widths[0]),
        start_width_0 + px(40.0),
        "releasing the drag must leave the resized width in place"
    );
    assert_eq!(
        table_state.read_with(vcx, |s, _app| s.focused_cell()),
        selected_cell,
        "starting, performing, or ending a column-border drag must never change the grid's \
         focused/selected cell"
    );
    assert_eq!(
        vcx.update(|window, cx| window.focused(cx)),
        focused_before,
        "ending the drag must not move keyboard focus either"
    );
}

#[gpui::test]
fn dragging_a_column_header_border_past_the_minimum_clamps_exactly_at_it(
    cx: &mut gpui::TestAppContext,
) {
    let (view, vcx) = view_with_results(cx, sample_result());
    vcx.run_until_parked();

    let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());
    let handle_bounds = vcx
        .debug_bounds(column_resize_handle_debug_selector(&table_state, 0))
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
        view.read_with(vcx, |v, _app| v.column_widths[0]),
        px(theme::MIN_COLUMN_WIDTH),
        "dragging far past the configured minimum must clamp exactly at it, not go to zero \
         or negative"
    );
}

#[gpui::test]
fn resizing_a_column_immediately_grows_the_horizontal_scroll_content_extent(
    cx: &mut gpui::TestAppContext,
) {
    // Mirrors `horizontal_scrollbar_is_shown_after_the_first_frame_when_columns_overflow`'s
    // wide-columns setup, so the horizontal thumb is already painted
    // before the drag and its shrinkage afterward reflects a real
    // content-extent change, not one appearing from nothing.
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
        Session::new_for_render_test(SessionState::Results(Duration::from_millis(1)), result)
    });
    let (view, vcx) =
        cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
    vcx.run_until_parked();

    let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());
    let scroll = table_state.read_with(vcx, |s, _app| s.scroll().clone());
    let thumb_width_before = vcx
        .debug_bounds(horizontal_thumb_debug_selector(&scroll))
        .expect("40 overflowing wide columns must already show a horizontal thumb")
        .size
        .width;

    let handle_bounds = vcx
        .debug_bounds(column_resize_handle_debug_selector(&table_state, 0))
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
        position: point(origin.x + px(400.0), origin.y),
        pressed_button: Some(MouseButton::Left),
        modifiers: Modifiers::default(),
    });
    vcx.simulate_event(MouseUpEvent {
        button: MouseButton::Left,
        position: point(origin.x + px(400.0), origin.y),
        modifiers: Modifiers::default(),
        click_count: 1,
    });
    vcx.run_until_parked();

    let thumb_width_after = vcx
        .debug_bounds(horizontal_thumb_debug_selector(&scroll))
        .expect("the horizontal thumb must still be painted after widening one column")
        .size
        .width;

    assert!(
        thumb_width_after < thumb_width_before,
        "widening a column by 400px must immediately recompute the horizontal content \
         extent -- reflected here as a smaller thumb, since a larger content extent behind \
         the same fixed viewport shrinks the thumb's share of the track -- without any \
         extra scroll or window-resize event: before={thumb_width_before:?} \
         after={thumb_width_after:?}"
    );
}

#[gpui::test]
fn a_manually_resized_column_survives_sync_dimensions_as_more_rows_stream_in(
    cx: &mut gpui::TestAppContext,
) {
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
    let session = cx.new(|_cx| Session::new_for_render_test(SessionState::Running, first_batch));
    let session_for_view = session.clone();
    let (view, vcx) =
        cx.add_window_view(|_window, cx| super::ResultsView::new(session_for_view, "t", cx));

    let resized_width = px(275.0);
    view.update_in(vcx, |view, window, cx| {
        view.resize_column(0, resized_width, window, cx);
    });
    assert_eq!(
        view.read_with(vcx, |v, _app| v.column_widths[0]),
        resized_width
    );

    // A second batch arrives with a much longer cell in the same
    // column -- the auto-fit estimate for it would exceed
    // `resized_width`, so any leak of the auto-fit path back into a
    // manually resized column would show up as a width change here.
    session.update(vcx, |session, _cx| {
        session.set_result_for_test(ResultSet {
            columns,
            rows: vec![
                Row(vec![Value::Text("ab".to_owned())]),
                Row(vec![Value::Text(
                    "a much longer value than before, wide enough to grow an auto-fit column"
                        .to_owned(),
                )]),
            ],
            affected: None,
            notices: Vec::new(),
        });
    });
    view.update(vcx, super::ResultsView::sync_dimensions);

    assert_eq!(
        view.read_with(vcx, |v, _app| v.column_widths[0]),
        resized_width,
        "a manually resized column's width must survive further streamed rows rather than \
         being overwritten by sync_dimensions's auto-fit measurement"
    );
}

#[gpui::test]
fn manual_column_resize_does_not_survive_reset_for_new_result(cx: &mut gpui::TestAppContext) {
    let (view, vcx) = view_with_results(cx, sample_result());
    vcx.run_until_parked();

    let auto_fit_width = view.read_with(vcx, |v, _app| v.column_widths[0]);
    let resized_width = auto_fit_width + px(120.0);
    view.update_in(vcx, |view, window, cx| {
        view.resize_column(0, resized_width, window, cx);
    });
    assert_eq!(
        view.read_with(vcx, |v, _app| v.column_widths[0]),
        resized_width
    );

    view.update(vcx, |view, cx| {
        view.show_live("public.orders", cx);
    });

    assert_eq!(
        view.read_with(vcx, |v, _app| v.column_widths[0]),
        auto_fit_width,
        "show_live's reset_for_new_result must clear a manual override, so a fresh result \
         renders with the auto-fit width again rather than a stale manual one"
    );
}

// -- cell selection / copy -------------------------------------------

fn view_with_results(
    cx: &mut gpui::TestAppContext,
    result: ResultSet,
) -> (gpui::Entity<ResultsView>, &mut gpui::VisualTestContext) {
    let state = SessionState::Results(Duration::from_millis(1));
    let session = cx.new(|_cx| Session::new_for_render_test(state, result));
    cx.add_window_view(|_window, cx| ResultsView::new(session, "public.orders", cx))
}

#[gpui::test]
fn clicking_a_cell_focuses_the_grid_and_a_following_copy_key_copies_its_value(
    cx: &mut gpui::TestAppContext,
) {
    let (view, vcx) = view_with_results(cx, sample_result());
    vcx.run_until_parked();

    let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());
    let cell_bounds = vcx
        .debug_bounds(body_first_cell_debug_selector(&table_state))
        .expect("the top-of-viewport body cell must be painted");
    vcx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position: gpui::point(
            cell_bounds.origin.x + px(5.0),
            cell_bounds.origin.y + px(5.0),
        ),
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    vcx.run_until_parked();

    assert_eq!(
        table_state.read_with(vcx, |s, _app| s.focused_cell()),
        Some((0, 0)),
        "clicking the top-of-viewport body cell must select row 0, column 0"
    );

    vcx.dispatch_action(Copy);
    let copied = vcx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(
        copied.as_deref(),
        Some("1"),
        "Cmd/Ctrl-C after a click must copy the clicked cell's value, proving the click \
         also focused the grid (dispatch_action only reaches a focused view's key bindings)"
    );
}

#[gpui::test]
fn copy_with_no_selection_never_writes_to_the_clipboard(cx: &mut gpui::TestAppContext) {
    let (view, vcx) = view_with_results(cx, sample_result());
    vcx.run_until_parked();

    assert_eq!(vcx.read_from_clipboard().and_then(|item| item.text()), None);
    view.update_in(vcx, |view, window, cx| {
        view.copy_focused_cell(&Copy, window, cx);
    });
    assert_eq!(
        vcx.read_from_clipboard().and_then(|item| item.text()),
        None,
        "copying with no selection must not write anything to the clipboard"
    );
}

#[gpui::test]
fn copy_writes_the_full_formatted_value_not_a_truncated_display_string(
    cx: &mut gpui::TestAppContext,
) {
    let long_value = "a very long value that would visually truncate in a narrow cell but \
                       must still be copied in full"
        .to_owned();
    let result = ResultSet {
        columns: vec![column("v", "text")],
        rows: vec![Row(vec![Value::Text(long_value.clone())])],
        affected: None,
        notices: Vec::new(),
    };
    let (view, vcx) = view_with_results(cx, result);
    vcx.run_until_parked();

    view.update(vcx, |view, cx| {
        view.table_state
            .update(cx, |state, _cx| state.set_focused_cell(0, 0));
    });
    view.update_in(vcx, |view, window, cx| {
        view.copy_focused_cell(&Copy, window, cx);
    });

    let copied = vcx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some(long_value.as_str()));
}

#[gpui::test]
fn copy_of_a_null_cell_writes_an_empty_string(cx: &mut gpui::TestAppContext) {
    let result = ResultSet {
        columns: vec![column("v", "text")],
        rows: vec![Row(vec![Value::Null])],
        affected: None,
        notices: Vec::new(),
    };
    let (view, vcx) = view_with_results(cx, result);
    vcx.run_until_parked();

    view.update(vcx, |view, cx| {
        view.table_state
            .update(cx, |state, _cx| state.set_focused_cell(0, 0));
    });
    view.update_in(vcx, |view, window, cx| {
        view.copy_focused_cell(&Copy, window, cx);
    });

    let copied = vcx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(
        copied.as_deref(),
        Some(""),
        "a NULL cell must copy as an empty string, not the literal \"NULL\" the grid \
         displays"
    );
}

#[gpui::test]
fn a_selection_outside_a_shrunken_result_is_cleared_and_copy_stays_a_noop(
    cx: &mut gpui::TestAppContext,
) {
    let mut result = sample_result();
    result
        .rows
        .push(Row(vec![Value::Int(3), Value::Text("extra".to_owned())]));
    let (view, vcx) = view_with_results(cx, result);
    vcx.run_until_parked();

    view.update(vcx, |view, cx| {
        view.table_state
            .update(cx, |state, _cx| state.set_focused_cell(2, 1));
    });

    // The session's result shrinks back to `sample_result`'s two rows,
    // taking the just-set selection at row 2 out of bounds.
    let session = view.read_with(vcx, |v, _app| v.session.clone());
    session.update(vcx, |session, _cx| {
        session.set_result_for_test(sample_result());
    });
    // `Session::set_result_for_test` bypasses `cx.notify()`, so the view
    // is synced explicitly here rather than relying on the observer.
    view.update(vcx, super::ResultsView::sync_dimensions);

    let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());
    assert_eq!(
        table_state.read_with(vcx, |s, _app| s.focused_cell()),
        None,
        "a selection that no longer fits the shrunken result must be cleared"
    );

    // Re-rendering (the highlight path) and invoking copy (the domain
    // lookup path) must both stay safe with no selection left to act on.
    vcx.run_until_parked();
    view.update_in(vcx, |view, window, cx| {
        view.copy_focused_cell(&Copy, window, cx);
    });
    assert_eq!(vcx.read_from_clipboard().and_then(|item| item.text()), None);
}

#[gpui::test]
fn copy_of_a_selection_past_a_smaller_results_bounds_stays_a_noop(cx: &mut gpui::TestAppContext) {
    // Sets an out-of-bounds selection directly on `TableState` rather
    // than going through `sync_dimensions` (which would clear it): this
    // exercises `copy_focused_cell`'s own `.get()` guard against a
    // `Some` selection whose (row, col) has no matching value, not the
    // no-selection (`None`) path a cleared selection would take
    // instead.
    let (view, vcx) = view_with_results(cx, sample_result());
    vcx.run_until_parked();

    view.update(vcx, |view, cx| {
        view.table_state
            .update(cx, |state, _cx| state.set_focused_cell(50, 50));
    });
    view.update_in(vcx, |view, window, cx| {
        view.copy_focused_cell(&Copy, window, cx);
    });

    assert_eq!(
        vcx.read_from_clipboard().and_then(|item| item.text()),
        None,
        "a selection past the result's own rows/columns must not panic and must not write \
         anything to the clipboard"
    );
}

#[gpui::test]
fn an_empty_result_set_selects_nothing_and_copy_is_a_noop(cx: &mut gpui::TestAppContext) {
    let (view, vcx) = view_with_results(cx, ResultSet::default());
    vcx.run_until_parked();

    vcx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position: gpui::point(px(50.0), px(50.0)),
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    vcx.run_until_parked();

    let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());
    assert_eq!(
        table_state.read_with(vcx, |s, _app| s.focused_cell()),
        None,
        "an empty result has no cell to select"
    );

    view.update_in(vcx, |view, window, cx| {
        view.copy_focused_cell(&Copy, window, cx);
    });
    assert_eq!(vcx.read_from_clipboard().and_then(|item| item.text()), None);
}

#[gpui::test]
fn arrow_keys_over_an_empty_result_set_select_nothing_and_do_not_panic(
    cx: &mut gpui::TestAppContext,
) {
    // `move_focused_cell` computes `row_count - 1`/`col_count - 1` to
    // clamp a new selection: an empty result must return before that
    // subtraction, or it would underflow.
    let (view, vcx) = cx.add_window_view(|window, cx| {
        let state = SessionState::Results(Duration::from_millis(1));
        let session = cx.new(|_cx| Session::new_for_render_test(state, ResultSet::default()));
        let view = ResultsView::new(session, "public.orders", cx);
        window.focus(&view.focus_handle(cx));
        view
    });
    vcx.run_until_parked();

    vcx.dispatch_action(CellDown);
    vcx.dispatch_action(CellRight);

    let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());
    assert_eq!(
        table_state.read_with(vcx, |s, _app| s.focused_cell()),
        None,
        "an empty result has no cell for an arrow key to select"
    );
}

#[gpui::test]
fn arrow_keys_move_the_selection_one_cell_at_a_time_and_clamp_at_the_bounds(
    cx: &mut gpui::TestAppContext,
) {
    let (view, vcx) = cx.add_window_view(|window, cx| {
        let state = SessionState::Results(Duration::from_millis(1));
        let session = cx.new(|_cx| Session::new_for_render_test(state, sample_result()));
        let view = ResultsView::new(session, "public.orders", cx);
        window.focus(&view.focus_handle(cx));
        view
    });
    vcx.run_until_parked();
    let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());

    // No wraparound past the top-left corner.
    vcx.dispatch_action(CellUp);
    vcx.dispatch_action(CellLeft);
    assert_eq!(
        table_state.read_with(vcx, |s, _app| s.focused_cell()),
        Some((0, 0)),
        "moving up/left with nothing selected must land on (0, 0), not go negative"
    );

    vcx.dispatch_action(CellDown);
    vcx.dispatch_action(CellRight);
    assert_eq!(
        table_state.read_with(vcx, |s, _app| s.focused_cell()),
        Some((1, 1)),
        "CellDown/CellRight must move exactly one row/column at a time"
    );

    // `sample_result` has exactly 2 rows and 2 columns: (1, 1) is
    // already the bottom-right corner, so further Down/Right must not
    // move past it.
    vcx.dispatch_action(CellDown);
    vcx.dispatch_action(CellRight);
    assert_eq!(
        table_state.read_with(vcx, |s, _app| s.focused_cell()),
        Some((1, 1)),
        "moving past the last row/column must clamp at the bounds rather than wrap or \
         go out of range"
    );
}

// -- Text view: shared result fixture ---------------------------------

fn text_column_result(rows: Vec<Row>) -> ResultSet {
    ResultSet {
        columns: vec![column("Text", "nvarchar")],
        rows,
        affected: None,
        notices: Vec::new(),
    }
}

// -- Text view: results bar count text ---------------------------------

#[test]
fn results_bar_count_text_reads_rows_for_grid_and_lines_for_text() {
    let state = SessionState::Results(Duration::from_millis(1));
    assert_eq!(results_bar_count_text(&state, 17, None), "17");
    assert_eq!(results_bar_count_text(&state, 17, Some(12)), "12 lines");
}

#[test]
fn results_bar_count_text_reads_lines_for_a_truncated_text_view() {
    let state = SessionState::Truncated {
        elapsed: Duration::from_millis(1),
        rows: 5_000,
    };
    assert_eq!(
        results_bar_count_text(&state, 100, None),
        "5000 (truncated at 100)"
    );
    assert_eq!(
        results_bar_count_text(&state, 100, Some(80)),
        "80 lines (truncated at 100)"
    );
}

#[test]
fn results_bar_count_text_is_a_dash_for_non_result_states() {
    for state in [
        SessionState::Empty,
        SessionState::Connecting,
        SessionState::Connected,
        SessionState::Error("boom".to_owned()),
    ] {
        assert_eq!(results_bar_count_text(&state, 5, None), "-");
    }
}

// -- Text view: default selection and reset -----------------------------

#[gpui::test]
fn a_document_shaped_result_defaults_to_the_text_view(cx: &mut gpui::TestAppContext) {
    let result = text_column_result(vec![
        Row(vec![Value::Text("line one".to_owned())]),
        Row(vec![Value::Text("line two".to_owned())]),
    ]);
    let (view, vcx) = view_with_results(cx, result);
    vcx.run_until_parked();
    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Text
    );
}

#[gpui::test]
fn a_non_document_shaped_result_defaults_to_the_grid_view(cx: &mut gpui::TestAppContext) {
    let (view, vcx) = view_with_results(cx, sample_result());
    vcx.run_until_parked();
    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Grid
    );
}

#[gpui::test]
fn a_single_row_with_no_newline_defaults_to_the_grid_view(cx: &mut gpui::TestAppContext) {
    let result = text_column_result(vec![Row(vec![Value::Text("just one line".to_owned())])]);
    let (view, vcx) = view_with_results(cx, result);
    vcx.run_until_parked();
    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Grid
    );
}

#[gpui::test]
fn switching_to_a_new_result_discards_a_manual_view_choice_and_recomputes_the_default(
    cx: &mut gpui::TestAppContext,
) {
    let (view, vcx) = view_with_results(cx, sample_result());
    vcx.run_until_parked();
    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Grid
    );

    // A manual choice on a non-document result: forced into Text even
    // though the grid is the computed default.
    view.update(vcx, |view, cx| {
        view.set_view_mode_for_test(ViewMode::Text, cx);
    });
    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Text
    );

    // A new document-shaped result becomes current via show_snapshot,
    // one of the two reset points: the manual choice must not survive,
    // and the new result's own default (Text, since it is document
    // shaped) is what actually renders -- not a coincidental repeat of
    // the stale manual choice.
    let document = text_column_result(vec![
        Row(vec![Value::Text("a".to_owned())]),
        Row(vec![Value::Text("b".to_owned())]),
    ]);
    view.update(vcx, |view, cx| {
        view.show_snapshot(
            super::ResultsSnapshot {
                source_label: "doc".into(),
                state: SessionState::Results(Duration::from_millis(1)),
                result: document,
            },
            cx,
        );
    });
    vcx.run_until_parked();
    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Text
    );

    // Now prove it was actually recomputed, not left over: manually
    // force Grid, then show a NON-document snapshot and confirm the
    // manual choice is discarded in favor of that result's own Grid
    // default.
    view.update(vcx, |view, cx| {
        view.set_view_mode_for_test(ViewMode::Grid, cx);
    });
    view.update(vcx, |view, cx| {
        view.show_snapshot(
            super::ResultsSnapshot {
                source_label: "doc2".into(),
                state: SessionState::Results(Duration::from_millis(1)),
                result: text_column_result(vec![
                    Row(vec![Value::Text("x".to_owned())]),
                    Row(vec![Value::Text("y".to_owned())]),
                ]),
            },
            cx,
        );
    });
    vcx.run_until_parked();
    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Text,
        "the freshly-arrived document-shaped result must default to Text again, proving the \
         default was recomputed rather than the prior manual Grid choice leaking through"
    );
}

#[gpui::test]
fn the_default_is_not_computed_while_the_query_is_still_running(cx: &mut gpui::TestAppContext) {
    let result = text_column_result(vec![Row(vec![Value::Text(
        "only one row so far".to_owned(),
    )])]);
    let session = cx.new(|_cx| Session::new_for_render_test(SessionState::Running, result));
    let (view, vcx) =
        cx.add_window_view(|_window, cx| ResultsView::new(session, "public.orders", cx));
    vcx.run_until_parked();
    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Grid,
        "Grid must keep rendering while Running, exactly as today, with no default computed \
         yet from a still-partial result"
    );
}
// -- Text view: switch disabled state ------------------------------

#[gpui::test]
fn clicking_the_disabled_text_segment_on_a_multi_column_result_does_not_switch_the_view(
    cx: &mut gpui::TestAppContext,
) {
    let (view, vcx) = view_with_results(cx, sample_result());
    vcx.run_until_parked();
    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Grid,
        "sample_result has two columns, so it is not document shaped and Grid is its default"
    );

    let text_segment_bounds = vcx
        .debug_bounds("results-view-text")
        .expect("the Text segment must still be painted, only disabled, not omitted");
    vcx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position: gpui::point(
            text_segment_bounds.origin.x + px(5.0),
            text_segment_bounds.origin.y + px(5.0),
        ),
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    vcx.simulate_event(MouseUpEvent {
        button: MouseButton::Left,
        position: gpui::point(
            text_segment_bounds.origin.x + px(5.0),
            text_segment_bounds.origin.y + px(5.0),
        ),
        modifiers: Modifiers::default(),
        click_count: 1,
    });
    vcx.run_until_parked();

    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Grid,
        "a disabled Text segment must be inert: clicking it on a multi-column result must \
         not switch the view away from Grid"
    );
}

#[gpui::test]
fn clicking_the_enabled_text_segment_on_a_single_text_column_result_switches_to_text(
    cx: &mut gpui::TestAppContext,
) {
    // One text column but a single newline-free row: NOT document shaped, so
    // Grid is the default -- yet the Text segment is enabled (single text
    // column) and must actually switch when clicked, independent of the
    // row-count/newline condition that only governs the default.
    let result = text_column_result(vec![Row(vec![Value::Text("just one line".to_owned())])]);
    let (view, vcx) = view_with_results(cx, result);
    vcx.run_until_parked();
    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Grid,
        "a single newline-free row is not document shaped, so Grid is the default"
    );

    let text_segment_bounds = vcx
        .debug_bounds("results-view-text")
        .expect("the Text segment must be painted");
    vcx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position: gpui::point(
            text_segment_bounds.origin.x + px(5.0),
            text_segment_bounds.origin.y + px(5.0),
        ),
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    vcx.simulate_event(MouseUpEvent {
        button: MouseButton::Left,
        position: gpui::point(
            text_segment_bounds.origin.x + px(5.0),
            text_segment_bounds.origin.y + px(5.0),
        ),
        modifiers: Modifiers::default(),
        click_count: 1,
    });
    vcx.run_until_parked();

    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Text,
        "the Text segment is enabled for any single-text-column result and must switch \
         the view even when Grid was the default"
    );
}

// -- Text view: copy -----------------------------------------------

#[gpui::test]
fn copy_while_the_text_view_is_active_copies_nothing_with_no_selection(
    cx: &mut gpui::TestAppContext,
) {
    let result = text_column_result(vec![
        Row(vec![Value::Text("CREATE PROCEDURE p".to_owned())]),
        Row(vec![Value::Text("    AS".to_owned())]),
    ]);
    let (view, vcx) = view_with_results(cx, result);
    vcx.run_until_parked();
    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Text
    );

    view.update_in(vcx, |view, window, cx| {
        view.copy_focused_cell(&Copy, window, cx);
    });

    let copied = vcx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(
        copied.as_deref(),
        Some(""),
        "Cmd/Ctrl-C in the Text view must copy the selection - if there is no selection, it must copy an empty string"
    );
}

#[gpui::test]
fn copy_while_the_text_view_is_active_copies_selection(cx: &mut gpui::TestAppContext) {
    let result = text_column_result(vec![
        Row(vec![Value::Text("CREATE PROCEDURE p".to_owned())]),
        Row(vec![Value::Text("    AS".to_owned())]),
    ]);
    let (view, vcx) = view_with_results(cx, result);
    vcx.run_until_parked();
    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Text
    );

    view.update(vcx, |view, cx| {
        view.text_view.update(cx, |text_view, cx| {
            text_view.set_text_caret_for_test(0, 7, false, cx);
            text_view.set_text_caret_for_test(1, 4, true, cx);
        });
    });
    view.update_in(vcx, |view, window, cx| {
        view.copy_focused_cell(&Copy, window, cx);
    });

    let copied = vcx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(
        copied.as_deref(),
        Some("PROCEDURE p\n    "),
        "Cmd/Ctrl-C in the Text view must copy the selection"
    );
}

#[gpui::test]
fn copy_while_the_grid_is_active_still_copies_the_focused_cell(cx: &mut gpui::TestAppContext) {
    let (view, vcx) = view_with_results(cx, sample_result());
    vcx.run_until_parked();
    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Grid
    );

    view.update(vcx, |view, cx| {
        view.table_state
            .update(cx, |state, _cx| state.set_focused_cell(0, 0));
    });
    view.update_in(vcx, |view, window, cx| {
        view.copy_focused_cell(&Copy, window, cx);
    });

    let copied = vcx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(
        copied.as_deref(),
        Some("1"),
        "Grid's Cmd/Ctrl-C behavior must be unaffected by the Text view's own copy path"
    );
}

// -- Text view: selection reset on a new result -----------------------

#[gpui::test]
fn switching_to_a_new_result_clears_any_text_selection(cx: &mut gpui::TestAppContext) {
    let result = text_column_result(vec![
        Row(vec![Value::Text("a".to_owned())]),
        Row(vec![Value::Text("b".to_owned())]),
    ]);
    let (view, vcx) = view_with_results(cx, result);
    vcx.run_until_parked();
    view.update(vcx, |view, cx| {
        view.text_view.update(cx, |text_view, cx| {
            text_view.set_text_caret_for_test(0, 0, false, cx);
        });
    });
    assert!(
        view.read_with(vcx, |v, app| v
            .text_view
            .read(app)
            .text_selection_for_test())
            .is_some()
    );

    view.update(vcx, |view, cx| {
        view.show_snapshot(
            super::ResultsSnapshot {
                source_label: "doc".into(),
                state: SessionState::Results(Duration::from_millis(1)),
                result: sample_result(),
            },
            cx,
        );
    });
    assert_eq!(
        view.read_with(vcx, |v, app| v
            .text_view
            .read(app)
            .text_selection_for_test()),
        None
    );
}

// -- Text view: rendering smoke tests -----------------------------------

#[gpui::test]
fn renders_the_grid_when_manually_selected_for_a_document_shaped_result(
    cx: &mut gpui::TestAppContext,
) {
    let result = text_column_result(vec![
        Row(vec![Value::Text("a".to_owned())]),
        Row(vec![Value::Text("b".to_owned())]),
    ]);
    let (view, vcx) = view_with_results(cx, result);
    vcx.run_until_parked();
    view.update(vcx, |view, cx| {
        view.set_view_mode_for_test(ViewMode::Grid, cx);
    });
    vcx.run_until_parked();
    assert_eq!(
        view.read_with(vcx, |v, _app| v.view_mode_for_test()),
        ViewMode::Grid
    );
}

#[cfg(test)]
mod value_panel_view_tests {
    use std::time::Duration;

    use gpui::{
        AppContext as _, Focusable as _, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
        MouseUpEvent, point, px,
    };
    use zsql_core::{ColumnMeta, ResultSet, Row, Value};
    use zsql_ui::table::body_first_cell_debug_selector;

    use super::{ResultsView, SessionState};
    use crate::session::Session;
    use crate::ui::results::{CloseValuePanel, FocusValuePanel, ToggleValuePanel, ValuePanel};
    use crate::ui::value_panel::view::{CopyTreeNodeValue, FocusGridFromPanel};

    fn column(name: &str, type_name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            nullable: true,
        }
    }

    fn json_result() -> ResultSet {
        ResultSet {
            columns: vec![
                column("id", "int8"),
                column("payload", "jsonb"),
                column("note", "text"),
            ],
            rows: vec![
                Row(vec![
                    Value::Int(1),
                    Value::Json(r#"{"items":[{"sku":"A1"}]}"#.to_owned()),
                    Value::Text("hi".to_owned()),
                ]),
                Row(vec![
                    Value::Int(2),
                    Value::Json(r#"{"items":[{"sku":"B2"}]}"#.to_owned()),
                    Value::Null,
                ]),
            ],
            affected: None,
            notices: Vec::new(),
        }
    }

    fn view_with_results(
        cx: &mut gpui::TestAppContext,
        result: ResultSet,
    ) -> (gpui::Entity<ResultsView>, &mut gpui::VisualTestContext) {
        let state = SessionState::Results(Duration::from_millis(1));
        let session = cx.new(|_cx| Session::new_for_render_test(state, result));
        cx.add_window_view(|window, cx| {
            let view = ResultsView::new(session, "public.orders", cx);
            window.focus(&view.focus_handle(cx));
            view
        })
    }

    #[gpui::test]
    fn space_toggles_the_panel_open_then_closed(cx: &mut gpui::TestAppContext) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();
        view.update(vcx, |view, cx| {
            view.table_state
                .update(cx, |state, _cx| state.set_focused_cell(0, 0));
        });

        vcx.dispatch_action(ToggleValuePanel);
        assert!(view.read_with(vcx, |v, app| v.value_panel.read(app).is_open()));

        vcx.dispatch_action(ToggleValuePanel);
        assert!(!view.read_with(vcx, |v, app| v.value_panel.read(app).is_open()));
    }

    #[gpui::test]
    fn esc_closes_the_panel_and_leaves_the_focused_cell_untouched(cx: &mut gpui::TestAppContext) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();
        view.update(vcx, |view, cx| {
            view.table_state
                .update(cx, |state, _cx| state.set_focused_cell(1, 0));
            view.value_panel.update(cx, ValuePanel::open);
        });

        vcx.dispatch_action(CloseValuePanel);

        view.read_with(vcx, |v, app| {
            assert!(!v.value_panel.read(app).is_open());
            assert_eq!(v.table_state.read(app).focused_cell(), Some((1, 0)));
        });
    }

    #[gpui::test]
    fn double_click_opens_the_panel_for_the_clicked_cell(cx: &mut gpui::TestAppContext) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();

        let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());
        let cell_bounds = vcx
            .debug_bounds(body_first_cell_debug_selector(&table_state))
            .expect("the top-of-viewport body cell must be painted");
        let position = point(
            cell_bounds.origin.x + px(5.0),
            cell_bounds.origin.y + px(5.0),
        );
        let mouse_down = |click_count| MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count,
            first_mouse: false,
        };
        vcx.simulate_event(mouse_down(1));
        vcx.run_until_parked();
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            Some((0, 0)),
            "the first mouse-down of the double click must select the cell, same as a plain click"
        );

        vcx.simulate_event(mouse_down(2));
        vcx.run_until_parked();

        assert!(view.read_with(vcx, |v, app| v.value_panel.read(app).is_open()));
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            Some((0, 0))
        );
    }

    #[gpui::test]
    fn opening_the_panel_leaves_the_focused_cell_selection_untouched(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();
        view.update(vcx, |view, cx| {
            view.table_state
                .update(cx, |state, _cx| state.set_focused_cell(0, 1));
        });

        vcx.dispatch_action(ToggleValuePanel);
        vcx.run_until_parked();

        view.read_with(vcx, |v, app| {
            assert!(v.value_panel.read(app).is_open());
            assert_eq!(v.table_state.read(app).focused_cell(), Some((0, 1)));
        });
    }

    #[gpui::test]
    fn context_menu_actions_operate_on_the_right_clicked_cell(cx: &mut gpui::TestAppContext) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.open_cell_context_menu(1, 0, point(px(10.0), px(10.0)), cx);
        });
        view.read_with(vcx, |v, app| {
            assert_eq!(v.table_state.read(app).focused_cell(), Some((1, 0)));
            assert!(v.cell_context_menu.is_some());
        });

        view.update(vcx, |view, cx| {
            view.copy_column_name(cx);
        });
        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some("id"));

        view.update(vcx, |view, cx| {
            view.open_cell_context_menu(1, 1, point(px(10.0), px(10.0)), cx);
            view.copy_row_as_json(cx);
        });
        let copied = vcx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&copied).unwrap();
        assert_eq!(parsed["id"], serde_json::json!(2));
        assert_eq!(
            parsed["payload"],
            serde_json::json!({"items": [{"sku": "B2"}]})
        );
        assert_eq!(parsed["note"], serde_json::Value::Null);
    }

    #[gpui::test]
    fn view_value_from_the_context_menu_opens_the_panel_and_closes_the_menu(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.open_cell_context_menu(1, 1, point(px(10.0), px(10.0)), cx);
            view.view_value_from_menu(cx);
        });

        view.read_with(vcx, |v, app| {
            assert!(v.value_panel.read(app).is_open());
            assert!(v.cell_context_menu.is_none());
            assert_eq!(v.table_state.read(app).focused_cell(), Some((1, 1)));
        });
    }

    #[gpui::test]
    fn unpinned_panel_follows_focus_and_pinned_panel_freezes_on_its_cell(
        cx: &mut gpui::TestAppContext,
    ) {
        // The results view keys panel content by `31 * row + col` (see
        // `sync_value_panel_content`): (0, 0) -> 0, (1, 0) -> 31.
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();
        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 0);
                cx.notify();
            });
            view.value_panel.update(cx, ValuePanel::open);
            cx.notify();
        });
        vcx.run_until_parked(); // a render pass syncs the panel's content

        view.read_with(vcx, |v, app| {
            assert_eq!(
                v.value_panel
                    .read(app)
                    .state_for_test()
                    .content()
                    .map(|c| c.id),
                Some(0),
                "an open unpinned panel must target the focused cell"
            );
        });

        // Unpinned: moving the grid's selection re-targets the panel.
        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(1, 0);
                cx.notify();
            });
            cx.notify();
        });
        vcx.run_until_parked();
        view.read_with(vcx, |v, app| {
            assert_eq!(
                v.value_panel
                    .read(app)
                    .state_for_test()
                    .content()
                    .map(|c| c.id),
                Some(31),
                "an unpinned panel must follow the grid's live selection"
            );
        });

        // Pin, then move the grid: the panel must keep its pinned content.
        view.update(vcx, |view, cx| {
            view.value_panel
                .update(cx, |p, _cx| p.state_mut_for_test().pin());
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 0);
                cx.notify();
            });
            cx.notify();
        });
        vcx.run_until_parked();
        view.read_with(vcx, |v, app| {
            assert!(
                v.value_panel.read(app).state_for_test().is_pinned(),
                "the panel must report itself pinned"
            );
            assert_eq!(
                v.table_state.read(app).focused_cell(),
                Some((0, 0)),
                "the grid's own selection must keep moving normally"
            );
            assert_eq!(
                v.value_panel
                    .read(app)
                    .state_for_test()
                    .content()
                    .map(|c| c.id),
                Some(31),
                "a pinned panel must keep showing its pinned content despite the grid \
                 selection moving"
            );
        });
    }

    #[gpui::test]
    fn tab_moves_focus_between_the_grid_and_the_panel(cx: &mut gpui::TestAppContext) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();
        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 1);
                cx.notify();
            });
            view.value_panel.update(cx, ValuePanel::open);
            cx.notify();
        });
        vcx.run_until_parked();

        let panel_focus =
            view.read_with(vcx, |v, app| v.value_panel.read(app).focus_handle().clone());
        // `FocusGridFromPanel` returns focus to the grid pane the panel tabbed
        // in from: the panel's parent handle is the view's own focus handle.
        let grid_focus = view.read_with(vcx, ResultsView::focus_handle);

        vcx.dispatch_action(FocusValuePanel);
        vcx.update(|window, cx| {
            assert_eq!(window.focused(cx).as_ref(), Some(&panel_focus));
        });

        vcx.dispatch_action(FocusGridFromPanel);
        vcx.update(|window, cx| {
            assert_eq!(window.focused(cx).as_ref(), Some(&grid_focus));
        });
    }

    /// Cmd/Ctrl-C with the panel focused over a non-JSON cell (no parsed
    /// tree to copy a node from) must still copy the panel's own target
    /// cell -- the same text `Copy value`/`copy_focused_cell` produce --
    /// rather than silently doing nothing.
    #[gpui::test]
    fn copy_with_the_panel_focused_over_a_non_json_cell_copies_the_cells_value(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 2); // the "note" text column
                cx.notify();
            });
            view.value_panel.update(cx, ValuePanel::open);
            cx.notify();
        });
        vcx.run_until_parked();

        vcx.dispatch_action(FocusValuePanel);
        vcx.dispatch_action(CopyTreeNodeValue);
        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some("hi"),
            "with no parsed JSON tree to copy a node from, Cmd/Ctrl-C must fall back to the \
             panel's own target cell value"
        );
    }

    #[gpui::test]
    fn dragging_the_divider_resizes_the_panel_clamped_to_configured_bounds(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();
        view.update(vcx, |view, cx| {
            view.table_state
                .update(cx, |state, _cx| state.set_focused_cell(0, 0));
            view.value_panel.update(cx, ValuePanel::open);
            cx.notify();
        });
        vcx.run_until_parked();

        let (min_width, max_width, start_width) = view.read_with(vcx, |v, _app| {
            (
                v.value_panel_min_width,
                v.value_panel_max_width,
                v.value_panel_width,
            )
        });

        let divider_bounds = vcx
            .debug_bounds("value-panel-divider")
            .expect("the resize divider must be painted while the panel is docked open");
        let origin = divider_bounds.origin;

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: origin,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.value_panel_width),
            start_width,
            "pressing down on the divider must not itself resize the panel"
        );

        // `on_mouse_move` only fires while the pointer sits inside the
        // dragged element's own hitbox, so the drag stays within the test
        // window's bounds (1920x1080) rather than moving off-screen: the
        // panel docks on the right edge, so dragging to the window's left
        // edge grows it (clamped at the configured maximum) and dragging to
        // its right edge shrinks it (clamped at the configured minimum).
        let window_left = px(0.0);
        let window_right = px(1_900.0);

        vcx.simulate_event(MouseMoveEvent {
            position: point(window_left, origin.y),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.value_panel_width),
            max_width,
            "dragging to the window's left edge must clamp the panel at its configured maximum \
             width"
        );

        vcx.simulate_event(MouseMoveEvent {
            position: point(window_right, origin.y),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.value_panel_width),
            min_width,
            "dragging to the window's right edge must clamp the panel at its configured \
             minimum width"
        );

        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: point(window_right, origin.y),
            modifiers: Modifiers::default(),
            click_count: 1,
        });

        // Releasing ends the drag: a further move must not resize the panel.
        vcx.simulate_event(MouseMoveEvent {
            position: origin,
            pressed_button: None,
            modifiers: Modifiers::default(),
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.value_panel_width),
            min_width,
            "moving the mouse after mouse-up must not resume resizing the panel"
        );
    }
}
