use std::sync::{Arc, Mutex};

use gpui::{
    Bounds, Context, Entity, EntityInputHandler, IntoElement, Modifiers, Pixels, Point, Render,
    TestAppContext, VisualTestContext, Window, div, point, prelude::*, px,
};

use super::{
    Backspace, Copy, Cut, DeleteForward, MoveEnd, MoveHome, MoveLeft, MoveRight, Paste, SelectAll,
    SelectLeft, SelectRight, Submit, TextFieldEvent, TextFieldState,
};
use crate::text_field::scroll;
use crate::text_field::theme;
use crate::theme::ActiveTheme;

impl TextFieldState {
    /// The pixel point that hit-tests back to `offset`, computed from the
    /// most recent paint's shaped line and bounds, and corrected for the
    /// field's current scroll offset. Lets mouse-handling tests drive
    /// `on_mouse_down`/`on_mouse_move` with real, paint-derived coordinates
    /// instead of guessed pixel offsets, whether or not the field is
    /// scrolled.
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
        let x = line.x_for_index(offset) - self.scroll_offset;
        point(bounds.left() + x, bounds.top())
    }
}

// -- horizontal scroll test harness -------------------------------------

/// Width of the fixed-width wrapper the horizontal-scroll tests embed a
/// field in, narrow enough that a moderately long value overflows it.
const NARROW_FIELD_WIDTH: Pixels = px(150.0);

/// Wraps a `TextFieldState` in a fixed-width container, since the field
/// itself always fills its parent's width (`w_full`) -- the scroll tests
/// need a parent narrower than the test window to force real overflow.
struct NarrowFieldHarness {
    field: Entity<TextFieldState>,
}

impl Render for NarrowFieldHarness {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().w(NARROW_FIELD_WIDTH).child(self.field.clone())
    }
}

