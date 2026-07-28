use std::sync::{Arc, Mutex};

use gpui::{Bounds, EntityInputHandler, Modifiers, Pixels, Point, TestAppContext, point};

use super::{
    Backspace, Copy, Cut, DeleteForward, MoveEnd, MoveLeft, MoveRight, Paste, SelectAll,
    SelectLeft, SelectRight, Submit, TextFieldEvent, TextFieldState,
};
use crate::text_field::theme;

#[cfg(test)]
impl TextFieldState {
    /// The pixel point that hit-tests back to `offset`, computed from the
    /// most recent paint's shaped line and bounds. Lets mouse-handling tests
    /// drive `on_mouse_down`/`on_mouse_move` with real, paint-derived
    /// coordinates instead of guessed pixel offsets.
    ///
    /// # Panics
    ///
    /// Panics if no paint has run yet.
    fn point_for_offset_for_test(&self, offset: usize) -> Point<Pixels> {
        let bounds = self
            .last_bounds
            .expect("a paint must run before computing a point for an offset");
        let line = self
            .last_line
            .as_ref()
            .expect("a paint must run before computing a point for an offset");
        point(bounds.left() + line.x_for_index(offset), bounds.top())
    }
}

fn build_field<'a>(
    cx: &'a mut TestAppContext,
    initial_value: Option<&str>,
) -> (
    gpui::Entity<TextFieldState>,
    &'a mut gpui::VisualTestContext,
) {
    cx.add_window_view(|window, cx| {
        let state = TextFieldState::new("Search", initial_value, cx);
        window.focus(&state.focus_handle);
        state
    })
}

// -- render smoke --------------------------------------------------

#[gpui::test]
fn field_renders_one_frame_without_panicking_when_unfocused(cx: &mut TestAppContext) {
    let (_field, vcx) = cx.add_window_view(|_window, cx| {
        TextFieldState::new("Search connections", Some("staging"), cx)
    });
    vcx.run_until_parked();
}

#[gpui::test]
fn field_renders_one_frame_without_panicking_when_focused(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("select 1"));
    vcx.run_until_parked();
    // Exercise the selection-quad and cursor-quad paint paths too.
    field.update(vcx, |field, cx| {
        field.model.set_selection(0, 3);
        cx.notify();
    });
    vcx.run_until_parked();
}

#[gpui::test]
fn field_with_empty_content_renders_the_placeholder_without_panicking(cx: &mut TestAppContext) {
    let (_field, vcx) = build_field(cx, None);
    vcx.run_until_parked();
}

// -- masking -----------------------------------------------------------

#[gpui::test]
fn a_masked_field_renders_without_panicking_and_keeps_its_real_value(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("hunter2"));
    field.update(vcx, |field, cx| field.set_masked(true, cx));
    vcx.run_until_parked();

    field.read_with(vcx, |field, _cx| {
        assert!(field.is_masked());
        assert_eq!(field.value().as_ref(), "hunter2");
    });
}

#[gpui::test]
fn revealing_a_masked_field_clears_the_masked_flag_and_keeps_the_value(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("s3cret"));
    field.update(vcx, |field, cx| field.set_masked(true, cx));
    field.update(vcx, |field, cx| field.set_masked(false, cx));
    vcx.run_until_parked();

    field.read_with(vcx, |field, _cx| {
        assert!(!field.is_masked());
        assert_eq!(field.value().as_ref(), "s3cret");
    });
}

#[gpui::test]
fn typing_into_a_masked_field_edits_the_real_content(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, None);
    field.update(vcx, |field, cx| field.set_masked(true, cx));
    vcx.simulate_input("pw");
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.value().as_ref(), "pw");
    });
}

#[gpui::test]
fn clicking_a_masked_field_places_the_cursor_at_the_content_offset_not_the_mask_glyph_offset(
    cx: &mut TestAppContext,
) {
    // "p\u{e9}ss" mixes a multi-byte char in with ASCII so a masked click
    // that naively used the mask's display index (== char count) instead
    // of converting back to a content byte offset would land in the
    // wrong place.
    let (field, vcx) = build_field(cx, Some("p\u{e9}ss"));
    field.update(vcx, |field, cx| field.set_masked(true, cx));
    vcx.run_until_parked();

    // Third mask glyph -> third char -> byte offset 4 (after the 2-byte
    // 'e9').
    let click_point = field.read_with(vcx, |field, _cx| field.point_for_offset_for_test(3));
    vcx.simulate_click(click_point, Modifiers::default());
    vcx.run_until_parked();

    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.model.cursor(), 4);
    });
}

