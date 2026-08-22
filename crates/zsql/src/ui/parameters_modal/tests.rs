use std::collections::HashMap;

use gpui::{Focusable, TestAppContext};
use zsql_core::sql::params::detect_parameters;

use super::ParametersModalView;

fn init_test(cx: &mut TestAppContext) {
    cx.update(|cx| {
        zsql_ui::text_field::init(cx, &zsql_ui::text_field::TextFieldBindings::default());
    });
}

#[gpui::test]
fn a_freshly_built_modal_is_closed(cx: &mut TestAppContext) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| ParametersModalView::new(cx));
    modal.read_with(vcx, |view, _cx| assert!(!view.is_open()));
}

#[gpui::test]
fn opening_seeds_one_field_per_detected_parameter(cx: &mut TestAppContext) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| ParametersModalView::new(cx));
    let sql = "SELECT * FROM orders WHERE status = :status AND total >= :min_total";
    let parameters = detect_parameters(sql, "postgres");
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            "orders_report.sql".to_owned(),
            sql.to_owned(),
            parameters,
            HashMap::new(),
            "session:orders_report.sql".to_owned(),
            "postgres",
            cx,
        );
    });
    vcx.run_until_parked();

    modal.read_with(vcx, |view, _cx| {
        assert!(view.is_open());
        assert_eq!(view.parameter_count(), 2);
        assert_eq!(
            super::row_label(&view.fields[0].row),
            ":status",
            "a colon parameter's row must label itself with its leading colon, distinct \
             from a positional or @-style row"
        );
    });
}

#[gpui::test]
fn a_query_with_no_parameters_opens_with_no_fields(cx: &mut TestAppContext) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| ParametersModalView::new(cx));
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            "orders_report.sql".to_owned(),
            "SELECT 1".to_owned(),
            Vec::new(),
            HashMap::new(),
            "session:orders_report.sql".to_owned(),
            "postgres",
            cx,
        );
    });
    vcx.run_until_parked();

    modal.read_with(vcx, |view, _cx| assert_eq!(view.parameter_count(), 0));
}

#[gpui::test]
fn close_emits_cancelled_and_mutates_nothing(cx: &mut TestAppContext) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| ParametersModalView::new(cx));
    let sql = "SELECT * FROM orders WHERE status = :status";
    let parameters = detect_parameters(sql, "postgres");
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            "orders_report.sql".to_owned(),
            sql.to_owned(),
            parameters,
            HashMap::new(),
            "session:orders_report.sql".to_owned(),
            "postgres",
            cx,
        );
    });
    vcx.run_until_parked();

    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let events_for_sub = events.clone();
    vcx.update(|_window, cx| {
        cx.subscribe(
            &modal,
            move |_modal, event: &super::ParametersModalEvent, _cx| {
                events_for_sub.borrow_mut().push(format!("{event:?}"));
            },
        )
        .detach();
    });

    modal.update(vcx, super::ParametersModalView::close);
    vcx.run_until_parked();

    modal.read_with(vcx, |view, _cx| assert!(!view.is_open()));
    assert!(
        events.borrow().iter().any(|e| e == "Cancelled"),
        "a Cancelled event must have been emitted: {:?}",
        events.borrow()
    );
}

#[gpui::test]
fn escape_closes_the_modal(cx: &mut TestAppContext) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| ParametersModalView::new(cx));
    let sql = "SELECT * FROM orders WHERE status = :status";
    let parameters = detect_parameters(sql, "postgres");
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            "orders_report.sql".to_owned(),
            sql.to_owned(),
            parameters,
            HashMap::new(),
            "session:orders_report.sql".to_owned(),
            "postgres",
            cx,
        );
    });
    vcx.run_until_parked();

    let field_focus = modal.read_with(vcx, |view, cx| {
        view.fields[0].field.read(cx).focus_handle(cx)
    });
    vcx.update(|window, _cx| window.focus(&field_focus));
    vcx.run_until_parked();

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    modal.read_with(vcx, |view, _cx| {
        assert!(!view.is_open(), "Escape must close the modal");
    });
}

#[gpui::test]
fn confirm_with_an_empty_required_field_does_not_close_or_emit(cx: &mut TestAppContext) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| ParametersModalView::new(cx));
    let sql = "SELECT * FROM orders WHERE status = :status";
    let parameters = detect_parameters(sql, "postgres");
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            "orders_report.sql".to_owned(),
            sql.to_owned(),
            parameters,
            HashMap::new(),
            "session:orders_report.sql".to_owned(),
            "postgres",
            cx,
        );
    });
    vcx.run_until_parked();

    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let events_for_sub = events.clone();
    vcx.update(|_window, cx| {
        cx.subscribe(
            &modal,
            move |_modal, event: &super::ParametersModalEvent, _cx| {
                events_for_sub.borrow_mut().push(format!("{event:?}"));
            },
        )
        .detach();
    });

    modal.update(vcx, super::ParametersModalView::confirm);
    vcx.run_until_parked();

    modal.read_with(vcx, |view, _cx| {
        assert!(view.is_open(), "an empty required field must block the run");
    });
    assert!(
        events.borrow().is_empty(),
        "an empty required field must never emit an event: {:?}",
        events.borrow()
    );
}

