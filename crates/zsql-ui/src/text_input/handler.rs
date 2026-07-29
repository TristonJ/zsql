//! `EntityInputHandler` method plumbing, generic over [`TextSource`]. Each
//! consumer's own `EntityInputHandler` impl calls straight into these free
//! functions and only adds what genuinely differs per view: notifying
//! observers, and the pixel geometry (`lines`, `line_height`) a paint pass
//! already computed.

use std::ops::Range;

use gpui::{Bounds, Pixels, ShapedLine, UTF16Selection, point};

use super::TextSource;
use super::geometry::line_top;
use super::utf16::{byte_offset_to_utf16, byte_range_from_utf16, byte_range_to_utf16};

/// The flat byte range a `replace_text_in_range`-style call should operate
/// on: the given UTF-16 range if present, else the active IME composition
/// range, else the current selection (collapsed to the cursor if there is
/// none).
pub fn resolve_replace_range<T: TextSource + ?Sized>(
    source: &T,
    range_utf16: Option<Range<usize>>,
) -> Range<usize> {
    if let Some(range_utf16) = range_utf16 {
        let text = source.text();
        return byte_range_from_utf16(&text, range_utf16);
    }
    if let Some(marked) = source.marked_range() {
        return marked;
    }
    let cursor = source.cursor_offset();
    source.selection_range().unwrap_or(cursor..cursor)
}

/// The text sliced from `source` at UTF-16 `range_utf16`, and the UTF-16
/// range it actually resolved to (identical unless `range_utf16` reached
/// past the text).
pub fn text_for_range<T: TextSource + ?Sized>(
    source: &T,
    range_utf16: Range<usize>,
) -> (String, Range<usize>) {
    let text = source.text();
    let byte_range = byte_range_from_utf16(&text, range_utf16);
    let actual = byte_range_to_utf16(&text, byte_range.clone());
    (text[byte_range].to_owned(), actual)
}

/// The active selection (or the collapsed cursor, if none) as a
/// UTF-16-ranged [`UTF16Selection`].
pub fn selected_text_range<T: TextSource + ?Sized>(source: &T) -> UTF16Selection {
    let text = source.text();
    let cursor = source.cursor_offset();
    let range = source.selection_range().unwrap_or(cursor..cursor);
    UTF16Selection {
        range: byte_range_to_utf16(&text, range),
        reversed: source.selection_reversed(),
    }
}

/// The active IME composition range, in UTF-16 units, or `None`.
pub fn marked_text_range<T: TextSource + ?Sized>(source: &T) -> Option<Range<usize>> {
    let text = source.text();
    source
        .marked_range()
        .map(|range| byte_range_to_utf16(&text, range))
}

/// Clear the active IME composition, if any.
pub fn unmark_text<T: TextSource + ?Sized>(source: &mut T) {
    source.set_marked_range(None);
}

/// Replace [`resolve_replace_range`]'s target with `new_text` and clear the
/// IME composition range. The caller still owns notifying its own
/// observers (`cx.notify`, an edit listener, a keystroke-blink reset).
pub fn replace_text_in_range<T: TextSource + ?Sized>(
    source: &mut T,
    range_utf16: Option<Range<usize>>,
    new_text: &str,
) {
    let range = resolve_replace_range(source, range_utf16);
    source.replace_range(range, new_text);
    source.set_marked_range(None);
}

/// Replace [`resolve_replace_range`]'s target with `new_text`, mark the
/// inserted text as the active IME composition (or clear it, if `new_text`
/// is empty), and apply `new_selected_range_utf16` if given.
pub fn replace_and_mark_text_in_range<T: TextSource + ?Sized>(
    source: &mut T,
    range_utf16: Option<Range<usize>>,
    new_text: &str,
    new_selected_range_utf16: Option<Range<usize>>,
) {
    let range = resolve_replace_range(source, range_utf16);
    let inserted_start = range.start;
    source.replace_range(range, new_text);
    let inserted_len = source.cursor_offset().saturating_sub(inserted_start);

    source.set_marked_range(if inserted_len == 0 {
        None
    } else {
        Some(inserted_start..inserted_start + inserted_len)
    });

    if let Some(relative_utf16) = new_selected_range_utf16 {
        // `new_selected_range_utf16` is UTF-16-relative to `new_text` itself
        // (NSTextInputClient's `setMarkedText:selectedRange:` semantics),
        // not to the whole source, so it must be resolved against
        // `new_text` before adding `inserted_start`.
        let relative = byte_range_from_utf16(new_text, relative_utf16);
        let selection_start = inserted_start + relative.start;
        let selection_end = inserted_start + relative.end;
        source.set_selection(selection_start, selection_end);
    }
}

