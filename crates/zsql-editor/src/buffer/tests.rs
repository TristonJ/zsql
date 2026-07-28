use super::{Position, TextBuffer};
use crate::theme::EDITOR_HISTORY_CAP;

// -- storage / accessors ------------------------------------------

#[test]
fn a_new_buffer_is_a_single_empty_line_with_the_cursor_at_the_origin() {
    let buffer = TextBuffer::new();
    assert_eq!(buffer.lines(), &[String::new()]);
    assert_eq!(buffer.cursor(), Position::new(0, 0));
    assert!(buffer.selection().is_none());
    assert_eq!(buffer.text(), "");
}

#[test]
fn from_text_splits_on_newlines_into_lines() {
    let buffer = TextBuffer::from_text("select 1;\nfrom dual;\n");
    assert_eq!(
        buffer.lines(),
        &[
            "select 1;".to_owned(),
            "from dual;".to_owned(),
            String::new()
        ]
    );
    assert_eq!(buffer.text(), "select 1;\nfrom dual;\n");
}

// -- cursor movement: left/right, line wrapping ---------------------

#[test]
fn move_right_advances_one_char_at_a_time() {
    let mut buffer = TextBuffer::from_text("abc");
    buffer.move_right();
    assert_eq!(buffer.cursor(), Position::new(0, 1));
    buffer.move_right();
    assert_eq!(buffer.cursor(), Position::new(0, 2));
}

#[test]
fn move_right_at_end_of_line_wraps_to_the_next_line_start() {
    let mut buffer = TextBuffer::from_text("ab\ncd");
    buffer.move_right();
    buffer.move_right();
    assert_eq!(
        buffer.cursor(),
        Position::new(0, 2),
        "should be at end of first line"
    );
    buffer.move_right();
    assert_eq!(
        buffer.cursor(),
        Position::new(1, 0),
        "should wrap to the next line's start"
    );
}

#[test]
fn move_right_at_document_end_does_not_move_past_it() {
    let mut buffer = TextBuffer::from_text("ab");
    buffer.move_right();
    buffer.move_right();
    buffer.move_right();
    buffer.move_right();
    assert_eq!(buffer.cursor(), Position::new(0, 2));
}

#[test]
fn move_left_at_line_start_wraps_to_the_previous_line_end() {
    let mut buffer = TextBuffer::from_text("ab\ncd");
    buffer.move_down();
    assert_eq!(buffer.cursor(), Position::new(1, 0));
    buffer.move_left();
    assert_eq!(
        buffer.cursor(),
        Position::new(0, 2),
        "should wrap to the previous line's end"
    );
}

#[test]
fn move_left_at_document_start_does_not_move_before_it() {
    let mut buffer = TextBuffer::from_text("ab\ncd");
    buffer.move_left();
    assert_eq!(buffer.cursor(), Position::new(0, 0));
}

// -- cursor movement: up/down, desired column ------------------------

#[test]
fn move_down_clamps_to_a_shorter_line_then_restores_the_desired_column() {
    let mut buffer = TextBuffer::from_text("abcdef\nxy\nghijkl");
    for _ in 0..4 {
        buffer.move_right();
    }
    buffer.move_down();
    assert_eq!(
        buffer.cursor(),
        Position::new(1, 2),
        "the short middle line should clamp the column"
    );
    buffer.move_down();
    assert_eq!(
        buffer.cursor(),
        Position::new(2, 4),
        "desired column of 4 should be restored once the line is long enough again"
    );
}

#[test]
fn move_up_at_first_line_does_not_move() {
    let mut buffer = TextBuffer::from_text("abc\ndef");
    buffer.move_right();
    buffer.move_up();
    assert_eq!(buffer.cursor(), Position::new(0, 1));
}

#[test]
fn move_up_moves_to_the_line_above_preserving_the_desired_column() {
    let mut buffer = TextBuffer::from_text("abcdef\nxy\nghijkl");
    buffer.move_down();
    buffer.move_down();
    for _ in 0..4 {
        buffer.move_right();
    }
    assert_eq!(buffer.cursor(), Position::new(2, 4));
    buffer.move_up();
    assert_eq!(
        buffer.cursor(),
        Position::new(1, 2),
        "the short middle line should clamp the column"
    );
    buffer.move_up();
    assert_eq!(
        buffer.cursor(),
        Position::new(0, 4),
        "desired column of 4 should be restored once the line is long enough again"
    );
}

#[test]
fn move_up_clears_an_active_selection_instead_of_extending_it() {
    let mut buffer = TextBuffer::from_text("abc\ndef");
    buffer.move_down();
    buffer.extend_right();
    assert!(buffer.has_selection());
    buffer.move_up();
    assert!(!buffer.has_selection());
    assert_eq!(buffer.cursor(), Position::new(0, 1));
}

#[test]
fn move_down_at_last_line_does_not_move() {
    let mut buffer = TextBuffer::from_text("abc\ndef");
    buffer.move_down();
    buffer.move_down();
    assert_eq!(buffer.cursor(), Position::new(1, 0));
}

#[test]
fn horizontal_movement_resets_the_desired_column() {
    let mut buffer = TextBuffer::from_text("abcdef\nxy\nghijkl");
    for _ in 0..4 {
        buffer.move_right();
    }
    buffer.move_down(); // clamps to (1, 2), desired_column still 4
    buffer.move_left(); // now at (1, 1); should reset desired_column to 1
    buffer.move_down();
    assert_eq!(
        buffer.cursor(),
        Position::new(2, 1),
        "desired column should follow the most recent horizontal move, not the stale 4"
    );
}

// -- home/end, document start/end ------------------------------------