// -- value / round-trip ----------------------------------------------

#[gpui::test]
fn value_after_set_value_round_trips(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("initial"));
    field.update(vcx, |field, cx| field.set_value("replaced", cx));
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.value().as_ref(), "replaced");
    });
}

#[gpui::test]
fn set_value_quiet_updates_the_value_the_same_as_set_value(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("initial"));
    field.update(vcx, |field, _cx| field.set_value_quiet("replaced"));
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.value().as_ref(), "replaced");
    });
}

// -- submit ----------------------------------------------------------

#[gpui::test]
fn enter_emits_submit_and_no_other_key_does(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("select 1"));

    let submits: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let recorded = submits.clone();
    vcx.update(|_window, cx| {
        cx.subscribe(&field, move |_field, event, _cx| {
            if matches!(event, TextFieldEvent::Submit) {
                *recorded.lock().expect("submit count lock poisoned") += 1;
            }
        })
        .detach();
    });

    vcx.dispatch_action(MoveRight);
    vcx.dispatch_action(Backspace);
    vcx.dispatch_action(SelectAll);
    vcx.dispatch_action(Copy);
    vcx.run_until_parked();
    assert_eq!(
        *submits.lock().expect("submit count lock poisoned"),
        0,
        "no non-Enter key should submit"
    );

    vcx.dispatch_action(Submit);
    vcx.run_until_parked();
    assert_eq!(
        *submits.lock().expect("submit count lock poisoned"),
        1,
        "Enter should submit exactly once"
    );
}

#[gpui::test]
fn enter_never_inserts_a_newline(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("select 1"));
    vcx.dispatch_action(MoveEnd);
    vcx.dispatch_action(Submit);
    vcx.run_until_parked();
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.value().as_ref(), "select 1");
    });
}

// -- movement / editing actions ------------------------------------

#[gpui::test]
fn move_right_and_backspace_actions_delegate_to_the_model(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("abc"));
    field.update(vcx, |field, _cx| field.model.set_cursor(0));

    vcx.dispatch_action(MoveRight);
    vcx.dispatch_action(MoveRight);
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.model.cursor(), 2);
    });

    vcx.dispatch_action(Backspace);
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.value().as_ref(), "ac");
        assert_eq!(field.model.cursor(), 1);
    });
}

#[gpui::test]
fn move_left_and_delete_forward_actions_delegate_to_the_model(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("abc"));

    vcx.dispatch_action(MoveLeft);
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.model.cursor(), 2);
    });

    vcx.dispatch_action(DeleteForward);
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.value().as_ref(), "ab");
        assert_eq!(field.model.cursor(), 2);
    });
}

#[gpui::test]
fn shift_extend_and_select_all_actions_delegate_to_the_model(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("hello"));
    field.update(vcx, |field, _cx| field.model.set_cursor(0));

    vcx.dispatch_action(SelectRight);
    vcx.dispatch_action(SelectRight);
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.model.selected_text(), "he");
    });

    vcx.dispatch_action(SelectLeft);
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.model.selected_text(), "h");
    });

    vcx.dispatch_action(SelectAll);
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.model.selected_text(), "hello");
    });
}

// -- clipboard -------------------------------------------------------

#[gpui::test]
fn cut_copies_the_selection_to_the_clipboard_then_removes_it(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("hello world"));
    field.update(vcx, |field, _cx| field.model.set_selection(6, 11));

    vcx.dispatch_action(Cut);
    vcx.run_until_parked();

    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.value().as_ref(), "hello ");
    });
    vcx.update(|_window, cx| {
        let clipboard = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .expect("cut should have written to the clipboard");
        assert_eq!(clipboard, "world");
    });
}

#[gpui::test]
fn copy_writes_the_selection_to_the_clipboard_and_leaves_content_unchanged(
    cx: &mut TestAppContext,
) {
    let (field, vcx) = build_field(cx, Some("hello world"));
    field.update(vcx, |field, _cx| field.model.set_selection(6, 11));

    vcx.dispatch_action(Copy);
    vcx.run_until_parked();

    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.value().as_ref(), "hello world");
        assert_eq!(field.model.selected_text(), "world");
    });
    vcx.update(|_window, cx| {
        let clipboard = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .expect("copy should have written to the clipboard");
        assert_eq!(clipboard, "world");
    });
}

