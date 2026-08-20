use std::sync::{Arc, Mutex};

use gpui::{
    Entity, EntityInputHandler, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, TestAppContext, VisualTestContext,
};

use gpui::{Hsla, UnderlineStyle, rgb};

use super::element::build_runs;
use super::{
    Backspace, Copy, Cut, DeleteForward, EditorView, MoveDocumentEnd, MoveDocumentStart, MoveDown,
    MoveLeft, MoveLineEnd, MoveLineStart, MoveRight, MoveUp, Newline, Paste, Position, QueryRunner,
    Redo, RunQuery, SelectAll, SelectDocumentEnd, SelectDocumentStart, SelectDown, SelectLeft,
    SelectLineEnd, SelectLineStart, SelectRight, SelectUp, Undo,
};
use crate::HighlightKind;
use crate::theme::syntax_color;
use zsql_ui::theme::ActiveTheme;

/// A `QueryRunner` double that records every SQL string it was asked to
/// run instead of running anything, in place of a real session/database.
fn recording_query_runner() -> (QueryRunner, Arc<Mutex<Vec<String>>>) {
    let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = queries.clone();
    let runner: QueryRunner = Box::new(move |sql, _cx| {
        recorded.lock().expect("queries lock poisoned").push(sql);
    });
    (runner, queries)
}

/// The entity under test plus the SQL text its `QueryRunner` recorded.
struct Harness {
    editor: Entity<EditorView>,
    queries: Arc<Mutex<Vec<String>>>,
}

/// Build an [`EditorView`] as a window's root view, focused, wired to a
/// recording `QueryRunner` so `RunQuery` can be asserted against without
/// a real session or database.
fn build_harness(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
    let (runner, queries) = recording_query_runner();

    let (editor, vcx) = cx.add_window_view(|window, cx| {
        let view = EditorView::new(runner, cx);
        window.focus(&view.focus_handle);
        view
    });

    (Harness { editor, queries }, vcx)
}

// -- viewport / paint coverage -----------------------------------------

#[gpui::test]
// Line counts in this test are always tiny, so the `usize -> f32`
// conversion below cannot lose meaningful precision.
#[allow(clippy::cast_precision_loss)]
fn moving_the_cursor_below_the_fold_scrolls_it_into_view(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, _cx| {
        let many_lines = (0..60)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        view.set_text_for_test(&many_lines);
    });
    vcx.dispatch_action(MoveDocumentEnd);
    vcx.run_until_parked();

    harness.editor.update(vcx, |view, _cx| {
        let viewport = view.scroll_handle.bounds().size.height;
        assert!(
            viewport > gpui::px(0.0),
            "the pane has a measured height after paint"
        );
        let scroll = -view.scroll_handle.offset().y;
        let line_height = gpui::px(crate::theme::EDITOR_LINE_HEIGHT);
        let cursor_top = gpui::px(crate::theme::EDITOR_PADDING_Y)
            + line_height * view.buffer_for_test().cursor().line as f32;
        assert!(
            cursor_top >= scroll && cursor_top + line_height <= scroll + viewport + gpui::px(1.0),
            "the cursor line must be within the viewport after autoscroll"
        );
    });
}

#[gpui::test]
fn painting_with_an_active_ime_composition_marks_the_span(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    vcx.update(|window, cx| {
        harness.editor.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(None, "ab", Some(0..2), window, cx);
        });
    });
    vcx.run_until_parked();
    harness.editor.update(vcx, |view, _cx| {
        assert!(
            view.marked_range.is_some(),
            "the composition stays marked across a paint"
        );
    });
}

#[gpui::test]
fn unmark_text_clears_the_ime_composition(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    vcx.update(|window, cx| {
        harness.editor.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(None, "ab", Some(0..2), window, cx);
            assert!(view.marked_range.is_some());
            view.unmark_text(window, cx);
        });
    });
    harness.editor.update(vcx, |view, _cx| {
        assert!(
            view.marked_range.is_none(),
            "unmark_text clears the composition"
        );
    });
}