#[gpui::test]
fn confirm_with_every_field_filled_emits_confirmed_with_substituted_sql(cx: &mut TestAppContext) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| ParametersModalView::new(cx));
    let sql = "SELECT * FROM orders WHERE status = :status";
    let parameters = detect_parameters(sql, "postgres");
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            "orders_report.sql".to_owned(),
            sql.to_owned(),
            parameters,
            HashMap::new(),
            "session:orders_report.sql".to_owned(),
            "postgres",
            cx,
        );
        view.fields[0]
            .field
            .update(cx, |field, cx| field.set_value("shipped", cx));
    });
    vcx.run_until_parked();

    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let events_for_sub = events.clone();
    vcx.update(|_window, cx| {
        cx.subscribe(
            &modal,
            move |_modal, event: &super::ParametersModalEvent, _cx| {
                events_for_sub.borrow_mut().push(format!("{event:?}"));
            },
        )
        .detach();
    });

    modal.update(vcx, super::ParametersModalView::confirm);
    vcx.run_until_parked();

    modal.read_with(vcx, |view, _cx| assert!(!view.is_open()));
    let recorded = events.borrow();
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].starts_with("Confirmed"));
    assert!(recorded[0].contains("status = 'shipped'"));
}

#[gpui::test]
fn a_parameter_with_a_remembered_value_is_prefilled(cx: &mut TestAppContext) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| ParametersModalView::new(cx));
    let sql = "SELECT * FROM orders WHERE status = :status";
    let parameters = detect_parameters(sql, "postgres");
    let mut history = HashMap::new();
    history.insert("status".to_owned(), vec!["shipped".to_owned()]);
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            "orders_report.sql".to_owned(),
            sql.to_owned(),
            parameters,
            history,
            "session:orders_report.sql".to_owned(),
            "postgres",
            cx,
        );
    });
    vcx.run_until_parked();

    modal.read_with(vcx, |view, cx| {
        assert_eq!(view.fields[0].field.read(cx).value().as_ref(), "shipped");
    });
}

#[gpui::test]
fn a_parameter_with_multiple_remembered_values_prefills_only_the_most_recent(
    cx: &mut TestAppContext,
) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| ParametersModalView::new(cx));
    let sql = "SELECT * FROM orders WHERE status = :status";
    let parameters = detect_parameters(sql, "postgres");
    let mut history = HashMap::new();
    history.insert(
        "status".to_owned(),
        vec!["shipped".to_owned(), "pending".to_owned()],
    );
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            "orders_report.sql".to_owned(),
            sql.to_owned(),
            parameters,
            history,
            "session:orders_report.sql".to_owned(),
            "postgres",
            cx,
        );
    });
    vcx.run_until_parked();

    modal.read_with(vcx, |view, cx| {
        assert_eq!(view.fields[0].field.read(cx).value().as_ref(), "shipped");
    });
}

#[gpui::test]
fn tab_and_shift_tab_cycle_focus_through_fields_and_footer_buttons_in_visual_order(
    cx: &mut TestAppContext,
) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| ParametersModalView::new(cx));
    let sql = "SELECT * FROM orders WHERE status = :status AND total >= :min_total";
    let parameters = detect_parameters(sql, "postgres");
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            "orders_report.sql".to_owned(),
            sql.to_owned(),
            parameters,
            HashMap::new(),
            "session:orders_report.sql".to_owned(),
            "postgres",
            cx,
        );
    });
    vcx.run_until_parked();

    let handles = modal.read_with(vcx, |view, cx| {
        vec![
            view.fields[0].field.read(cx).focus_handle(cx),
            view.fields[1].field.read(cx).focus_handle(cx),
            view.cancel_focus.clone(),
            view.run_focus.clone(),
        ]
    });

    // Opening the modal auto-focuses the first field.
    vcx.update(|window, cx| {
        assert_eq!(window.focused(cx), Some(handles[0].clone()));
    });

    for expected in &handles[1..] {
        vcx.simulate_keystrokes("tab");
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            assert_eq!(window.focused(cx), Some(expected.clone()));
        });
    }

    // One more Tab wraps back to the first field.
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        assert_eq!(window.focused(cx), Some(handles[0].clone()));
    });

    // Shift-Tab walks the same order backward, starting from field[0]
    // (where the forward wraparound above left focus).
    for expected in handles.iter().rev() {
        vcx.simulate_keystrokes("shift-tab");
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            assert_eq!(window.focused(cx), Some(expected.clone()));
        });
    }
}

