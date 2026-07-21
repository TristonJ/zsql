//! Pure, `gpui`-window-independent single-line text editing model: content,
//! cursor, and selection anchor tracked as byte offsets, their movement and
//! editing operations, UTF-8 <-> UTF-16 offset conversions for the OS text
//! input boundary, a placeholder-visibility predicate, and the cursor-blink
//! state machine. Nothing here depends on `gpui::Window` or `gpui::Context`,
//! so every operation is directly unit-testable without a rendered window.

use std::ops::Range;
use std::time::Duration;

/// Idle time, after the last keystroke, that must elapse before a focused
/// field's cursor resumes blinking.
pub const CURSOR_BLINK_RESUME_DELAY: Duration = Duration::from_millis(500);
/// Interval on which a focused, idle cursor's visibility toggles.
pub const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(530);

/// A single-line text buffer with a cursor and an optional selection, both
/// tracked as byte offsets into `content` that always land on a `char`
/// boundary. Embedded newlines are never stored: every text-mutating
/// operation strips `\n`/`\r` first, so pasting or committing IME text that
/// contains a line break cannot smuggle a second line into a field meant to
/// hold exactly one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldModel {
    content: String,
    cursor: usize,
    /// The other end of the active selection, if any. `None` means no
    /// selection is being tracked. `Some(anchor)` where `anchor == cursor`
    /// means a selection was started but has since collapsed back to a
    /// point; [`FieldModel::selection`] treats that the same as `None`.
    anchor: Option<usize>,
}

impl Default for FieldModel {
    fn default() -> Self {
        Self::new()
    }
}

impl FieldModel {
    #[must_use]
    pub fn new() -> Self {
        Self {
            content: String::new(),
            cursor: 0,
            anchor: None,
        }
    }

    /// Build a model over `text`, cursor at the end. Any embedded newline is
    /// stripped, preserving the single-line invariant.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        let content = strip_newlines(text);
        let cursor = content.len();
        Self {
            content,
            cursor,
            anchor: None,
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.content
    }

    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The active selection's endpoints in byte order (`start <= end`), or
    /// `None` if nothing is selected (including a selection that has
    /// collapsed back to a single point).
    #[must_use]
    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        match anchor.cmp(&self.cursor) {
            std::cmp::Ordering::Less => Some(anchor..self.cursor),
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(self.cursor..anchor),
        }
    }

    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    #[must_use]
    pub fn selected_text(&self) -> &str {
        match self.selection() {
            Some(range) => &self.content[range],
            None => "",
        }
    }

    /// Whether the selection's anchor is after the cursor -- i.e. the
    /// selection was built backward from where it now ends. Mirrors what an
    /// OS text input API expects `UTF16Selection::reversed` to report.
    #[must_use]
    pub fn selection_reversed(&self) -> bool {
        matches!(self.anchor, Some(anchor) if anchor > self.cursor)
    }

    /// The selection anchor, or the cursor if there is none. The point a
    /// fresh shift-extend or shift-click should extend from.
    #[must_use]
    pub fn anchor(&self) -> usize {
        self.anchor.unwrap_or(self.cursor)
    }

    // -- movement --------------------------------------------------------

    pub fn move_left(&mut self) {
        let target = prev_char_boundary(&self.content, self.cursor);
        self.move_cursor_to(target, false);
    }

    pub fn extend_left(&mut self) {
        let target = prev_char_boundary(&self.content, self.cursor);
        self.move_cursor_to(target, true);
    }

    pub fn move_right(&mut self) {
        let target = next_char_boundary(&self.content, self.cursor);
        self.move_cursor_to(target, false);
    }

    pub fn extend_right(&mut self) {
        let target = next_char_boundary(&self.content, self.cursor);
        self.move_cursor_to(target, true);
    }

    pub fn move_home(&mut self) {
        self.move_cursor_to(0, false);
    }

    pub fn extend_home(&mut self) {
        self.move_cursor_to(0, true);
    }

    pub fn move_end(&mut self) {
        let target = self.content.len();
        self.move_cursor_to(target, false);
    }

    pub fn extend_end(&mut self) {
        let target = self.content.len();
        self.move_cursor_to(target, true);
    }

    /// Select the entire content, anchored at the start.
    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.content.len();
    }

    /// Move the cursor to `offset` with no active selection, clamped to a
    /// char boundary within the content. Used by input handling that sets
    /// the cursor directly (mouse clicks, IME) rather than via a movement
    /// action.
    pub fn set_cursor(&mut self, offset: usize) {
        self.cursor = clamp_to_char_boundary(&self.content, offset);
        self.anchor = None;
    }

    /// Set the selection to span `anchor` to `cursor`, both clamped to a
    /// char boundary within the content. Used by input handling that sets a
    /// selection directly (shift-click, drag, IME) rather than via an
    /// extend-movement action.
    pub fn set_selection(&mut self, anchor: usize, cursor: usize) {
        self.anchor = Some(clamp_to_char_boundary(&self.content, anchor));
        self.cursor = clamp_to_char_boundary(&self.content, cursor);
    }

    // -- editing -----------------------------------------------------------

    /// Insert `text` at the cursor, first replacing the active selection if
    /// there is one.
    pub fn insert_text(&mut self, text: &str) {
        self.replace_selection(text);
    }

    /// Replace the active selection (or, with no selection, insert at the
    /// cursor) with `text`, collapsing the cursor to just after the
    /// inserted text and clearing any selection.
    pub fn replace_selection(&mut self, text: &str) {
        let range = self.selection().unwrap_or(self.cursor..self.cursor);
        self.replace_range(range, text);
    }

    /// Replace `range` (byte offsets, expected to fall on char boundaries)
    /// with `text`, collapsing the cursor to just after the inserted text
    /// and clearing any selection. The lower-level primitive
    /// [`FieldModel::replace_selection`] and the OS input handler's
    /// arbitrary-range replacement both funnel through here.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str) {
        let clean = strip_newlines(text);
        self.content.replace_range(range.clone(), &clean);
        self.cursor = range.start + clean.len();
        self.anchor = None;
    }

    /// Remove the active selection, or the character before the cursor.
    pub fn backspace(&mut self) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let start = prev_char_boundary(&self.content, self.cursor);
        self.content.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.anchor = None;
    }

    /// Remove the active selection, or the character after the cursor.
    pub fn delete_forward(&mut self) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        if self.cursor >= self.content.len() {
            return;
        }
        let end = next_char_boundary(&self.content, self.cursor);
        self.content.replace_range(self.cursor..end, "");
        self.anchor = None;
    }

    // -- internal helpers ----------------------------------------------

    fn move_cursor_to(&mut self, target: usize, extend: bool) {
        let target = clamp_to_char_boundary(&self.content, target);
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = target;
    }

    fn delete_selection(&mut self) {
        let Some(range) = self.selection() else {
            return;
        };
        self.content.replace_range(range.clone(), "");
        self.cursor = range.start;
        self.anchor = None;
    }
}

