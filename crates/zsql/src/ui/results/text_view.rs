use std::{ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, Entity, Font, Hsla, ListSizingBehavior, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Render, ScrollHandle, ScrollStrategy, SharedString,
    StyledText, TextRun, UniformListScrollHandle, Window, div, font, point, prelude::*, px, rgb,
    uniform_list,
};
use zsql_core::{ResultSet, Row, Value};
use zsql_editor::{Highlighter, SqlHighlighter, StyleSpan, syntax_color};
use zsql_ui::{
    scrollable::{
        Axis, ScrollSource, ScrollableState, ScrollbarStyle, WithScrollbars,
        restrict_wheel_to_own_axis,
    },
    table::{TableStyle, measure, row_number_cell_shell},
    theme::{ActiveTheme, Theme},
};

use crate::ui::theme;

/// A memoized Text-view content width plus the inputs it was measured from,
/// so a re-render that changes none of them (scroll, hover, selection, an
/// unrelated notify) reuses the width instead of re-shaping every line.
struct TextContentExtent {
    /// Document byte length -- a new or still-streaming document changes it,
    /// which is what invalidates the cache.
    document_len: usize,
    /// The data font family the width was measured in.
    font_family: SharedString,
    /// The text size the width was measured at.
    font_size: Pixels,
    /// The widest line's shaped width, before slack is added.
    width: Pixels,
}

/// One position within the Text view's assembled document: a 0-based source
/// line index and a byte offset into that line's own text. Ordered
/// lexicographically by `(line, byte)`, matching reading order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TextCaret {
    line: usize,
    byte: usize,
}

pub struct TextView {
    /// The current text document, assembled from the result.
    document: Option<Rc<str>>,
    /// The Text view's selection, as the (anchor, cursor) document positions
    /// in the order a click/drag/shift-click set them. `None` when nothing
    /// is selected.
    selection: Option<(TextCaret, TextCaret)>,
    /// Whether the mouse button is currently held down over the Text view,
    /// extending `text_selection` as it moves.
    selecting: bool,
    /// Derives syntax spans for the Text view's assembled document
    highlighter: SqlHighlighter,
    /// Vertical scroll position shared by the virtualized gutter and body
    row_scroll_handle: UniformListScrollHandle,
    /// Horizontal scroll position
    col_scroll_handle: ScrollHandle,
    /// Backs the Text view body pane's scrollbars: its vertical axis follows
    /// the shared row list and its horizontal axis the current longest line's
    /// extent
    scroll_state: Entity<ScrollableState>,
    /// Cached widest-line pixel width backing the Text view's horizontal
    /// scroll extent. Measured with real text shaping (see
    /// [`ResultsView::measure_text_content_width`])
    content_extent: Option<TextContentExtent>,
}

