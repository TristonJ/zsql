//! The multiline text buffer, cursor, and selection model.

use std::cmp::Ordering;
use std::collections::VecDeque;

use crate::theme;

/// A cursor or selection endpoint: a zero-based line index and a zero-based
/// column measured in `char`s, not bytes, so a position never lands inside a
/// multi-byte character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

impl Position {
    #[must_use]
    pub fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

impl PartialOrd for Position {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Position {
    fn cmp(&self, other: &Self) -> Ordering {
        self.line
            .cmp(&other.line)
            .then(self.column.cmp(&other.column))
    }
}

/// An anchor-to-cursor selection range. `anchor` is the endpoint where the
/// selection started; `cursor` is the endpoint that keeps moving. Either one
/// may be earlier in the document than the other -- use [`Selection::ordered`]
/// to get them as `(start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Position,
    pub cursor: Position,
}

impl Selection {
    /// The endpoints in document order: `start <= end`.
    #[must_use]
    pub fn ordered(&self) -> (Position, Position) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }
}

/// The kind of a buffer edit: text insertion or deletion. Consecutive
/// single-unit edits only coalesce into one undo group when their kind matches;
/// a change in kind starts a new group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Insert,
    Delete,
}

/// Everything undo/redo must restore: the document text and the
/// caret/selection state in effect at that point.
#[derive(Clone)]
struct HistoryEntry {
    lines: Vec<String>,
    cursor: Position,
    anchor: Option<Position>,
    desired_column: usize,
}

/// The undo group currently open to absorb more edits, if any.
struct OpenGroup {
    kind: EditKind,
    /// Whether a further single-unit edit of the same kind may still merge
    /// into this group. `false` for a group that is itself already a
    /// multi-character insert, a newline, or a selection-replacing edit --
    /// those always stand alone rather than seed a run.
    coalescing: bool,
}