#[test]
fn move_line_end_and_move_line_start_go_to_the_line_boundaries() {
    let mut buffer = TextBuffer::from_text("hello\nworld");
    buffer.move_line_end();
    assert_eq!(buffer.cursor(), Position::new(0, 5));
    buffer.move_line_start();
    assert_eq!(buffer.cursor(), Position::new(0, 0));
}

#[test]
fn move_document_end_and_move_document_start_go_to_the_document_boundaries() {
    let mut buffer = TextBuffer::from_text("hello\nworld\n!");
    buffer.move_document_end();
    assert_eq!(buffer.cursor(), Position::new(2, 1));
    buffer.move_document_start();
    assert_eq!(buffer.cursor(), Position::new(0, 0));
}

// -- selection ---------------------------------------------------------

#[test]
fn extend_right_builds_a_selection_from_the_starting_cursor() {
    let mut buffer = TextBuffer::from_text("hello");
    buffer.extend_right();
    buffer.extend_right();
    buffer.extend_right();
    let selection = buffer.selection().expect("expected an active selection");
    assert_eq!(selection.anchor, Position::new(0, 0));
    assert_eq!(selection.cursor, Position::new(0, 3));
    assert_eq!(buffer.selected_text(), "hel");
}

#[test]
fn a_plain_move_after_extending_collapses_the_selection() {
    let mut buffer = TextBuffer::from_text("hello");
    buffer.extend_right();
    buffer.extend_right();
    assert!(buffer.has_selection());
    buffer.move_right();
    assert!(!buffer.has_selection());
}

#[test]
fn extend_movement_reversed_selects_backward_from_the_anchor() {
    let mut buffer = TextBuffer::from_text("hello");
    buffer.move_line_end();
    buffer.extend_left();
    buffer.extend_left();
    let (start, end) = buffer.selection().expect("expected a selection").ordered();
    assert_eq!(start, Position::new(0, 3));
    assert_eq!(end, Position::new(0, 5));
    assert_eq!(buffer.selected_text(), "lo");
}

#[test]
fn select_all_selects_the_entire_document() {
    let mut buffer = TextBuffer::from_text("select 1;\nfrom dual;");
    buffer.select_all();
    assert_eq!(buffer.selected_text(), "select 1;\nfrom dual;");
    assert_eq!(buffer.selection().unwrap().anchor, Position::new(0, 0));
    assert_eq!(buffer.cursor(), Position::new(1, 10));
}

#[test]
fn select_line_on_the_first_line_selects_its_full_text() {
    let mut buffer = TextBuffer::from_text("select 1;\nfrom dual;\nwhere 1=1;");
    buffer.select_line(0);
    let (start, end) = buffer.selection().unwrap().ordered();
    assert_eq!(start, Position::new(0, 0));
    assert_eq!(end, Position::new(0, "select 1;".chars().count()));
    assert_eq!(buffer.selected_text(), "select 1;");
}

#[test]
fn select_line_on_a_middle_line_selects_only_that_line() {
    let mut buffer = TextBuffer::from_text("select 1;\nfrom dual;\nwhere 1=1;");
    buffer.select_line(1);
    assert_eq!(buffer.selected_text(), "from dual;");
    assert_eq!(
        buffer.cursor(),
        Position::new(1, "from dual;".chars().count())
    );
    let (start, end) = buffer.selection().unwrap().ordered();
    assert_eq!(start, Position::new(1, 0));
    assert_eq!(end, Position::new(1, "from dual;".chars().count()));
    assert_eq!(buffer.text(), "select 1;\nfrom dual;\nwhere 1=1;");
}

#[test]
fn select_line_on_an_empty_line_places_the_cursor_with_no_visible_selection() {
    let mut buffer = TextBuffer::from_text("a\nb\n");
    buffer.select_line(2);
    assert!(buffer.selection().is_none());
    assert_eq!(buffer.cursor(), Position::new(2, 0));

    let mut empty_buffer = TextBuffer::from_text("");
    empty_buffer.select_line(0);
    assert!(empty_buffer.selection().is_none());
    assert_eq!(empty_buffer.cursor(), Position::new(0, 0));
}

#[test]
fn select_line_on_the_last_line_without_a_trailing_newline_selects_to_its_end() {
    let mut buffer = TextBuffer::from_text("one\ntwo");
    buffer.select_line(1);
    assert_eq!(buffer.selected_text(), "two");
    assert_eq!(buffer.cursor(), Position::new(1, 3));
}

#[test]
fn select_line_clamps_an_out_of_range_line_to_the_last_line() {
    let mut buffer = TextBuffer::from_text("one\ntwo\nthree");
    buffer.select_line(999);
    assert_eq!(buffer.selected_text(), "three");
    assert_eq!(buffer.cursor(), Position::new(2, 5));
}

#[test]
fn select_line_ignores_any_prior_cursor_column() {
    let mut early_cursor = TextBuffer::from_text("select 1;\nfrom dual;");
    early_cursor.set_cursor(Position::new(0, 0));
    early_cursor.select_line(1);

    let mut late_cursor = TextBuffer::from_text("select 1;\nfrom dual;");
    late_cursor.move_document_end();
    late_cursor.select_line(1);

    assert_eq!(early_cursor.selection(), late_cursor.selection());
    assert_eq!(early_cursor.cursor(), late_cursor.cursor());
}