#[gpui::test]
fn bounds_for_range_has_geometry_for_one_line_and_none_across_lines(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness
        .editor
        .update(vcx, |view, _cx| view.set_text_for_test("select\nfrom"));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        harness.editor.update(cx, |view, cx| {
            let element_bounds = view.last_bounds.expect("a paint has run");
            assert!(
                view.bounds_for_range(0..3, element_bounds, window, cx)
                    .is_some(),
                "a single-line range has geometry"
            );
            assert!(
                view.bounds_for_range(0..8, element_bounds, window, cx)
                    .is_none(),
                "a range spanning two lines declines geometry"
            );
        });
    });
}

#[gpui::test]
fn painting_a_selection_spanning_interior_lines_does_not_panic(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness
        .editor
        .update(vcx, |view, _cx| view.set_text_for_test("aaa\nbbb\nccc"));
    vcx.dispatch_action(SelectAll);
    vcx.run_until_parked();
    harness.editor.update(vcx, |view, _cx| {
        assert!(
            view.buffer_for_test().selection().is_some(),
            "select-all leaves a selection spanning all three lines"
        );
    });
}

// -- typed / IME input --------------------------------------------------

#[gpui::test]
fn typing_inserts_characters_into_the_buffer_via_the_input_handler(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    vcx.simulate_input("select 1");
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), "select 1");
    });
}

#[gpui::test]
fn ime_composition_marks_replaces_and_commits_text(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, _cx| {
        view.set_text_for_test("select ");
        view.buffer.move_document_end();
    });

    // The IME starts composing "n" and proposes the cursor sit right
    // after it, as if more candidate keystrokes are still coming.
    vcx.update(|window, cx| {
        harness.editor.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(None, "n", Some(1..1), window, cx);
        });
    });
    let marked_range_utf16 = vcx.update(|window, cx| {
        harness
            .editor
            .update(cx, |view, cx| view.marked_text_range(window, cx))
    });
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), "select n");
        assert_eq!(
            view.buffer_for_test().cursor(),
            Position::new(0, 8),
            "the proposed selection should follow the composed text"
        );
    });
    assert_eq!(marked_range_utf16, Some(7..8));

    // The composition continues, replacing the marked text with the
    // next candidate.
    vcx.update(|window, cx| {
        harness.editor.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(None, "now", None, window, cx);
        });
    });
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), "select now");
        assert_eq!(view.marked_range, Some(7..10));
    });

    // Commit: the OS replaces the marked range with no explicit range
    // argument, and the composition ends.
    vcx.update(|window, cx| {
        harness.editor.update(cx, |view, cx| {
            view.replace_text_in_range(None, "now", window, cx);
        });
    });
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), "select now");
        assert!(
            view.marked_range.is_none(),
            "committing text must clear the composition range"
        );
    });
}

#[gpui::test]
fn utf16_offsets_round_trip_through_a_surrogate_pair(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, cx| {
        // U+1F600 sits outside the BMP, so it is one `char` but two
        // UTF-16 code units -- exactly the case a naive char-count
        // implementation of the UTF-16 boundary math would get wrong.
        view.set_text_for_test("a\u{1F600}b");
        view.buffer
            .set_selection(Position::new(0, 1), Position::new(0, 2));
        cx.notify();
    });
    vcx.run_until_parked();

    let (selection, emoji_text, actual_range_utf16, hit_char_index) = vcx.update(|window, cx| {
        harness.editor.update(cx, |view, cx| {
            let selection = view
                .selected_text_range(false, window, cx)
                .expect("a selection should report a UTF-16 range");
            let mut actual_range = None;
            let emoji_text = view
                .text_for_range(1..3, &mut actual_range, window, cx)
                .expect("range 1..3 should resolve to the emoji");
            let click_point = view.point_for_position_for_test(Position::new(0, 1));
            let hit_char_index = view.character_index_for_point(click_point, window, cx);
            (selection, emoji_text, actual_range, hit_char_index)
        })
    });

    assert_eq!(
        selection.range,
        1..3,
        "the emoji occupies UTF-16 code units 1..3"
    );
    assert!(!selection.reversed);
    assert_eq!(emoji_text, "\u{1F600}");
    assert_eq!(actual_range_utf16, Some(1..3));
    assert_eq!(
        hit_char_index,
        Some(1),
        "a point at the emoji's leading edge should hit UTF-16 offset 1"
    );
}