fn build_narrow_field<'a>(
    cx: &'a mut TestAppContext,
    initial_value: Option<&str>,
) -> (Entity<TextFieldState>, &'a mut VisualTestContext) {
    let (harness, vcx) = cx.add_window_view(|window, cx| {
        let field = cx.new(|cx| TextFieldState::new("Search", initial_value, cx));
        window.focus(&field.read(cx).focus_handle);
        NarrowFieldHarness { field }
    });
    let field = harness.read_with(vcx, |harness, _cx| harness.field.clone());
    (field, vcx)
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

#[gpui::test]
fn clicking_a_scrolled_masked_field_places_the_cursor_at_the_correct_content_offset(
    cx: &mut TestAppContext,
) {
    let (field, vcx) = build_narrow_field(cx, Some(OVERFLOWING_VALUE));
    field.update(vcx, |field, cx| field.set_masked(true, cx));
    vcx.run_until_parked();

    vcx.dispatch_action(MoveEnd);
    vcx.run_until_parked();
    field.read_with(vcx, |field, _cx| {
        assert!(
            field.scroll_offset > Pixels::ZERO,
            "the field must be scrolled for this test to be meaningful"
        );
    });

    // OVERFLOWING_VALUE is ASCII, so its char count equals its byte offset:
    // the mask glyph's display index and the content byte offset coincide,
    // letting the assertion below pin the content offset directly while
    // still exercising the scroll-offset correction against a masked line.
    let target_offset = OVERFLOWING_VALUE.len() - 3;
    let click_point = field.read_with(vcx, |field, _cx| {
        field.point_for_offset_for_test(target_offset)
    });
    vcx.simulate_click(click_point, Modifiers::default());
    vcx.run_until_parked();

    field.read_with(vcx, |field, _cx| {
        assert_eq!(
            field.model.cursor(),
            target_offset,
            "a click on a scrolled masked field must resolve to the content byte offset \
             under the pointer, not the character position it would be at offset 0"
        );
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
fn bounds_for_range_shifts_by_the_scroll_offset_on_a_scrolled_field(cx: &mut TestAppContext) {
    let (field, vcx) = build_narrow_field(cx, Some(OVERFLOWING_VALUE));
    vcx.run_until_parked();

    vcx.dispatch_action(MoveEnd);
    vcx.run_until_parked();
    let scroll_offset = field.read_with(vcx, |field, _cx| field.scroll_offset);
    assert!(
        scroll_offset > Pixels::ZERO,
        "the field must be scrolled for this test to be meaningful"
    );

    let target = OVERFLOWING_VALUE.len() - 4;
    let bounds = vcx.update(|window, cx| {
        field.update(cx, |field, cx| {
            let element_bounds = field.last_bounds.expect("paint must have run");
            field.bounds_for_range(target..target + 1, element_bounds, window, cx)
        })
    });

    field.read_with(vcx, |field, _cx| {
        let line = field.last_line.as_ref().expect("paint must have run");
        let element_bounds = field.last_bounds.expect("paint must have run");

        let unshifted_left = element_bounds.left() + line.x_for_index(target);
        let unshifted_right = element_bounds.left() + line.x_for_index(target + 1);
        let expected_bounds = Bounds::from_corners(
            point(unshifted_left - scroll_offset, element_bounds.top()),
            point(
                unshifted_right - scroll_offset,
                element_bounds.top() + theme::FIELD_LINE_HEIGHT,
            ),
        );

        assert_eq!(
            bounds,
            Some(expected_bounds),
            "bounds_for_range on a scrolled field must shift the unscrolled bounds left by \
             the current scroll_offset so the IME candidate window anchors to the caret's \
             actual on-screen position"
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

// -- horizontal scroll ---------------------------------------------------

/// A value long enough to overflow [`NARROW_FIELD_WIDTH`] regardless of
/// glyph metrics, so the scroll tests exercise a genuinely scrolled field
/// rather than one whose content still fits.
const OVERFLOWING_VALUE: &str = "the quick brown fox jumps over the lazy dog, repeatedly";

fn last_content_and_viewport_width(field: &TextFieldState) -> (Pixels, Pixels) {
    let bounds = field
        .last_bounds
        .expect("a paint must have run before measuring widths");
    let line = field
        .last_line
        .as_ref()
        .expect("a paint must have run before measuring widths");
    (line.width, bounds.size.width)
}

#[gpui::test]
fn typing_past_the_right_edge_keeps_the_caret_visible(cx: &mut TestAppContext) {
    let (field, vcx) = build_narrow_field(cx, None);
    vcx.run_until_parked();

    vcx.simulate_input(OVERFLOWING_VALUE);
    vcx.run_until_parked();

    let (caret_x, viewport_width, offset) = field.read_with(vcx, |field, _cx| {
        let (content_width, viewport_width) = last_content_and_viewport_width(field);
        let line = field.last_line.as_ref().expect("paint must have run");
        let caret_x = line.x_for_index(field.model.cursor()) - field.scroll_offset;
        assert!(
            content_width > viewport_width,
            "the typed value must actually overflow the narrow field for this test to be meaningful"
        );
        (caret_x, viewport_width, field.scroll_offset)
    });

    assert!(
        caret_x >= Pixels::ZERO && caret_x <= viewport_width,
        "the caret painted at {caret_x:?} must stay within the field's [0, {viewport_width:?}] viewport"
    );
    assert!(
        offset > Pixels::ZERO,
        "the field must actually have scrolled to keep the caret visible"
    );
}

#[gpui::test]
fn home_and_end_jump_the_offset_to_the_content_ends(cx: &mut TestAppContext) {
    let (field, vcx) = build_narrow_field(cx, Some(OVERFLOWING_VALUE));
    vcx.run_until_parked();

    vcx.dispatch_action(MoveEnd);
    vcx.run_until_parked();
    let (offset_at_end, max_offset) = field.read_with(vcx, |field, _cx| {
        let (content_width, viewport_width) = last_content_and_viewport_width(field);
        (
            field.scroll_offset,
            scroll::max_scroll_offset(content_width, viewport_width),
        )
    });
    assert!(
        max_offset > Pixels::ZERO,
        "the field's content must actually overflow for this test to be meaningful"
    );
    assert_eq!(
        offset_at_end, max_offset,
        "End should scroll the offset to its maximum clamp"
    );

    vcx.dispatch_action(MoveHome);
    vcx.run_until_parked();
    let offset_at_home = field.read_with(vcx, |field, _cx| field.scroll_offset);
    assert_eq!(
        offset_at_home,
        Pixels::ZERO,
        "Home should scroll the offset back to zero"
    );
}

#[gpui::test]
fn clicking_a_visible_position_on_a_scrolled_field_places_the_cursor_at_the_right_offset(
    cx: &mut TestAppContext,
) {
    let (field, vcx) = build_narrow_field(cx, Some(OVERFLOWING_VALUE));
    vcx.run_until_parked();

    vcx.dispatch_action(MoveEnd);
    vcx.run_until_parked();
    field.read_with(vcx, |field, _cx| {
        assert!(
            field.scroll_offset > Pixels::ZERO,
            "the field must be scrolled for this test to be meaningful"
        );
    });

    let target_offset = OVERFLOWING_VALUE.len() - 3;
    let click_point = field.read_with(vcx, |field, _cx| {
        field.point_for_offset_for_test(target_offset)
    });
    vcx.simulate_click(click_point, Modifiers::default());
    vcx.run_until_parked();

    field.read_with(vcx, |field, _cx| {
        assert_eq!(
            field.model.cursor(),
            target_offset,
            "a click on a scrolled field must resolve to the character under the pointer"
        );
    });
}

#[gpui::test]
fn shift_wheel_scrolls_a_long_value_and_is_a_no_op_on_a_value_that_fits(cx: &mut TestAppContext) {
    use gpui::{ScrollDelta, ScrollWheelEvent, TouchPhase};

    // -- a long value: shift-wheel scrolls it --
    // The field's initial value places the cursor (and so the caret-follow
    // offset) at the content's end already, so the wheel gesture below
    // scrolls back toward the start to exercise a real offset change.
    let (long_field, vcx) = build_narrow_field(cx, Some(OVERFLOWING_VALUE));
    vcx.run_until_parked();
    let offset_before = long_field.read_with(vcx, |field, _cx| field.scroll_offset);
    assert!(
        offset_before > Pixels::ZERO,
        "the field must start scrolled for this test to be meaningful"
    );

    let bounds = long_field.read_with(vcx, |field, _cx| {
        field.last_bounds.expect("paint must have run")
    });
    vcx.simulate_event(ScrollWheelEvent {
        position: bounds.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(40.0))),
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        touch_phase: TouchPhase::Moved,
    });
    vcx.run_until_parked();

    let offset_after = long_field.read_with(vcx, |field, _cx| field.scroll_offset);
    assert!(
        offset_after < offset_before,
        "a shift-held wheel gesture over an overflowing field must scroll it toward the start"
    );

    // -- a short value that fits: the same gesture is a no-op --
    let (short_field, vcx) = build_narrow_field(cx, Some("short"));
    vcx.run_until_parked();

    let short_bounds = short_field.read_with(vcx, |field, _cx| {
        field.last_bounds.expect("paint must have run")
    });
    vcx.simulate_event(ScrollWheelEvent {
        position: short_bounds.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-40.0))),
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        touch_phase: TouchPhase::Moved,
    });
    vcx.run_until_parked();

    short_field.read_with(vcx, |field, _cx| {
        assert_eq!(
            field.scroll_offset,
            Pixels::ZERO,
            "a field whose content already fits must not scroll on a shift-held wheel gesture"
        );
    });
}

#[gpui::test]
fn an_end_or_home_keystroke_re_follows_the_caret_after_a_wheel_scroll_away(
    cx: &mut TestAppContext,
) {
    use gpui::{ScrollDelta, ScrollWheelEvent, TouchPhase};

    // The cursor sits at the content's end, so the initial caret-follow
    // leaves the field scrolled to its maximum offset.
    let (field, vcx) = build_narrow_field(cx, Some(OVERFLOWING_VALUE));
    vcx.run_until_parked();
    let end_offset = field.read_with(vcx, |field, _cx| field.scroll_offset);
    assert!(end_offset > Pixels::ZERO);

    let bounds = field.read_with(vcx, |field, _cx| {
        field.last_bounds.expect("paint must have run")
    });
    vcx.simulate_event(ScrollWheelEvent {
        position: bounds.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(40.0))),
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        touch_phase: TouchPhase::Moved,
    });
    vcx.run_until_parked();
    assert!(
        field.read_with(vcx, |field, _cx| field.scroll_offset) < end_offset,
        "the wheel gesture must have scrolled the viewport away from the caret"
    );

    // End with the cursor already at the end moves no cursor, but must
    // still bring the caret back into view.
    vcx.dispatch_action(MoveEnd);
    vcx.run_until_parked();
    field.read_with(vcx, |field, _cx| {
        assert_eq!(
            field.scroll_offset, end_offset,
            "End must re-follow the caret to the content's end even when the cursor did not move"
        );
    });

    // The mirrored case: cursor at the start, wheel toward the end, then
    // Home with the cursor already at offset 0.
    vcx.dispatch_action(MoveHome);
    vcx.run_until_parked();
    assert_eq!(
        field.read_with(vcx, |field, _cx| field.scroll_offset),
        Pixels::ZERO
    );
    vcx.simulate_event(ScrollWheelEvent {
        position: bounds.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-40.0))),
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        touch_phase: TouchPhase::Moved,
    });
    vcx.run_until_parked();
    assert!(field.read_with(vcx, |field, _cx| field.scroll_offset) > Pixels::ZERO);
    vcx.dispatch_action(MoveHome);
    vcx.run_until_parked();
    field.read_with(vcx, |field, _cx| {
        assert_eq!(
            field.scroll_offset,
            Pixels::ZERO,
            "Home must re-follow the caret to the start even when the cursor did not move"
        );
    });
}