#[gpui::test]
fn copy_with_no_selection_writes_nothing_to_the_clipboard(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("hello world"));
    vcx.update(|_window, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("sentinel".to_owned()));
    });
    field.update(vcx, |field, _cx| field.model.set_cursor(3));
    assert!(!field.read_with(vcx, |field, _cx| field.model.has_selection()));

    vcx.dispatch_action(Copy);
    vcx.run_until_parked();

    vcx.update(|_window, cx| {
        let clipboard = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .expect("clipboard should still hold the sentinel");
        assert_eq!(
            clipboard, "sentinel",
            "copy with no selection must not touch the clipboard"
        );
    });
}

#[gpui::test]
fn paste_inserts_the_clipboard_text_at_the_cursor(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("select "));
    vcx.update(|_window, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("1".to_owned()));
    });
    field.update(vcx, |field, _cx| field.model.move_end());

    vcx.dispatch_action(Paste);
    vcx.run_until_parked();

    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.value().as_ref(), "select 1");
    });
}

// -- typed / IME input --------------------------------------------------

#[gpui::test]
fn typing_inserts_characters_into_the_field_via_the_input_handler(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, None);
    vcx.simulate_input("select 1");
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.value().as_ref(), "select 1");
    });
}

#[gpui::test]
fn ime_composition_marks_then_commits_text(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("select "));
    field.update(vcx, |field, _cx| field.model.move_end());

    vcx.update(|window, cx| {
        field.update(cx, |field, cx| {
            field.replace_and_mark_text_in_range(None, "n", Some(1..1), window, cx);
        });
    });
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.value().as_ref(), "select n");
        assert_eq!(field.marked_range, Some(7..8));
    });

    vcx.update(|window, cx| {
        field.update(cx, |field, cx| {
            field.replace_text_in_range(None, "now", window, cx);
        });
    });
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.value().as_ref(), "select now");
        assert!(
            field.marked_range.is_none(),
            "committing text must clear the composition range"
        );
    });
}

#[gpui::test]
fn marked_text_range_converts_byte_range_to_utf16(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, None);
    field.update(vcx, |field, cx| {
        // Use multi-byte emoji to distinguish byte from UTF-16 offsets.
        field.set_value("a\u{1F600}b", cx);
    });

    vcx.update(|window, cx| {
        field.update(cx, |field, cx| {
            // Mark the emoji (bytes 1..5) which is UTF-16 range 1..3.
            field.replace_and_mark_text_in_range(Some(1..3), "\u{1F600}", Some(0..2), window, cx);
        });
    });

    let marked_range =
        vcx.update(|window, cx| field.update(cx, |field, cx| field.marked_text_range(window, cx)));

    assert_eq!(
        marked_range,
        Some(1..3),
        "marked_text_range must convert byte range to UTF-16 range"
    );
}

#[gpui::test]
fn text_for_range_returns_slice_and_writes_actual_range(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, None);
    field.update(vcx, |field, cx| {
        // Use multi-byte emoji to distinguish byte from UTF-16 offsets.
        field.set_value("a\u{1F600}b", cx);
    });
    vcx.run_until_parked();

    let (text, actual_range) = vcx.update(|window, cx| {
        field.update(cx, |field, cx| {
            let mut actual = None;
            let text = field.text_for_range(1..3, &mut actual, window, cx);
            (text, actual)
        })
    });

    assert_eq!(
        text,
        Some("\u{1F600}".to_owned()),
        "text_for_range must return the slice for the UTF-16 range"
    );
    assert_eq!(
        actual_range,
        Some(1..3),
        "text_for_range must write back the UTF-16 range"
    );
}

#[gpui::test]
fn character_index_for_point_converts_byte_offset_to_utf16(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("a\u{1F600}b"));
    vcx.run_until_parked();

    let point = field.read_with(vcx, |field, _cx| {
        // Get the point for byte offset 5 (after the emoji), which is UTF-16 index 3.
        field.point_for_offset_for_test(5)
    });

    let utf16_index = vcx.update(|window, cx| {
        field.update(cx, |field, cx| {
            field.character_index_for_point(point, window, cx)
        })
    });

    assert_eq!(
        utf16_index,
        Some(3),
        "character_index_for_point must convert byte offset to UTF-16 index"
    );
}