#[gpui::test]
fn ime_selected_range_is_resolved_against_the_inserted_text_not_the_document(
    cx: &mut TestAppContext,
) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, _cx| {
        // A leading astral character (two UTF-16 code units, one char)
        // is exactly the case where resolving `new_selected_range_utf16`
        // against the whole post-insert document -- instead of against
        // `new_text` alone, as NSTextInputClient's `setMarkedText:
        // selectedRange:` specifies -- misaligns the UTF-16 count and
        // produces the wrong selection.
        view.set_text_for_test("\u{1F600}");
        view.buffer.move_document_end();
    });

    // The IME composes "ab" and asks for UTF-16 units 1..2 of that
    // composed text selected, i.e. just the "b".
    vcx.update(|window, cx| {
        harness.editor.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(None, "ab", Some(1..2), window, cx);
        });
    });

    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), "\u{1F600}ab");
        assert_eq!(
            view.buffer_for_test().selected_text(),
            "b",
            "the selected range is relative to the inserted text, not the \
             whole document's UTF-16 offsets"
        );
    });
}

// -- movement / editing actions ------------------------------------

#[gpui::test]
fn move_right_and_backspace_actions_delegate_to_the_buffer(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness
        .editor
        .update(vcx, |view, _cx| view.set_text_for_test("abc"));

    vcx.dispatch_action(MoveRight);
    vcx.dispatch_action(MoveRight);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().cursor(), Position::new(0, 2));
    });

    vcx.dispatch_action(Backspace);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), "ac");
        assert_eq!(view.buffer_for_test().cursor(), Position::new(0, 1));
    });
}

#[gpui::test]
fn move_left_and_delete_forward_actions_delegate_to_the_buffer(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness
        .editor
        .update(vcx, |view, _cx| view.set_text_for_test("abc"));
    vcx.dispatch_action(MoveRight);
    vcx.dispatch_action(MoveRight);
    vcx.dispatch_action(MoveRight);

    vcx.dispatch_action(MoveLeft);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().cursor(), Position::new(0, 2));
    });

    vcx.dispatch_action(DeleteForward);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), "ab");
        assert_eq!(
            view.buffer_for_test().cursor(),
            Position::new(0, 2),
            "delete-forward removes the next character without moving the cursor"
        );
    });
}

#[gpui::test]
fn move_up_down_line_start_and_line_end_actions_delegate_to_the_buffer(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness
        .editor
        .update(vcx, |view, _cx| view.set_text_for_test("abc\nde"));

    vcx.dispatch_action(MoveLineEnd);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().cursor(), Position::new(0, 3));
    });

    vcx.dispatch_action(MoveDown);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(
            view.buffer_for_test().cursor(),
            Position::new(1, 2),
            "the desired column (3) clamps to the shorter line's length"
        );
    });

    vcx.dispatch_action(MoveLineStart);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().cursor(), Position::new(1, 0));
    });

    vcx.dispatch_action(MoveUp);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().cursor(), Position::new(0, 0));
    });
}

#[gpui::test]
fn move_document_start_and_end_actions_delegate_to_the_buffer(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, _cx| {
        view.set_text_for_test("select 1;\nfrom orders\nwhere true;");
        view.buffer.set_cursor(Position::new(1, 2));
    });

    vcx.dispatch_action(MoveDocumentEnd);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(
            view.buffer_for_test().cursor(),
            Position::new(2, "where true;".chars().count())
        );
    });

    vcx.dispatch_action(MoveDocumentStart);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().cursor(), Position::new(0, 0));
    });
}

#[gpui::test]
fn select_up_down_and_line_boundary_actions_extend_the_selection(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, _cx| {
        view.set_text_for_test("select 1;\nfrom orders");
        view.buffer.set_cursor(Position::new(1, 4));
    });

    vcx.dispatch_action(SelectLineEnd);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().selected_text(), " orders");
    });

    vcx.dispatch_action(SelectLineStart);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().selected_text(), "from");
    });

    vcx.dispatch_action(SelectUp);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().selected_text(), "select 1;\nfrom");
    });

    vcx.dispatch_action(SelectDown);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().selected_text(), "from");
    });
}

