//! OS text/IME input plumbing for [`EditorView`]: an implementation of
//! `zsql_ui::text_input::TextSource` over [`crate::TextBuffer`], and the
//! `EntityInputHandler` impl that routes through `zsql_ui::text_input`'s
//! shared plumbing.
//!
//! `TextSource` offsets are flat byte offsets into `buffer.text()`, the
//! multi-line document joined by `\n`; `TextBuffer`'s own line/column
//! `Position` is char-indexed within each line. This module is the
//! translation boundary between the two: [`EditorView::position_for_offset`]
//! and [`EditorView::offset_for_position`] convert one way,
//! [`EditorView`]'s own `marked_range` field (kept char-indexed so
//! [`super::element::build_runs`]'s per-line highlighter-span math never has
//! to reason about bytes) converts the other via
//! [`char_offset_to_byte`]/[`byte_offset_to_char`].

use std::ops::Range;

use gpui::{Bounds, Context, EntityInputHandler, Pixels, Point, UTF16Selection, Window, px};
use zsql_ui::text_input::{self, TextSource};

use super::EditorView;
use crate::Position;
use crate::theme;

impl TextSource for EditorView {
    fn text(&self) -> String {
        self.buffer.text()
    }

    fn cursor_offset(&self) -> usize {
        self.offset_for_position(self.buffer.cursor())
    }

    fn selection_range(&self) -> Option<Range<usize>> {
        let selection = self.buffer.selection()?;
        let (start, end) = selection.ordered();
        Some(self.offset_for_position(start)..self.offset_for_position(end))
    }

    fn selection_reversed(&self) -> bool {
        self.buffer
            .selection()
            .is_some_and(|selection| selection.anchor > selection.cursor)
    }

    fn set_selection(&mut self, anchor: usize, cursor: usize) {
        let anchor_pos = self.position_for_offset(anchor);
        let cursor_pos = self.position_for_offset(cursor);
        self.buffer.set_selection(anchor_pos, cursor_pos);
    }

    fn marked_range(&self) -> Option<Range<usize>> {
        let text = self.buffer.text();
        self.marked_range.clone().map(|char_range| {
            char_offset_to_byte(&text, char_range.start)..char_offset_to_byte(&text, char_range.end)
        })
    }

    fn set_marked_range(&mut self, range: Option<Range<usize>>) {
        let text = self.buffer.text();
        self.marked_range = range.map(|byte_range| {
            byte_offset_to_char(&text, byte_range.start)..byte_offset_to_char(&text, byte_range.end)
        });
    }

    fn replace_range(&mut self, range: Range<usize>, text: &str) {
        let start_pos = self.position_for_offset(range.start);
        let end_pos = self.position_for_offset(range.end);
        self.buffer.set_selection(start_pos, end_pos);
        self.buffer.insert_text(text);
    }

    fn line_position(&self, offset: usize) -> (usize, usize) {
        let mut remaining = offset;
        for (line_index, line) in self.buffer.lines().iter().enumerate() {
            let len = line.len();
            if remaining <= len {
                return (line_index, remaining);
            }
            remaining -= len + 1; // +1 for the newline joining it to the next line
        }
        let last = self.buffer.lines().len() - 1;
        (last, self.buffer.lines()[last].len())
    }

    fn offset_for_line_position(&self, line: usize, in_line_offset: usize) -> usize {
        let line = line.min(self.buffer.lines().len() - 1);
        let mut offset = 0;
        for earlier in &self.buffer.lines()[..line] {
            offset += earlier.len() + 1;
        }
        offset + in_line_offset.min(self.buffer.lines()[line].len())
    }
}

impl EditorView {
    /// The document position at flat byte `offset` (see
    /// [`TextSource::line_position`]), clamped to the document.
    fn position_for_offset(&self, offset: usize) -> Position {
        let (line, in_line_byte) = self.line_position(offset);
        let column = self.buffer.lines()[line][..in_line_byte].chars().count();
        Position::new(line, column)
    }

    /// The flat byte offset of `position`. Inverse of
    /// [`EditorView::position_for_offset`].
    fn offset_for_position(&self, position: Position) -> usize {
        let in_line_byte = self.buffer.line_byte_offset(position);
        self.offset_for_line_position(position.line, in_line_byte)
    }
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let (text, actual) = text_input::text_for_range(self, range_utf16);
        actual_range.replace(actual);
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(text_input::selected_text_range(self))
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        text_input::marked_text_range(self)
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        text_input::unmark_text(self);
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Only a real IME composition finishing is notable enough to log --
        // this same path also runs on every plain typed keystroke, which
        // would otherwise flood a debug trace.
        if self.marked_range.is_some() {
            tracing::debug!(
                chars = new_text.chars().count(),
                "editor committing ime composition"
            );
        }
        text_input::replace_text_in_range(self, range_utf16, new_text);
        self.notify_edit(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        text_input::replace_and_mark_text_in_range(
            self,
            range_utf16,
            new_text,
            new_selected_range_utf16,
        );
        self.notify_edit(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        text_input::bounds_for_range(
            self,
            range_utf16,
            element_bounds,
            &self.last_lines,
            px(theme::EDITOR_LINE_HEIGHT),
        )
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let position = self.position_for_point(point)?;
        let offset = self.offset_for_position(position);
        Some(text_input::character_index_for_point(self, offset))
    }
}

/// The byte offset of the `char_offset`-th character of `text`, clamped to
/// `text`'s length.
fn char_offset_to_byte(text: &str, char_offset: usize) -> usize {
    text.char_indices()
        .nth(char_offset)
        .map_or(text.len(), |(byte_idx, _)| byte_idx)
}

/// The number of characters in `text` before byte offset `byte_offset`.
/// Inverse of [`char_offset_to_byte`].
fn byte_offset_to_char(text: &str, byte_offset: usize) -> usize {
    text[..byte_offset].chars().count()
}
