//! Syntax-highlighting seam for the editor. A `Highlighter` is fed the
//! buffer's full text and, from a whole-document parse, exposes the style
//! spans each line should be painted with. It sees the whole document
//! rather than one line at a time because SQL constructs that a line
//! viewed in isolation cannot resolve on its own -- an unterminated string
//! or a block comment opened on an earlier line -- need the surrounding
//! text for correct highlighting. `PlainHighlighter` is a no-op
//! implementation; `SqlHighlighter` parses SQL with tree-sitter.
//! `TextBuffer` and its editing operations do not depend on this trait.

use std::collections::HashMap;
use std::ops::Range;

use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

/// Semantic class of a span of text, used to pick the color it paints with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    Keyword,
    String,
    Number,
    Comment,
    Function,
    Operator,
    Identifier,
    Punctuation,
}

/// A half-open, character-indexed range within a single line that should be
/// styled distinctly from the rest of the line, and the kind of styling it
/// should get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleSpan {
    pub start: usize,
    pub end: usize,
    pub kind: HighlightKind,
}

/// Derives style spans from a buffer's text, one line at a time, from a
/// parse of the whole document.
pub trait Highlighter {
    /// Re-derive this highlighter's spans from the buffer's current full
    /// text. Called whenever the buffer's text may have changed.
    fn set_text(&mut self, text: &str);

    /// The style spans covering line `line_index`, in that line's own
    /// char-indexed coordinates. Empty for a line with no styled spans or
    /// past the end of the last-parsed text.
    fn spans_for_line(&self, line_index: usize) -> &[StyleSpan];
}

/// A `Highlighter` that never styles anything: every line renders with no
/// spans, i.e. as plain text.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlainHighlighter;

impl Highlighter for PlainHighlighter {
    fn set_text(&mut self, _text: &str) {}

    fn spans_for_line(&self, _line_index: usize) -> &[StyleSpan] {
        &[]
    }
}

/// A tree-sitter-backed `Highlighter` for SQL.
// Caches the last-parsed text and its derived per-line spans; `set_text`
// re-parses the full buffer on each change rather than reusing the previous
// tree, which is fine for editor-sized SQL text, and skips the reparse
// entirely when the incoming text matches what is already cached.
pub struct SqlHighlighter {
    parser: Parser,
    query: Query,
    text: String,
    lines: Vec<Vec<StyleSpan>>,
    #[cfg(test)]
    reparse_count: usize,
}

impl SqlHighlighter {
    /// # Panics
    ///
    /// Panics if the bundled SQL grammar or its highlights query fails to
    /// load. Both are fixed, compiled-in constants, so this can only
    /// indicate a build-time problem with the grammar dependency, never
    /// something a caller's SQL text could trigger.
    #[must_use]
    pub fn new() -> Self {
        let language: Language = tree_sitter_sequel::LANGUAGE.into();
        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .expect("bundled SQL grammar should load");
        let query = Query::new(&language, &patched_highlights_query())
            .expect("bundled SQL highlights query should compile");
        Self {
            parser,
            query,
            text: String::new(),
            lines: vec![Vec::new()],
            #[cfg(test)]
            reparse_count: 0,
        }
    }

    /// Number of full-document reparses performed since construction.
    /// Test-only: lets tests confirm the unchanged-text fast path in
    /// `set_text` is actually skipping work, not just producing the same
    /// output by coincidence.
    #[cfg(test)]
    fn reparse_count(&self) -> usize {
        self.reparse_count
    }
}

impl Default for SqlHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter for SqlHighlighter {
    fn set_text(&mut self, text: &str) {
        if text == self.text {
            return;
        }
        tracing::debug!(
            bytes = text.len(),
            lines = text.lines().count().max(1),
            "sql highlighter re-parsing buffer"
        );
        #[cfg(test)]
        {
            self.reparse_count += 1;
        }
        self.lines = highlight_lines(&mut self.parser, &self.query, text);
        text.clone_into(&mut self.text);
    }