#[gpui::test]
fn bounds_for_range_returns_correct_geometry(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("a\u{1F600}b"));
    vcx.run_until_parked();

    let bounds = vcx.update(|window, cx| {
        field.update(cx, |field, cx| {
            let element_bounds = field.last_bounds.expect("paint must have run");
            // Request bounds for the emoji (UTF-16 range 1..3, byte range 1..5).
            field.bounds_for_range(1..3, element_bounds, window, cx)
        })
    });

    field.read_with(vcx, |field, _cx| {
        let line = field
            .last_line
            .as_ref()
            .expect("paint must have run");
        let element_bounds = field
            .last_bounds
            .expect("paint must have run");

        let expected_bounds = Bounds::from_corners(
            point(
                element_bounds.left() + line.x_for_index(1),
                element_bounds.top(),
            ),
            point(
                element_bounds.left() + line.x_for_index(5),
                element_bounds.top() + theme::FIELD_LINE_HEIGHT,
            ),
        );

        assert_eq!(
            bounds, Some(expected_bounds),
            "bounds_for_range must return the correct Bounds using line.x_for_index and FIELD_LINE_HEIGHT"
        );
    });
}

#[gpui::test]
fn unmark_text_clears_the_ime_composition(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, None);
    vcx.update(|window, cx| {
        field.update(cx, |field, cx| {
            field.replace_and_mark_text_in_range(None, "ab", Some(0..2), window, cx);
            assert!(field.marked_range.is_some());
            field.unmark_text(window, cx);
        });
    });
    field.read_with(vcx, |field, _cx| {
        assert!(field.marked_range.is_none());
    });
}

#[gpui::test]
fn utf16_offsets_round_trip_through_a_surrogate_pair(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, None);
    field.update(vcx, |field, cx| {
        // U+1F600 sits outside the BMP, so it is one `char` but two
        // UTF-16 code units -- exactly the case a naive byte- or
        // char-count implementation of the UTF-16 boundary math would
        // get wrong.
        field.set_value("a\u{1F600}b", cx);
        field.model.set_selection(1, 5);
    });

    let selection = vcx.update(|window, cx| {
        field.update(cx, |field, cx| {
            field
                .selected_text_range(false, window, cx)
                .expect("a selection should report a UTF-16 range")
        })
    });
    assert_eq!(
        selection.range,
        1..3,
        "the emoji occupies UTF-16 code units 1..3"
    );
}

// -- mouse -------------------------------------------------------------

#[gpui::test]
fn click_places_the_cursor_at_the_clicked_offset(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("hello world"));
    vcx.run_until_parked();

    let click_point = field.read_with(vcx, |field, _cx| field.point_for_offset_for_test(5));
    vcx.simulate_click(click_point, Modifiers::default());
    vcx.run_until_parked();

    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.model.cursor(), 5);
        assert!(!field.model.has_selection());
    });
}

#[gpui::test]
fn shift_click_extends_a_selection_from_the_prior_cursor_position(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("hello world"));
    vcx.run_until_parked();

    let first_click = field.read_with(vcx, |field, _cx| field.point_for_offset_for_test(2));
    vcx.simulate_click(first_click, Modifiers::default());
    vcx.run_until_parked();
    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.model.cursor(), 2);
        assert!(!field.model.has_selection());
    });

    let shift_click = field.read_with(vcx, |field, _cx| field.point_for_offset_for_test(7));
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    vcx.simulate_click(shift_click, shift);
    vcx.run_until_parked();

    field.read_with(vcx, |field, _cx| {
        assert_eq!(
            field.model.selection(),
            Some(2..7),
            "shift-click should extend the selection from the fixed anchor at the prior cursor"
        );
    });
}

#[gpui::test]
fn drag_extends_a_selection(cx: &mut TestAppContext) {
    let (field, vcx) = build_field(cx, Some("hello world"));
    vcx.run_until_parked();

    let start = field.read_with(vcx, |field, _cx| field.point_for_offset_for_test(0));
    let end = field.read_with(vcx, |field, _cx| field.point_for_offset_for_test(5));

    vcx.simulate_event(gpui::MouseDownEvent {
        button: gpui::MouseButton::Left,
        position: start,
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    vcx.simulate_event(gpui::MouseMoveEvent {
        position: end,
        pressed_button: Some(gpui::MouseButton::Left),
        modifiers: Modifiers::default(),
    });
    vcx.simulate_event(gpui::MouseUpEvent {
        button: gpui::MouseButton::Left,
        position: end,
        modifiers: Modifiers::default(),
        click_count: 1,
    });
    vcx.run_until_parked();

    field.read_with(vcx, |field, _cx| {
        assert_eq!(field.model.selection(), Some(0..5));
    });
}