#[gpui::test]
fn select_document_start_and_end_actions_extend_the_selection(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, _cx| {
        view.set_text_for_test("select 1;\nfrom orders\nwhere true;");
    });

    vcx.dispatch_action(SelectDocumentEnd);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(
            view.buffer_for_test().selected_text(),
            "select 1;\nfrom orders\nwhere true;"
        );
    });

    vcx.dispatch_action(SelectDocumentStart);
    harness.editor.update(vcx, |view, _cx| {
        assert!(
            !view.buffer_for_test().has_selection(),
            "extending back to the anchor's own position collapses the selection"
        );
    });
}

#[gpui::test]
fn shift_right_actions_extend_a_selection_from_the_cursor(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness
        .editor
        .update(vcx, |view, _cx| view.set_text_for_test("hello"));

    vcx.dispatch_action(SelectRight);
    vcx.dispatch_action(SelectRight);
    vcx.dispatch_action(SelectRight);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().selected_text(), "hel");
    });
}

#[gpui::test]
fn shift_left_actions_extend_a_selection_from_the_cursor(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness
        .editor
        .update(vcx, |view, _cx| view.set_text_for_test("hello"));
    for _ in 0..5 {
        vcx.dispatch_action(MoveRight);
    }

    vcx.dispatch_action(SelectLeft);
    vcx.dispatch_action(SelectLeft);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().selected_text(), "lo");
    });
}

#[gpui::test]
fn select_all_action_selects_the_whole_document(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, _cx| {
        view.set_text_for_test("select 1;\nselect 2;");
    });

    vcx.dispatch_action(SelectAll);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(
            view.buffer_for_test().selected_text(),
            "select 1;\nselect 2;"
        );
    });
}

#[gpui::test]
fn newline_action_splits_the_current_line_at_the_cursor(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness
        .editor
        .update(vcx, |view, _cx| view.set_text_for_test("helloworld"));
    for _ in 0..5 {
        vcx.dispatch_action(MoveRight);
    }

    vcx.dispatch_action(Newline);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(
            view.buffer_for_test().lines(),
            &["hello".to_owned(), "world".to_owned()]
        );
    });
}

// -- clipboard -------------------------------------------------------

#[gpui::test]
fn copy_cut_and_paste_round_trip_through_the_gpui_clipboard(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness
        .editor
        .update(vcx, |view, _cx| view.set_text_for_test("hello world"));
    for _ in 0..5 {
        vcx.dispatch_action(SelectRight);
    }

    vcx.dispatch_action(Copy);
    let copied = vcx.read_from_clipboard().and_then(|item| item.text());
    assert_eq!(copied.as_deref(), Some("hello"));
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(
            view.buffer_for_test().text(),
            "hello world",
            "copy must not modify the buffer"
        );
    });

    vcx.dispatch_action(Cut);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), " world");
    });

    vcx.dispatch_action(Paste);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), "hello world");
    });
}

// -- run query ---------------------------------------------------------

#[gpui::test]
fn run_query_with_no_selection_runs_the_whole_buffer(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, _cx| {
        view.set_text_for_test("select * from orders");
    });

    vcx.dispatch_action(RunQuery);

    assert_eq!(
        harness
            .queries
            .lock()
            .expect("queries lock poisoned")
            .as_slice(),
        ["select * from orders"]
    );
}

#[gpui::test]
fn run_query_with_a_selection_runs_only_the_selected_text(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, _cx| {
        view.set_text_for_test("select 1;\nselect 2;");
        view.buffer.move_line_start();
        view.buffer.move_down();
        for _ in 0.."select 2;".chars().count() {
            view.buffer.extend_right();
        }
    });

    vcx.dispatch_action(RunQuery);

    assert_eq!(
        harness
            .queries
            .lock()
            .expect("queries lock poisoned")
            .as_slice(),
        ["select 2;"]
    );
}

#[gpui::test]
fn run_query_on_an_empty_buffer_is_a_noop(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    vcx.dispatch_action(RunQuery);
    assert!(
        harness
            .queries
            .lock()
            .expect("queries lock poisoned")
            .is_empty()
    );
}