#[test]
fn select_line_breaks_the_undo_group() {
    // Selects an empty line so select_line leaves no selection to
    // replace, isolating the undo-group break from insert_text's
    // separate selection-replacement behavior.
    let mut buffer = TextBuffer::from_text("\n");
    buffer.insert_text("a");
    buffer.insert_text("b");
    buffer.select_line(1);
    buffer.insert_text("c");
    buffer.insert_text("d");
    assert_eq!(buffer.text(), "ab\ncd");

    assert!(buffer.undo());
    assert_eq!(
        buffer.text(),
        "ab\n",
        "the second typing run alone should undo first"
    );
    assert!(buffer.undo());
    assert_eq!(buffer.text(), "\n");
    assert!(!buffer.undo());
}

// -- select_word --------------------------------------------------------

#[test]
fn select_word_in_the_middle_of_a_word_selects_the_whole_word() {
    let mut buffer = TextBuffer::from_text("hello world");
    buffer.select_word(Position::new(0, 2));
    assert_eq!(buffer.selected_text(), "hello");
    let (start, end) = buffer.selection().unwrap().ordered();
    assert_eq!(start, Position::new(0, 0));
    assert_eq!(end, Position::new(0, 5));
}

#[test]
fn select_word_at_a_words_first_char_selects_the_whole_word() {
    let mut buffer = TextBuffer::from_text("hello world");
    buffer.select_word(Position::new(0, 0));
    assert_eq!(buffer.selected_text(), "hello");
}

#[test]
fn select_word_at_a_words_last_char_selects_the_whole_word() {
    let mut buffer = TextBuffer::from_text("hello world");
    buffer.select_word(Position::new(0, 4));
    assert_eq!(buffer.selected_text(), "hello");
}

#[test]
fn select_word_on_a_word_starting_at_line_start_selects_from_column_zero() {
    let mut buffer = TextBuffer::from_text("start middle end");
    buffer.select_word(Position::new(0, 1));
    assert_eq!(buffer.selected_text(), "start");
    let (start, _) = buffer.selection().unwrap().ordered();
    assert_eq!(start, Position::new(0, 0));
}

#[test]
fn select_word_on_a_word_ending_at_line_end_selects_through_the_line_end() {
    let mut buffer = TextBuffer::from_text("hello world");
    buffer.select_word(Position::new(0, 10));
    assert_eq!(buffer.selected_text(), "world");
    let (_, end) = buffer.selection().unwrap().ordered();
    assert_eq!(end, Position::new(0, 11));
}

#[test]
fn select_word_on_whitespace_between_words_selects_only_the_whitespace_run() {
    let mut buffer = TextBuffer::from_text("foo   bar");
    buffer.select_word(Position::new(0, 4));
    assert_eq!(buffer.selected_text(), "   ");
    let (start, end) = buffer.selection().unwrap().ordered();
    assert_eq!(start, Position::new(0, 3));
    assert_eq!(end, Position::new(0, 6));
}

#[test]
fn select_word_on_a_punctuation_run_selects_only_that_run() {
    let mut buffer = TextBuffer::from_text("foo...bar");
    buffer.select_word(Position::new(0, 4));
    assert_eq!(buffer.selected_text(), "...");

    let mut operators = TextBuffer::from_text("a =<> b");
    operators.select_word(Position::new(0, 3));
    assert_eq!(operators.selected_text(), "=<>");
}

#[test]
fn select_word_on_an_empty_line_selects_nothing() {
    let mut buffer = TextBuffer::from_text("a\n\nb");
    buffer.select_word(Position::new(1, 0));
    assert!(buffer.selection().is_none());
    assert_eq!(buffer.cursor(), Position::new(1, 0));

    let mut empty_buffer = TextBuffer::from_text("");
    empty_buffer.select_word(Position::new(0, 0));
    assert!(empty_buffer.selection().is_none());
    assert_eq!(empty_buffer.cursor(), Position::new(0, 0));
}

#[test]
fn select_word_at_or_past_the_line_end_selects_nothing() {
    let mut buffer = TextBuffer::from_text("hello");
    buffer.select_word(Position::new(0, 5));
    assert!(buffer.selection().is_none());
    assert_eq!(buffer.cursor(), Position::new(0, 5));
}

#[test]
fn select_word_handles_accented_word_chars_by_char_column_not_byte_offset() {
    let mut buffer = TextBuffer::from_text("caf\u{e9} tea");
    buffer.select_word(Position::new(0, 3));
    assert_eq!(buffer.selected_text(), "caf\u{e9}");
    let (start, end) = buffer.selection().unwrap().ordered();
    assert_eq!(start, Position::new(0, 0));
    assert_eq!(end, Position::new(0, 4));
}

#[test]
fn select_word_handles_cjk_word_chars_by_char_column() {
    let mut buffer = TextBuffer::from_text("\u{4e2d}\u{6587} select");
    buffer.select_word(Position::new(0, 1));
    assert_eq!(buffer.selected_text(), "\u{4e2d}\u{6587}");
    let (start, end) = buffer.selection().unwrap().ordered();
    assert_eq!(start, Position::new(0, 0));
    assert_eq!(end, Position::new(0, 2));
}

#[test]
fn selected_text_spans_multiple_lines_correctly() {
    let mut buffer = TextBuffer::from_text("select *\nfrom orders\nwhere id = 1");
    for _ in 0..7 {
        buffer.move_right(); // just before '*'
    }
    buffer.extend_down();
    buffer.extend_down();
    buffer.extend_line_start();
    for _ in 0..5 {
        buffer.extend_right(); // "where" -> 5 chars
    }
    let selected = buffer.selected_text();
    assert_eq!(selected, "*\nfrom orders\nwhere");
}

#[test]
fn extending_back_to_the_anchor_collapses_the_selection() {
    let mut buffer = TextBuffer::from_text("hello");
    buffer.extend_right();
    buffer.extend_left();
    assert!(!buffer.has_selection());
}

