use gpui::{Focusable, TestAppContext};

use super::{Destination, SaveModalEvent, SaveModalKind, SaveModalView};

fn init_test(cx: &mut TestAppContext) {
    cx.update(|cx| {
        zsql_ui::text_field::init(cx, &zsql_ui::text_field::TextFieldBindings::default());
        crate::ui::save_modal::init(cx, &crate::ui::save_modal::SaveModalBindings::default());
    });
}

#[gpui::test]
fn renders_without_panicking_when_open_for_save(cx: &mut TestAppContext) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| SaveModalView::new(cx));
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            SaveModalKind::Save,
            "",
            Destination::Connection,
            "zsql-dev".to_owned(),
            std::path::PathBuf::from("/tmp/session"),
            std::path::PathBuf::from("/tmp/library"),
            None,
            cx,
        );
    });
    vcx.run_until_parked();

    modal.read_with(vcx, |view, _cx| {
        assert!(view.is_open());
    });
}

#[gpui::test]
fn opening_the_modal_focuses_the_name_field_without_the_caller_focusing_it_manually(
    cx: &mut TestAppContext,
) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| SaveModalView::new(cx));
    modal.update(vcx, |view, cx| {
        view.open(
            9,
            SaveModalKind::Save,
            "",
            Destination::Connection,
            "zsql-dev".to_owned(),
            std::path::PathBuf::from("/tmp/session-autofocus"),
            std::path::PathBuf::from("/tmp/library-autofocus"),
            None,
            cx,
        );
    });
    vcx.run_until_parked();

    let field_focus = modal.read_with(vcx, |view, cx| view.name_field.read(cx).focus_handle(cx));
    vcx.update(|window, _cx| {
        assert!(
            field_focus.is_focused(window),
            "open() must focus the name field on its own, without the caller \
             (a keybinding or context-menu seam with no window at open time) \
             having to focus it manually"
        );
    });
}

#[gpui::test]
fn up_and_down_while_the_name_field_is_focused_cycle_destination_without_losing_focus(
    cx: &mut TestAppContext,
) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| SaveModalView::new(cx));
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            SaveModalKind::Save,
            "top-customers",
            Destination::Connection,
            "zsql-dev".to_owned(),
            std::path::PathBuf::from("/tmp/session"),
            std::path::PathBuf::from("/tmp/library"),
            None,
            cx,
        );
    });
    vcx.run_until_parked();

    let field_focus = modal.read_with(vcx, |view, cx| view.name_field.read(cx).focus_handle(cx));
    vcx.update(|window, _cx| window.focus(&field_focus));
    vcx.run_until_parked();

    vcx.simulate_keystrokes("down");
    vcx.run_until_parked();

    modal.read_with(vcx, |view, cx| {
        assert_eq!(
            view.destination,
            Destination::Library,
            "pressing down must advance the destination to Library"
        );
        assert_eq!(
            view.name_field.read(cx).value().as_ref(),
            "top-customers",
            "the name field's content must be untouched by the arrow keystroke"
        );
    });
    vcx.update(|window, _cx| {
        assert!(
            field_focus.is_focused(window),
            "the name field must keep focus after the arrow keystroke"
        );
    });

    vcx.simulate_keystrokes("up");
    vcx.run_until_parked();
    modal.read_with(vcx, |view, _cx| {
        assert_eq!(view.destination, Destination::Connection);
    });
}

/// With destination rows visible (Save/Save-as), no ancestor key context
/// may intercept a digit keystroke -- every one of these names, several of
/// which contain digits a bare `1`/`2`/`3` destination-select binding would
/// otherwise swallow, must type into the name field exactly as written.
#[gpui::test]
fn digit_containing_names_type_exactly_with_destination_rows_visible(cx: &mut TestAppContext) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| SaveModalView::new(cx));
    modal.update(vcx, |view, cx| {
        view.open(
            1,
            SaveModalKind::Save,
            "",
            Destination::Connection,
            "zsql-dev".to_owned(),
            std::path::PathBuf::from("/tmp/session-digits"),
            std::path::PathBuf::from("/tmp/library-digits"),
            None,
            cx,
        );
    });
    vcx.run_until_parked();

    for name in ["q3-report", "top-10", "report-2024"] {
        modal.update(vcx, |view, cx| {
            view.name_field
                .update(cx, |field, cx| field.set_value("", cx));
        });
        vcx.run_until_parked();
        vcx.simulate_keystrokes(&name.chars().map(String::from).collect::<Vec<_>>().join(" "));
        vcx.run_until_parked();

        modal.read_with(vcx, |view, cx| {
            assert_eq!(
                view.name_field.read(cx).value().as_ref(),
                name,
                "no digit may be swallowed by a destination keybinding"
            );
        });
    }
}

