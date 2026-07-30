use std::time::Duration;

use gpui::{
    AppContext as _, Context, Focusable as _, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, point, px,
};
use zsql_core::value::UnknownValue;
use zsql_core::{ColumnMeta, ResultSet, Row, RowCount, Value};

use super::grid::column_width_from_parts;
use super::{
    CellDown, CellLeft, CellRight, CellUp, Copy, NextPage, PrevPage, ResultsView, SessionState,
    ViewMode, filtered_count_summary, results_bar_count_text,
};

use crate::session::Session;
use crate::ui::theme;
use zsql_ui::scrollable::horizontal_thumb_debug_selector;
use zsql_ui::table::{body_first_cell_debug_selector, column_resize_handle_debug_selector};
use zsql_ui::theme::Theme;

/// Test-only accessors used by `ui::sidebar`'s and `ui::tabs`'s tests
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

// -- filtered_count_summary --------------------------------------------------

fn preview_controls_for_summary(
    filtered: bool,
    base_total: Option<RowCount>,
    filtered_total: Option<RowCount>,
) -> super::pager::PreviewControls {
    let mut state = zsql_core::preview_state::PreviewQueryState::new(200);
    if let Some(base) = base_total {
        state.set_total_rows(Some(base));
    }
    if filtered {
        state.add_filter("status", "text", zsql_core::FilterOperator::Eq, "paid");
        // Re-set the (possibly still-unknown) filtered total explicitly:
        // adding a filter alone leaves any previously-set total in place,
        // exactly like a real requery clears it back to `None` until the
        // filtered count fetch resolves.
        state.set_total_rows(filtered_total);
    }
    super::pager::PreviewControls {
        state,
        dispatch: std::rc::Rc::new(|_action, _cx| {}),
    }
}

#[test]
fn filtered_count_summary_is_none_with_no_active_preview() {
    assert_eq!(filtered_count_summary(None), None);
}

#[test]
fn filtered_count_summary_is_none_with_no_filters_committed() {
    let controls = preview_controls_for_summary(false, Some(RowCount::Exact(12_480)), None);
    assert_eq!(filtered_count_summary(Some(&controls)), None);
}

#[test]
fn filtered_count_summary_is_none_while_the_filtered_total_is_not_yet_known() {
    let controls = preview_controls_for_summary(true, Some(RowCount::Exact(12_480)), None);
    assert_eq!(filtered_count_summary(Some(&controls)), None);
}

#[test]
fn filtered_count_summary_is_none_while_the_base_total_is_not_yet_known() {
    let controls = preview_controls_for_summary(true, None, Some(RowCount::Exact(3_102)));
    assert_eq!(filtered_count_summary(Some(&controls)), None);
}