fn strip_newlines(text: &str) -> String {
    text.chars()
        .filter(|&ch| ch != '\n' && ch != '\r')
        .collect()
}

fn prev_char_boundary(text: &str, offset: usize) -> usize {
    text[..offset]
        .char_indices()
        .next_back()
        .map_or(0, |(idx, _)| idx)
}

fn next_char_boundary(text: &str, offset: usize) -> usize {
    text[offset..]
        .chars()
        .next()
        .map_or(text.len(), |ch| offset + ch.len_utf8())
}

/// The nearest char boundary at or before `offset`, clamped to `text`'s
/// length. Used to make any externally-supplied byte offset (a mouse hit
/// test, an OS-supplied range) safe to slice or mutate with.
fn clamp_to_char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

// -- placeholder -------------------------------------------------------

/// Whether a field with `content` should show its placeholder instead.
#[must_use]
pub fn should_show_placeholder(content: &str) -> bool {
    content.is_empty()
}

// -- UTF-8 <-> UTF-16 offset conversions ---------------------------------
//
// `EntityInputHandler` deals in UTF-16 code unit offsets into the text the
// OS was told about. `FieldModel` only knows byte offsets, so these free
// functions bridge the two at the `gpui` boundary -- they stay out of
// `FieldModel` itself so it has no reason to know what an OS text input API
// counts in.

#[must_use]
pub fn byte_offset_from_utf16(text: &str, offset_utf16: usize) -> usize {
    let mut utf16_count = 0;
    for (byte_idx, ch) in text.char_indices() {
        if utf16_count >= offset_utf16 {
            return byte_idx;
        }
        utf16_count += ch.len_utf16();
    }
    text.len()
}

#[must_use]
pub fn byte_offset_to_utf16(text: &str, offset: usize) -> usize {
    text[..offset].chars().map(char::len_utf16).sum()
}