    fn spans_for_line(&self, line_index: usize) -> &[StyleSpan] {
        self.lines.get(line_index).map_or(&[], Vec::as_slice)
    }
}

/// The grammar's bundled highlights query, with its `@number`/`@float`
/// predicates patched to use a regex digit class. The upstream query writes
/// them as `%d`, a Lua string-pattern escape meaningless to tree-sitter's
/// own (POSIX-regex-based) predicate engine, so those two predicates never
/// match anything as shipped; every other capture is used unmodified.
fn patched_highlights_query() -> String {
    tree_sitter_sequel::HIGHLIGHTS_QUERY.replace("%d", "\\\\d")
}

/// Maps a capture name from the grammar's bundled highlights query to the
/// `HighlightKind` it paints as. Names covering their own reserved-word
/// tokens (`type.builtin`, `type.qualifier`, `storageclass`, `attribute`,
/// `conditional`, `keyword.operator`, `boolean`) fold into `Keyword`; names
/// covering identifier-like references (`type`, `variable`, `field`,
/// `parameter`) fold into `Identifier`; `spell` (always co-occurs with
/// `comment` on the same node) folds into `Comment`. Every capture name the
/// query defines maps to one of these; an unrecognized name is ignored
/// rather than styled.
fn highlight_kind_for_capture(name: &str) -> Option<HighlightKind> {
    match name {
        "keyword" | "type.builtin" | "type.qualifier" | "storageclass" | "attribute"
        | "conditional" | "keyword.operator" | "boolean" => Some(HighlightKind::Keyword),
        "string" => Some(HighlightKind::String),
        "number" | "float" => Some(HighlightKind::Number),
        "comment" | "spell" => Some(HighlightKind::Comment),
        "function.call" => Some(HighlightKind::Function),
        "operator" => Some(HighlightKind::Operator),
        "punctuation.bracket" | "punctuation.delimiter" => Some(HighlightKind::Punctuation),
        "type" | "variable" | "field" | "parameter" => Some(HighlightKind::Identifier),
        _ => None,
    }
}

/// Which kind wins when two captures land on the exact same byte range (for
/// example a called function name, captured as both a generic object
/// reference and a function call). Higher wins.
fn highlight_priority(kind: HighlightKind) -> u8 {
    match kind {
        HighlightKind::Function => 100,
        HighlightKind::Keyword => 90,
        HighlightKind::Number => 80,
        HighlightKind::String => 70,
        HighlightKind::Comment => 60,
        HighlightKind::Operator => 50,
        HighlightKind::Punctuation => 40,
        HighlightKind::Identifier => 10,
    }
}

/// Parse `text` and derive one `StyleSpan` list per line. Malformed or
/// non-SQL text never panics: tree-sitter always returns a (possibly
/// error-laden) tree, and the highlights query simply yields fewer or no
/// captures over it.
fn highlight_lines(parser: &mut Parser, query: &Query, text: &str) -> Vec<Vec<StyleSpan>> {
    let line_ranges = line_byte_ranges(text);
    let mut spans_by_line: Vec<Vec<StyleSpan>> = vec![Vec::new(); line_ranges.len()];

    // `parse` only returns `None` if a cancellation flag or timeout set via
    // `Parser::set_cancellation_flag`/`set_timeout_micros` fired; this
    // highlighter installs neither, so this is unreachable in practice --
    // guarded anyway so a future change to that can only degrade
    // highlighting, never panic.
    let Some(tree) = parser.parse(text, None) else {
        return spans_by_line;
    };

    let mut best: HashMap<(usize, usize), (u8, HighlightKind)> = HashMap::new();
    let mut cursor = QueryCursor::new();
    let capture_names = query.capture_names();
    let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
    while let Some(query_match) = matches.next() {
        for capture in query_match.captures {
            let Some(kind) = highlight_kind_for_capture(capture_names[capture.index as usize])
            else {
                continue;
            };
            let range = capture.node.byte_range();
            let priority = highlight_priority(kind);
            best.entry((range.start, range.end))
                .and_modify(|(best_priority, best_kind)| {
                    if priority > *best_priority {
                        *best_priority = priority;
                        *best_kind = kind;
                    }
                })
                .or_insert((priority, kind));
        }
    }

    let mut ranges: Vec<(Range<usize>, HighlightKind)> = best
        .into_iter()
        .map(|((start, end), (_, kind))| (start..end, kind))
        .collect();
    ranges.sort_by_key(|(range, _)| (range.start, range.end));

    for (byte_range, kind) in ranges {
        append_span_per_line(&line_ranges, text, byte_range, kind, &mut spans_by_line);
    }

    spans_by_line
}