#[test]
fn extend_up_selects_upward_and_preserves_the_desired_column_across_repeats() {
    let mut buffer = TextBuffer::from_text("abcdef\nxy\nghijkl");
    buffer.move_document_end();
    for _ in 0..2 {
        buffer.move_left();
    }
    // cursor (2, 4), desired_column 4
    buffer.extend_up();
    assert_eq!(
        buffer.cursor(),
        Position::new(1, 2),
        "the short middle line should clamp the column"
    );
    let (start, end) = buffer.selection().expect("expected a selection").ordered();
    assert_eq!(start, Position::new(1, 2));
    assert_eq!(end, Position::new(2, 4));

    buffer.extend_up();
    assert_eq!(
        buffer.cursor(),
        Position::new(0, 4),
        "desired column of 4 should be restored once the line is long enough again"
    );
    let (start, end) = buffer.selection().expect("expected a selection").ordered();
    assert_eq!(start, Position::new(0, 4));
    assert_eq!(end, Position::new(2, 4));
}

#[test]
fn extend_line_end_selects_from_the_cursor_to_the_line_end() {
    let mut buffer = TextBuffer::from_text("select *\nfrom orders");
    buffer.move_down();
    for _ in 0..5 {
        buffer.move_right();
    }
    assert_eq!(buffer.cursor(), Position::new(1, 5));
    buffer.extend_line_end();
    let selection = buffer.selection().expect("expected an active selection");
    assert_eq!(selection.anchor, Position::new(1, 5));
    assert_eq!(selection.cursor, Position::new(1, 11));
    assert_eq!(buffer.cursor(), Position::new(1, 11));
    assert_eq!(buffer.selected_text(), "orders");
}

#[test]
fn extend_document_start_selects_from_the_cursor_back_to_the_document_start() {
    let mut buffer = TextBuffer::from_text("select *\nfrom orders");
    buffer.move_down();
    buffer.move_line_end();
    buffer.extend_document_start();
    let selection = buffer.selection().expect("expected a selection");
    assert_eq!(
        selection.anchor,
        Position::new(1, 11),
        "anchor stays fixed at the starting cursor"
    );
    assert_eq!(buffer.cursor(), Position::new(0, 0));
    assert_eq!(buffer.selected_text(), "select *\nfrom orders");
}

#[test]
fn extend_document_end_selects_from_the_cursor_forward_to_the_document_end() {
    let mut buffer = TextBuffer::from_text("select *\nfrom orders");
    buffer.extend_document_end();
    let selection = buffer.selection().expect("expected a selection");
    assert_eq!(
        selection.anchor,
        Position::new(0, 0),
        "anchor stays fixed at the starting cursor"
    );
    assert_eq!(buffer.cursor(), Position::new(1, 11));
    assert_eq!(buffer.selected_text(), "select *\nfrom orders");
}

// -- editing: insert -----------------------------------------------

#[test]
fn insert_text_inserts_at_the_cursor_and_advances_it() {
    let mut buffer = TextBuffer::from_text("helo");
    for _ in 0..3 {
        buffer.move_right();
    }
    buffer.insert_text("l");
    assert_eq!(buffer.text(), "hello");
    assert_eq!(buffer.cursor(), Position::new(0, 4));
}

#[test]
fn insert_text_with_embedded_newlines_splits_across_lines() {
    let mut buffer = TextBuffer::from_text("ac");
    buffer.move_right();
    buffer.insert_text("XY\nZW");
    assert_eq!(buffer.text(), "aXY\nZWc");
    assert_eq!(buffer.cursor(), Position::new(1, 2));
}

#[test]
fn insert_text_replaces_an_active_selection() {
    let mut buffer = TextBuffer::from_text("hello world");
    buffer.move_line_end();
    for _ in 0..5 {
        buffer.extend_left();
    }
    assert_eq!(buffer.selected_text(), "world");
    buffer.insert_text("there");
    assert_eq!(buffer.text(), "hello there");
    assert!(!buffer.has_selection());
    assert_eq!(buffer.cursor(), Position::new(0, 11));
}

#[test]
fn insert_text_replacing_a_multiline_selection_joins_correctly() {
    let mut buffer = TextBuffer::from_text("one\ntwo\nthree");
    buffer.move_right();
    buffer.extend_down();
    buffer.extend_down();
    buffer.extend_right(); // selects "ne\ntwo\nth"
    assert_eq!(buffer.selected_text(), "ne\ntwo\nth");
    buffer.insert_text("X");
    assert_eq!(buffer.text(), "oXree");
    assert_eq!(buffer.cursor(), Position::new(0, 2));
}

#[test]
fn insert_newline_splits_the_current_line_at_the_cursor() {
    let mut buffer = TextBuffer::from_text("helloworld");
    buffer.cursor = Position::new(0, 5);
    buffer.insert_newline();
    assert_eq!(buffer.lines(), &["hello".to_owned(), "world".to_owned()]);
    assert_eq!(buffer.cursor(), Position::new(1, 0));
}

// -- editing: backspace ---------------------------------------------

#[test]
fn backspace_deletes_the_char_before_the_cursor() {
    let mut buffer = TextBuffer::from_text("hello");
    buffer.move_line_end();
    buffer.backspace();
    assert_eq!(buffer.text(), "hell");
    assert_eq!(buffer.cursor(), Position::new(0, 4));
}

#[test]
fn backspace_at_line_start_joins_with_the_previous_line() {
    let mut buffer = TextBuffer::from_text("hello\nworld");
    buffer.cursor = Position::new(1, 0);
    buffer.backspace();
    assert_eq!(buffer.lines(), &["helloworld".to_owned()]);
    assert_eq!(
        buffer.cursor(),
        Position::new(0, 5),
        "cursor should land at the join point"
    );
}