#[must_use]
pub fn byte_range_from_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    byte_offset_from_utf16(text, range.start)..byte_offset_from_utf16(text, range.end)
}

#[must_use]
pub fn byte_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    byte_offset_to_utf16(text, range.start)..byte_offset_to_utf16(text, range.end)
}

// -- masked-display offset conversion ------------------------------------
//
// A masked field (e.g. a password) shows one fixed-width placeholder glyph
// per character rather than `content` itself. Since the placeholder is a
// single ASCII byte per character, a masked display index equals the
// content's *char* count up to a point, letting cursor/selection/mouse
// offsets convert between "byte offset into `content`" and "index into the
// masked display string" without assuming `content` itself is ASCII.

/// The number of chars in `content` before byte offset `byte_offset` -- the
/// masked display index a real content cursor/selection offset maps to.
#[must_use]
pub fn char_count_before(content: &str, byte_offset: usize) -> usize {
    content[..byte_offset].chars().count()
}

/// The byte offset in `content` after `char_count` chars, clamped to
/// `content`'s length -- the inverse of [`char_count_before`], used to map a
/// masked display index (from a mouse hit test) back to a real content
/// offset.
#[must_use]
pub fn byte_offset_for_char_count(content: &str, char_count: usize) -> usize {
    content
        .char_indices()
        .nth(char_count)
        .map_or(content.len(), |(idx, _)| idx)
}

// -- cursor blink --------------------------------------------------------

/// Pure blink-visibility state machine, stepped by an interval timer on the
/// `gpui` executor. Typing (or any keydown) pauses blinking so the cursor
/// stays solid; blinking resumes once [`CURSOR_BLINK_RESUME_DELAY`] of idle
/// time has elapsed since the last keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlinkState {
    visible: bool,
    paused: bool,
    idle: Duration,
}

impl Default for BlinkState {
    fn default() -> Self {
        Self::new()
    }
}