/// Split `byte_range` across the lines it touches, converting each piece to
/// that line's char-indexed coordinates and appending it. Skips a piece that
/// would overlap the line's already-appended spans, so a future grammar
/// version producing overlapping (not just identical) captures degrades to
/// keeping the earlier one rather than corrupting run boundaries.
fn append_span_per_line(
    line_ranges: &[Range<usize>],
    text: &str,
    byte_range: Range<usize>,
    kind: HighlightKind,
    spans_by_line: &mut [Vec<StyleSpan>],
) {
    if byte_range.start >= byte_range.end {
        return;
    }
    let start_line = line_containing(line_ranges, byte_range.start);
    let end_line = line_containing(line_ranges, byte_range.end - 1);

    for line_index in start_line..=end_line {
        let line_range = line_ranges[line_index].clone();
        let clip_start = byte_range.start.max(line_range.start);
        let clip_end = byte_range.end.min(line_range.end);
        if clip_start >= clip_end {
            continue;
        }

        let line_text = &text[line_range.start..line_range.end];
        let char_start = line_text[..clip_start - line_range.start].chars().count();
        let char_end = line_text[..clip_end - line_range.start].chars().count();

        let line_spans = &mut spans_by_line[line_index];
        if let Some(last) = line_spans.last()
            && char_start < last.end
        {
            continue;
        }
        line_spans.push(StyleSpan {
            start: char_start,
            end: char_end,
            kind,
        });
    }
}

/// The line index whose range contains `byte_offset` (or, if `byte_offset`
/// falls on the newline joining two lines, the line before it).
fn line_containing(line_ranges: &[Range<usize>], byte_offset: usize) -> usize {
    let after = line_ranges.partition_point(|range| range.start <= byte_offset);
    after.saturating_sub(1).min(line_ranges.len() - 1)
}

