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

    /// Select an entire line, from its start to its end, regardless of where
    /// on that line the caller's originating click landed. `line` clamps to
    /// the document's last line index.
    pub fn select_line(&mut self, line: usize) {
        self.break_edit_group();
        let line = line.min(self.lines.len() - 1);
        self.anchor = Some(Position::new(line, 0));
        self.cursor = self.line_end(line);
        self.desired_column = self.cursor.column;
    }

    /// Select the run of same-class characters touching `at`, e.g. the word
    /// under a double-click. A run of alphanumeric/underscore characters is
    /// one class, a run of whitespace is another, and every other character
    /// forms a run of its own class with its punctuation neighbors. `at`
    /// clamps to the document as [`TextBuffer::set_cursor`] does; a clamped
    /// column at or past its line's end (including an empty line) selects
    /// nothing, placing the cursor there instead.
    pub fn select_word(&mut self, at: Position) {
        self.break_edit_group();
        let clamped = self.clamp(at);
        let chars: Vec<char> = self.lines[clamped.line].chars().collect();

        let Some(&clicked) = chars.get(clamped.column) else {
            self.cursor = clamped;
            self.anchor = None;
            self.desired_column = clamped.column;
            return;
        };

        let class = CharClass::of(clicked);
        let mut start = clamped.column;
        while start > 0 && CharClass::of(chars[start - 1]) == class {
            start -= 1;
        }
        let mut end = clamped.column;
        while end < chars.len() && CharClass::of(chars[end]) == class {
            end += 1;
        }

        self.anchor = Some(Position::new(clamped.line, start));
        self.cursor = Position::new(clamped.line, end);
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

/// The class of character a double-click word-selection expands over: a run
/// of the same class is treated as one "word" to select.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Whitespace,
    Punctuation,
}

impl CharClass {
    fn of(ch: char) -> Self {
        if ch.is_alphanumeric() || ch == '_' {
            Self::Word
        } else if ch.is_whitespace() {
            Self::Whitespace
        } else {
            Self::Punctuation
        }
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
mod tests;