#[test]
fn backspace_at_document_start_does_nothing() {
    let mut buffer = TextBuffer::from_text("hello");
    buffer.backspace();
    assert_eq!(buffer.text(), "hello");
    assert_eq!(buffer.cursor(), Position::new(0, 0));
}

#[test]
fn backspace_with_an_active_selection_deletes_the_selection_instead_of_one_char() {
    let mut buffer = TextBuffer::from_text("hello world");
    buffer.move_document_end();
    for _ in 0..5 {
        buffer.extend_left();
    }
    buffer.backspace();
    assert_eq!(buffer.text(), "hello ");
    assert!(!buffer.has_selection());
    assert_eq!(buffer.cursor(), Position::new(0, 6));
}

#[test]
fn backspace_over_a_selection_resyncs_the_desired_column_for_later_vertical_moves() {
    let mut buffer = TextBuffer::from_text("abcdefgh\nijklmnop");
    buffer.move_right();
    buffer.move_right(); // cursor (0, 2), desired_column 2
    buffer.extend_right();
    buffer.extend_right();
    buffer.extend_right(); // selection (0,2)-(0,5)
    buffer.backspace();
    assert_eq!(buffer.cursor(), Position::new(0, 2));
    buffer.move_down();
    assert_eq!(
        buffer.cursor(),
        Position::new(1, 2),
        "desired column should follow the collapsed selection start, not the far end"
    );
}

// -- editing: delete-forward ------------------------------------------

#[test]
fn delete_forward_deletes_the_char_after_the_cursor() {
    let mut buffer = TextBuffer::from_text("hello");
    buffer.delete_forward();
    assert_eq!(buffer.text(), "ello");
    assert_eq!(buffer.cursor(), Position::new(0, 0));
}

#[test]
fn delete_forward_at_line_end_joins_with_the_next_line() {
    let mut buffer = TextBuffer::from_text("hello\nworld");
    buffer.move_line_end();
    buffer.delete_forward();
    assert_eq!(buffer.lines(), &["helloworld".to_owned()]);
    assert_eq!(buffer.cursor(), Position::new(0, 5));
}

#[test]
fn delete_forward_at_document_end_does_nothing() {
    let mut buffer = TextBuffer::from_text("hello");
    buffer.move_document_end();
    buffer.delete_forward();
    assert_eq!(buffer.text(), "hello");
}

#[test]
fn delete_forward_with_an_active_selection_deletes_the_selection_instead_of_one_char() {
    let mut buffer = TextBuffer::from_text("hello world");
    buffer.extend_right();
    buffer.extend_right();
    buffer.extend_right();
    buffer.extend_right();
    buffer.extend_right();
    buffer.delete_forward();
    assert_eq!(buffer.text(), " world");
    assert_eq!(buffer.cursor(), Position::new(0, 0));
}

#[test]
fn delete_forward_over_a_selection_resyncs_the_desired_column_for_later_vertical_moves() {
    let mut buffer = TextBuffer::from_text("abcdefgh\nijklmnop");
    buffer.move_right();
    buffer.move_right(); // cursor (0, 2), desired_column 2
    buffer.extend_right();
    buffer.extend_right();
    buffer.extend_right(); // selection (0,2)-(0,5)
    buffer.delete_forward();
    assert_eq!(buffer.cursor(), Position::new(0, 2));
    buffer.move_down();
    assert_eq!(
        buffer.cursor(),
        Position::new(1, 2),
        "desired column should follow the collapsed selection start, not the far end"
    );
}

// -- UTF-8 correctness -------------------------------------------------

#[test]
fn multi_byte_characters_are_navigated_and_edited_by_char_not_byte() {
    let mut buffer = TextBuffer::from_text("caf\u{e9} \u{2603} \u{1f600}bar");
    // 11 chars: c a f e-acute space snowman space emoji b a r
    assert_eq!(buffer.lines()[0].chars().count(), 11);

    buffer.move_document_end();
    assert_eq!(buffer.cursor(), Position::new(0, 11));

    for _ in 0..3 {
        buffer.move_left();
    }
    // cursor now just before 'b' in "bar", after the emoji
    buffer.insert_text("X");
    assert_eq!(buffer.text(), "caf\u{e9} \u{2603} \u{1f600}Xbar");

    buffer.move_document_start();
    for _ in 0..4 {
        buffer.move_right();
    }
    // cursor after "caf\u{e9}", before the space
    buffer.backspace();
    assert_eq!(buffer.text(), "caf \u{2603} \u{1f600}Xbar");

    buffer.delete_forward();
    assert_eq!(buffer.text(), "caf\u{2603} \u{1f600}Xbar");
}

#[test]
fn selection_across_multi_byte_characters_extracts_correct_text() {
    let mut buffer = TextBuffer::from_text("na\u{efdc}ve caf\u{e9}");
    buffer.select_all();
    assert_eq!(buffer.selected_text(), "na\u{efdc}ve caf\u{e9}");
}

// -- query access --------------------------------------------------

#[test]
fn query_text_returns_the_full_document_when_nothing_is_selected() {
    let buffer = TextBuffer::from_text("select 1;\nselect 2;");
    assert_eq!(buffer.query_text(), "select 1;\nselect 2;");
}

#[test]
fn query_text_returns_only_the_selection_when_something_is_selected() {
    let mut buffer = TextBuffer::from_text("select 1;\nselect 2;");
    buffer.move_line_start();
    buffer.move_down();
    for _ in 0.."select 2;".len() {
        buffer.extend_right();
    }
    assert_eq!(buffer.query_text(), "select 2;");
    assert_eq!(buffer.text(), "select 1;\nselect 2;");
}