/// The pixel bounds of UTF-16 `range_utf16`, using `lines` from the most
/// recent paint. `None` if `range_utf16` spans more than one line (a
/// caller-facing simplification kept intentionally: an OS-facing marked/
/// queried range spanning multiple lines is rare, and declining it is
/// simpler than painting a misleading single-line box) or if the range's
/// line has not been painted yet.
pub fn bounds_for_range<T: TextSource + ?Sized>(
    source: &T,
    range_utf16: Range<usize>,
    element_bounds: Bounds<Pixels>,
    lines: &[ShapedLine],
    line_height: Pixels,
) -> Option<Bounds<Pixels>> {
    let text = source.text();
    let byte_range = byte_range_from_utf16(&text, range_utf16);
    let (start_line, start_col) = source.line_position(byte_range.start);
    let (end_line, end_col) = source.line_position(byte_range.end);
    if start_line != end_line {
        return None;
    }

    let line = lines.get(start_line)?;
    let top = line_top(element_bounds.top(), line_height, start_line);
    Some(Bounds::from_corners(
        point(element_bounds.left() + line.x_for_index(start_col), top),
        point(
            element_bounds.left() + line.x_for_index(end_col),
            top + line_height,
        ),
    ))
}

/// The UTF-16 index for a flat byte `offset` a consumer's own pixel-point
/// hit test resolved. Hit-testing itself stays per-consumer (it needs the
/// paint state -- shaped lines and bounds -- that this module deliberately
/// has no access to); this is just the UTF-16 conversion at the end of it.
#[must_use]
pub fn character_index_for_point<T: TextSource + ?Sized>(source: &T, offset: usize) -> usize {
    let text = source.text();
    byte_offset_to_utf16(&text, offset)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::ops::Range;

    use gpui::{Bounds, point, px, size};

    use super::{
        TextSource, bounds_for_range, character_index_for_point, marked_text_range,
        replace_and_mark_text_in_range, replace_text_in_range, resolve_replace_range,
        selected_text_range, text_for_range, unmark_text,
    };

    /// A minimal, multi-line `TextSource` test double: lines joined by
    /// `\n`, cursor/anchor as flat byte offsets. Exercises the generic
    /// plumbing's multi-line `line_position` path the way a document buffer
    /// would, without pulling in `zsql-editor`.
    struct FakeSource {
        lines: Vec<String>,
        cursor: usize,
        anchor: Option<usize>,
        marked: RefCell<Option<Range<usize>>>,
    }

    impl FakeSource {
        fn new(text: &str) -> Self {
            Self {
                lines: text.split('\n').map(str::to_owned).collect(),
                cursor: 0,
                anchor: None,
                marked: RefCell::new(None),
            }
        }
    }

    impl TextSource for FakeSource {
        fn text(&self) -> String {
            self.lines.join("\n")
        }

        fn cursor_offset(&self) -> usize {
            self.cursor
        }

        fn selection_range(&self) -> Option<Range<usize>> {
            let anchor = self.anchor?;
            Some(anchor.min(self.cursor)..anchor.max(self.cursor))
        }

        fn selection_reversed(&self) -> bool {
            matches!(self.anchor, Some(anchor) if anchor > self.cursor)
        }

        fn set_selection(&mut self, anchor: usize, cursor: usize) {
            self.anchor = Some(anchor);
            self.cursor = cursor;
        }

        fn marked_range(&self) -> Option<Range<usize>> {
            self.marked.borrow().clone()
        }

        fn set_marked_range(&mut self, range: Option<Range<usize>>) {
            *self.marked.borrow_mut() = range;
        }

        fn replace_range(&mut self, range: Range<usize>, text: &str) {
            let mut flat = self.text();
            flat.replace_range(range.clone(), text);
            self.lines = flat.split('\n').map(str::to_owned).collect();
            self.cursor = range.start + text.len();
            self.anchor = None;
        }

        fn line_position(&self, offset: usize) -> (usize, usize) {
            let mut remaining = offset;
            for (index, line) in self.lines.iter().enumerate() {
                let len = line.len();
                if remaining <= len {
                    return (index, remaining);
                }
                remaining -= len + 1;
            }
            let last = self.lines.len() - 1;
            (last, self.lines[last].len())
        }

        fn offset_for_line_position(&self, line: usize, in_line_offset: usize) -> usize {
            let line = line.min(self.lines.len() - 1);
            let mut offset = 0;
            for earlier in &self.lines[..line] {
                offset += earlier.len() + 1;
            }
            offset + in_line_offset.min(self.lines[line].len())
        }
    }

    #[test]
    fn replace_range_resolution_prefers_the_explicit_utf16_range() {
        let mut source = FakeSource::new("hello world");
        source.set_selection(0, 5);
        assert_eq!(resolve_replace_range(&source, Some(1..3)), 1..3);
    }

    #[test]
    fn replace_range_resolution_falls_back_to_the_marked_range() {
        let mut source = FakeSource::new("hello world");
        source.set_marked_range(Some(2..4));
        assert_eq!(resolve_replace_range(&source, None), 2..4);
    }

    #[test]
    fn replace_range_resolution_falls_back_to_the_selection_then_the_collapsed_cursor() {
        let mut source = FakeSource::new("hello world");
        source.set_selection(1, 4);
        assert_eq!(resolve_replace_range(&source, None), 1..4);

        source.set_selection(6, 6); // collapses: anchor == cursor
        assert_eq!(resolve_replace_range(&source, None), 6..6);
    }

    #[test]
    fn text_for_range_slices_by_utf16_offset_and_reports_the_actual_range() {
        let source = FakeSource::new("a\u{1F600}b");
        let (text, actual) = text_for_range(&source, 1..3);
        assert_eq!(text, "\u{1F600}");
        assert_eq!(actual, 1..3);
    }

    #[test]
    fn selected_text_range_reports_reversed_when_the_anchor_is_after_the_cursor() {
        let mut source = FakeSource::new("hello world");
        source.set_selection(5, 1);
        let selection = selected_text_range(&source);
        assert_eq!(selection.range, 1..5);
        assert!(selection.reversed);
    }

    #[test]
    fn selected_text_range_with_no_selection_collapses_to_the_cursor() {
        let mut source = FakeSource::new("hello");
        source.cursor = 3;
        let selection = selected_text_range(&source);
        assert_eq!(selection.range, 3..3);
        assert!(!selection.reversed);
    }

    #[test]
    fn marked_text_range_and_unmark_text_round_trip() {
        let mut source = FakeSource::new("hello");
        source.set_marked_range(Some(1..3));
        assert_eq!(marked_text_range(&source), Some(1..3));
        unmark_text(&mut source);
        assert_eq!(marked_text_range(&source), None);
    }

    #[test]
    fn replace_text_in_range_replaces_and_clears_the_marked_range() {
        let mut source = FakeSource::new("hello world");
        source.set_marked_range(Some(0..5));
        replace_text_in_range(&mut source, None, "goodbye");
        assert_eq!(source.text(), "goodbye world");
        assert_eq!(source.marked_range(), None);
    }

    #[test]
    fn replace_and_mark_text_in_range_marks_the_inserted_text() {
        let mut source = FakeSource::new("select ");
        source.cursor = 7;
        replace_and_mark_text_in_range(&mut source, None, "n", Some(1..1));
        assert_eq!(source.text(), "select n");
        assert_eq!(source.marked_range(), Some(7..8));
        assert_eq!(
            source.cursor_offset(),
            8,
            "the proposed selection follows the composed text"
        );
    }

    #[test]
    fn replace_and_mark_text_in_range_with_empty_text_clears_the_marked_range() {
        let mut source = FakeSource::new("select n");
        source.set_marked_range(Some(7..8));
        replace_and_mark_text_in_range(&mut source, Some(7..8), "", None);
        assert_eq!(source.text(), "select ");
        assert_eq!(source.marked_range(), None);
    }

    #[test]
    fn replace_and_mark_resolves_the_selected_range_against_the_inserted_text_not_the_document() {
        // A leading astral character (two UTF-16 code units, one char) is
        // exactly the case where resolving `new_selected_range_utf16`
        // against the whole document -- instead of against `new_text`
        // alone -- misaligns the UTF-16 count.
        let mut source = FakeSource::new("\u{1F600}");
        source.cursor = 4; // past the 4-byte emoji
        replace_and_mark_text_in_range(&mut source, None, "ab", Some(1..2));
        assert_eq!(source.text(), "\u{1F600}ab");
        assert_eq!(
            source.selection_range(),
            Some(5..6),
            "selects just the 'b' following the emoji"
        );
    }

    #[test]
    fn bounds_for_range_declines_a_range_spanning_two_lines() {
        // A `ShapedLine` can only be built through a real `gpui` text
        // system, unavailable to a plain unit test; the decline check runs
        // before this function ever indexes into `lines`, so an empty slice
        // is enough to pin it without one. The single-line pixel-math path
        // is already covered end to end by each consumer's own
        // `bounds_for_range` test, which paints a real window first.
        let source = FakeSource::new("select\nfrom");
        let element_bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(200.0), px(100.0)));
        assert!(
            bounds_for_range(&source, 0..8, element_bounds, &[], px(20.0)).is_none(),
            "a range spanning two lines declines geometry"
        );
    }

    #[test]
    fn character_index_for_point_converts_the_offset_to_utf16() {
        let source = FakeSource::new("a\u{1F600}b");
        assert_eq!(character_index_for_point(&source, 5), 3);
    }
}