#[gpui::test]
fn positional_rows_render_question_mark_number_labels_with_the_bare_token_highlighted(
    cx: &mut TestAppContext,
) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| ParametersModalView::new(cx));
    let sql = "SELECT * FROM orders WHERE status = ? AND prior_status = ?";
    let parameters = detect_parameters(sql, "mysql");
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            "orders_report.sql".to_owned(),
            sql.to_owned(),
            parameters,
            HashMap::new(),
            "session:orders_report.sql".to_owned(),
            "mysql",
            cx,
        );
    });
    vcx.run_until_parked();

    modal.read_with(vcx, |view, _cx| {
        assert_eq!(view.fields.len(), 2);
        assert_eq!(super::row_label(&view.fields[0].row), "?1");
        assert_eq!(super::row_label(&view.fields[1].row), "?2");
        assert_eq!(
            &view.fields[0].row.line_text
                [view.fields[0].row.token_start..view.fields[0].row.token_end],
            "?",
            "the snippet highlights the real bare-? token, not the ?1 display label"
        );
        assert_eq!(
            &view.fields[1].row.line_text
                [view.fields[1].row.token_start..view.fields[1].row.token_end],
            "?"
        );
    });
}

#[gpui::test]
fn an_at_name_row_on_mssql_renders_its_at_prefixed_label_and_highlights_the_real_token(
    cx: &mut TestAppContext,
) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| ParametersModalView::new(cx));
    let sql = "SELECT * FROM orders WHERE start_date >= @start_date";
    let parameters = detect_parameters(sql, "mssql");
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            "orders_report.sql".to_owned(),
            sql.to_owned(),
            parameters,
            HashMap::new(),
            "session:orders_report.sql".to_owned(),
            "mssql",
            cx,
        );
    });
    vcx.run_until_parked();

    modal.read_with(vcx, |view, _cx| {
        assert_eq!(view.fields.len(), 1);
        assert_eq!(super::row_label(&view.fields[0].row), "@start_date");
        assert_eq!(
            &view.fields[0].row.line_text
                [view.fields[0].row.token_start..view.fields[0].row.token_end],
            "@start_date",
            "the snippet highlights the real @name token, not a re-derived label"
        );
    });
}

#[gpui::test]
fn a_colon_and_at_parameter_sharing_a_name_prefill_from_independent_remembered_values(
    cx: &mut TestAppContext,
) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| ParametersModalView::new(cx));
    let sql = "WHERE status = :status OR legacy_status = @status";
    let parameters = detect_parameters(sql, "mssql");
    let mut history = HashMap::new();
    history.insert("status".to_owned(), vec!["shipped".to_owned()]);
    history.insert("@status".to_owned(), vec!["legacy_shipped".to_owned()]);
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            "orders_report.sql".to_owned(),
            sql.to_owned(),
            parameters,
            history,
            "session:orders_report.sql".to_owned(),
            "mssql",
            cx,
        );
    });
    vcx.run_until_parked();

    modal.read_with(vcx, |view, cx| {
        assert_eq!(view.fields.len(), 2);
        assert_eq!(view.fields[0].field.read(cx).value().as_ref(), "shipped");
        assert_eq!(
            view.fields[1].field.read(cx).value().as_ref(),
            "legacy_shipped"
        );
    });
}

#[gpui::test]
fn positional_parameters_prefill_from_remembered_values_keyed_by_their_question_mark_number_label(
    cx: &mut TestAppContext,
) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| ParametersModalView::new(cx));
    let sql = "SELECT * FROM orders WHERE a = ? AND b = ?";
    let parameters = detect_parameters(sql, "mysql");
    let mut history = HashMap::new();
    history.insert("?1".to_owned(), vec!["x".to_owned()]);
    history.insert("?2".to_owned(), vec!["y".to_owned()]);
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            "orders_report.sql".to_owned(),
            sql.to_owned(),
            parameters,
            history,
            "session:orders_report.sql".to_owned(),
            "mysql",
            cx,
        );
    });
    vcx.run_until_parked();

    modal.read_with(vcx, |view, cx| {
        assert_eq!(view.fields.len(), 2);
        assert_eq!(view.fields[0].row.key, "?1");
        assert_eq!(view.fields[1].row.key, "?2");
        assert_eq!(view.fields[0].field.read(cx).value().as_ref(), "x");
        assert_eq!(view.fields[1].field.read(cx).value().as_ref(), "y");
    });
}