#[gpui::test]
fn a_wheel_past_the_clamp_boundary_leaves_the_offset_at_its_maximum(cx: &mut TestAppContext) {
    use gpui::{ScrollDelta, ScrollWheelEvent, TouchPhase};

    // Cursor at the end: the field starts saturated at its maximum offset.
    let (field, vcx) = build_narrow_field(cx, Some(OVERFLOWING_VALUE));
    vcx.run_until_parked();
    let max_offset = field.read_with(vcx, |field, _cx| field.scroll_offset);
    assert!(max_offset > Pixels::ZERO);

    let bounds = field.read_with(vcx, |field, _cx| {
        field.last_bounds.expect("paint must have run")
    });
    vcx.simulate_event(ScrollWheelEvent {
        position: bounds.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(-40.0))),
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        touch_phase: TouchPhase::Moved,
    });
    vcx.run_until_parked();
    field.read_with(vcx, |field, _cx| {
        assert_eq!(
            field.scroll_offset, max_offset,
            "a wheel gesture past the clamp boundary must leave the offset saturated, unchanged"
        );
    });
}

#[gpui::test]
fn native_horizontal_wheel_scrolls_a_long_value_and_is_a_no_op_on_a_value_that_fits(
    cx: &mut TestAppContext,
) {
    use gpui::{ScrollDelta, ScrollWheelEvent, TouchPhase};

    // -- a long value: a native horizontal delta scrolls it, no shift held --
    let (long_field, vcx) = build_narrow_field(cx, Some(OVERFLOWING_VALUE));
    vcx.run_until_parked();
    let offset_before = long_field.read_with(vcx, |field, _cx| field.scroll_offset);
    assert!(
        offset_before > Pixels::ZERO,
        "the field must start scrolled for this test to be meaningful"
    );

    let bounds = long_field.read_with(vcx, |field, _cx| {
        field.last_bounds.expect("paint must have run")
    });
    vcx.simulate_event(ScrollWheelEvent {
        position: bounds.center(),
        delta: ScrollDelta::Pixels(point(px(40.0), px(0.0))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
    });
    vcx.run_until_parked();

    let offset_after = long_field.read_with(vcx, |field, _cx| field.scroll_offset);
    assert!(
        offset_after < offset_before,
        "a native horizontal wheel delta over an overflowing field must scroll it toward the \
         start, matching the sign of a shift-held vertical delta"
    );

    // -- a short value that fits: the same gesture is a no-op --
    let (short_field, vcx) = build_narrow_field(cx, Some("short"));
    vcx.run_until_parked();

    let short_bounds = short_field.read_with(vcx, |field, _cx| {
        field.last_bounds.expect("paint must have run")
    });
    vcx.simulate_event(ScrollWheelEvent {
        position: short_bounds.center(),
        delta: ScrollDelta::Pixels(point(px(-40.0), px(0.0))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
    });
    vcx.run_until_parked();

    short_field.read_with(vcx, |field, _cx| {
        assert_eq!(
            field.scroll_offset,
            Pixels::ZERO,
            "a field whose content already fits must not scroll on a native horizontal wheel delta"
        );
    });
}

#[gpui::test]
fn a_plain_vertical_wheel_over_an_overflowing_field_does_not_scroll_it(cx: &mut TestAppContext) {
    use gpui::{ScrollDelta, ScrollWheelEvent, TouchPhase};

    // No shift held and no horizontal component: this is the bare wheel
    // notch a field embedded in a scrollable page would receive, and it
    // must leave the field's own scroll offset untouched so the page (not
    // the field) is the thing that scrolls.
    let (field, vcx) = build_narrow_field(cx, Some(OVERFLOWING_VALUE));
    vcx.run_until_parked();
    let offset_before = field.read_with(vcx, |field, _cx| field.scroll_offset);
    assert!(
        offset_before > Pixels::ZERO,
        "the field must start scrolled for this test to be meaningful"
    );

    let bounds = field.read_with(vcx, |field, _cx| {
        field.last_bounds.expect("paint must have run")
    });
    vcx.simulate_event(ScrollWheelEvent {
        position: bounds.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(40.0))),
        modifiers: Modifiers::default(),
        touch_phase: TouchPhase::Moved,
    });
    vcx.run_until_parked();

    field.read_with(vcx, |field, _cx| {
        assert_eq!(
            field.scroll_offset, offset_before,
            "a plain vertical wheel notch (no shift, no horizontal delta) over an \
             overflowing field must not change its scroll offset"
        );
    });
}

