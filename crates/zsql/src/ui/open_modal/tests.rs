use gpui::TestAppContext;
use zsql_ui::text_field::TextFieldEvent;

use super::{LibraryScript, OpenModalEvent, OpenModalView, PickerTarget, SessionScript};

fn init(cx: &mut TestAppContext) {
    cx.update(|cx| {
        zsql_ui::text_field::init(cx);
        super::init(cx);
    });
}

fn session(file_name: &str) -> SessionScript {
    SessionScript {
        file_name: file_name.to_owned(),
        relative_time: "2s".to_owned(),
    }
}

#[gpui::test]
fn opening_seeds_every_row_with_an_empty_filter(cx: &mut TestAppContext) {
    init(cx);
    let (view, vcx) = cx.add_window_view(|_window, cx| OpenModalView::new(cx));
    view.update(vcx, |view, cx| {
        view.open(
            "zsql-dev".to_owned(),
            vec![session("top-customers.sql")],
            vec![("top-customers.sql".to_owned(), 1)],
            vec![LibraryScript {
                name: "revenue-report".to_owned(),
                relative_time: "2w".to_owned(),
            }],
            vec![],
            cx,
        );
    });

    view.read_with(vcx, |view, _cx| {
        assert!(view.is_open());
        assert_eq!(view.rows_for_test().len(), 2);
        assert_eq!(view.selected_for_test(), Some(0));
    });
}

#[gpui::test]
fn typing_into_the_filter_field_narrows_the_rows(cx: &mut TestAppContext) {
    init(cx);
    let (view, vcx) = cx.add_window_view(|_window, cx| OpenModalView::new(cx));
    view.update(vcx, |view, cx| {
        view.open(
            "zsql-dev".to_owned(),
            vec![session("top-customers.sql")],
            vec![("top-customers.sql".to_owned(), 1)],
            vec![LibraryScript {
                name: "revenue-report".to_owned(),
                relative_time: "2w".to_owned(),
            }],
            vec![],
            cx,
        );
    });

    let filter_field = view.read_with(vcx, |view, _cx| view.filter_field.clone());
    filter_field.update(vcx, |field, cx| field.set_value("revenue", cx));
    vcx.run_until_parked();

    view.read_with(vcx, |view, _cx| {
        assert_eq!(view.rows_for_test().len(), 1);
        assert_eq!(view.rows_for_test()[0].label, "revenue-report.sql");
    });
}

#[gpui::test]
fn arrow_keys_advance_the_selection(cx: &mut TestAppContext) {
    init(cx);
    let (view, vcx) = cx.add_window_view(|_window, cx| OpenModalView::new(cx));
    view.update(vcx, |view, cx| {
        view.open(
            "zsql-dev".to_owned(),
            vec![session("a.sql"), session("b.sql")],
            vec![("a.sql".to_owned(), 1), ("b.sql".to_owned(), 2)],
            vec![],
            vec![],
            cx,
        );
    });

    view.update_in(vcx, |view, window, cx| {
        view.select_next(&super::SelectNextRow, window, cx);
    });

    view.read_with(vcx, |view, _cx| {
        assert_eq!(view.selected_for_test(), Some(1));
    });
}

#[gpui::test]
fn up_arrow_retreats_the_selection(cx: &mut TestAppContext) {
    init(cx);
    let (view, vcx) = cx.add_window_view(|_window, cx| OpenModalView::new(cx));
    view.update(vcx, |view, cx| {
        view.open(
            "zsql-dev".to_owned(),
            vec![session("a.sql"), session("b.sql")],
            vec![("a.sql".to_owned(), 1), ("b.sql".to_owned(), 2)],
            vec![],
            vec![],
            cx,
        );
    });

    view.update_in(vcx, |view, window, cx| {
        view.select_next(&super::SelectNextRow, window, cx);
    });

    view.update_in(vcx, |view, window, cx| {
        view.select_previous(&super::SelectPreviousRow, window, cx);
    });

    view.read_with(vcx, |view, _cx| {
        assert_eq!(view.selected_for_test(), Some(0));
    });
}