#[gpui::test]
fn run_query_on_a_whitespace_only_buffer_is_a_noop(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness
        .editor
        .update(vcx, |view, _cx| view.set_text_for_test("   \n  \t"));
    vcx.dispatch_action(RunQuery);
    assert!(
        harness
            .queries
            .lock()
            .expect("queries lock poisoned")
            .is_empty()
    );
}

#[gpui::test]
fn cmd_enter_and_ctrl_enter_keystrokes_both_dispatch_run_query(cx: &mut TestAppContext) {
    cx.update(|cx| super::init(cx, &super::EditorBindings::default()));
    let (harness, vcx) = build_harness(cx);
    harness
        .editor
        .update(vcx, |view, _cx| view.set_text_for_test("select 1"));

    vcx.simulate_keystrokes("cmd-enter");
    assert_eq!(
        harness
            .queries
            .lock()
            .expect("queries lock poisoned")
            .as_slice(),
        ["select 1"],
        "cmd-enter should dispatch RunQuery"
    );

    vcx.simulate_keystrokes("ctrl-enter");
    assert_eq!(
        harness
            .queries
            .lock()
            .expect("queries lock poisoned")
            .as_slice(),
        ["select 1", "select 1"],
        "ctrl-enter should also dispatch RunQuery"
    );
}

#[gpui::test]
fn run_current_query_runs_the_buffer_without_dispatching_the_run_query_action(
    cx: &mut TestAppContext,
) {
    // An embedding app's own Run affordance (e.g. a workspace header's
    // button) calls this method directly rather than dispatching the
    // `RunQuery` action, since it must work regardless of which element
    // holds keyboard focus. Pin that this public method alone -- with no
    // action dispatch and no focused window -- reaches the same
    // `QueryRunner` seam.
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, _cx| {
        view.set_text_for_test("select * from orders");
    });

    harness.editor.update(vcx, EditorView::run_current_query);

    assert_eq!(
        harness
            .queries
            .lock()
            .expect("queries lock poisoned")
            .as_slice(),
        ["select * from orders"]
    );
}

// -- rendering -----------------------------------------------------

#[gpui::test]
fn renders_a_multiline_buffer_with_a_selection_without_panicking(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, cx| {
        view.set_text_for_test("select *\nfrom orders\nwhere status = 'paid'");
        view.buffer.move_document_end();
        for _ in 0..6 {
            view.buffer.extend_left();
        }
        cx.notify();
    });
    vcx.run_until_parked();
}

/// The default `EditorView` paints with the real SQL highlighter, not
/// `PlainHighlighter`; this drives a full paint over SQL text exercising
/// keywords, a string, a number, and both comment forms, asserting only
/// that painting a frame does not panic (gpui cannot render headlessly
/// here, so pixel colors are the human's visual pass via `cargo run`).
#[gpui::test]
fn rendering_a_multiline_sql_buffer_with_the_sql_highlighter_does_not_panic(
    cx: &mut TestAppContext,
) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, cx| {
        view.set_text_for_test(
            "-- top comment\nSELECT id, 'paid' AS status /* inline */\nFROM orders WHERE total > 42.5",
        );
        cx.notify();
    });
    vcx.run_until_parked();
}

// -- highlighting --------------------------------------------------

#[gpui::test]
fn build_runs_keeps_the_highlight_color_and_gains_the_underline_where_they_overlap(
    cx: &mut TestAppContext,
) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, _cx| {
        view.set_text_for_test("SELECT 1");
        // Marks columns 2..6 ("LECT"), overlapping part of the "SELECT"
        // keyword span (0..6) but not all of it, and not touching the
        // "1" literal's span (7..8) at all.
        view.marked_range = Some(2..6);
    });

    let (font, base_color) = vcx.update(|window, _cx| {
        let style = window.text_style();
        (style.font(), style.color)
    });

    harness.editor.update(vcx, |view, cx| {
        let active_theme = cx.theme();
        let runs = build_runs(view, 0, "SELECT 1", &font, base_color, active_theme);

        let keyword_color = Hsla::from(rgb(syntax_color(active_theme, HighlightKind::Keyword)));
        let number_color = Hsla::from(rgb(syntax_color(active_theme, HighlightKind::Number)));
        let underline = UnderlineStyle {
            color: Some(base_color),
            thickness: gpui::px(1.0),
            wavy: false,
        };

        assert_eq!(
            runs.len(),
            4,
            "expected: unmarked keyword head, marked+underlined keyword \
             tail, unstyled space, number literal"
        );
        assert_eq!(runs[0].color, keyword_color);
        assert_eq!(runs[0].underline, None);

        assert_eq!(
            runs[1].color, keyword_color,
            "the overlapping run keeps the keyword's highlight color"
        );
        assert_eq!(
            runs[1].underline,
            Some(underline),
            "the overlapping run also gains the IME underline"
        );

        assert_eq!(runs[2].color, base_color);
        assert_eq!(runs[2].underline, None);

        assert_eq!(runs[3].color, number_color);
        assert_eq!(runs[3].underline, None);
    });
}

