//! Shared, view-agnostic machinery behind `gpui`'s `EntityInputHandler` for
//! a text-editing surface: UTF-16 code-unit offset conversion, marked-range
//! (IME composition) resolution, the `EntityInputHandler` method bodies
//! themselves, and pure caret/selection paint-quad geometry. Parameterized
//! over the [`TextSource`] trait so it works for both a multi-line document
//! buffer and a single-line field model without either one adopting the
//! other's storage or undo semantics.
//!
//! All offsets this module deals in are byte offsets into
//! [`TextSource::text`], on `char` boundaries. A [`TextSource`] whose
//! content is itself char-indexed or line/column-indexed converts at its
//! own trait boundary; this module never assumes anything about the
//! internal representation.

mod geometry;
mod handler;
mod utf16;

pub use geometry::{SelectionLineSpan, caret_quad, line_top, selection_quad, selection_quads};
pub use handler::{
    bounds_for_range, character_index_for_point, marked_text_range, replace_and_mark_text_in_range,
    replace_text_in_range, resolve_replace_range, selected_text_range, text_for_range, unmark_text,
};
pub use utf16::{
    byte_offset_from_utf16, byte_offset_to_utf16, byte_range_from_utf16, byte_range_to_utf16,
};

use std::ops::Range;

/// The text-editing surface the shared `EntityInputHandler` plumbing in this
/// module operates on: text access, cursor/selection, IME marked-range
/// state, edit application, and byte-offset <-> line-position mapping.
///
/// Every offset here is a byte offset into [`TextSource::text`], expected to
/// fall on a `char` boundary. A single-line source has exactly one line
/// (line index `0`); a multi-line source maps `line_position`/
/// `offset_for_line_position` across its own line breaks.
pub trait TextSource {
    /// The full text this source's offsets index into.
    fn text(&self) -> String;

    /// The cursor's flat byte offset into [`TextSource::text`].
    fn cursor_offset(&self) -> usize;

    /// The active selection as an ordered byte range (`start <= end`), or
    /// `None` if nothing is selected.
    fn selection_range(&self) -> Option<Range<usize>>;

    /// Whether the active selection's anchor comes after its cursor, i.e.
    /// the selection was built backward from where it now ends.
    fn selection_reversed(&self) -> bool;

    /// Set the selection to span `anchor` to `cursor`, both flat byte
    /// offsets into [`TextSource::text`].
    fn set_selection(&mut self, anchor: usize, cursor: usize);

    /// The active IME composition range, as a flat byte range, or `None`
    /// while no composition is in progress.
    fn marked_range(&self) -> Option<Range<usize>>;

    /// Replace the active IME composition range.
    fn set_marked_range(&mut self, range: Option<Range<usize>>);

    /// Replace `range` (byte offsets, expected to fall on `char`
    /// boundaries) with `text`, moving the cursor to just after the
    /// inserted text and clearing any selection.
    fn replace_range(&mut self, range: Range<usize>, text: &str);

    /// The line index and in-line byte offset that flat byte `offset` falls
    /// on.
    fn line_position(&self, offset: usize) -> (usize, usize);

    /// The flat byte offset for a `(line, in_line_offset)` pair. Inverse of
    /// [`TextSource::line_position`].
    fn offset_for_line_position(&self, line: usize, in_line_offset: usize) -> usize;
}