impl BlinkState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            visible: true,
            paused: false,
            idle: Duration::ZERO,
        }
    }

    #[must_use]
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Called on any keydown/typing: pauses blinking and forces the cursor
    /// solid.
    pub fn on_keystroke(&mut self) {
        self.visible = true;
        self.paused = true;
        self.idle = Duration::ZERO;
    }

    /// Advance the state machine as if `elapsed` idle time (no further
    /// input) has passed since the previous tick. While paused, only the
    /// idle clock advances, resuming blinking once
    /// [`CURSOR_BLINK_RESUME_DELAY`] has accumulated; once blinking, each
    /// tick toggles visibility.
    pub fn tick(&mut self, elapsed: Duration) {
        if self.paused {
            self.idle += elapsed;
            if self.idle >= CURSOR_BLINK_RESUME_DELAY {
                self.paused = false;
                self.idle = Duration::ZERO;
                self.visible = true;
            }
            return;
        }
        self.visible = !self.visible;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        BlinkState, CURSOR_BLINK_RESUME_DELAY, FieldModel, byte_offset_for_char_count,
        byte_offset_from_utf16, byte_offset_to_utf16, byte_range_from_utf16, byte_range_to_utf16,
        char_count_before, should_show_placeholder,
    };

    // -- storage / accessors ------------------------------------------

    #[test]
    fn a_new_model_is_empty_with_the_cursor_at_the_origin() {
        let model = FieldModel::new();
        assert_eq!(model.text(), "");
        assert_eq!(model.cursor(), 0);
        assert!(model.selection().is_none());
    }

    #[test]
    fn from_text_places_the_cursor_at_the_end() {
        let model = FieldModel::from_text("hello");
        assert_eq!(model.text(), "hello");
        assert_eq!(model.cursor(), 5);
    }

    #[test]
    fn from_text_strips_embedded_newlines() {
        let model = FieldModel::from_text("hello\nworld\r\n!");
        assert_eq!(model.text(), "helloworld!");
    }

    // -- movement: left/right --------------------------------------------

    #[test]
    fn move_right_advances_one_char_at_a_time() {
        let mut model = FieldModel::from_text("abc");
        model.set_cursor(0);
        model.move_right();
        assert_eq!(model.cursor(), 1);
        model.move_right();
        assert_eq!(model.cursor(), 2);
    }

    #[test]
    fn move_right_at_end_does_not_move_past_it() {
        let mut model = FieldModel::from_text("ab");
        model.move_right();
        assert_eq!(model.cursor(), 2);
    }

    #[test]
    fn move_left_at_start_does_not_move_before_it() {
        let mut model = FieldModel::from_text("ab");
        model.set_cursor(0);
        model.move_left();
        assert_eq!(model.cursor(), 0);
    }

    #[test]
    fn move_left_steps_back_one_char() {
        let mut model = FieldModel::from_text("abc");
        model.move_left();
        assert_eq!(model.cursor(), 2);
    }

    // -- movement: home/end -------------------------------------------

    #[test]
    fn move_home_and_move_end_go_to_the_content_boundaries() {
        let mut model = FieldModel::from_text("hello");
        model.move_home();
        assert_eq!(model.cursor(), 0);
        model.move_end();
        assert_eq!(model.cursor(), 5);
    }

    // -- selection: shift-extend ------------------------------------------

    #[test]
    fn extend_right_builds_a_selection_from_the_starting_cursor() {
        let mut model = FieldModel::from_text("hello");
        model.set_cursor(0);
        model.extend_right();
        model.extend_right();
        model.extend_right();
        let selection = model.selection().expect("expected an active selection");
        assert_eq!(selection, 0..3);
        assert_eq!(model.selected_text(), "hel");
    }

    #[test]
    fn extend_left_selects_backward_from_the_anchor() {
        let mut model = FieldModel::from_text("hello");
        model.extend_left();
        model.extend_left();
        let selection = model.selection().expect("expected an active selection");
        assert_eq!(selection, 3..5);
        assert_eq!(model.selected_text(), "lo");
        assert!(
            model.selection_reversed(),
            "the anchor (end) is after the cursor (start)"
        );
    }

    #[test]
    fn a_plain_move_after_extending_collapses_the_selection() {
        let mut model = FieldModel::from_text("hello");
        model.set_cursor(0);
        model.extend_right();
        model.extend_right();
        assert!(model.has_selection());
        model.move_right();
        assert!(!model.has_selection());
    }

    #[test]
    fn extending_back_to_the_anchor_collapses_the_selection() {
        let mut model = FieldModel::from_text("hello");
        model.set_cursor(0);
        model.extend_right();
        model.extend_left();
        assert!(!model.has_selection());
    }

    #[test]
    fn extend_home_and_extend_end_select_to_the_content_boundaries() {
        let mut model = FieldModel::from_text("hello world");
        model.set_cursor(5);
        model.extend_end();
        assert_eq!(model.selected_text(), " world");
        model.set_cursor(5);
        model.extend_home();
        assert_eq!(model.selected_text(), "hello");
    }

    #[test]
    fn select_all_selects_the_entire_content() {
        let mut model = FieldModel::from_text("select 1");
        model.select_all();
        assert_eq!(model.selected_text(), "select 1");
        assert_eq!(model.selection(), Some(0..8));
    }

    // -- set_cursor / set_selection (mouse) --------------------------

    #[test]
    fn set_cursor_moves_the_cursor_and_clears_any_selection() {
        let mut model = FieldModel::from_text("abc");
        model.set_cursor(0);
        model.extend_right();
        assert!(model.has_selection());
        model.set_cursor(2);
        assert_eq!(model.cursor(), 2);
        assert!(!model.has_selection());
    }

    #[test]
    fn set_cursor_clamps_to_the_content_length() {
        let mut model = FieldModel::from_text("abc");
        model.set_cursor(999);
        assert_eq!(model.cursor(), 3);
    }

    #[test]
    fn set_selection_spans_the_given_anchor_and_cursor() {
        let mut model = FieldModel::from_text("hello world");
        model.set_selection(2, 7);
        assert_eq!(model.selection(), Some(2..7));
        assert_eq!(model.selected_text(), "llo w");
    }

    #[test]
    fn set_selection_click_then_drag_extends_from_the_fixed_anchor() {
        // Mirrors a mouse-down at offset 2 (click, no selection yet) then a
        // drag to offset 7: the anchor used for the drag's set_selection
        // call must be the click point, not wherever the cursor currently
        // sits.
        let mut model = FieldModel::from_text("hello world");
        model.set_cursor(2);
        let anchor = model.anchor();
        model.set_selection(anchor, 7);
        assert_eq!(model.selection(), Some(2..7));
    }

    // -- editing: insert -----------------------------------------------

    #[test]
    fn insert_text_inserts_at_the_cursor_and_advances_it() {
        let mut model = FieldModel::from_text("helo");
        model.set_cursor(3);
        model.insert_text("l");
        assert_eq!(model.text(), "hello");
        assert_eq!(model.cursor(), 4);
    }

    #[test]
    fn insert_text_replaces_an_active_selection() {
        let mut model = FieldModel::from_text("hello world");
        model.set_selection(6, 11);
        model.insert_text("there");
        assert_eq!(model.text(), "hello there");
        assert!(!model.has_selection());
        assert_eq!(model.cursor(), 11);
    }

    #[test]
    fn insert_text_never_inserts_a_newline() {
        let mut model = FieldModel::from_text("select ");
        model.move_end();
        model.insert_text("1\nfrom dual");
        assert_eq!(model.text(), "select 1from dual");
    }

    #[test]
    fn replace_range_replaces_an_arbitrary_byte_range() {
        let mut model = FieldModel::from_text("hello world");
        model.replace_range(0..5, "goodbye");
        assert_eq!(model.text(), "goodbye world");
        assert_eq!(model.cursor(), 7);
    }

    // -- editing: backspace / delete-forward ----------------------------

    #[test]
    fn backspace_deletes_the_char_before_the_cursor() {
        let mut model = FieldModel::from_text("hello");
        model.backspace();
        assert_eq!(model.text(), "hell");
        assert_eq!(model.cursor(), 4);
    }

    #[test]
    fn backspace_at_start_does_nothing() {
        let mut model = FieldModel::from_text("hello");
        model.set_cursor(0);
        model.backspace();
        assert_eq!(model.text(), "hello");
        assert_eq!(model.cursor(), 0);
    }

    #[test]
    fn backspace_with_an_active_selection_deletes_the_selection_instead_of_one_char() {
        let mut model = FieldModel::from_text("hello world");
        model.set_selection(6, 11);
        model.backspace();
        assert_eq!(model.text(), "hello ");
        assert!(!model.has_selection());
        assert_eq!(model.cursor(), 6);
    }

    #[test]
    fn delete_forward_deletes_the_char_after_the_cursor() {
        let mut model = FieldModel::from_text("hello");
        model.set_cursor(0);
        model.delete_forward();
        assert_eq!(model.text(), "ello");
        assert_eq!(model.cursor(), 0);
    }

    #[test]
    fn delete_forward_at_end_does_nothing() {
        let mut model = FieldModel::from_text("hello");
        model.delete_forward();
        assert_eq!(model.text(), "hello");
    }

    #[test]
    fn delete_forward_with_an_active_selection_deletes_the_selection_instead_of_one_char() {
        let mut model = FieldModel::from_text("hello world");
        model.set_selection(0, 5);
        model.delete_forward();
        assert_eq!(model.text(), " world");
        assert_eq!(model.cursor(), 0);
    }

    // -- UTF-8 correctness -------------------------------------------------

    #[test]
    fn multi_byte_characters_are_navigated_and_edited_by_char_not_byte() {
        let mut model = FieldModel::from_text("caf\u{e9} \u{2603} \u{1f600}bar");
        model.move_end();
        for _ in 0..3 {
            model.move_left();
        }
        // cursor now just before 'b' in "bar", after the emoji
        model.insert_text("X");
        assert_eq!(model.text(), "caf\u{e9} \u{2603} \u{1f600}Xbar");
    }

    // -- clipboard-shaped scenarios ------------------------------------

    #[test]
    fn pasted_clipboard_text_is_inserted_at_the_cursor() {
        let mut model = FieldModel::from_text("select  ");
        model.set_cursor(7);
        let clipboard_text = "1";
        model.insert_text(clipboard_text);
        assert_eq!(model.text(), "select 1 ");
    }

    #[test]
    fn cut_shaped_scenario_copies_then_removes_the_selection() {
        let mut model = FieldModel::from_text("hello world");
        model.set_selection(6, 11);
        let copied = model.selected_text().to_owned();
        model.backspace();
        assert_eq!(copied, "world");
        assert_eq!(model.text(), "hello ");
    }

    // -- placeholder ---------------------------------------------------

    #[test]
    fn should_show_placeholder_is_true_only_when_content_is_empty() {
        assert!(should_show_placeholder(""));
        assert!(!should_show_placeholder("x"));
    }

    // -- UTF-16 offset conversions -----------------------------------------

    #[test]
    fn ascii_utf16_offsets_match_byte_offsets() {
        let text = "hello world";
        assert_eq!(byte_offset_from_utf16(text, 5), 5);
        assert_eq!(byte_offset_to_utf16(text, 5), 5);
        assert_eq!(byte_range_from_utf16(text, 0..5), 0..5);
        assert_eq!(byte_range_to_utf16(text, 0..5), 0..5);
    }

    #[test]
    fn utf16_offsets_round_trip_through_a_surrogate_pair() {
        // U+1F600 sits outside the BMP: one `char`, four UTF-8 bytes, two
        // UTF-16 code units -- exactly the case a naive byte-count or
        // char-count implementation of the UTF-16 boundary math gets wrong.
        let text = "a\u{1F600}b";
        assert_eq!(byte_offset_from_utf16(text, 1), 1, "just after 'a'");
        assert_eq!(
            byte_offset_from_utf16(text, 3),
            5,
            "past both UTF-16 units of the emoji, at 'b'"
        );
        assert_eq!(byte_offset_to_utf16(text, 1), 1);
        assert_eq!(byte_offset_to_utf16(text, 5), 3);
        assert_eq!(byte_range_to_utf16(text, 1..5), 1..3);
        assert_eq!(byte_range_from_utf16(text, 1..3), 1..5);
    }

    // -- masked-display offset conversion -------------------------------

    #[test]
    fn char_count_before_counts_ascii_chars_one_per_byte() {
        assert_eq!(char_count_before("hello", 0), 0);
        assert_eq!(char_count_before("hello", 3), 3);
        assert_eq!(char_count_before("hello", 5), 5);
    }

    #[test]
    fn char_count_before_counts_multi_byte_chars_as_one_char_each() {
        // "p" + a-with-acute (2 bytes) + "ss": byte offset 3 sits just after
        // the 2-byte char, which is the 2nd char.
        let content = "p\u{e1}ss";
        assert_eq!(char_count_before(content, 3), 2);
        assert_eq!(char_count_before(content, content.len()), 4);
    }

    #[test]
    fn byte_offset_for_char_count_is_the_inverse_of_char_count_before() {
        let content = "p\u{e1}ss\u{e9}";
        for byte_offset in content.char_indices().map(|(idx, _)| idx) {
            let chars = char_count_before(content, byte_offset);
            assert_eq!(byte_offset_for_char_count(content, chars), byte_offset);
        }
    }

    #[test]
    fn byte_offset_for_char_count_past_the_end_clamps_to_the_content_length() {
        assert_eq!(byte_offset_for_char_count("abc", 99), 3);
    }

    // -- cursor blink --------------------------------------------------

    #[test]
    fn a_new_blink_state_starts_visible_and_unpaused() {
        let blink = BlinkState::new();
        assert!(blink.visible());
    }

    #[test]
    fn tick_alternates_visibility_when_not_paused() {
        let mut blink = BlinkState::new();
        assert!(blink.visible());
        blink.tick(Duration::from_millis(1));
        assert!(!blink.visible());
        blink.tick(Duration::from_millis(1));
        assert!(blink.visible());
        blink.tick(Duration::from_millis(1));
        assert!(!blink.visible());
    }

    #[test]
    fn a_keystroke_pauses_blinking_and_forces_the_cursor_solid() {
        let mut blink = BlinkState::new();
        blink.tick(Duration::from_millis(1)); // now hidden
        assert!(!blink.visible());
        blink.on_keystroke();
        assert!(blink.visible(), "a keystroke forces the cursor solid");
        blink.tick(Duration::from_millis(1));
        assert!(
            blink.visible(),
            "while paused, ticks must not toggle visibility"
        );
    }

    #[test]
    fn blinking_resumes_once_the_idle_delay_has_elapsed_since_the_last_keystroke() {
        let mut blink = BlinkState::new();
        blink.on_keystroke();
        assert!(blink.visible());

        // Idle time short of the resume threshold: still paused, still solid.
        blink.tick(
            CURSOR_BLINK_RESUME_DELAY
                .checked_sub(Duration::from_millis(1))
                .expect("resume delay is well above one millisecond"),
        );
        assert!(blink.visible(), "not enough idle time has elapsed yet");

        // Crossing the threshold resumes blinking (and this tick's own
        // elapsed time counts toward it).
        blink.tick(Duration::from_millis(2));
        assert!(
            blink.visible(),
            "resumes visible, then blinks on later ticks"
        );
        blink.tick(Duration::from_millis(1));
        assert!(!blink.visible(), "blinking has resumed and now toggles");
    }
}