impl TextView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            document: None,
            selection: None,
            selecting: false,
            highlighter: SqlHighlighter::new(),
            row_scroll_handle: UniformListScrollHandle::new(),
            col_scroll_handle: ScrollHandle::new(),
            scroll_state: cx.new(ScrollableState::new),
            content_extent: None,
        }
    }

    /// Update the Text view's document from `result`, returning `true` if the
    /// result is parsable as a text document & was set.
    /// TODO: We should optimize this to only re-assemble the document if the result has changed.
    #[tracing::instrument(level = "debug", skip(self, result), fields(result_rows = result.rows.len()))]
    pub fn update_document(&mut self, result: &ResultSet) -> bool {
        if !result.is_document_shaped() {
            self.document = None;
            return false;
        }

        let Some(doc) = assemble_document(result) else {
            self.document = None;
            return false;
        };
        self.document = Some(Rc::from(doc));
        true
    }

    /// Get the Text view's current document, if any.
    pub fn document(&self) -> Option<&str> {
        self.document.as_deref()
    }

    /// Whether this view currently has a valid text document
    pub fn has_document(&self) -> bool {
        self.document.is_some()
    }

    /// Get the currently selected text, if any, from the view's document
    pub fn selected_text(&self) -> Option<String> {
        let document = self.document.as_deref()?;
        self.get_selected_text(document)
    }

    /// The current line count of the Text view's document
    pub fn line_count(&self) -> Option<usize> {
        self.document.as_deref().map(document_line_count)
    }

    /// Reset the state - clears selection scroll, etc.
    pub fn reset(&mut self) {
        self.document = None;
        self.selection = None;
        self.selecting = false;
        self.row_scroll_handle
            .scroll_to_item(0, ScrollStrategy::Top);
        self.col_scroll_handle.set_offset(point(px(0.0), px(0.0)));
        self.content_extent = None;
    }

    /// The widest Text-view line's shaped pixel width, memoized in
    /// `text_content_extent` and recomputed only when the document length,
    /// data font, or text size changes. A re-render that changes none of them
    /// (scroll, hover, selection, an unrelated notify) reuses the cached
    /// width rather than re-shaping every line.
    fn text_content_width(
        &mut self,
        document_len: usize,
        font_family: &SharedString,
        font_size: Pixels,
        lines: &[String],
        line_runs: &[Vec<TextRun>],
        window: &Window,
    ) -> Pixels {
        if let Some(cached) = &self.content_extent
            && cached.document_len == document_len
            && cached.font_family == *font_family
            && cached.font_size == font_size
        {
            return cached.width;
        }

        let width = Self::measure_text_content_width(lines, line_runs, font_size, window);
        self.content_extent = Some(TextContentExtent {
            document_len,
            font_family: font_family.clone(),
            font_size,
            width,
        });
        width
    }

    /// Shape every line with the same runs the body paints and return the
    /// widest, so the horizontal scroll extent matches the text that is
    /// actually drawn -- correct for a proportional data font (kerning,
    /// ligatures, variable advances), not just a monospace one.
    fn measure_text_content_width(
        lines: &[String],
        line_runs: &[Vec<TextRun>],
        font_size: Pixels,
        window: &Window,
    ) -> Pixels {
        let text_system = window.text_system();
        let mut widest = px(0.0);
        for (line, runs) in lines.iter().zip(line_runs) {
            if line.is_empty() {
                continue;
            }
            let width = text_system.layout_line(line, font_size, runs, None).width;
            if width > widest {
                widest = width;
            }
        }
        widest
    }

    /// The Text view's body while wrap is off: a pinned, virtualized
    /// line-number gutter beside a virtualized, horizontally scrolling body
    /// list -- only the lines within (or near) the current viewport are ever
    /// built into elements, mirroring the grid's own row virtualization.
    /// Both lists share one vertical scroll position via
    /// [`ResultsView::text_row_scroll_handle`].
    fn render_text_virtualized_body(
        &mut self,
        lines: &Rc<[String]>,
        line_runs: &Rc<[Vec<TextRun>]>,
        gutter_width: Pixels,
        content_extent: Pixels,
        style: &zsql_ui::table::TableStyle,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let line_count = lines.len();
        let gutter_style = *style;
        let gutter_list = restrict_wheel_to_own_axis(
            uniform_list(
                "results-text-gutter-list",
                line_count,
                move |range: Range<usize>, _window: &mut Window, _cx: &mut App| {
                    range
                        .map(|index| {
                            row_number_cell_shell(gutter_width, &gutter_style)
                                .h(theme::TEXT_VIEW_LINE_HEIGHT)
                                .child((index + 1).to_string())
                                .into_any_element()
                        })
                        .collect::<Vec<_>>()
                },
            )
            .flex_1()
            .track_scroll(self.row_scroll_handle.clone()),
        );

        let gutter = div()
            .id("results-text-gutter")
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(gutter_width)
            .h_full()
            .child(gutter_list);

        let col_scroll_handle = self.col_scroll_handle.clone();
        let row_scroll_handle = self.row_scroll_handle.clone();
        // line counts stay far below f32's exact-integer range
        #[allow(clippy::cast_precision_loss)]
        let vertical_extent = line_count as f32 * f32::from(theme::TEXT_VIEW_LINE_HEIGHT);
        self.scroll_state.update(cx, |state, _cx| {
            state.vertical(Axis::new(
                ScrollSource::UniformList(row_scroll_handle),
                vertical_extent,
            ));
            state.horizontal(Axis::new(
                ScrollSource::Container(col_scroll_handle),
                f32::from(content_extent),
            ));
        });

        let body_lines = lines.clone();
        let body_runs = line_runs.clone();
        let body_list = restrict_wheel_to_own_axis(
            uniform_list(
                "results-text-body-list",
                line_count,
                cx.processor(move |_this, range: Range<usize>, _window, cx| {
                    range
                        .map(|index| {
                            let line = body_lines.get(index).map_or("", String::as_str);
                            let runs = body_runs.get(index).cloned().unwrap_or_default();
                            render_text_view_line(index, line, runs, false, cx)
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .min_w(content_extent)
            .with_sizing_behavior(ListSizingBehavior::Auto)
            .track_scroll(self.row_scroll_handle.clone()),
        );

        let body = div()
            .id("results-text-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .w_full()
            .overflow_x_hidden()
            .track_scroll(&self.col_scroll_handle)
            .on_scroll_wheel(ScrollableState::wheel_handler(&self.scroll_state))
            .font_family(&cx.theme().fonts.data)
            .text_size(px(theme::TEXT_VIEW_FONT_SIZE))
            .child(body_list)
            .with_scrollbars(&self.scroll_state, ScrollbarStyle::default(), cx);

        div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(gutter)
            .child(body)
            .into_any_element()
    }

    /// Set the Text view's selection cursor to `(line, byte)`, extending from
    /// the existing anchor when `extend` (a shift-click) -- otherwise
    /// starting a fresh selection anchored at `(line, byte)` and arming it to
    /// extend further as the mouse drags (see
    /// [`ResultsView::extend_text_selection_while_dragging`]). A shift-click
    /// is a discrete jump, not a drag: it does not arm dragging.
    fn set_text_caret(&mut self, line: usize, byte: usize, extend: bool, cx: &mut Context<Self>) {
        let cursor = TextCaret { line, byte };
        let anchor = if extend {
            self.selection.map_or(cursor, |(anchor, _)| anchor)
        } else {
            cursor
        };
        self.selection = Some((anchor, cursor));
        self.selecting = !extend;
        cx.notify();
    }

    /// Move the Text view's selection cursor to `(line, byte)` while a drag
    /// begun by [`ResultsView::set_text_caret`] is in progress, keeping the
    /// existing anchor. A no-op once the drag has ended.
    fn extend_text_selection_while_dragging(
        &mut self,
        line: usize,
        byte: usize,
        cx: &mut Context<Self>,
    ) {
        if !self.selecting {
            return;
        }
        let Some((anchor, _)) = self.selection else {
            return;
        };
        self.selection = Some((anchor, TextCaret { line, byte }));
        cx.notify();
    }

    /// End a Text view selection drag, leaving the selection itself in
    /// place. A no-op if nothing was being dragged.
    fn end_text_selection_drag(&mut self, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        self.selecting = false;
        cx.notify();
    }

    /// Get the slice of text in `document` that is currently selected, or `None` if
    /// there is no selection.
    fn get_selected_text(&self, document: &str) -> Option<String> {
        let (anchor, cursor) = self.selection?;
        let (start, end) = if anchor < cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        let selected_text = document
            .lines()
            .enumerate()
            .filter_map(|(line_index, line)| {
                if line_index < start.line || line_index > end.line {
                    return None;
                }
                let start_byte = if line_index == start.line {
                    start.byte.min(line.len())
                } else {
                    0
                };
                let end_byte = if line_index == end.line {
                    end.byte.min(line.len())
                } else {
                    line.len()
                };
                Some(&line[start_byte..end_byte])
            });
        Some(join_document_lines(selected_text))
    }
}

impl Render for TextView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(document) = self.document.as_ref() else {
            tracing::warn!("Attempted to render a TextView with an empty document");
            return div();
        };

        self.highlighter.set_text(document);
        let lines: Rc<[String]> = document
            .split('\n')
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .into();
        let line_count = lines.len();

        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let style = TableStyle::themed(active_theme);
        let gutter_width = measure::row_number_column_width(
            line_count,
            &style,
            theme::CELL_CHAR_WIDTH,
            theme::ROW_NUMBER_MIN_WIDTH,
        );

        let font_family: SharedString = active_theme.fonts.data.clone().into();
        let run_font = font(active_theme.fonts.data.clone());
        let base_color = Hsla::from(rgb(colors.text_primary));
        let selection_bg = Hsla::from(theme::text_selection_bg(active_theme));
        let selection = self.selection;
        let line_runs: Rc<[Vec<TextRun>]> = lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let spans = self.highlighter.spans_for_line(index);
                let selection_range =
                    selection.and_then(|sel| line_selection_range(sel, index, line.len()));
                text_view_line_runs(
                    line,
                    spans,
                    selection_range.as_ref(),
                    &run_font,
                    base_color,
                    selection_bg,
                    active_theme,
                )
            })
            .collect::<Vec<_>>()
            .into();

        // Size the horizontal scroll extent from the widest line's real
        // shaped width (memoized), so the scrollbar thumb and reach stay
        // accurate for a proportional data font, not just a monospace one.
        let content_width = self.text_content_width(
            document.len(),
            &font_family,
            px(theme::TEXT_VIEW_FONT_SIZE),
            &lines,
            &line_runs,
            window,
        );
        let content_extent = content_width + theme::TEXT_VIEW_CONTENT_EXTENT_SLACK;

        let content = self.render_text_virtualized_body(
            &lines,
            &line_runs,
            gutter_width,
            content_extent,
            &style,
            cx,
        );

        div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(rgb(colors.bg_app))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _: &MouseUpEvent, _window, cx| {
                    view.end_text_selection_drag(cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, _: &MouseUpEvent, _window, cx| {
                    view.end_text_selection_drag(cx);
                }),
            )
            .child(content)
    }
}

/// The byte sub-range of line `line_index` (of length `line_byte_len`) that
/// `selection`'s ordered anchor/cursor covers, or `None` if the line falls
/// outside the selection or the selection is empty (a plain click with no
/// drag).
fn line_selection_range(
    (anchor, cursor): (TextCaret, TextCaret),
    line_index: usize,
    line_byte_len: usize,
) -> Option<Range<usize>> {
    let (start, end) = if anchor <= cursor {
        (anchor, cursor)
    } else {
        (cursor, anchor)
    };
    if line_index < start.line || line_index > end.line {
        return None;
    }
    let range_start = if line_index == start.line {
        start.byte
    } else {
        0
    };
    let range_end = if line_index == end.line {
        end.byte
    } else {
        line_byte_len
    };
    (range_start < range_end).then_some(range_start..range_end)
}

fn join_document_lines<'a>(iter: impl Iterator<Item = &'a str>) -> String {
    let mut all_have_newline = true;
    let temp_vec = iter
        .inspect(|txt| {
            if all_have_newline {
                all_have_newline = txt.ends_with('\n');
            }
        })
        .collect::<Vec<_>>();
    temp_vec.join(if all_have_newline { "" } else { "\n" })
}

