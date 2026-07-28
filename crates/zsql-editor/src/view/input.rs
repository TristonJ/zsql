//! OS text/IME input plumbing for [`EditorView`]: the `EntityInputHandler`
//! implementation `gpui` drives keyboard and IME input through, plus the
//! UTF-16 code-unit <-> flat char-offset conversions it needs since
//! `EntityInputHandler` counts in UTF-16 while `TextBuffer` counts in chars.

use std::ops::Range;

use gpui::{Bounds, Context, EntityInputHandler, Pixels, Point, UTF16Selection, Window, point, px};

use super::EditorView;
use super::element::line_top;
use crate::theme;

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.buffer.text();
        let char_range = char_range_from_utf16(&text, range_utf16);
        actual_range.replace(char_range_to_utf16(&text, char_range.clone()));
        Some(
            text.chars()
                .skip(char_range.start)
                .take(char_range.len())
                .collect(),
        )
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let text = self.buffer.text();
        let cursor_offset = self.buffer.char_offset_for_position(self.buffer.cursor());
        let (start, end, reversed) = match self.buffer.selection() {
            Some(selection) => {
                let (start_pos, end_pos) = selection.ordered();
                let start = self.buffer.char_offset_for_position(start_pos);
                let end = self.buffer.char_offset_for_position(end_pos);
                (start, end, selection.cursor == start_pos)
            }
            None => (cursor_offset, cursor_offset, false),
        };
        Some(UTF16Selection {
            range: char_range_to_utf16(&text, start..end),
            reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let text = self.buffer.text();
        self.marked_range
            .clone()
            .map(|range| char_range_to_utf16(&text, range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let char_range = self.resolve_replace_range(range_utf16);
        let start_pos = self.buffer.position_for_char_offset(char_range.start);
        let end_pos = self.buffer.position_for_char_offset(char_range.end);
        self.buffer.set_selection(start_pos, end_pos);
        self.buffer.insert_text(new_text);
        self.marked_range = None;
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
        let char_range = self.resolve_replace_range(range_utf16);
        let start_pos = self.buffer.position_for_char_offset(char_range.start);
        let end_pos = self.buffer.position_for_char_offset(char_range.end);
        self.buffer.set_selection(start_pos, end_pos);
        self.buffer.insert_text(new_text);

        let inserted_start = char_range.start;
        let inserted_len = new_text.chars().count();
        self.marked_range = if new_text.is_empty() {
            None
        } else {
            Some(inserted_start..inserted_start + inserted_len)
        };

        if let Some(relative_utf16) = new_selected_range_utf16 {
            // `new_selected_range_utf16` is UTF-16-relative to `new_text` itself
            // (NSTextInputClient's `setMarkedText:selectedRange:` semantics), not
            // to the document as a whole, so it must be resolved against
            // `new_text` before adding `inserted_start`.
            let relative = char_range_from_utf16(new_text, relative_utf16);
            let selection_start = self
                .buffer
                .position_for_char_offset(inserted_start + relative.start);
            let selection_end = self
                .buffer
                .position_for_char_offset(inserted_start + relative.end);
            self.buffer.set_selection(selection_start, selection_end);
        }

        self.notify_edit(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let text = self.buffer.text();
        let char_range = char_range_from_utf16(&text, range_utf16);
        let start = self.buffer.position_for_char_offset(char_range.start);
        let end = self.buffer.position_for_char_offset(char_range.end);
        if start.line != end.line {
            // A marked/queried range spanning multiple lines is rare for SQL
            // IME input; keep the OS-facing geometry simple and decline it
            // rather than paint a misleading single-line box.
            return None;
        }

        let line = self.last_lines.get(start.line)?;
        let line_height = px(theme::EDITOR_LINE_HEIGHT);
        let top = line_top(element_bounds.top(), line_height, start.line);
        Some(Bounds::from_corners(
            point(
                element_bounds.left() + line.x_for_index(self.buffer.line_byte_offset(start)),
                top,
            ),
            point(
                element_bounds.left() + line.x_for_index(self.buffer.line_byte_offset(end)),
                top + line_height,
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let position = self.position_for_point(point)?;
        let text = self.buffer.text();
        let char_offset = self.buffer.char_offset_for_position(position);
        Some(char_offset_to_utf16(&text, char_offset))
    }
}

impl EditorView {
    /// The flat char range a `replace_text_in_range`-style call should
    /// operate on: the given UTF-16 range if present, else the active IME
    /// composition range, else the current selection (collapsed to the
    /// cursor if there is none).
    fn resolve_replace_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        if let Some(range_utf16) = range_utf16 {
            let text = self.buffer.text();
            return char_range_from_utf16(&text, range_utf16);
        }
        if let Some(marked) = self.marked_range.clone() {
            return marked;
        }
        let cursor = self.buffer.char_offset_for_position(self.buffer.cursor());
        match self.buffer.selection() {
            Some(selection) => {
                let (start, end) = selection.ordered();
                self.buffer.char_offset_for_position(start)
                    ..self.buffer.char_offset_for_position(end)
            }
            None => cursor..cursor,
        }
    }
}

// -- UTF-16 <-> flat char offset conversions --------------------------------
//
// `EntityInputHandler` deals in UTF-16 code unit offsets into "the text" the
// OS was told about, which here is the whole document (`buffer.text()`).
// `TextBuffer` itself only knows char offsets, so these free functions
// bridge the two at the gpui boundary -- they stay out of the buffer module
// so it has no reason to know what an OS text input API counts in.

fn char_offset_from_utf16(text: &str, offset_utf16: usize) -> usize {
    let mut utf16_count = 0;
    for (char_index, ch) in text.chars().enumerate() {
        if utf16_count >= offset_utf16 {
            return char_index;
        }
        utf16_count += ch.len_utf16();
    }
    text.chars().count()
}

fn char_offset_to_utf16(text: &str, offset: usize) -> usize {
    text.chars().take(offset).map(char::len_utf16).sum()
}

fn char_range_from_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    char_offset_from_utf16(text, range.start)..char_offset_from_utf16(text, range.end)
}

fn char_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    char_offset_to_utf16(text, range.start)..char_offset_to_utf16(text, range.end)
}