#[gpui::test]
fn typing_a_keyword_highlights_it_on_the_next_render(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);

    let (font, base_color) = vcx.update(|window, _cx| {
        let style = window.text_style();
        (style.font(), style.color)
    });

    harness.editor.update(vcx, |view, cx| {
        let active_theme = cx.theme();
        let runs = build_runs(view, 0, "", &font, base_color, active_theme);
        assert_eq!(
            runs.first().map(|run| run.color),
            Some(base_color),
            "an empty buffer has nothing highlighted yet"
        );
    });

    // `insert_text_for_test` goes through the same manual-edit path real
    // typing does (it fires the `EditListener`), unlike
    // `set_text_for_test`.
    harness.editor.update(vcx, |view, cx| {
        view.insert_text_for_test("SELECT", cx);
    });

    harness.editor.update(vcx, |view, cx| {
        let active_theme = cx.theme();
        let runs = build_runs(view, 0, "SELECT", &font, base_color, active_theme);
        let keyword_color = Hsla::from(rgb(syntax_color(active_theme, HighlightKind::Keyword)));
        assert_eq!(
            runs.first().map(|run| run.color),
            Some(keyword_color),
            "the keyword just typed is highlighted without any extra step"
        );
    });
}

// -- mouse -----------------------------------------------------------

#[gpui::test]
fn mouse_down_places_the_cursor_and_dragging_extends_a_selection(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, cx| {
        view.set_text_for_test("select 1\nfrom orders");
        cx.notify();
    });
    vcx.run_until_parked();

    // Click right after "from " on the second line.
    let click_target = Position::new(1, 5);
    let click_point = harness.editor.read_with(vcx, |view, _cx| {
        view.point_for_position_for_test(click_target)
    });
    vcx.update(|window, cx| {
        harness.editor.update(cx, |view, cx| {
            view.on_mouse_down(
                &MouseDownEvent {
                    button: MouseButton::Left,
                    position: click_point,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                },
                window,
                cx,
            );
        });
    });
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().cursor(), click_target);
        assert!(!view.buffer_for_test().has_selection());
        assert!(view.is_selecting);
    });

    // Drag to the end of "orders" to extend a selection.
    let drag_target = Position::new(1, "from orders".chars().count());
    let drag_point = harness.editor.read_with(vcx, |view, _cx| {
        view.point_for_position_for_test(drag_target)
    });
    vcx.update(|window, cx| {
        harness.editor.update(cx, |view, cx| {
            view.on_mouse_move(
                &MouseMoveEvent {
                    position: drag_point,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                },
                window,
                cx,
            );
        });
    });
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().selected_text(), "orders");
    });

    vcx.update(|window, cx| {
        harness.editor.update(cx, |view, cx| {
            view.on_mouse_up(
                &MouseUpEvent {
                    button: MouseButton::Left,
                    position: drag_point,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                },
                window,
                cx,
            );
        });
    });
    harness.editor.update(vcx, |view, _cx| {
        assert!(!view.is_selecting, "mouse-up should end the drag");
    });
}