// -- set_cursor / set_selection --------------------------------------

#[test]
fn set_cursor_moves_the_cursor_and_clears_any_selection() {
    let mut buffer = TextBuffer::from_text("abc\ndef");
    buffer.extend_right();
    assert!(buffer.has_selection());
    buffer.set_cursor(Position::new(1, 2));
    assert_eq!(buffer.cursor(), Position::new(1, 2));
    assert!(!buffer.has_selection());
}

#[test]
fn set_cursor_clamps_to_the_document() {
    let mut buffer = TextBuffer::from_text("ab\ncd");
    buffer.set_cursor(Position::new(9, 9));
    assert_eq!(buffer.cursor(), Position::new(1, 2));
}

#[test]
fn set_selection_spans_the_given_anchor_and_cursor() {
    let mut buffer = TextBuffer::from_text("select *\nfrom orders");
    buffer.set_selection(Position::new(0, 7), Position::new(1, 4));
    let selection = buffer.selection().expect("expected an active selection");
    assert_eq!(selection.anchor, Position::new(0, 7));
    assert_eq!(selection.cursor, Position::new(1, 4));
    assert_eq!(buffer.selected_text(), "*\nfrom");
}

#[test]
fn set_selection_clamps_both_endpoints_to_the_document() {
    let mut buffer = TextBuffer::from_text("ab\ncd");
    buffer.set_selection(Position::new(0, 0), Position::new(50, 50));
    let selection = buffer.selection().expect("expected an active selection");
    assert_eq!(selection.cursor, Position::new(1, 2));
}

// -- position <-> offset conversions ----------------------------------

#[test]
fn line_byte_offset_finds_the_byte_index_of_a_multi_byte_column() {
    let buffer = TextBuffer::from_text("caf\u{e9} tea");
    // column 4 is right after the e-acute (2 bytes), so byte offset 5
    assert_eq!(buffer.line_byte_offset(Position::new(0, 4)), 5);
}

#[test]
fn char_offset_for_position_counts_newlines_as_one_character() {
    let buffer = TextBuffer::from_text("ab\ncd");
    assert_eq!(buffer.char_offset_for_position(Position::new(0, 0)), 0);
    assert_eq!(
        buffer.char_offset_for_position(Position::new(0, 2)),
        2,
        "end of the first line, just before the newline"
    );
    assert_eq!(
        buffer.char_offset_for_position(Position::new(1, 0)),
        3,
        "start of the second line, just after the newline"
    );
    assert_eq!(buffer.char_offset_for_position(Position::new(1, 2)), 5);
}

#[test]
fn position_for_char_offset_is_the_inverse_of_char_offset_for_position() {
    let buffer = TextBuffer::from_text("select *\nfrom orders\nwhere id = 1");
    let total = buffer.text().chars().count();
    for offset in 0..=total {
        let position = buffer.position_for_char_offset(offset);
        assert_eq!(
            buffer.char_offset_for_position(position),
            offset,
            "round trip failed for offset {offset}"
        );
    }
}

#[test]
fn position_for_char_offset_clamps_past_the_document_end() {
    let buffer = TextBuffer::from_text("ab\ncd");
    assert_eq!(buffer.position_for_char_offset(999), Position::new(1, 2));
}

// -- undo/redo -----------------------------------------------------

#[test]
fn undo_on_a_fresh_buffer_is_a_noop() {
    let mut buffer = TextBuffer::from_text("hello");
    assert!(!buffer.undo());
    assert_eq!(buffer.text(), "hello");
    assert_eq!(buffer.cursor(), Position::new(0, 0));
}

#[test]
fn redo_with_nothing_undone_is_a_noop() {
    let mut buffer = TextBuffer::from_text("hello");
    buffer.move_document_end();
    assert!(!buffer.redo());
    assert_eq!(buffer.text(), "hello");
    assert_eq!(buffer.cursor(), Position::new(0, 5));
}

#[test]
fn consecutive_single_char_insertions_coalesce_into_one_undo_group() {
    let mut buffer = TextBuffer::new();
    for ch in "hello".chars() {
        buffer.insert_text(&ch.to_string());
    }
    assert_eq!(buffer.text(), "hello");

    assert!(buffer.undo());
    assert_eq!(
        buffer.text(),
        "",
        "one undo should remove the whole typed word, not one character"
    );
    assert_eq!(buffer.cursor(), Position::new(0, 0));
}

#[test]
fn caret_movement_between_typing_runs_breaks_the_undo_group() {
    let mut buffer = TextBuffer::new();
    buffer.insert_text("a");
    buffer.insert_text("b");
    buffer.move_left();
    buffer.insert_text("c");
    buffer.insert_text("d");
    assert_eq!(buffer.text(), "acdb");

    assert!(buffer.undo());
    assert_eq!(
        buffer.text(),
        "ab",
        "the second typing run alone should undo first"
    );
    assert!(buffer.undo());
    assert_eq!(
        buffer.text(),
        "",
        "the first typing run should need its own, second undo"
    );
    assert!(!buffer.undo());
}

#[test]
fn home_end_movement_breaks_the_undo_group() {
    let mut buffer = TextBuffer::new();
    buffer.insert_text("a");
    buffer.insert_text("b");
    buffer.move_line_start();
    buffer.move_line_end();
    buffer.insert_text("c");
    buffer.insert_text("d");
    assert_eq!(buffer.text(), "abcd");

    assert!(buffer.undo());
    assert_eq!(buffer.text(), "ab");
    assert!(buffer.undo());
    assert_eq!(buffer.text(), "");
    assert!(!buffer.undo());
}