/// A multiline text buffer with a cursor and an optional selection.
///
/// Storage: `lines` holds one `String` per line, none of which contain a
/// `\n`; the document is their `\n`-joined concatenation. This makes the
/// line-oriented operations an editor needs most (home/end, up/down,
/// splitting a line on Enter, joining two lines on backspace/delete at a
/// line boundary) direct `Vec` indexing instead of scanning a flat string
/// for newlines. `lines` is never empty -- an empty document is a single
/// empty line, and `lines.len()` is always the document's line count.
pub struct TextBuffer {
    lines: Vec<String>,
    cursor: Position,
    /// The other end of the active selection, if any. `None` means no
    /// selection is being tracked. `Some(anchor)` where `anchor == cursor`
    /// means a selection was started but has since collapsed back to a
    /// point; [`TextBuffer::selection`] treats that the same as `None`.
    anchor: Option<Position>,
    /// The column that vertical movement (`move_up`/`move_down`) tries to
    /// return to on every line it visits, even after passing through
    /// shorter lines that force a clamp. Reset to the cursor's actual
    /// column by every horizontal movement.
    desired_column: usize,
    /// Snapshots to restore to on `undo`, oldest first, capped at
    /// [`theme::EDITOR_HISTORY_CAP`] groups.
    undo_stack: VecDeque<HistoryEntry>,
    /// Snapshots to restore to on `redo`. Cleared by any new edit.
    redo_stack: Vec<HistoryEntry>,
    /// The undo group still open to absorb more edits, if any.
    open_group: Option<OpenGroup>,
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TextBuffer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: Position::new(0, 0),
            anchor: None,
            desired_column: 0,
            undo_stack: VecDeque::new(),
            redo_stack: Vec::new(),
            open_group: None,
        }
    }

    /// Build a buffer preloaded with `text`, e.g. for a tab seeded with
    /// auto-generated SQL. The cursor starts at the document's beginning.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        let lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
        Self {
            lines,
            cursor: Position::new(0, 0),
            anchor: None,
            desired_column: 0,
            undo_stack: VecDeque::new(),
            redo_stack: Vec::new(),
            open_group: None,
        }
    }

    /// The document's lines, for a view to render.
    #[must_use]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    #[must_use]
    pub fn cursor(&self) -> Position {
        self.cursor
    }

    /// The active selection, or `None` if nothing is selected (including a
    /// selection that has collapsed back to a single point).
    #[must_use]
    pub fn selection(&self) -> Option<Selection> {
        let anchor = self.anchor?;
        if anchor == self.cursor {
            None
        } else {
            Some(Selection {
                anchor,
                cursor: self.cursor,
            })
        }
    }

    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    /// The full document text, lines joined by `\n`.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// The currently selected text, or an empty string if nothing is
    /// selected. Spans multiple lines correctly, joined by `\n`.
    #[must_use]
    pub fn selected_text(&self) -> String {
        let Some(selection) = self.selection() else {
            return String::new();
        };
        let (start, end) = selection.ordered();
        self.text_in_range(start, end)
    }

    /// Text to run when the user asks to "run the query": the selection if
    /// there is one, otherwise the whole document.
    #[must_use]
    pub fn query_text(&self) -> String {
        if self.has_selection() {
            self.selected_text()
        } else {
            self.text()
        }
    }

    // -- movement ------------------------------------------------------

    pub fn move_left(&mut self) {
        let target = self.left_of(self.cursor);
        self.move_cursor_to(target, false);
    }

    pub fn extend_left(&mut self) {
        let target = self.left_of(self.cursor);
        self.move_cursor_to(target, true);
    }

    pub fn move_right(&mut self) {
        let target = self.right_of(self.cursor);
        self.move_cursor_to(target, false);
    }

    pub fn extend_right(&mut self) {
        let target = self.right_of(self.cursor);
        self.move_cursor_to(target, true);
    }

    pub fn move_up(&mut self) {
        let target = self.above(self.cursor);
        self.move_cursor_to_preserving_desired_column(target, false);
    }

    pub fn extend_up(&mut self) {
        let target = self.above(self.cursor);
        self.move_cursor_to_preserving_desired_column(target, true);
    }

    pub fn move_down(&mut self) {
        let target = self.below(self.cursor);
        self.move_cursor_to_preserving_desired_column(target, false);
    }

    pub fn extend_down(&mut self) {
        let target = self.below(self.cursor);
        self.move_cursor_to_preserving_desired_column(target, true);
    }

    pub fn move_line_start(&mut self) {
        let target = Position::new(self.cursor.line, 0);
        self.move_cursor_to(target, false);
    }

    pub fn extend_line_start(&mut self) {
        let target = Position::new(self.cursor.line, 0);
        self.move_cursor_to(target, true);
    }

    pub fn move_line_end(&mut self) {
        let target = self.line_end(self.cursor.line);
        self.move_cursor_to(target, false);
    }

    pub fn extend_line_end(&mut self) {
        let target = self.line_end(self.cursor.line);
        self.move_cursor_to(target, true);
    }

    pub fn move_document_start(&mut self) {
        self.move_cursor_to(Position::new(0, 0), false);
    }

    pub fn extend_document_start(&mut self) {
        self.move_cursor_to(Position::new(0, 0), true);
    }

    pub fn move_document_end(&mut self) {
        let target = self.document_end();
        self.move_cursor_to(target, false);
    }

    pub fn extend_document_end(&mut self) {
        let target = self.document_end();
        self.move_cursor_to(target, true);
    }

    /// Select the entire document, anchored at the start.
    pub fn select_all(&mut self) {
        self.break_edit_group();
        self.anchor = Some(Position::new(0, 0));
        self.cursor = self.document_end();
        self.desired_column = self.cursor.column;
    }

    /// Move the cursor to `position` with no active selection, clamped to
    /// the document. Used by input handling that sets the cursor directly
    /// (mouse clicks, IME) rather than via a movement action.
    pub fn set_cursor(&mut self, position: Position) {
        self.break_edit_group();
        let clamped = self.clamp(position);
        self.cursor = clamped;
        self.anchor = None;
        self.desired_column = clamped.column;
    }

    /// Set the selection to span `anchor` to `cursor`, both clamped to the
    /// document. Used by input handling that sets a selection directly
    /// (shift-click, IME, OS-driven range replacement) rather than via an
    /// extend-movement action.
    ///
    /// Only breaks the active undo group if this actually repositions the
    /// caret or selection. OS text input re-asserts "the selection is right
    /// here, at the cursor" ahead of every keystroke it replaces, even when
    /// nothing has moved -- treating that as a break would defeat coalescing
    /// for ordinary typing, which flows through this same path.
    pub fn set_selection(&mut self, anchor: Position, cursor: Position) {
        let clamped_anchor = self.clamp(anchor);
        let clamped_cursor = self.clamp(cursor);
        let previous_anchor = self.anchor.unwrap_or(self.cursor);
        if previous_anchor != clamped_anchor || self.cursor != clamped_cursor {
            self.break_edit_group();
        }
        self.anchor = Some(clamped_anchor);
        self.cursor = clamped_cursor;
        self.desired_column = clamped_cursor.column;
    }

    // -- position <-> offset conversions --------------------------------

    /// The byte offset of `position`'s column within its own line. Lets a
    /// caller slice or index into that line's raw string -- for example,
    /// mapping a cursor position to a pixel x-offset via a shaped text line.
    #[must_use]
    pub fn line_byte_offset(&self, position: Position) -> usize {
        let line = position.line.min(self.lines.len() - 1);
        byte_offset(&self.lines[line], position.column)
    }

    /// The document-flat character offset of `position`, treating each line
    /// break as one character. Inverse of
    /// [`TextBuffer::position_for_char_offset`]. Used to translate between
    /// this buffer's line/column positions and the flat offsets an OS text
    /// input API deals in.
    #[must_use]
    pub fn char_offset_for_position(&self, position: Position) -> usize {
        let line = position.line.min(self.lines.len() - 1);
        let mut offset = 0;
        for earlier in &self.lines[..line] {
            offset += char_len(earlier) + 1; // +1 for the newline joining it to the next line
        }
        offset + position.column.min(char_len(&self.lines[line]))
    }

    /// The document position at flat character `offset` (see
    /// [`TextBuffer::char_offset_for_position`]), clamped to the document.
    #[must_use]
    pub fn position_for_char_offset(&self, offset: usize) -> Position {
        let mut remaining = offset;
        for (line, text) in self.lines.iter().enumerate() {
            let len = char_len(text);
            if remaining <= len {
                return Position::new(line, remaining);
            }
            remaining -= len + 1; // + 1 to also consume the newline after this line
        }
        self.document_end()
    }

    // -- editing ---------------------------------------------------------

    /// Insert `text` at the cursor, first deleting the active selection if
    /// there is one. `text` may itself contain `\n`, splitting across
    /// lines. A run of single-character calls coalesces into one undo group.
    pub fn insert_text(&mut self, text: &str) {
        let coalesces = !self.has_selection() && text != "\n" && text.chars().count() == 1;
        self.insert_text_impl(text, coalesces);
    }

    /// Insert `text` at the cursor as its own undo group, even if it is a
    /// single character. Used by paste and other bulk-insert entry points
    /// whose result must not merge with an adjacent run of typed characters.
    pub fn insert_pasted_text(&mut self, text: &str) {
        self.insert_text_impl(text, false);
    }

    fn insert_text_impl(&mut self, text: &str, coalesces: bool) {
        if text.is_empty() && !self.has_selection() {
            return;
        }
        self.begin_edit(EditKind::Insert, coalesces);

        if self.has_selection() {
            self.delete_selection();
        }

        let Position { line, column } = self.cursor;
        let current = self.lines[line].clone();
        let byte_col = byte_offset(&current, column);
        let before = &current[..byte_col];
        let after = &current[byte_col..];

        let pieces: Vec<&str> = text.split('\n').collect();
        let last = pieces.len() - 1;

        if last == 0 {
            self.lines[line] = format!("{before}{}{after}", pieces[0]);
            self.cursor = Position::new(line, column + char_len(pieces[0]));
        } else {
            self.lines[line] = format!("{before}{}", pieces[0]);
            for (offset, piece) in pieces[1..last].iter().enumerate() {
                self.lines.insert(line + 1 + offset, (*piece).to_owned());
            }
            let final_line = format!("{}{after}", pieces[last]);
            self.lines.insert(line + last, final_line);
            self.cursor = Position::new(line + last, char_len(pieces[last]));
        }

        self.anchor = None;
        self.desired_column = self.cursor.column;
    }

    /// Split the current line at the cursor, moving everything after it to
    /// a new line.
    pub fn insert_newline(&mut self) {
        self.insert_text("\n");
    }

    /// Delete the active selection, or the character before the cursor.
    /// Joins with the previous line if the cursor is at the start of a
    /// non-first line. A no-op at the document start pushes no undo entry.
    pub fn backspace(&mut self) {
        if !self.has_selection() && self.cursor == Position::new(0, 0) {
            return;
        }
        self.begin_edit(EditKind::Delete, !self.has_selection());

        if self.has_selection() {
            self.delete_selection();
            return;
        }

        let Position { line, column } = self.cursor;
        if column > 0 {
            let removed_start = byte_offset(&self.lines[line], column - 1);
            let removed_end = byte_offset(&self.lines[line], column);
            self.lines[line].replace_range(removed_start..removed_end, "");
            self.cursor.column = column - 1;
        } else if line > 0 {
            let removed = self.lines.remove(line);
            let prev_line = line - 1;
            let join_column = char_len(&self.lines[prev_line]);
            self.lines[prev_line].push_str(&removed);
            self.cursor = Position::new(prev_line, join_column);
        }

        self.anchor = None;
        self.desired_column = self.cursor.column;
    }

    /// Delete the active selection, or the character after the cursor.
    /// Joins with the next line if the cursor is at the end of a non-last
    /// line. A no-op at the document end pushes no undo entry.
    pub fn delete_forward(&mut self) {
        if !self.has_selection() && self.cursor == self.document_end() {
            return;
        }
        self.begin_edit(EditKind::Delete, !self.has_selection());

        if self.has_selection() {
            self.delete_selection();
            return;
        }

        let Position { line, column } = self.cursor;
        let len = char_len(&self.lines[line]);
        if column < len {
            let removed_start = byte_offset(&self.lines[line], column);
            let removed_end = byte_offset(&self.lines[line], column + 1);
            self.lines[line].replace_range(removed_start..removed_end, "");
        } else if line + 1 < self.lines.len() {
            let next = self.lines.remove(line + 1);
            self.lines[line].push_str(&next);
        }

        self.anchor = None;
        self.desired_column = self.cursor.column;
    }

    // -- undo/redo ---------------------------------------------------------

    /// Undo the most recent edit group, restoring both the text and the
    /// caret/selection exactly as they were immediately before that group's
    /// first edit. Returns `false` and leaves the buffer untouched if there
    /// is nothing to undo.
    pub fn undo(&mut self) -> bool {
        let Some(entry) = self.undo_stack.pop_back() else {
            return false;
        };
        self.redo_stack.push(self.snapshot());
        self.restore(entry);
        self.open_group = None;
        tracing::info!(remaining_undos = self.undo_stack.len(), "buffer undo");
        true
    }

    /// Redo the most recently undone edit group, restoring both the text
    /// and the caret/selection exactly as they were immediately after that
    /// group's last edit. Returns `false` and leaves the buffer untouched
    /// if there is nothing to redo.
    pub fn redo(&mut self) -> bool {
        let Some(entry) = self.redo_stack.pop() else {
            return false;
        };
        self.push_undo_entry(self.snapshot());
        self.restore(entry);
        self.open_group = None;
        tracing::info!(remaining_redos = self.redo_stack.len(), "buffer redo");
        true
    }

    fn snapshot(&self) -> HistoryEntry {
        HistoryEntry {
            lines: self.lines.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
            desired_column: self.desired_column,
        }
    }

    fn restore(&mut self, entry: HistoryEntry) {
        self.lines = entry.lines;
        self.cursor = entry.cursor;
        self.anchor = entry.anchor;
        self.desired_column = entry.desired_column;
    }

    /// Push `entry` as the most recent undo step, evicting the oldest step
    /// if that would exceed [`theme::EDITOR_HISTORY_CAP`].
    fn push_undo_entry(&mut self, entry: HistoryEntry) {
        self.undo_stack.push_back(entry);
        if self.undo_stack.len() > theme::EDITOR_HISTORY_CAP {
            self.undo_stack.pop_front();
        }
    }

    /// Record undo history for an about-to-happen edit of `kind`, before any
    /// mutation. Continues the currently open group -- rather than starting
    /// a new one -- when `coalesces` is set and the open group is itself
    /// still open to coalescing and of the same kind, e.g. a run of
    /// single-character insertions. Any edit clears the redo stack: it
    /// discards whatever branch of history redo pointed at.
    fn begin_edit(&mut self, kind: EditKind, coalesces: bool) {
        self.redo_stack.clear();
        let continues_open_group = coalesces
            && self
                .open_group
                .as_ref()
                .is_some_and(|group| group.coalescing && group.kind == kind);
        if continues_open_group {
            return;
        }
        let before = self.snapshot();
        self.push_undo_entry(before);
        self.open_group = Some(OpenGroup {
            kind,
            coalescing: coalesces,
        });
    }

    /// Break the currently open undo group, if any, so the next edit starts
    /// a fresh one instead of coalescing into it. Called by every operation
    /// that moves the caret or selection independent of an edit.
    fn break_edit_group(&mut self) {
        self.open_group = None;
    }

    // -- internal helpers --------------------------------------------------

    fn line_end(&self, line: usize) -> Position {
        Position::new(line, char_len(&self.lines[line]))
    }

    fn document_end(&self) -> Position {
        let last = self.lines.len() - 1;
        self.line_end(last)
    }

    fn clamp(&self, pos: Position) -> Position {
        let line = pos.line.min(self.lines.len() - 1);
        let column = pos.column.min(char_len(&self.lines[line]));
        Position::new(line, column)
    }

    fn left_of(&self, pos: Position) -> Position {
        if pos.column > 0 {
            Position::new(pos.line, pos.column - 1)
        } else if pos.line > 0 {
            self.line_end(pos.line - 1)
        } else {
            pos
        }
    }

    fn right_of(&self, pos: Position) -> Position {
        let len = char_len(&self.lines[pos.line]);
        if pos.column < len {
            Position::new(pos.line, pos.column + 1)
        } else if pos.line + 1 < self.lines.len() {
            Position::new(pos.line + 1, 0)
        } else {
            pos
        }
    }

    /// One line up, at `desired_column` clamped to that line's length, or
    /// unchanged if already on the first line.
    fn above(&self, pos: Position) -> Position {
        if pos.line == 0 {
            return pos;
        }
        let target_line = pos.line - 1;
        Position::new(
            target_line,
            self.desired_column.min(char_len(&self.lines[target_line])),
        )
    }

    /// One line down, at `desired_column` clamped to that line's length, or
    /// unchanged if already on the last line.
    fn below(&self, pos: Position) -> Position {
        if pos.line + 1 >= self.lines.len() {
            return pos;
        }
        let target_line = pos.line + 1;
        Position::new(
            target_line,
            self.desired_column.min(char_len(&self.lines[target_line])),
        )
    }

    /// Move the cursor to `target` (clamped to the document), updating the
    /// selection anchor per `extend`, and resync `desired_column` to the
    /// resulting column. Used by every movement except up/down, which must
    /// preserve `desired_column` across repeated calls.
    fn move_cursor_to(&mut self, target: Position, extend: bool) {
        self.move_cursor_to_preserving_desired_column(target, extend);
        self.desired_column = self.cursor.column;
    }

    fn move_cursor_to_preserving_desired_column(&mut self, target: Position, extend: bool) {
        self.break_edit_group();
        let target = self.clamp(target);
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        self.cursor = target;
    }

    fn text_in_range(&self, start: Position, end: Position) -> String {
        if start.line == end.line {
            return substring(&self.lines[start.line], start.column, end.column);
        }

        let mut out = String::new();
        out.push_str(&substring(
            &self.lines[start.line],
            start.column,
            char_len(&self.lines[start.line]),
        ));
        out.push('\n');
        for line in &self.lines[start.line + 1..end.line] {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(&substring(&self.lines[end.line], 0, end.column));
        out
    }

    /// Remove the active selection's text, collapsing the cursor to its
    /// start and clearing the selection. No-op if nothing is selected.
    fn delete_selection(&mut self) {
        let Some(selection) = self.selection() else {
            return;
        };
        let (start, end) = selection.ordered();

        if start.line == end.line {
            let s = byte_offset(&self.lines[start.line], start.column);
            let e = byte_offset(&self.lines[start.line], end.column);
            self.lines[start.line].replace_range(s..e, "");
        } else {
            let end_line = self.lines.remove(end.line);
            let end_byte = byte_offset(&end_line, end.column);
            let tail = end_line[end_byte..].to_owned();

            for _ in (start.line + 1)..end.line {
                self.lines.remove(start.line + 1);
            }

            let start_byte = byte_offset(&self.lines[start.line], start.column);
            self.lines[start.line].truncate(start_byte);
            self.lines[start.line].push_str(&tail);
        }

        self.cursor = start;
        self.anchor = None;
        self.desired_column = self.cursor.column;
    }
}

fn char_len(line: &str) -> usize {
    line.chars().count()
}

/// The byte offset of character index `column` in `line`, or `line.len()`
/// if `column` is at or past the end. Used to translate a char-indexed
/// [`Position`] into a byte range for `str` slicing/mutation without ever
/// splitting a multi-byte character.
fn byte_offset(line: &str, column: usize) -> usize {
    line.char_indices()
        .nth(column)
        .map_or(line.len(), |(idx, _)| idx)
}

fn substring(line: &str, start_col: usize, end_col: usize) -> String {
    let start = byte_offset(line, start_col);
    let end = byte_offset(line, end_col);
    line[start..end].to_owned()
}

#[cfg(test)]
mod tests {
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
}