#[gpui::test]
fn double_clicking_a_word_selects_exactly_that_word(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, cx| {
        view.set_text_for_test("select 1\nfrom orders\nwhere id = 1");
        cx.notify();
    });
    vcx.run_until_parked();

    // Double-click in the middle of "orders", not at either end.
    let click_target = Position::new(1, 8);
    let click_point = harness.editor.read_with(vcx, |view, _cx| {
        view.point_for_position_for_test(click_target)
    });
    vcx.update(|window, cx| {
        harness.editor.update(cx, |view, cx| {
            view.on_mouse_down(
                &MouseDownEvent {
                    button: MouseButton::Left,
                    position: click_point,
                    modifiers: Modifiers::default(),
                    click_count: 2,
                    first_mouse: false,
                },
                window,
                cx,
            );
        });
    });
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().selected_text(), "orders");
    });
}

#[gpui::test]
fn triple_clicking_selects_the_whole_line_regardless_of_click_column(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, cx| {
        view.set_text_for_test("select 1\nfrom orders\nwhere id = 1");
        cx.notify();
    });
    vcx.run_until_parked();

    // Triple-click in the middle of "orders", not at either end.
    let click_target = Position::new(1, 8);
    let click_point = harness.editor.read_with(vcx, |view, _cx| {
        view.point_for_position_for_test(click_target)
    });
    vcx.update(|window, cx| {
        harness.editor.update(cx, |view, cx| {
            view.on_mouse_down(
                &MouseDownEvent {
                    button: MouseButton::Left,
                    position: click_point,
                    modifiers: Modifiers::default(),
                    click_count: 3,
                    first_mouse: false,
                },
                window,
                cx,
            );
        });
    });
    harness.editor.update(vcx, |view, _cx| {
        let line_len = "from orders".chars().count();
        assert_eq!(view.buffer_for_test().selected_text(), "from orders");
        assert_eq!(
            view.buffer_for_test().selection().unwrap().ordered(),
            (Position::new(1, 0), Position::new(1, line_len))
        );
    });
}

// -- on_edit / set_text / compact ---------------------------------------

/// An `on_edit` listener double that counts how many times it fired.
fn counting_edit_listener() -> (crate::EditListener, Arc<Mutex<usize>>) {
    let count = Arc::new(Mutex::new(0));
    let counted = count.clone();
    let listener: crate::EditListener = Box::new(move |_cx| {
        *counted.lock().expect("edit count lock poisoned") += 1;
    });
    (listener, count)
}

#[gpui::test]
fn typing_fires_the_on_edit_listener(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    let (listener, count) = counting_edit_listener();
    harness
        .editor
        .update(vcx, |view, _cx| view.set_on_edit(listener));

    vcx.simulate_input("ab");

    assert_eq!(*count.lock().expect("edit count lock poisoned"), 2);
}

#[gpui::test]
fn backspace_delete_newline_cut_and_paste_all_fire_the_on_edit_listener(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    let (listener, count) = counting_edit_listener();
    harness.editor.update(vcx, |view, _cx| {
        view.set_text_for_test("ab");
        view.set_on_edit(listener);
        view.buffer.move_document_end();
    });

    vcx.dispatch_action(Backspace);
    vcx.dispatch_action(DeleteForward);
    vcx.dispatch_action(Newline);
    vcx.dispatch_action(SelectAll);
    vcx.dispatch_action(Cut);
    vcx.dispatch_action(Paste);

    assert_eq!(*count.lock().expect("edit count lock poisoned"), 5);
}

#[gpui::test]
fn cursor_movement_and_selection_alone_do_not_fire_the_on_edit_listener(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    let (listener, count) = counting_edit_listener();
    harness.editor.update(vcx, |view, _cx| {
        view.set_text_for_test("select 1");
        view.set_on_edit(listener);
    });

    vcx.dispatch_action(MoveRight);
    vcx.dispatch_action(MoveLineEnd);
    vcx.dispatch_action(MoveLineStart);
    vcx.dispatch_action(SelectRight);
    vcx.dispatch_action(SelectAll);

    assert_eq!(*count.lock().expect("edit count lock poisoned"), 0);
}

#[gpui::test]
fn set_text_replaces_the_buffer_without_firing_the_on_edit_listener(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    let (listener, count) = counting_edit_listener();
    harness.editor.update(vcx, |view, cx| {
        view.set_on_edit(listener);
        view.set_text("select * from orders", cx);
    });

    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.text(), "select * from orders");
    });
    assert_eq!(
        *count.lock().expect("edit count lock poisoned"),
        0,
        "a programmatic set_text is not a manual edit"
    );
}