#[gpui::test]
fn confirming_the_selected_row_emits_open_with_its_target_and_closes(cx: &mut TestAppContext) {
    init(cx);
    let (view, vcx) = cx.add_window_view(|_window, cx| OpenModalView::new(cx));
    view.update(vcx, |view, cx| {
        view.open(
            "zsql-dev".to_owned(),
            vec![session("top-customers.sql")],
            vec![("top-customers.sql".to_owned(), 42)],
            vec![],
            vec![],
            cx,
        );
    });

    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let events_for_sub = events.clone();
    vcx.update(|_window, cx| {
        cx.subscribe(&view, move |_view, event: &OpenModalEvent, _cx| {
            events_for_sub.borrow_mut().push(event.clone());
        })
        .detach();
    });

    view.update(vcx, |view, cx| {
        view.filter_field.update(cx, |field, cx| {
            cx.emit(TextFieldEvent::Submit);
            let _ = field;
        });
    });
    vcx.run_until_parked();

    view.read_with(vcx, |view, _cx| {
        assert!(!view.is_open(), "confirming must close the picker");
    });
    let events = events.borrow();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        OpenModalEvent::Open(PickerTarget::FocusTab(42))
    ));
}

#[gpui::test]
fn a_named_session_script_not_open_anywhere_can_still_be_confirmed_and_opened(
    cx: &mut TestAppContext,
) {
    init(cx);
    let (view, vcx) = cx.add_window_view(|_window, cx| OpenModalView::new(cx));
    view.update(vcx, |view, cx| {
        view.open(
            "zsql-dev".to_owned(),
            vec![session("top-customers.sql")],
            vec![],
            vec![],
            vec![],
            cx,
        );
    });

    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let events_for_sub = events.clone();
    vcx.update(|_window, cx| {
        cx.subscribe(&view, move |_view, event: &OpenModalEvent, _cx| {
            events_for_sub.borrow_mut().push(event.clone());
        })
        .detach();
    });

    view.update(vcx, |view, cx| {
        view.filter_field.update(cx, |field, cx| {
            cx.emit(TextFieldEvent::Submit);
            let _ = field;
        });
    });
    vcx.run_until_parked();

    let events = events.borrow();
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        OpenModalEvent::Open(PickerTarget::OpenSessionScript(name)) if name == "top-customers.sql"
    ));
}

#[gpui::test]
fn escape_closes_without_emitting_open(cx: &mut TestAppContext) {
    init(cx);
    let (view, vcx) = cx.add_window_view(|_window, cx| OpenModalView::new(cx));
    view.update(vcx, |view, cx| {
        view.open(
            "zsql-dev".to_owned(),
            vec![session("top-customers.sql")],
            vec![("top-customers.sql".to_owned(), 1)],
            vec![],
            vec![],
            cx,
        );
    });

    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let events_for_sub = events.clone();
    vcx.update(|_window, cx| {
        cx.subscribe(&view, move |_view, event: &OpenModalEvent, _cx| {
            events_for_sub.borrow_mut().push(event.clone());
        })
        .detach();
    });

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    view.read_with(vcx, |view, _cx| {
        assert!(!view.is_open(), "escape must close the picker");
        assert!(view.rows_for_test().is_empty());
    });
    let events = events.borrow();
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], OpenModalEvent::Cancelled));
}

#[gpui::test]
fn browse_files_emits_the_browse_event_and_closes(cx: &mut TestAppContext) {
    init(cx);
    let (view, vcx) = cx.add_window_view(|_window, cx| OpenModalView::new(cx));
    view.update(vcx, |view, cx| {
        view.open("zsql-dev".to_owned(), vec![], vec![], vec![], vec![], cx);
    });

    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let events_for_sub = events.clone();
    vcx.update(|_window, cx| {
        cx.subscribe(&view, move |_view, event: &OpenModalEvent, _cx| {
            events_for_sub.borrow_mut().push(event.clone());
        })
        .detach();
    });

    view.update(vcx, OpenModalView::browse_files);

    view.read_with(vcx, |view, _cx| assert!(!view.is_open()));
    assert!(matches!(events.borrow()[0], OpenModalEvent::BrowseFiles));
}