#[test]
fn filtered_count_summary_formats_both_totals_once_known() {
    let controls = preview_controls_for_summary(
        true,
        Some(RowCount::Exact(12_480)),
        Some(RowCount::Exact(3_102)),
    );
    assert_eq!(
        filtered_count_summary(Some(&controls)),
        Some(("3,102".to_owned(), "filtered of 12,480".to_owned()))
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
        Value::Unknown(UnknownValue::None),
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

fn recording_preview_controls() -> (
    super::pager::PreviewControls,
    std::rc::Rc<std::cell::RefCell<Vec<super::pager::PreviewAction>>>,
) {
    let recorded = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded_for_dispatch = recorded.clone();
    let controls = super::pager::PreviewControls {
        state: zsql_core::preview_state::PreviewQueryState::new(200),
        dispatch: std::rc::Rc::new(move |action, _cx| {
            recorded_for_dispatch.borrow_mut().push(action);
        }),
    };
    (controls, recorded)
}

#[gpui::test]
fn prev_page_and_next_page_actions_are_no_ops_with_no_active_preview_controls(
    cx: &mut gpui::TestAppContext,
) {
    let session =
        cx.new(|_cx| Session::new_for_render_test(SessionState::Connected, ResultSet::default()));
    let (view, vcx) = cx.add_window_view(|_window, cx| ResultsView::new(session, "t", cx));

    // No `set_preview_controls` call: `preview` stays `None`, matching a
    // script tab, a schema tab, or a "no results" state. Neither action
    // must panic or otherwise have a visible effect.
    view.update_in(vcx, |view, window, cx| {
        view.prev_page(&PrevPage, window, cx);
        view.next_page(&NextPage, window, cx);
    });
}

#[gpui::test]
fn prev_page_and_next_page_actions_reach_the_active_tabs_preview_dispatch(
    cx: &mut gpui::TestAppContext,
) {
    let session =
        cx.new(|_cx| Session::new_for_render_test(SessionState::Connected, ResultSet::default()));
    let (view, vcx) = cx.add_window_view(|_window, cx| ResultsView::new(session, "t", cx));

    let (controls, recorded) = recording_preview_controls();
    view.update(vcx, |view, cx| {
        view.set_preview_controls(Some(controls), cx);
    });

    view.update_in(vcx, |view, window, cx| {
        view.prev_page(&PrevPage, window, cx);
        view.next_page(&NextPage, window, cx);
    });

    let recorded = recorded.borrow();
    assert_eq!(
        recorded.as_slice(),
        [
            super::pager::PreviewAction::PrevPage,
            super::pager::PreviewAction::NextPage
        ]
    );
}

// -- filter bar -------------------------------------------------------------

fn preview_controls_with_filters(
    filters: &zsql_core::FilterState,
) -> (
    super::pager::PreviewControls,
    std::rc::Rc<std::cell::RefCell<Vec<super::pager::PreviewAction>>>,
) {
    let mut state = zsql_core::preview_state::PreviewQueryState::new(200);
    for condition in filters.conditions() {
        state.add_filter(
            condition.column(),
            condition.type_name(),
            condition.operator(),
            condition.value(),
        );
    }
    let recorded = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let recorded_for_dispatch = recorded.clone();
    let controls = super::pager::PreviewControls {
        state,
        dispatch: std::rc::Rc::new(move |action, _cx| {
            recorded_for_dispatch.borrow_mut().push(action);
        }),
    };
    (controls, recorded)
}

fn view_with_sample_result_and_controls(
    cx: &mut gpui::TestAppContext,
    controls: Option<super::pager::PreviewControls>,
) -> (gpui::Entity<ResultsView>, &mut gpui::VisualTestContext) {
    let session = cx.new(|_cx| {
        Session::new_for_render_test(SessionState::Results(Duration::default()), sample_result())
    });
    let (view, vcx) = cx.add_window_view(|_window, cx| ResultsView::new(session, "t", cx));
    if let Some(controls) = controls {
        view.update(vcx, |view, cx| {
            view.set_preview_controls(Some(controls), cx);
        });
    }
    (view, vcx)
}

#[gpui::test]
fn begin_add_filter_opens_the_column_picker(cx: &mut gpui::TestAppContext) {
    let (controls, _recorded) = recording_preview_controls();
    let (view, vcx) = view_with_sample_result_and_controls(cx, Some(controls));

    view.update_in(vcx, |view, window, cx| {
        view.begin_add_filter(window, cx);
    });

    view.read_with(vcx, |view, _app| {
        assert!(view.filter_column_picker_is_open_for_test());
        assert!(
            !view.filter_editor_is_open_for_test(),
            "the editor only opens once a column is picked"
        );
    });
}

#[gpui::test]
fn begin_add_filter_toggles_the_column_picker_closed_again(cx: &mut gpui::TestAppContext) {
    let (controls, _recorded) = recording_preview_controls();
    let (view, vcx) = view_with_sample_result_and_controls(cx, Some(controls));

    view.update_in(vcx, |view, window, cx| {
        view.begin_add_filter(window, cx);
        view.begin_add_filter(window, cx);
    });

    view.read_with(vcx, |view, _app| {
        assert!(!view.filter_column_picker_is_open_for_test());
    });
}

#[gpui::test]
fn begin_add_filter_is_a_noop_with_no_active_preview_controls(cx: &mut gpui::TestAppContext) {
    let (view, vcx) = view_with_sample_result_and_controls(cx, None);

    view.update_in(vcx, |view, window, cx| {
        view.begin_add_filter(window, cx);
    });

    view.read_with(vcx, |view, _app| {
        assert!(
            !view.filter_column_picker_is_open_for_test(),
            "a detached tab's filter bar must not open the column picker"
        );
        assert!(!view.filter_editor_is_open_for_test());
    });
}

#[gpui::test]
fn typing_a_space_into_the_filter_value_editor_inserts_it_instead_of_driving_the_grid(
    cx: &mut gpui::TestAppContext,
) {
    // The grid binds plain keys (space, arrows) on its own key context, and
    // the filter editors render inside that context: the real keymap must be
    // registered so this test exercises the contention between the grid's
    // `space` binding and plain text insertion.
    cx.update(|cx| {
        super::init(cx);
        zsql_ui::text_field::init(cx);
    });
    let (controls, _recorded) = recording_preview_controls();
    let (view, vcx) = view_with_sample_result_and_controls(cx, Some(controls));

    view.update_in(vcx, |view, window, cx| {
        view.begin_add_filter(window, cx);
        view.pick_filter_column(&column("status", "text"), window, cx);
    });
    vcx.run_until_parked();

    vcx.simulate_keystrokes("a space b");
    vcx.run_until_parked();

    view.read_with(vcx, |view, cx| {
        assert_eq!(
            view.filter_editor_value_for_test(cx).as_deref(),
            Some("a b"),
            "space must insert text into the focused filter value editor, not fire the grid's \
             value-panel binding"
        );
    });
}

#[gpui::test]
fn picking_the_first_column_opens_an_editor_targeting_it(cx: &mut gpui::TestAppContext) {
    let (controls, _recorded) = recording_preview_controls();
    let (view, vcx) = view_with_sample_result_and_controls(cx, Some(controls));

    view.update_in(vcx, |view, window, cx| {
        view.begin_add_filter(window, cx);
        view.pick_filter_column(&column("id", "int8"), window, cx);
    });

    view.read_with(vcx, |view, _app| {
        assert!(
            !view.filter_column_picker_is_open_for_test(),
            "picking a column must close the picker"
        );
        assert!(view.filter_editor_is_open_for_test());
        assert_eq!(
            view.filter_editor_operator_for_test(),
            Some(zsql_core::FilterOperator::Eq),
            "a fresh filter defaults to the equals operator"
        );
    });
}

#[gpui::test]
fn picking_a_non_first_column_opens_an_editor_targeting_it(cx: &mut gpui::TestAppContext) {
    let (controls, recorded) = recording_preview_controls();
    let (view, vcx) = view_with_sample_result_and_controls(cx, Some(controls));

    view.update_in(vcx, |view, window, cx| {
        view.begin_add_filter(window, cx);
        view.pick_filter_column(&column("status", "text"), window, cx);
        view.set_filter_editor_value_for_test("paid", cx);
        view.commit_filter_edit(cx);
    });

    assert_eq!(
        recorded.borrow().as_slice(),
        [super::pager::PreviewAction::AddFilter {
            column: "status".to_owned(),
            type_name: "text".to_owned(),
            operator: zsql_core::FilterOperator::Eq,
            value: "paid".to_owned(),
        }],
        "a filter must be reachable against any column, not only the first"
    );
}

#[gpui::test]
fn the_filter_bar_reactivates_once_a_detached_tab_is_replaced_by_a_live_generated_tab(
    cx: &mut gpui::TestAppContext,
) {
    let (view, vcx) = view_with_sample_result_and_controls(cx, None);
    view.update_in(vcx, |view, window, cx| {
        view.begin_add_filter(window, cx);
    });
    view.read_with(vcx, |view, _app| {
        assert!(!view.filter_column_picker_is_open_for_test());
    });

    let (controls, _recorded) = recording_preview_controls();
    view.update(vcx, |view, cx| {
        view.set_preview_controls(Some(controls), cx);
    });
    view.update_in(vcx, |view, window, cx| {
        view.begin_add_filter(window, cx);
        view.pick_filter_column(&column("id", "int8"), window, cx);
    });

    view.read_with(vcx, |view, _app| {
        assert!(
            view.filter_editor_is_open_for_test(),
            "regenerating the tab must reactivate the filter bar"
        );
    });
}

#[gpui::test]
fn committing_a_new_filter_dispatches_add_filter_with_the_typed_value(
    cx: &mut gpui::TestAppContext,
) {
    let (controls, recorded) = recording_preview_controls();
    let (view, vcx) = view_with_sample_result_and_controls(cx, Some(controls));

    view.update_in(vcx, |view, window, cx| {
        view.begin_add_filter(window, cx);
        view.pick_filter_column(&column("id", "int8"), window, cx);
        view.set_filter_editor_value_for_test("paid", cx);
        view.commit_filter_edit(cx);
    });

    assert_eq!(
        recorded.borrow().as_slice(),
        [super::pager::PreviewAction::AddFilter {
            column: "id".to_owned(),
            type_name: "int8".to_owned(),
            operator: zsql_core::FilterOperator::Eq,
            value: "paid".to_owned(),
        }]
    );
    view.read_with(vcx, |view, _app| {
        assert!(
            !view.filter_editor_is_open_for_test(),
            "committing must close the editor"
        );
    });
}

#[gpui::test]
fn the_operator_menu_updates_the_editor_and_closes_itself(cx: &mut gpui::TestAppContext) {
    let (controls, _recorded) = recording_preview_controls();
    let (view, vcx) = view_with_sample_result_and_controls(cx, Some(controls));

    view.update_in(vcx, |view, window, cx| {
        view.begin_add_filter(window, cx);
        view.pick_filter_column(&column("id", "int8"), window, cx);
        view.toggle_filter_editor_menu(cx);
    });
    view.read_with(vcx, |view, _app| {
        assert!(view.filter_editor_menu_open_for_test());
    });

    view.update(vcx, |view, cx| {
        view.set_filter_editor_operator(zsql_core::FilterOperator::Ge, cx);
    });

    view.read_with(vcx, |view, _app| {
        assert_eq!(
            view.filter_editor_operator_for_test(),
            Some(zsql_core::FilterOperator::Ge)
        );
        assert!(
            !view.filter_editor_menu_open_for_test(),
            "picking an operator must close the menu"
        );
    });
}

#[gpui::test]
fn cancel_filter_edit_closes_the_editor_without_dispatching(cx: &mut gpui::TestAppContext) {
    let (controls, recorded) = recording_preview_controls();
    let (view, vcx) = view_with_sample_result_and_controls(cx, Some(controls));

    view.update_in(vcx, |view, window, cx| {
        view.begin_add_filter(window, cx);
        view.pick_filter_column(&column("id", "int8"), window, cx);
        view.set_filter_editor_value_for_test("paid", cx);
        view.cancel_filter_edit(cx);
    });

    assert!(recorded.borrow().is_empty(), "cancel must not dispatch");
    view.read_with(vcx, |view, _app| {
        assert!(!view.filter_editor_is_open_for_test());
    });
}

#[gpui::test]
fn begin_edit_filter_prefills_the_editor_from_the_existing_condition(
    cx: &mut gpui::TestAppContext,
) {
    let mut filters = zsql_core::FilterState::new();
    filters.add_condition("status", "text", zsql_core::FilterOperator::Eq, "paid");
    let condition = filters.conditions()[0].clone();
    let (controls, _recorded) = preview_controls_with_filters(&filters);
    let (view, vcx) = view_with_sample_result_and_controls(cx, Some(controls));

    view.update_in(vcx, |view, window, cx| {
        view.begin_edit_filter(&condition, window, cx);
    });

    view.read_with(vcx, |view, cx| {
        assert_eq!(
            view.filter_editor_operator_for_test(),
            Some(zsql_core::FilterOperator::Eq)
        );
        assert_eq!(
            view.filter_editor_value_for_test(cx).as_deref(),
            Some("paid")
        );
    });
}

#[gpui::test]
fn committing_an_edited_filter_dispatches_update_filter(cx: &mut gpui::TestAppContext) {
    let mut filters = zsql_core::FilterState::new();
    let id = filters.add_condition("status", "text", zsql_core::FilterOperator::Eq, "paid");
    let condition = filters.conditions()[0].clone();
    let (controls, recorded) = preview_controls_with_filters(&filters);
    let (view, vcx) = view_with_sample_result_and_controls(cx, Some(controls));

    view.update_in(vcx, |view, window, cx| {
        view.begin_edit_filter(&condition, window, cx);
        view.set_filter_editor_value_for_test("pending", cx);
        view.commit_filter_edit(cx);
    });

    assert_eq!(
        recorded.borrow().as_slice(),
        [super::pager::PreviewAction::UpdateFilter {
            id,
            operator: zsql_core::FilterOperator::Eq,
            value: "pending".to_owned(),
        }]
    );
}

#[gpui::test]
fn render_filter_bar_does_not_panic_across_every_editing_state(cx: &mut gpui::TestAppContext) {
    let mut filters = zsql_core::FilterState::new();
    filters.add_condition("status", "text", zsql_core::FilterOperator::Eq, "paid");
    filters.add_condition(
        "id",
        "int8",
        zsql_core::FilterOperator::Gt,
        "now() - interval '1 day'",
    );
    let (controls, _recorded) = preview_controls_with_filters(&filters);
    let (view, vcx) = view_with_sample_result_and_controls(cx, Some(controls));

    // Committed chips (including an fx-tagged expression value) and a live
    // dispatcher, with no editor open: covered by the window's initial draw.
    vcx.run_until_parked();

    // The column picker open, before a target column is chosen. Each state
    // renders through the window's own draw rather than a direct
    // render_filter_bar call: hover-state hooks inside the bar may only run
    // during a draw pass.
    view.update_in(vcx, |view, window, cx| {
        view.begin_add_filter(window, cx);
    });
    vcx.run_until_parked();

    // Mid-edit, with the operator menu open.
    view.update_in(vcx, |view, window, cx| {
        view.pick_filter_column(&column("status", "text"), window, cx);
        view.toggle_filter_editor_menu(cx);
    });
    vcx.run_until_parked();

    // Detached (no active preview controls): every control renders inert.
    view.update(vcx, |view, cx| view.set_preview_controls(None, cx));
    vcx.run_until_parked();
}
