//! The custom `Element` that shapes and paints an [`EditorView`]'s buffer
//! contents -- lines, cursor, and selection highlight -- and wires OS text
//! input into it via `window.handle_input`, plus the paint-math helpers it
//! depends on.

use std::ops::Range;

use gpui::{
    App, Bounds, Element, ElementId, ElementInputHandler, Entity, Font, GlobalElementId, Hsla,
    InspectorElementId, LayoutId, PaintQuad, Pixels, ShapedLine, SharedString, Style, TextRun,
    UnderlineStyle, Window, fill, point, prelude::*, px, relative, rgb, rgba, size,
};
use zsql_ui::theme::{ActiveTheme, Theme};

use super::EditorView;
use crate::theme;
use crate::{Position, Selection, TextBuffer};

/// The custom `Element` that shapes and paints the buffer's lines, cursor,
/// and selection, and wires OS text input into `EditorView` via
/// `window.handle_input`.
pub(super) struct EditorContentElement {
    editor: Entity<EditorView>,
}

impl EditorContentElement {
    pub(super) fn new(editor: Entity<EditorView>) -> Self {
        Self { editor }
    }
}

pub(super) struct EditorPrepaintState {
    lines: Vec<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
}

impl IntoElement for EditorContentElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorContentElement {
    type RequestLayoutState = ();
    type PrepaintState = EditorPrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let line_count = self.editor.read(cx).buffer.lines().len().max(1);
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = total_line_height(line_count).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let active_theme = cx.theme();
        let editor = self.editor.read(cx);
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let font = text_style.font();
        let color = text_style.color;
        let line_height = px(theme::EDITOR_LINE_HEIGHT);

        let cursor_position = editor.buffer.cursor();
        let selection = editor.buffer.selection();

        let lines: Vec<ShapedLine> = editor
            .buffer
            .lines()
            .iter()
            .enumerate()
            .map(|(line_index, raw_line)| {
                let runs = build_runs(editor, line_index, raw_line, &font, color, active_theme);
                window.text_system().shape_line(
                    SharedString::from(raw_line.clone()),
                    font_size,
                    &runs,
                    None,
                )
            })
            .collect();

        let cursor = editor
            .focus_handle
            .is_focused(window)
            .then(|| {
                let line = lines.get(cursor_position.line)?;
                let x = line.x_for_index(editor.buffer.line_byte_offset(cursor_position));
                let top = line_top(bounds.top(), line_height, cursor_position.line);
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + x, top),
                        size(theme::EDITOR_CURSOR_WIDTH, line_height),
                    ),
                    rgb(active_theme.colors.accent),
                ))
            })
            .flatten();

        let selection_quads = selection.map_or_else(Vec::new, |selection| {
            selection_highlight_quads(selection, &lines, &editor.buffer, bounds, active_theme)
        });

        EditorPrepaintState {
            lines,
            cursor,
            selection: selection_quads,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.editor.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );

        for quad in prepaint.selection.drain(..) {
            window.paint_quad(quad);
        }

        let line_height = px(theme::EDITOR_LINE_HEIGHT);
        for (line_index, line) in prepaint.lines.iter().enumerate() {
            let origin = point(
                bounds.left(),
                line_top(bounds.top(), line_height, line_index),
            );
            line.paint(origin, line_height, window, cx)
                .expect("shaped editor line should paint");
        }

        if let Some(cursor) = prepaint.cursor.take() {
            window.paint_quad(cursor);
        }

        let lines = std::mem::take(&mut prepaint.lines);
        self.editor.update(cx, |editor, _cx| {
            editor.last_lines = lines;
            editor.last_bounds = Some(bounds);
            editor.autoscroll_to_cursor();
        });
    }
}

/// The pixel y-offset of `line_index`'s top edge, within an element whose
/// content starts at `origin`.
// Line indices here are always small (an SQL editor pane, not a huge
// document), so the `usize -> f32` conversion below cannot lose meaningful
// precision.
#[allow(clippy::cast_precision_loss)]
pub(super) fn line_top(origin: Pixels, line_height: Pixels, line_index: usize) -> Pixels {
    origin + line_height * line_index as f32
}

/// The total pixel height of `line_count` lines at the editor's configured
/// line height. See [`line_top`] for why the `usize -> f32` conversion here
/// is safe.
#[allow(clippy::cast_precision_loss)]
fn total_line_height(line_count: usize) -> Pixels {
    px(theme::EDITOR_LINE_HEIGHT * line_count as f32)
}

/// Height of the editor's scrollable content: every line plus the vertical
/// padding above the first and below the last. Used as the scroll region's
/// inner height so its scrollable extent matches the text, not the viewport.
pub(super) fn editor_content_height(line_count: usize) -> Pixels {
    total_line_height(line_count) + px(theme::EDITOR_PADDING_Y * 2.0)
}