/// The byte range of each line's own content (excluding its trailing `\n`)
/// within `text`, in document order. Mirrors `TextBuffer::from_text`'s own
/// `split('\n')` line splitting, so line indices agree with the buffer's.
fn line_byte_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for line in text.split('\n') {
        let end = start + line.len();
        ranges.push(start..end);
        start = end + 1;
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::{HighlightKind, Highlighter, PlainHighlighter, SqlHighlighter, StyleSpan};

    #[test]
    fn plain_highlighter_returns_no_spans_for_any_line() {
        let mut highlighter = PlainHighlighter;
        highlighter.set_text("SELECT * FROM orders");
        assert!(highlighter.spans_for_line(0).is_empty());
        highlighter.set_text("");
        assert!(highlighter.spans_for_line(0).is_empty());
        highlighter.set_text("-- comment with unicode: \u{1F600}");
        assert!(highlighter.spans_for_line(0).is_empty());
    }

    /// The span on `line_index` whose text (sliced from `line`) equals
    /// `needle`, or `None` if no span matches. Lets tests assert against
    /// substrings instead of hand-computed char offsets.
    fn span_for_text(spans: &[StyleSpan], line: &str, needle: &str) -> Option<StyleSpan> {
        let chars: Vec<char> = line.chars().collect();
        spans.iter().copied().find(|span| {
            chars
                .get(span.start..span.end)
                .is_some_and(|slice| slice.iter().collect::<String>() == needle)
        })
    }

    fn kind_of(
        highlighter: &SqlHighlighter,
        line_index: usize,
        line: &str,
        needle: &str,
    ) -> HighlightKind {
        let spans = highlighter.spans_for_line(line_index);
        span_for_text(spans, line, needle)
            .unwrap_or_else(|| panic!("no span for {needle:?} on line {line_index} ({line:?})"))
            .kind
    }

    #[test]
    fn select_from_where_are_keywords() {
        let mut highlighter = SqlHighlighter::new();
        let sql = "SELECT a FROM t WHERE a = 1";
        highlighter.set_text(sql);
        assert_eq!(
            kind_of(&highlighter, 0, sql, "SELECT"),
            HighlightKind::Keyword
        );
        assert_eq!(
            kind_of(&highlighter, 0, sql, "FROM"),
            HighlightKind::Keyword
        );
        assert_eq!(
            kind_of(&highlighter, 0, sql, "WHERE"),
            HighlightKind::Keyword
        );
    }

    #[test]
    fn a_quoted_string_literal_is_a_string() {
        let mut highlighter = SqlHighlighter::new();
        let sql = "SELECT 'paid' AS status";
        highlighter.set_text(sql);
        assert_eq!(
            kind_of(&highlighter, 0, sql, "'paid'"),
            HighlightKind::String
        );
    }

    #[test]
    fn integer_and_float_literals_are_numbers() {
        let mut highlighter = SqlHighlighter::new();
        let sql = "SELECT 42, 3.14";
        highlighter.set_text(sql);
        assert_eq!(kind_of(&highlighter, 0, sql, "42"), HighlightKind::Number);
        assert_eq!(kind_of(&highlighter, 0, sql, "3.14"), HighlightKind::Number);
    }

    #[test]
    fn line_and_block_comments_are_comments() {
        let mut highlighter = SqlHighlighter::new();
        let sql = "SELECT 1; -- trailing note\n/* a block comment */\nSELECT 2;";
        highlighter.set_text(sql);
        assert_eq!(
            kind_of(
                &highlighter,
                0,
                "SELECT 1; -- trailing note",
                "-- trailing note"
            ),
            HighlightKind::Comment
        );
        assert_eq!(
            kind_of(
                &highlighter,
                1,
                "/* a block comment */",
                "/* a block comment */"
            ),
            HighlightKind::Comment
        );
    }

    #[test]
    fn a_called_function_name_is_a_function() {
        let mut highlighter = SqlHighlighter::new();
        let sql = "SELECT count(*) FROM orders";
        highlighter.set_text(sql);
        assert_eq!(
            kind_of(&highlighter, 0, sql, "count"),
            HighlightKind::Function
        );
    }

    #[test]
    fn parens_and_a_comma_are_punctuation() {
        let mut highlighter = SqlHighlighter::new();
        let sql = "SELECT count(a, b)";
        highlighter.set_text(sql);
        assert_eq!(
            kind_of(&highlighter, 0, sql, "("),
            HighlightKind::Punctuation
        );
        assert_eq!(
            kind_of(&highlighter, 0, sql, ")"),
            HighlightKind::Punctuation
        );
        assert_eq!(
            kind_of(&highlighter, 0, sql, ","),
            HighlightKind::Punctuation
        );
    }

    #[test]
    fn a_comparison_operator_is_an_operator() {
        let mut highlighter = SqlHighlighter::new();
        let sql = "SELECT a FROM t WHERE a <> 1";
        highlighter.set_text(sql);
        assert_eq!(kind_of(&highlighter, 0, sql, "<>"), HighlightKind::Operator);
    }

    #[test]
    fn a_plain_unqualified_identifier_is_unstyled() {
        let mut highlighter = SqlHighlighter::new();
        let sql = "SELECT a FROM t";
        highlighter.set_text(sql);
        assert_eq!(
            kind_of(&highlighter, 0, sql, "a"),
            HighlightKind::Identifier
        );
        assert_eq!(
            kind_of(&highlighter, 0, sql, "t"),
            HighlightKind::Identifier
        );
    }

    #[test]
    fn a_block_comment_opened_on_one_line_and_closed_two_lines_later_highlights_every_line() {
        // Proves the whole-document-parse redesign: a line viewed alone
        // (line 1, just "still commented") has no lexical clue it is inside
        // a comment; only the full-buffer parse can tell.
        let mut highlighter = SqlHighlighter::new();
        let sql = "SELECT 1; /* start\nstill commented\nend */ SELECT 2;";
        highlighter.set_text(sql);

        let lines: Vec<&str> = sql.split('\n').collect();
        assert_eq!(lines.len(), 3);

        let line0_spans = highlighter.spans_for_line(0);
        let comment_start = span_for_text(line0_spans, lines[0], "/* start")
            .expect("line 0 has a comment span starting the block comment");
        assert_eq!(comment_start.kind, HighlightKind::Comment);

        let line1_spans = highlighter.spans_for_line(1);
        assert_eq!(
            line1_spans,
            &[StyleSpan {
                start: 0,
                end: lines[1].chars().count(),
                kind: HighlightKind::Comment,
            }],
            "the entire middle line is inside the still-open block comment"
        );

        let line2_spans = highlighter.spans_for_line(2);
        let comment_end = span_for_text(line2_spans, lines[2], "end */")
            .expect("line 2 has a comment span closing the block comment");
        assert_eq!(comment_end.kind, HighlightKind::Comment);
        let keyword = span_for_text(line2_spans, lines[2], "SELECT")
            .expect("line 2 has a keyword span after the comment closes");
        assert_eq!(keyword.kind, HighlightKind::Keyword);
    }

    #[test]
    fn char_offsets_are_correct_across_a_multi_byte_line() {
        // The cafe' string literal sits after a multi-byte identifier, so a
        // naive byte-offset-as-char-offset conversion would land the
        // string's span too far to the right.
        let mut highlighter = SqlHighlighter::new();
        let sql = "SELECT caf\u{e9} AS caf\u{e9}, 'na\u{efdc}ve'";
        highlighter.set_text(sql);
        let spans = highlighter.spans_for_line(0);

        let literal = span_for_text(spans, sql, "'na\u{efdc}ve'")
            .expect("the multi-byte string literal has a span");
        assert_eq!(literal.kind, HighlightKind::String);

        let chars: Vec<char> = sql.chars().collect();
        let literal_text: String = chars[literal.start..literal.end].iter().collect();
        assert_eq!(
            literal_text, "'na\u{efdc}ve'",
            "char-indexed span boundaries must land exactly on the literal, \
             not be skewed by its multi-byte characters"
        );
    }

    #[test]
    fn malformed_sql_does_not_panic() {
        let mut highlighter = SqlHighlighter::new();
        for text in [
            "SELECT 'unterminated",
            "SELECT )( FROM (",
            "this is just plain english prose, not SQL at all.",
            "",
            "\u{1F600}\u{1F600}\u{1F600}",
        ] {
            highlighter.set_text(text);
            // Any spans produced must at least be valid char ranges into
            // their own line; the real assertion is that the call above
            // did not panic.
            for (line_index, line) in text.split('\n').enumerate() {
                let line_len = line.chars().count();
                for span in highlighter.spans_for_line(line_index) {
                    assert!(span.start <= span.end && span.end <= line_len);
                }
            }
        }
    }

    #[test]
    fn set_text_skips_reparsing_when_the_text_is_unchanged() {
        let mut highlighter = SqlHighlighter::new();
        highlighter.set_text("SELECT 1");
        assert_eq!(highlighter.reparse_count(), 1);
        highlighter.set_text("SELECT 1");
        assert_eq!(
            highlighter.reparse_count(),
            1,
            "a second set_text with identical text must not trigger another reparse"
        );
    }

    #[test]
    fn set_text_with_new_text_updates_the_spans() {
        let mut highlighter = SqlHighlighter::new();
        highlighter.set_text("SELECT a");
        highlighter.set_text("SELECT 'now a string'");
        assert_eq!(
            kind_of(&highlighter, 0, "SELECT 'now a string'", "'now a string'"),
            HighlightKind::String
        );
    }
}