/// One line of the Text view's body: `line`'s text painted with `runs`,
/// wrapped or not per `wrap`, and draggable/clickable to set the Text view's
/// selection (shift-click extends from the existing anchor; a plain
/// click-and-drag extends continuously as the mouse moves).
fn render_text_view_line(
    index: usize,
    line: &str,
    runs: Vec<TextRun>,
    wrap: bool,
    cx: &Context<TextView>,
) -> AnyElement {
    let styled = StyledText::new(line.to_owned()).with_runs(runs);
    let layout_for_down = styled.layout().clone();
    let layout_for_move = styled.layout().clone();

    div()
        .id(("results-text-line", index))
        .flex()
        .items_center()
        .min_w_0()
        .w_full()
        .when(wrap, |el| {
            el.whitespace_normal().min_h(theme::TEXT_VIEW_LINE_HEIGHT)
        })
        .when(!wrap, |el| {
            el.flex_shrink_0()
                .h(theme::TEXT_VIEW_LINE_HEIGHT)
                .whitespace_nowrap()
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, event: &MouseDownEvent, _window, cx| {
                let byte = match layout_for_down.index_for_position(event.position) {
                    Ok(b) | Err(b) => b,
                };

                view.set_text_caret(index, byte, event.modifiers.shift, cx);
            }),
        )
        .on_mouse_move(
            cx.listener(move |view, event: &MouseMoveEvent, _window, cx| {
                let byte = match layout_for_move.index_for_position(event.position) {
                    Ok(b) | Err(b) => b,
                };
                view.extend_text_selection_while_dragging(index, byte, cx);
            }),
        )
        .child(styled)
        .into_any_element()
}