#[gpui::test]
fn compact_mode_toggles_and_renders_without_a_gutter(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    harness.editor.update(vcx, |view, _cx| {
        assert!(
            !view.is_compact(),
            "editors start in full, non-compact mode"
        );
        view.set_text_for_test("select * from orders limit 200");
        view.set_compact(true);
        assert!(view.is_compact());
    });
    vcx.run_until_parked();
}

// -- undo/redo -------------------------------------------------------

#[gpui::test]
fn undo_action_reverts_the_last_edit_and_resyncs_the_highlighter(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    let (listener, count) = counting_edit_listener();
    harness
        .editor
        .update(vcx, |view, _cx| view.set_on_edit(listener));

    vcx.simulate_input("SELECT");
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), "SELECT");
        assert!(
            !view.highlighter.spans_for_line(0).is_empty(),
            "SELECT should be highlighted as a keyword before the undo"
        );
    });

    vcx.dispatch_action(Undo);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(
            view.buffer_for_test().text(),
            "",
            "undo should revert the coalesced typing run"
        );
        assert!(
            view.highlighter.spans_for_line(0).is_empty(),
            "the highlighter must resync to the emptied, restored text"
        );
    });
    assert!(
        *count.lock().expect("edit count lock poisoned") > 0,
        "undo goes through the same notify_edit path as other mutating actions"
    );
}

#[gpui::test]
fn redo_action_reapplies_the_undone_edit(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    vcx.simulate_input("SELECT");

    vcx.dispatch_action(Undo);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), "");
    });

    vcx.dispatch_action(Redo);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), "SELECT");
        assert_eq!(view.buffer_for_test().cursor(), Position::new(0, 6));
    });
}

#[gpui::test]
fn undo_on_a_fresh_editor_is_a_noop_and_does_not_fire_the_edit_listener(cx: &mut TestAppContext) {
    let (harness, vcx) = build_harness(cx);
    let (listener, count) = counting_edit_listener();
    harness
        .editor
        .update(vcx, |view, _cx| view.set_on_edit(listener));

    vcx.dispatch_action(Undo);
    vcx.dispatch_action(Redo);

    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), "");
    });
    assert_eq!(
        *count.lock().expect("edit count lock poisoned"),
        0,
        "a no-op undo/redo must not fire the edit listener or resync anything"
    );
}

#[gpui::test]
fn secondary_z_keystroke_dispatches_undo(cx: &mut TestAppContext) {
    cx.update(|cx| super::init(cx, &super::EditorBindings::default()));
    let (harness, vcx) = build_harness(cx);
    vcx.simulate_input("ab");
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), "ab");
    });

    vcx.simulate_keystrokes("secondary-z");
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(
            view.buffer_for_test().text(),
            "",
            "secondary-z should dispatch Undo"
        );
    });
}

#[gpui::test]
fn shift_secondary_z_secondary_y_and_ctrl_y_keystrokes_all_dispatch_redo(cx: &mut TestAppContext) {
    cx.update(|cx| super::init(cx, &super::EditorBindings::default()));
    let (harness, vcx) = build_harness(cx);
    vcx.simulate_input("ab");
    vcx.dispatch_action(Undo);
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(view.buffer_for_test().text(), "");
    });

    vcx.simulate_keystrokes("shift-secondary-z");
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(
            view.buffer_for_test().text(),
            "ab",
            "shift-secondary-z should dispatch Redo"
        );
    });

    vcx.dispatch_action(Undo);
    vcx.simulate_keystrokes("secondary-y");
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(
            view.buffer_for_test().text(),
            "ab",
            "secondary-y should also dispatch Redo"
        );
    });

    vcx.dispatch_action(Undo);
    vcx.simulate_keystrokes("ctrl-y");
    harness.editor.update(vcx, |view, _cx| {
        assert_eq!(
            view.buffer_for_test().text(),
            "ab",
            "ctrl-y should also dispatch Redo"
        );
    });
}