#[gpui::test]
fn pressing_up_or_down_in_rename_mode_does_not_switch_the_fixed_destination(
    cx: &mut TestAppContext,
) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| SaveModalView::new(cx));
    modal.update(vcx, |view, cx| {
        view.open(
            3,
            SaveModalKind::Rename,
            "top-customers",
            Destination::Library,
            "zsql-dev".to_owned(),
            std::path::PathBuf::from("/tmp/session-rename"),
            std::path::PathBuf::from("/tmp/library-rename"),
            None,
            cx,
        );
    });
    vcx.run_until_parked();

    let field_focus = modal.read_with(vcx, |view, cx| view.name_field.read(cx).focus_handle(cx));
    vcx.update(|window, _cx| window.focus(&field_focus));
    vcx.run_until_parked();

    vcx.simulate_keystrokes("down");
    vcx.run_until_parked();

    modal.read_with(vcx, |view, _cx| {
        assert_eq!(
            view.destination,
            Destination::Library,
            "a Rename modal's destination is fixed to the tab's own backing \
             and must not respond to Up/Down navigation"
        );
    });
}

#[gpui::test]
fn enter_confirms_only_when_the_name_is_valid(cx: &mut TestAppContext) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| SaveModalView::new(cx));
    modal.update(vcx, |view, cx| {
        view.open(
            7,
            SaveModalKind::Save,
            "",
            Destination::Connection,
            "zsql-dev".to_owned(),
            std::path::PathBuf::from("/tmp/session-escape"),
            std::path::PathBuf::from("/tmp/library-escape"),
            None,
            cx,
        );
    });
    vcx.run_until_parked();

    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let events_for_sub = events.clone();
    vcx.update(|_window, cx| {
        cx.subscribe(&modal, move |_modal, event: &SaveModalEvent, _cx| {
            events_for_sub.borrow_mut().push(format!("{event:?}"));
        })
        .detach();
    });

    let field_focus = modal.read_with(vcx, |view, cx| view.name_field.read(cx).focus_handle(cx));
    vcx.update(|window, _cx| window.focus(&field_focus));
    vcx.run_until_parked();

    // An empty name must not be confirmable via Enter.
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    modal.read_with(vcx, |view, _cx| {
        assert!(
            view.is_open(),
            "Enter with an invalid (empty) name must not close the modal"
        );
    });

    vcx.simulate_input("top-customers");
    vcx.run_until_parked();
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();

    modal.read_with(vcx, |view, _cx| {
        assert!(
            !view.is_open(),
            "Enter with a valid name must confirm and close"
        );
    });
    assert!(
        events.borrow().iter().any(|e| e.starts_with("Confirmed")),
        "a Confirmed event must have been emitted: {:?}",
        events.borrow()
    );
}