#[test]
fn set_cursor_breaks_the_undo_group() {
    let mut buffer = TextBuffer::new();
    buffer.insert_text("a");
    buffer.insert_text("b");
    buffer.set_cursor(Position::new(0, 0));
    buffer.insert_text("c");
    buffer.insert_text("d");
    assert_eq!(buffer.text(), "cdab");

    assert!(buffer.undo());
    assert_eq!(buffer.text(), "ab");
    assert!(buffer.undo());
    assert_eq!(buffer.text(), "");
    assert!(!buffer.undo());
}

#[test]
fn set_selection_breaks_the_undo_group() {
    let mut buffer = TextBuffer::new();
    buffer.insert_text("a");
    buffer.insert_text("b");
    buffer.set_selection(Position::new(0, 0), Position::new(0, 1));
    buffer.insert_text("X");
    assert_eq!(buffer.text(), "Xb");

    assert!(buffer.undo());
    assert_eq!(buffer.text(), "ab");
    assert!(buffer.undo());
    assert_eq!(buffer.text(), "");
    assert!(!buffer.undo());
}

#[test]
fn inserting_a_newline_breaks_the_undo_group_on_both_sides() {
    let mut buffer = TextBuffer::new();
    buffer.insert_text("a");
    buffer.insert_text("b");
    buffer.insert_newline();
    buffer.insert_text("c");
    buffer.insert_text("d");
    assert_eq!(buffer.text(), "ab\ncd");

    assert!(buffer.undo());
    assert_eq!(buffer.text(), "ab\n");
    assert!(buffer.undo());
    assert_eq!(buffer.text(), "ab");
    assert!(buffer.undo());
    assert_eq!(buffer.text(), "");
    assert!(!buffer.undo());
}

#[test]
fn switching_from_insert_to_delete_breaks_the_undo_group() {
    let mut buffer = TextBuffer::from_text("xy");
    buffer.move_document_end();
    buffer.insert_text("a");
    buffer.insert_text("b");
    buffer.backspace();
    assert_eq!(buffer.text(), "xya");

    assert!(buffer.undo());
    assert_eq!(
        buffer.text(),
        "xyab",
        "the backspace alone should undo first"
    );
    assert!(buffer.undo());
    assert_eq!(
        buffer.text(),
        "xy",
        "the typing run needs its own, separate undo"
    );
}

#[test]
fn consecutive_single_char_backspaces_coalesce_into_one_undo_group() {
    let mut buffer = TextBuffer::from_text("hello");
    buffer.move_document_end();
    buffer.backspace();
    buffer.backspace();
    buffer.backspace();
    assert_eq!(buffer.text(), "he");

    assert!(buffer.undo());
    assert_eq!(buffer.text(), "hello");
}

#[test]
fn a_multi_char_insertion_forms_its_own_group_and_does_not_merge_with_adjacent_runs() {
    let mut buffer = TextBuffer::new();
    buffer.insert_text("a");
    buffer.insert_text("b");
    buffer.insert_text("XYZ"); // a single multi-char insert, e.g. a paste
    buffer.insert_text("c");
    buffer.insert_text("d");
    assert_eq!(buffer.text(), "abXYZcd");

    assert!(buffer.undo());
    assert_eq!(buffer.text(), "abXYZ", "the last typing run undoes alone");
    assert!(buffer.undo());
    assert_eq!(
        buffer.text(),
        "ab",
        "the pasted text undoes alone, separate from either run"
    );
    assert!(buffer.undo());
    assert_eq!(buffer.text(), "", "the first typing run undoes last");
    assert!(!buffer.undo());
}

#[test]
fn a_single_char_paste_forms_its_own_group_and_does_not_merge_with_adjacent_typed_runs() {
    let mut buffer = TextBuffer::new();
    buffer.insert_text("a");
    buffer.insert_text("b");
    buffer.insert_pasted_text("X"); // a single-character paste
    buffer.insert_text("c");
    buffer.insert_text("d");
    assert_eq!(buffer.text(), "abXcd");

    assert!(buffer.undo());
    assert_eq!(buffer.text(), "abX", "the last typing run undoes alone");
    assert!(buffer.undo());
    assert_eq!(
        buffer.text(),
        "ab",
        "the single-char paste undoes alone, separate from either typing run"
    );
    assert!(buffer.undo());
    assert_eq!(buffer.text(), "", "the first typing run undoes last");
    assert!(!buffer.undo());
}

#[test]
fn backspace_at_document_start_is_a_true_noop_and_pushes_no_undo_history() {
    let mut buffer = TextBuffer::from_text("hello");
    buffer.insert_text("x"); // gives undo/redo stacks something to lose if the bug regresses
    buffer.undo();
    let redo_available_before = buffer.redo();
    assert!(redo_available_before, "sanity: redo was available");
    buffer.undo(); // back to a clean slate with an empty undo/redo history

    buffer.backspace();
    assert_eq!(buffer.text(), "hello");
    assert!(
        !buffer.undo(),
        "a no-op backspace at the document start must not push a phantom undo entry"
    );
}

#[test]
fn delete_forward_at_document_end_is_a_true_noop_and_pushes_no_undo_history() {
    let mut buffer = TextBuffer::from_text("hello");
    buffer.move_document_end();

    buffer.delete_forward();
    assert_eq!(buffer.text(), "hello");
    assert!(
        !buffer.undo(),
        "a no-op delete-forward at the document end must not push a phantom undo entry"
    );
}