#[gpui::test]
fn deleting_most_of_a_scrolled_fields_text_re_clamps_the_offset(cx: &mut TestAppContext) {
    let (field, vcx) = build_narrow_field(cx, Some(OVERFLOWING_VALUE));
    vcx.run_until_parked();

    vcx.dispatch_action(MoveEnd);
    vcx.run_until_parked();
    field.read_with(vcx, |field, _cx| {
        assert!(
            field.scroll_offset > Pixels::ZERO,
            "the field must be scrolled for this test to be meaningful"
        );
    });

    field.update(vcx, |field, cx| {
        field.set_value("hi", cx);
    });
    vcx.run_until_parked();

    field.read_with(vcx, |field, _cx| {
        assert_eq!(
            field.scroll_offset,
            Pixels::ZERO,
            "the offset must re-clamp to zero once the remaining content fits, \
             not show stale trailing blank space"
        );
    });
}

#[gpui::test]
fn selection_painting_on_scrolled_content_stays_aligned_with_the_glyphs(cx: &mut TestAppContext) {
    let (field, vcx) = build_narrow_field(cx, Some(OVERFLOWING_VALUE));
    vcx.run_until_parked();

    vcx.dispatch_action(MoveEnd);
    vcx.run_until_parked();
    field.read_with(vcx, |field, _cx| {
        assert!(
            field.scroll_offset > Pixels::ZERO,
            "the field must be scrolled for this test to be meaningful"
        );
    });

    let select_from = OVERFLOWING_VALUE.len() - 10;
    let select_to = OVERFLOWING_VALUE.len();

    // The selection span's paint geometry (via the same shared
    // `text_input::selection_quad` helper the field's own paint pass
    // calls), built from the scroll-shifted x's the field would use --
    // both edges must land inside the visible viewport, not off to the
    // side where the click below could never have reached them.
    let (start_x, end_x, viewport_width) = field.read_with(vcx, |field, cx| {
        let bounds = field.last_bounds.expect("paint must have run");
        let line = field.last_line.as_ref().expect("paint must have run");
        let offset = field.scroll_offset;
        let span = crate::text_input::SelectionLineSpan {
            line_index: 0,
            start_x: line.x_for_index(select_from) - offset,
            end_x: line.x_for_index(select_to) - offset,
        };
        let quad = crate::text_input::selection_quad(
            &span,
            bounds,
            theme::FIELD_LINE_HEIGHT,
            cx.theme().colors.accent_wash_hover(),
        );
        (
            quad.bounds.origin.x - bounds.left(),
            quad.bounds.origin.x + quad.bounds.size.width - bounds.left(),
            bounds.size.width,
        )
    });
    assert!(
        start_x >= Pixels::ZERO && start_x <= viewport_width,
        "the selection's start edge at {start_x:?} must paint inside the [0, {viewport_width:?}] viewport"
    );
    assert!(
        end_x >= Pixels::ZERO && end_x <= viewport_width,
        "the selection's end edge at {end_x:?} must paint inside the [0, {viewport_width:?}] viewport"
    );

    // The same two content offsets, hit-tested back through the field's own
    // offset-aware point-for-offset helper and driven through a real
    // click-then-shift-click, must resolve to exactly this span -- proving
    // the paint geometry and the hit-test geometry agree on where the
    // glyphs sit once the field is scrolled. Each point is (re)computed
    // just before its click, against whatever the prior action's repaint
    // just settled the offset to.
    let start_point = field.read_with(vcx, |field, _cx| {
        field.point_for_offset_for_test(select_from)
    });
    vcx.simulate_click(start_point, Modifiers::default());
    vcx.run_until_parked();

    let end_point = field.read_with(vcx, |field, _cx| field.point_for_offset_for_test(select_to));
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    vcx.simulate_click(end_point, shift);
    vcx.run_until_parked();

    field.read_with(vcx, |field, _cx| {
        assert_eq!(
            field.model.selection(),
            Some(select_from..select_to),
            "a click then a shift-click at the selection's own painted edges must reselect it exactly"
        );
    });
}