/// A file landing at the destination path between the last validated
/// keystroke and pressing Enter (e.g. this app's own detached background
/// library writers) must never panic `confirm`. `can_save` only reflects
/// the last-keystroke validation; the fresh re-validation inside `confirm`
/// must catch the new duplicate, restore `open` with the error, and keep
/// the modal open -- the same outcome the keystroke-time path would show.
#[gpui::test]
fn a_file_appearing_between_the_last_keystroke_and_enter_shows_the_error_without_panicking(
    cx: &mut TestAppContext,
) {
    init_test(cx);
    let session_dir = std::env::temp_dir().join(format!(
        "zsql-save-modal-toctou-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&session_dir).expect("must create session dir");
    let _cleanup = TestDirGuard(session_dir.clone());

    let (modal, vcx) = cx.add_window_view(|_window, cx| SaveModalView::new(cx));
    modal.update(vcx, |view, cx| {
        view.open(
            5,
            SaveModalKind::Save,
            "",
            Destination::Connection,
            "zsql-dev".to_owned(),
            session_dir.clone(),
            std::path::PathBuf::from("/tmp/library-toctou"),
            None,
            cx,
        );
    });
    vcx.run_until_parked();

    let field_focus = modal.read_with(vcx, |view, cx| view.name_field.read(cx).focus_handle(cx));
    vcx.update(|window, _cx| window.focus(&field_focus));
    vcx.run_until_parked();
    vcx.simulate_input("top-customers");
    vcx.run_until_parked();
    modal.read_with(vcx, |view, _cx| {
        assert!(view.can_save(), "the name must validate at keystroke time");
    });

    // A background writer lands the exact same file the modal is about to
    // save to, after the last keystroke's validation but before Enter.
    std::fs::create_dir_all(session_dir.join("scripts")).expect("must create scripts dir");
    std::fs::write(
        session_dir.join("scripts").join("top-customers.sql"),
        "select 'raced';",
    )
    .expect("must write the racing file");

    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();

    modal.read_with(vcx, |view, cx| {
        assert!(
            view.is_open(),
            "a fresh duplicate must keep the modal open, not confirm over it"
        );
        assert_eq!(
            view.name_field.read(cx).value().as_ref(),
            "top-customers",
            "the typed name must be preserved, not lost"
        );
    });
}

/// Opening Rename pre-filled with the tab's own current name, destination,
/// and current file must not immediately show a duplicate error against the
/// file the tab itself already owns.
#[gpui::test]
fn opening_rename_pre_filled_with_its_own_name_shows_no_error(cx: &mut TestAppContext) {
    init_test(cx);
    let session_dir = std::env::temp_dir().join(format!(
        "zsql-save-modal-self-conflict-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&session_dir).expect("must create session dir");
    std::fs::create_dir_all(session_dir.join("scripts")).expect("must create scripts dir");
    std::fs::write(
        session_dir.join("scripts").join("top-customers.sql"),
        "select 1;",
    )
    .expect("must write");
    let _cleanup = TestDirGuard(session_dir.clone());

    let (modal, vcx) = cx.add_window_view(|_window, cx| SaveModalView::new(cx));
    modal.update(vcx, |view, cx| {
        view.open(
            9,
            SaveModalKind::Rename,
            "top-customers",
            Destination::Connection,
            "zsql-dev".to_owned(),
            session_dir.clone(),
            std::path::PathBuf::from("/tmp/library-self-conflict"),
            Some(session_dir.join("scripts").join("top-customers.sql")),
            cx,
        );
    });
    vcx.run_until_parked();

    modal.read_with(vcx, |view, _cx| {
        assert!(
            view.can_save(),
            "a tab's own current file must never trip its own duplicate-name check on open"
        );
    });
}

/// Save-as from an external tab whose file stem happens to match an
/// existing session script must never be exempted from the duplicate check
/// just because the modal was opened pre-filled with that stem: `Save-as`
/// never has a "current file" of its own (see `SaveModalView::open`'s doc),
/// so a name collision with an unrelated existing file is a genuine
/// conflict, not a self-reference to the external tab's own (entirely
/// different) path.
#[gpui::test]
fn save_as_from_an_external_tab_still_conflicts_with_an_existing_session_script_of_the_same_stem(
    cx: &mut TestAppContext,
) {
    init_test(cx);
    let session_dir = std::env::temp_dir().join(format!(
        "zsql-save-modal-external-save-as-conflict-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&session_dir).expect("must create session dir");
    std::fs::create_dir_all(session_dir.join("scripts")).expect("must create scripts dir");
    std::fs::write(session_dir.join("scripts").join("migrate.sql"), "select 1;")
        .expect("must write");
    let _cleanup = TestDirGuard(session_dir.clone());

    let (modal, vcx) = cx.add_window_view(|_window, cx| SaveModalView::new(cx));
    modal.update(vcx, |view, cx| {
        // Mirrors `WorkspaceView::open_save_modal`'s External arm: seeded
        // from the external file's stem and the ordinary Connection
        // default, with no current file to exclude.
        view.open(
            11,
            SaveModalKind::SaveAs,
            "migrate",
            Destination::Connection,
            "zsql-dev".to_owned(),
            session_dir.clone(),
            std::path::PathBuf::from("/tmp/library-external-save-as-conflict"),
            None,
            cx,
        );
    });
    vcx.run_until_parked();

    modal.read_with(vcx, |view, _cx| {
        assert!(
            !view.can_save(),
            "a name matching an existing, unrelated session script must still conflict, \
             even though the modal opened pre-filled with that exact name"
        );
    });
}

/// A temp directory this test owns exclusively, removed on drop.
struct TestDirGuard(std::path::PathBuf);
impl Drop for TestDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[gpui::test]
fn escape_closes_the_modal_and_emits_cancelled_without_confirming(cx: &mut TestAppContext) {
    init_test(cx);
    let (modal, vcx) = cx.add_window_view(|_window, cx| SaveModalView::new(cx));
    modal.update(vcx, |view, cx| {
        view.open(
            7,
            SaveModalKind::Save,
            "",
            Destination::Connection,
            "zsql-dev".to_owned(),
            std::path::PathBuf::from("/tmp/session-escape"),
            std::path::PathBuf::from("/tmp/library-escape"),
            None,
            cx,
        );
    });
    vcx.run_until_parked();

    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let events_for_sub = events.clone();
    vcx.update(|_window, cx| {
        cx.subscribe(&modal, move |_modal, event: &SaveModalEvent, _cx| {
            events_for_sub.borrow_mut().push(format!("{event:?}"));
        })
        .detach();
    });

    let field_focus = modal.read_with(vcx, |view, cx| view.name_field.read(cx).focus_handle(cx));
    vcx.update(|window, _cx| window.focus(&field_focus));
    vcx.run_until_parked();

    vcx.simulate_input("top-customers");
    vcx.run_until_parked();
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    modal.read_with(vcx, |view, _cx| {
        assert!(!view.is_open(), "Escape must close the modal");
    });
    assert!(
        events.borrow().iter().any(|e| e == "Cancelled"),
        "a Cancelled event must have been emitted: {:?}",
        events.borrow()
    );
    assert!(
        events.borrow().iter().all(|e| !e.starts_with("Confirmed")),
        "Escape must never emit Confirmed: {:?}",
        events.borrow()
    );
}
