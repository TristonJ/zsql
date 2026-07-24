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

use crate::ui::{
    results::{TextCaret, TextContentExtent},
    theme,
};

pub struct TextView {
    /// The current text document, assembled from the result.
    text_document: Option<Rc<str>>,
    /// The Text view's selection, as the (anchor, cursor) document positions
    /// in the order a click/drag/shift-click set them -- not necessarily
    /// `anchor <= cursor`. `None` when nothing is selected.
    text_selection: Option<(TextCaret, TextCaret)>,
    /// Whether the mouse button is currently held down over the Text view,
    /// extending `text_selection` as it moves. Sends a shift-click's fixed
    /// jump-and-release through the same [`ResultsView::set_text_caret`]
    /// path without leaving a drag in progress.
    text_selecting: bool,
    /// Derives syntax spans for the Text view's assembled document. Reused
    /// across renders so an unchanged document skips reparsing (see
    /// `SqlHighlighter::set_text`).
    text_highlighter: SqlHighlighter,
    /// Vertical scroll position shared by the Text view's virtualized gutter
    /// and body lists while wrap is off, so scrolling either one scrolls
    /// both in lockstep (mirrors the grid's own row-synced scrolling).
    text_row_scroll_handle: UniformListScrollHandle,
    /// Horizontal scroll position of the Text view's body pane while wrap is
    /// off; the gutter never scrolls horizontally.
    text_col_scroll_handle: ScrollHandle,
    /// Backs the Text view body pane's horizontal scrollbar: its axis is
    /// reconfigured each render from the current longest line's extent, and
    /// [`WithScrollbars`] overlays the track+thumb the same way the grid's
    /// `Table` does.
    text_scroll_state: Entity<ScrollableState>,
    /// Vertical scroll position of the Text view's single unified line list
    /// while wrap is on, where lines are not virtualized (their heights vary
    /// with how each wraps) and no horizontal axis is needed.
    text_wrap_scroll_handle: ScrollHandle,
    /// Cached widest-line pixel width backing the Text view's horizontal
    /// scroll extent. Measured with real text shaping (see
    /// [`ResultsView::measure_text_content_width`]) so it stays correct for a
    /// proportional data font, not just a monospace one, and recomputed only
    /// when the inputs that width depends on change.
    text_content_extent: Option<TextContentExtent>,
}

impl TextView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            text_document: None,
            text_selection: None,
            text_selecting: false,
            text_highlighter: SqlHighlighter::new(),
            text_row_scroll_handle: UniformListScrollHandle::new(),
            text_col_scroll_handle: ScrollHandle::new(),
            text_wrap_scroll_handle: ScrollHandle::new(),
            text_scroll_state: cx.new(ScrollableState::new),
            text_content_extent: None,
        }
    }

    /// Update the Text view's document from `result`, returning `true` if the
    /// result is parsable as a text document & was set.
    /// TODO: We should optimize this to only re-assemble the document if the result has changed.
    #[tracing::instrument(level = "debug", skip(self, result), fields(result_rows = result.rows.len()))]
    pub fn update_document(&mut self, result: &ResultSet) -> bool {
        if !result.is_document_shaped() {
            self.text_document = None;
            return false;
        }

        let Some(doc) = assemble_document(result) else {
            self.text_document = None;
            return false;
        };
        self.text_document = Some(Rc::from(doc));
        true
    }

    /// Get the Text view's current document, if any.
    pub fn document(&self) -> Option<&str> {
        self.text_document.as_deref()
    }

    /// Whether this view currently has a valid text document
    pub fn has_document(&self) -> bool {
        self.text_document.is_some()
    }

    /// Get the currently selected text, if any, from the view's document
    pub fn selected_text(&self) -> Option<String> {
        let Some(document) = self.text_document.as_deref() else {
            return None;
        };
        self.get_selected_text(document)
    }

    /// The current line count of the Text view's document
    pub fn line_count(&self) -> Option<usize> {
        self.text_document.as_ref().map(|d| d.split('\n').count())
    }

    /// Reset the state - clears selection scroll, etc.
    pub fn reset(&mut self) {
        self.text_document = None;
        self.text_selection = None;
        self.text_selecting = false;
        self.text_row_scroll_handle
            .scroll_to_item(0, ScrollStrategy::Top);
        self.text_col_scroll_handle
            .set_offset(point(px(0.0), px(0.0)));
        self.text_wrap_scroll_handle
            .set_offset(point(px(0.0), px(0.0)));
        self.text_content_extent = None;
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
        if let Some(cached) = &self.text_content_extent
            && cached.document_len == document_len
            && cached.font_family == *font_family
            && cached.font_size == font_size
        {
            return cached.width;
        }

        let width = Self::measure_text_content_width(lines, line_runs, font_size, window);
        self.text_content_extent = Some(TextContentExtent {
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
            .track_scroll(self.text_row_scroll_handle.clone()),
        );

        let gutter = div()
            .id("results-text-gutter")
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(gutter_width)
            .h_full()
            .child(gutter_list);

        // Point the body pane's horizontal axis at the widest line's measured
        // extent (see `text_content_width`). Only the horizontal axis is
        // configured, so `with_scrollbars` paints just the bottom track -- the
        // vertical list keeps its own native wheel scrolling with no vertical
        // thumb, unchanged from before.
        let col_scroll_handle = self.text_col_scroll_handle.clone();
        self.text_scroll_state.update(cx, |state, _cx| {
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
            .track_scroll(self.text_row_scroll_handle.clone()),
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
            .track_scroll(&self.text_col_scroll_handle)
            .on_scroll_wheel(ScrollableState::wheel_handler(&self.text_scroll_state))
            .font_family(&cx.theme().fonts.data)
            .text_size(px(theme::TEXT_VIEW_FONT_SIZE))
            .child(body_list)
            .with_scrollbars(&self.text_scroll_state, ScrollbarStyle::default(), cx);

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
            self.text_selection.map_or(cursor, |(anchor, _)| anchor)
        } else {
            cursor
        };
        self.text_selection = Some((anchor, cursor));
        self.text_selecting = !extend;
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
        if !self.text_selecting {
            return;
        }
        let Some((anchor, _)) = self.text_selection else {
            return;
        };
        self.text_selection = Some((anchor, TextCaret { line, byte }));
        cx.notify();
    }

    /// End a Text view selection drag, leaving the selection itself in
    /// place. A no-op if nothing was being dragged.
    fn end_text_selection_drag(&mut self, cx: &mut Context<Self>) {
        if !self.text_selecting {
            return;
        }
        self.text_selecting = false;
        cx.notify();
    }

    /// Get the slice of text in `document` that is currently selected, or `None` if
    /// there is no selection.
    fn get_selected_text(&self, document: &str) -> Option<String> {
        let (anchor, cursor) = self.text_selection?;
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
        let Some(document) = self.text_document.as_ref() else {
            tracing::warn!("Attempted to render a TextView with an empty document");
            return div();
        };

        self.text_highlighter.set_text(document);
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
        let selection_bg = Hsla::from(rgb(theme::text_selection_bg(active_theme)));
        let selection = self.text_selection;
        let line_runs: Rc<[Vec<TextRun>]> = lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let spans = self.text_highlighter.spans_for_line(index);
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