/// One highlight quad per line the selection spans, covering the selected
/// columns on the first/last line and the full line width (plus a small pad,
/// so a selected line break reads as selected too) on any line in between.
fn selection_highlight_quads(
    selection: Selection,
    lines: &[ShapedLine],
    buffer: &TextBuffer,
    bounds: Bounds<Pixels>,
    active_theme: &Theme,
) -> Vec<PaintQuad> {
    let (start, end) = selection.ordered();
    let line_height = px(theme::EDITOR_LINE_HEIGHT);

    (start.line..=end.line)
        .filter_map(|line_index| {
            let line = lines.get(line_index)?;
            let start_x = if line_index == start.line {
                line.x_for_index(buffer.line_byte_offset(start))
            } else {
                px(0.0)
            };
            let end_x = if line_index == end.line {
                line.x_for_index(buffer.line_byte_offset(end))
            } else {
                line.width + px(theme::EDITOR_SELECTION_EOL_PAD)
            };
            let top = line_top(bounds.top(), line_height, line_index);
            Some(fill(
                Bounds::from_corners(
                    point(bounds.left() + start_x, top),
                    point(bounds.left() + end_x, top + line_height),
                ),
                rgba(theme::selection_bg(active_theme)),
            ))
        })
        .collect()
}

/// The active IME composition range, clipped to `line_index`'s own
/// char-indexed coordinates, or `None` if there is no active composition or
/// it does not touch this line.
fn active_marked_range_on_line(
    editor: &EditorView,
    line_index: usize,
    line_char_len: usize,
) -> Option<Range<usize>> {
    let marked_range = editor.marked_range.as_ref()?;
    let line_start = editor
        .buffer
        .char_offset_for_position(Position::new(line_index, 0));
    let line_end = line_start + line_char_len;

    let start = marked_range.start.clamp(line_start, line_end);
    let end = marked_range.end.clamp(line_start, line_end);
    (start < end).then_some(start - line_start..end - line_start)
}

/// Style runs for one buffer line: colored per the highlighter's spans for
/// that line, with an underline over the portion (if any) of the active IME
/// composition that falls on this line. A run inside the composition keeps
/// its highlight color and additionally gets the underline, rather than the
/// underline replacing the highlight color.
pub(super) fn build_runs(
    editor: &EditorView,
    line_index: usize,
    raw_line: &str,
    font: &Font,
    color: Hsla,
    active_theme: &Theme,
) -> Vec<TextRun> {
    let line_char_len = raw_line.chars().count();
    let spans = editor.highlighter.spans_for_line(line_index);
    let marked_range = active_marked_range_on_line(editor, line_index, line_char_len);

    // Every span/marked-range boundary that falls inside this line, plus the
    // line's own ends. Between any two consecutive boundaries the set of
    // spans/marked-range covering the text cannot change, so each such
    // interval becomes exactly one run.
    let mut boundaries: Vec<usize> = vec![0, line_char_len];
    for span in spans {
        boundaries.push(span.start.min(line_char_len));
        boundaries.push(span.end.min(line_char_len));
    }
    if let Some(marked) = &marked_range {
        boundaries.push(marked.start);
        boundaries.push(marked.end);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let underline_style = UnderlineStyle {
        color: Some(color),
        thickness: px(1.0),
        wavy: false,
    };

    let mut runs: Vec<TextRun> = Vec::new();
    for window in boundaries.windows(2) {
        let (start, end) = (window[0], window[1]);
        if start >= end {
            continue;
        }

        let run_color = spans
            .iter()
            .find(|span| span.start <= start && end <= span.end)
            .map_or(color, |span| {
                Hsla::from(rgb(theme::syntax_color(active_theme, span.kind)))
            });
        let underlined = marked_range
            .as_ref()
            .is_some_and(|marked| marked.start <= start && end <= marked.end);

        let byte_start = editor
            .buffer
            .line_byte_offset(Position::new(line_index, start));
        let byte_end = editor
            .buffer
            .line_byte_offset(Position::new(line_index, end));
        let run = TextRun {
            len: byte_end - byte_start,
            font: font.clone(),
            color: run_color,
            background_color: None,
            underline: underlined.then_some(underline_style),
            strikethrough: None,
        };

        match runs.last_mut() {
            Some(last) if last.color == run.color && last.underline == run.underline => {
                last.len += run.len;
            }
            _ => runs.push(run),
        }
    }

    if runs.is_empty() {
        runs.push(TextRun {
            len: raw_line.len(),
            font: font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }

    runs
}