#[test]
fn inserting_empty_text_with_no_selection_is_a_noop_and_pushes_no_undo_history() {
    let mut buffer = TextBuffer::from_text("hello");

    buffer.insert_text("");
    assert_eq!(buffer.text(), "hello");
    assert!(
        !buffer.undo(),
        "an empty insert with no selection must not push a phantom undo entry"
    );
}

#[test]
fn a_noop_edit_does_not_clear_an_existing_redo_stack() {
    let mut buffer = TextBuffer::new();
    buffer.insert_text("a");
    assert!(buffer.undo());
    assert_eq!(buffer.text(), "");

    buffer.backspace(); // no-op: already at the document start
    assert_eq!(buffer.text(), "");

    assert!(
        buffer.redo(),
        "a no-op edit must not clear a redo stack built by an earlier undo"
    );
    assert_eq!(buffer.text(), "a");
}

#[test]
fn undo_restores_the_caret_and_selection_from_before_the_group() {
    let mut buffer = TextBuffer::from_text("select ");
    buffer.move_document_end();
    for _ in 0..4 {
        buffer.extend_left();
    }
    let selection_before_edit = buffer.selection();
    let cursor_before_edit = buffer.cursor();
    assert_eq!(buffer.selected_text(), "ect ");

    buffer.backspace(); // deletes the selection as one undo group
    assert_eq!(buffer.text(), "sel");

    assert!(buffer.undo());
    assert_eq!(
        buffer.selection(),
        selection_before_edit,
        "undo restores the selection exactly as it was before the group's first edit"
    );
    assert_eq!(buffer.cursor(), cursor_before_edit);
    assert_eq!(buffer.text(), "select ");
}

#[test]
fn undo_redo_round_trip_is_byte_for_byte_and_cursor_for_cursor_identical() {
    let mut buffer = TextBuffer::new();
    for ch in "hello".chars() {
        buffer.insert_text(&ch.to_string());
    }
    let text_before_undo = buffer.text();
    let cursor_before_undo = buffer.cursor();

    assert!(buffer.undo());
    assert_eq!(buffer.text(), "");

    assert!(buffer.redo());
    assert_eq!(buffer.text(), text_before_undo);
    assert_eq!(buffer.cursor(), cursor_before_undo);
    assert!(
        buffer.selection().is_none(),
        "typing leaves no active selection"
    );
}

#[test]
fn redo_restores_the_caret_and_selection_from_after_the_group() {
    let mut buffer = TextBuffer::from_text("select ");
    buffer.move_document_end();
    for _ in 0..4 {
        buffer.extend_left();
    }
    buffer.backspace(); // deletes the selection ("ect ") as one group
    assert_eq!(buffer.text(), "sel");
    let text_after_edit = buffer.text();
    let cursor_after_edit = buffer.cursor();

    assert!(buffer.undo());
    assert_eq!(buffer.text(), "select ");

    assert!(buffer.redo());
    assert_eq!(buffer.text(), text_after_edit, "redo re-applies the delete");
    assert_eq!(buffer.cursor(), cursor_after_edit);
    assert!(
        buffer.selection().is_none(),
        "redo restores the post-delete state, which has no selection"
    );
}

#[test]
fn a_new_edit_after_undo_clears_the_redo_stack() {
    let mut buffer = TextBuffer::new();
    buffer.insert_text("a");
    buffer.insert_text("b");
    assert!(buffer.undo());
    assert_eq!(buffer.text(), "");

    buffer.insert_text("x");
    assert_eq!(buffer.text(), "x");

    assert!(
        !buffer.redo(),
        "the discarded 'ab' branch must not be recoverable after a new edit"
    );
    assert_eq!(buffer.text(), "x");
}

#[test]
fn history_is_capped_and_evicts_the_oldest_group() {
    let mut buffer = TextBuffer::new();
    let groups = EDITOR_HISTORY_CAP + 10;
    for i in 0..groups {
        // A movement between each insertion forces every character into
        // its own undo group, so `groups` characters produce `groups`
        // separate groups (bounded by the cap).
        buffer.insert_text(&(i % 10).to_string());
        buffer.move_right();
    }
    assert_eq!(buffer.lines()[0].chars().count(), groups);

    let mut undo_count = 0;
    while buffer.undo() {
        undo_count += 1;
    }
    assert_eq!(
        undo_count, EDITOR_HISTORY_CAP,
        "only the most recent EDITOR_HISTORY_CAP groups should remain undoable"
    );
    assert_eq!(
        buffer.lines()[0].chars().count(),
        groups - EDITOR_HISTORY_CAP,
        "undoing every retained group should leave exactly the evicted, oldest characters"
    );
}

#[test]
fn undo_across_a_multiline_edit_leaves_the_cursor_and_selection_within_bounds() {
    let mut buffer = TextBuffer::from_text("one\ntwo\nthree");
    buffer.move_right();
    buffer.extend_down();
    buffer.extend_down();
    buffer.extend_right(); // selects "ne\ntwo\nth"
    assert_eq!(buffer.selected_text(), "ne\ntwo\nth");
    buffer.insert_text("X");
    assert_eq!(buffer.text(), "oXree");

    assert!(buffer.undo());
    assert_eq!(buffer.text(), "one\ntwo\nthree");

    let cursor = buffer.cursor();
    assert!(cursor.line < buffer.lines().len());
    assert!(cursor.column <= buffer.lines()[cursor.line].chars().count());

    if let Some(selection) = buffer.selection() {
        let (start, end) = selection.ordered();
        assert!(start.line < buffer.lines().len());
        assert!(end.line < buffer.lines().len());
        assert!(start.column <= buffer.lines()[start.line].chars().count());
        assert!(end.column <= buffer.lines()[end.line].chars().count());
    }
}