/// Join a single-text-column result's rows into one document: a lone row's
/// value verbatim, or multiple rows joined with `'\n'`. If _every_ row
/// in the result is terminated with a newline, and there is more than one
/// row, we _don't_ join with an extra newline. This is to make displaying
/// things like `sp_helptext` a bit more natural. Returns None if the result
/// is empty.
fn assemble_document(result: &ResultSet) -> Option<String> {
    match result.rows.as_slice() {
        [] => None,
        [row] => Some(document_cell_text(row).to_owned()),
        rows => {
            let iter = rows.iter().map(document_cell_text);
            Some(join_document_lines(iter))
        }
    }
}

/// The text of `row`'s single column, or an empty string for a null or
/// absent cell.
fn document_cell_text(row: &Row) -> &str {
    match row.0.first() {
        Some(Value::Text(text)) => text.as_str(),
        _ => "",
    }
}

/// The number of lines in `document` under the split-on-newline convention:
/// an empty document is still one line, and a trailing newline yields one
/// extra empty final line.
fn document_line_count(document: &str) -> usize {
    document.split('\n').count()
}

/// One Text view line's `TextRun`s: `spans`' char-indexed ranges converted to
/// this line's own byte offsets and painted with
/// `zsql_editor::syntax_color`'s token roles, with `selection`'s byte range
/// (if any falls on this line) additionally shaded as a background.
fn text_view_line_runs(
    line: &str,
    spans: &[StyleSpan],
    selection: Option<&Range<usize>>,
    run_font: &Font,
    base_color: Hsla,
    selection_bg: Hsla,
    active_theme: &Theme,
) -> Vec<TextRun> {
    let line_len = line.len();
    let span_byte_range = |span: &StyleSpan| {
        let start = char_byte_offset(line, span.start).min(line_len);
        let end = char_byte_offset(line, span.end).min(line_len);
        start..end
    };

    let mut boundaries: Vec<usize> = vec![0, line_len];
    for span in spans {
        let range = span_byte_range(span);
        boundaries.push(range.start);
        boundaries.push(range.end);
    }
    if let Some(sel) = selection {
        boundaries.push(sel.start.min(line_len));
        boundaries.push(sel.end.min(line_len));
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut runs: Vec<TextRun> = Vec::new();
    for window in boundaries.windows(2) {
        let (start, end) = (window[0], window[1]);
        if start >= end {
            continue;
        }
        let color = spans
            .iter()
            .find(|span| {
                let range = span_byte_range(span);
                range.start <= start && end <= range.end
            })
            .map_or(base_color, |span| {
                Hsla::from(rgb(syntax_color(active_theme, span.kind)))
            });
        let background_color = selection
            .filter(|sel| sel.start <= start && end <= sel.end)
            .map(|_| selection_bg);
        let run = TextRun {
            len: end - start,
            font: run_font.clone(),
            color,
            background_color,
            underline: None,
            strikethrough: None,
        };
        match runs.last_mut() {
            Some(last)
                if last.color == run.color && last.background_color == run.background_color =>
            {
                last.len += run.len;
            }
            _ => runs.push(run),
        }
    }
    if runs.is_empty() {
        runs.push(TextRun {
            len: line_len,
            font: run_font.clone(),
            color: base_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    runs
}

/// The byte offset of `line`'s `char_index`-th character, or `line`'s full
/// byte length if `char_index` is at or past its end.
fn char_byte_offset(line: &str, char_index: usize) -> usize {
    line.char_indices()
        .nth(char_index)
        .map_or(line.len(), |(byte_index, _)| byte_index)
}

/// Test-only accessors used by `ui::results`'s tests, which drive the Text
/// view's selection through the [`ResultsView`]'s copy and reset paths.
#[cfg(test)]
impl TextView {
    pub(crate) fn set_text_caret_for_test(
        &mut self,
        line: usize,
        byte: usize,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        self.set_text_caret(line, byte, extend, cx);
    }

    /// The Text view's current selection as `((anchor_line, anchor_byte),
    /// (cursor_line, cursor_byte))`.
    pub(crate) fn text_selection_for_test(&self) -> Option<((usize, usize), (usize, usize))> {
        self.selection
            .map(|(anchor, cursor)| ((anchor.line, anchor.byte), (cursor.line, cursor.byte)))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, Hsla, TestAppContext, font, rgb};
    use zsql_core::{ColumnMeta, ResultSet, Row, Value};
    use zsql_editor::{HighlightKind, StyleSpan, syntax_color};
    use zsql_ui::theme::Theme;

    use super::{
        TextCaret, TextView, assemble_document, document_line_count, line_selection_range,
        text_view_line_runs,
    };
    use crate::ui::theme;

    fn column(name: &str, type_name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            nullable: false,
        }
    }

    fn text_column_result(rows: Vec<Row>) -> ResultSet {
        ResultSet {
            columns: vec![column("Text", "nvarchar")],
            rows,
            affected: None,
            notices: Vec::new(),
        }
    }

    // -- Assembling the document -------------------------------------------

    #[test]
    fn assemble_document_uses_a_single_rows_value_verbatim() {
        let result = text_column_result(vec![Row(vec![Value::Text(
            "CREATE PROCEDURE p\nAS\nBEGIN\nEND".to_owned(),
        )])]);
        assert_eq!(
            assemble_document(&result).as_deref(),
            Some("CREATE PROCEDURE p\nAS\nBEGIN\nEND"),
            "a single row's value must pass through unmodified, not be re-split/rejoined"
        );
    }

    #[test]
    fn assemble_document_joins_multiple_rows_with_newlines() {
        let result = text_column_result(vec![
            Row(vec![Value::Text("CREATE PROCEDURE p".to_owned())]),
            Row(vec![Value::Text("AS".to_owned())]),
            Row(vec![Value::Text("BEGIN".to_owned())]),
        ]);
        assert_eq!(
            assemble_document(&result).as_deref(),
            Some("CREATE PROCEDURE p\nAS\nBEGIN")
        );
    }

    #[test]
    fn assemble_document_renders_a_null_row_as_an_empty_line() {
        let result = text_column_result(vec![
            Row(vec![Value::Text("a".to_owned())]),
            Row(vec![Value::Null]),
            Row(vec![Value::Text("c".to_owned())]),
        ]);
        assert_eq!(assemble_document(&result).as_deref(), Some("a\n\nc"));
    }

    #[test]
    fn assemble_document_is_none_for_zero_rows() {
        assert_eq!(
            assemble_document(&text_column_result(Vec::new())),
            None,
            "an empty result yields no document, so the view falls back to the grid"
        );
    }

    #[test]
    fn document_line_count_matches_the_split_on_newline_convention() {
        assert_eq!(
            document_line_count(""),
            1,
            "an empty document is still 1 line"
        );
        assert_eq!(document_line_count("one line"), 1);
        assert_eq!(document_line_count("a\nb\nc"), 3);
        assert_eq!(
            document_line_count("a\nb\n"),
            3,
            "a trailing newline yields one extra empty final line"
        );
    }

    // -- Line runs ----------------------------------------------------------

    #[test]
    fn text_view_line_runs_with_no_spans_or_selection_is_one_base_colored_run() {
        let theme = Theme::default();
        let run_font = font(theme.fonts.data.clone());
        let base = Hsla::from(rgb(theme.colors.text_primary));
        let selection_bg = Hsla::from(theme::text_selection_bg(&theme));

        let line = "select 1";
        let runs = text_view_line_runs(line, &[], None, &run_font, base, selection_bg, &theme);

        assert_eq!(runs.len(), 1, "a plain line is a single run");
        assert_eq!(runs[0].len, line.len());
        assert_eq!(runs[0].color, base);
        assert_eq!(runs[0].background_color, None);
    }

    #[test]
    fn text_view_line_runs_converts_char_spans_to_byte_offsets_on_a_multibyte_line() {
        let theme = Theme::default();
        let run_font = font(theme.fonts.data.clone());
        let base = Hsla::from(rgb(theme.colors.text_primary));
        let selection_bg = Hsla::from(theme::text_selection_bg(&theme));

        // A lowercase e with an acute accent is two bytes, so char index 1 is
        // byte 2: a span over chars 1..3 must start after the whole accented
        // char, never split it mid-codepoint.
        let line = "\u{e9}12";
        let spans = [StyleSpan {
            start: 1,
            end: 3,
            kind: HighlightKind::Number,
        }];
        let runs = text_view_line_runs(line, &spans, None, &run_font, base, selection_bg, &theme);

        let number = Hsla::from(rgb(syntax_color(&theme, HighlightKind::Number)));
        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs[0].len,
            "\u{e9}".len(),
            "the multibyte char before the span stays one whole base-colored run"
        );
        assert_eq!(runs[0].color, base);
        assert_eq!(runs[1].color, number);
        assert_eq!(
            runs[1].len, 2,
            "the span covers exactly the two ASCII digits"
        );
        let total: usize = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, line.len(), "runs must tile the whole line exactly");
    }

    #[test]
    fn text_view_line_runs_shades_only_the_selected_byte_range() {
        let theme = Theme::default();
        let run_font = font(theme.fonts.data.clone());
        let base = Hsla::from(rgb(theme.colors.text_primary));
        let selection_bg = Hsla::from(theme::text_selection_bg(&theme));

        let line = "select";
        let selection = 2..4;
        let runs = text_view_line_runs(
            line,
            &[],
            Some(&selection),
            &run_font,
            base,
            selection_bg,
            &theme,
        );

        let shaded: Vec<_> = runs
            .iter()
            .filter(|r| r.background_color == Some(selection_bg))
            .collect();
        assert_eq!(shaded.len(), 1, "exactly the selected range carries the bg");
        assert_eq!(shaded[0].len, 2);
        let total: usize = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, line.len());
        let unshaded: usize = runs
            .iter()
            .filter(|r| r.background_color.is_none())
            .map(|r| r.len)
            .sum();
        assert_eq!(
            unshaded,
            line.len() - 2,
            "nothing outside the selection is shaded"
        );
    }

    #[test]
    fn text_view_line_runs_merges_adjacent_runs_of_the_same_color() {
        let theme = Theme::default();
        let run_font = font(theme.fonts.data.clone());
        let base = Hsla::from(rgb(theme.colors.text_primary));
        let selection_bg = Hsla::from(theme::text_selection_bg(&theme));

        // Two touching spans of the same kind must collapse into one run.
        let line = "abcd";
        let spans = [
            StyleSpan {
                start: 0,
                end: 2,
                kind: HighlightKind::Keyword,
            },
            StyleSpan {
                start: 2,
                end: 4,
                kind: HighlightKind::Keyword,
            },
        ];
        let runs = text_view_line_runs(line, &spans, None, &run_font, base, selection_bg, &theme);

        assert_eq!(
            runs.len(),
            1,
            "adjacent same-color windows merge into one run"
        );
        assert_eq!(runs[0].len, line.len());
    }

    // -- Selection byte ranges ---------------------------------------------

    #[test]
    fn line_selection_range_covers_only_the_lines_and_bytes_between_anchor_and_cursor() {
        let selection = (
            TextCaret { line: 0, byte: 2 },
            TextCaret { line: 2, byte: 1 },
        );
        assert_eq!(
            line_selection_range(selection, 0, 5),
            Some(2..5),
            "the anchor's own line is selected from its byte to the line's end"
        );
        assert_eq!(
            line_selection_range(selection, 1, 5),
            Some(0..5),
            "a line strictly between anchor and cursor is selected in full"
        );
        assert_eq!(
            line_selection_range(selection, 2, 5),
            Some(0..1),
            "the cursor's own line is selected from its start to its byte"
        );
        assert_eq!(
            line_selection_range(selection, 3, 5),
            None,
            "a line outside the selection's line range is not selected"
        );
    }

    #[test]
    fn line_selection_range_is_none_for_a_collapsed_selection() {
        let caret = TextCaret { line: 1, byte: 3 };
        assert_eq!(
            line_selection_range((caret, caret), 1, 10),
            None,
            "a plain click with no drag selects nothing to highlight"
        );
    }

    // -- Character-granular selection --------------------------------------

    #[gpui::test]
    fn clicking_then_shift_clicking_extends_the_selection_from_the_original_anchor(
        cx: &mut TestAppContext,
    ) {
        let view = cx.new(TextView::new);

        view.update(cx, |tv, cx| tv.set_text_caret(0, 1, false, cx));
        assert_eq!(
            view.update(cx, |tv, _| tv.text_selection_for_test()),
            Some(((0, 1), (0, 1)))
        );

        view.update(cx, |tv, cx| tv.set_text_caret(2, 2, true, cx));
        assert_eq!(
            view.update(cx, |tv, _| tv.text_selection_for_test()),
            Some(((0, 1), (2, 2))),
            "a shift-click must extend from the existing anchor rather than starting a new one"
        );

        view.update(cx, |tv, cx| tv.set_text_caret(1, 0, false, cx));
        assert_eq!(
            view.update(cx, |tv, _| tv.text_selection_for_test()),
            Some(((1, 0), (1, 0))),
            "a plain click (no shift) must start a fresh selection at the clicked position"
        );
    }

    #[gpui::test]
    fn dragging_after_a_click_extends_the_selection_but_a_shift_click_does_not_arm_dragging(
        cx: &mut TestAppContext,
    ) {
        let view = cx.new(TextView::new);

        view.update(cx, |tv, cx| tv.set_text_caret(0, 0, false, cx));
        view.update(cx, |tv, cx| {
            tv.extend_text_selection_while_dragging(1, 2, cx);
        });
        assert_eq!(
            view.update(cx, |tv, _| tv.text_selection_for_test()),
            Some(((0, 0), (1, 2))),
            "a drag begun by a plain click must extend the live selection as the mouse moves"
        );

        view.update(cx, TextView::end_text_selection_drag);
        view.update(cx, |tv, cx| {
            tv.extend_text_selection_while_dragging(0, 1, cx);
        });
        assert_eq!(
            view.update(cx, |tv, _| tv.text_selection_for_test()),
            Some(((0, 0), (1, 2))),
            "extending after the drag has ended must be a no-op"
        );

        view.update(cx, |tv, cx| tv.set_text_caret(0, 2, true, cx));
        view.update(cx, |tv, cx| {
            tv.extend_text_selection_while_dragging(1, 1, cx);
        });
        assert_eq!(
            view.update(cx, |tv, _| tv.text_selection_for_test()),
            Some(((0, 0), (0, 2))),
            "a shift-click does not arm dragging, so a subsequent move must not extend it"
        );
    }
}
