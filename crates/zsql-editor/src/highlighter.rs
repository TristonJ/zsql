//! Syntax-highlighting seam for the editor. `Highlighter` maps a line of
//! text to the style spans it should be painted with; `PlainHighlighter` is
//! a no-op implementation. `TextBuffer` and its editing operations do not
//! depend on this trait -- it exists so a real highlighter can be plugged in
//! later without touching the buffer.

/// A half-open, character-indexed range within a single line that should be
/// styled distinctly from the rest of the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleSpan {
    pub start: usize,
    pub end: usize,
}

/// Maps a line of text to the style spans it should be painted with.
pub trait Highlighter {
    fn spans(&self, line: &str) -> Vec<StyleSpan>;
}

/// A `Highlighter` that never styles anything: every line renders with no
/// spans, i.e. as plain text.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlainHighlighter;

impl Highlighter for PlainHighlighter {
    fn spans(&self, _line: &str) -> Vec<StyleSpan> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Highlighter, PlainHighlighter};

    #[test]
    fn plain_highlighter_returns_no_spans_for_any_line() {
        let highlighter = PlainHighlighter;
        assert!(highlighter.spans("SELECT * FROM orders").is_empty());
        assert!(highlighter.spans("").is_empty());
        assert!(
            highlighter
                .spans("-- comment with unicode: \u{1F600}")
                .is_empty()
        );
    }
}
