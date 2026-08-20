//! The custom `Element` that shapes and paints a [`TextFieldState`]'s text,
//! cursor, and selection, and wires OS text input into it via
//! `window.handle_input`.

use gpui::{
    App, Bounds, Element, ElementId, ElementInputHandler, Entity, Font, GlobalElementId, Hsla,
    InspectorElementId, LayoutId, PaintQuad, Pixels, ShapedLine, SharedString, Style, TextRun,
    UnderlineStyle, Window, point, prelude::*, relative, rgb,
};
use std::ops::Range;

use super::{
    MARKED_TEXT_UNDERLINE_WIDTH, MASK_GLYPH, TextFieldState, TextFieldStyle, char_count_before,
    should_show_placeholder,
};
use crate::text_field::scroll;
use crate::text_field::theme;
use crate::text_input;
use crate::theme::ActiveTheme;

/// The custom `Element` that shapes and paints the field's text, cursor, and
/// selection, and wires OS text input into `TextFieldState` via
/// `window.handle_input`.
pub(super) struct TextFieldContentElement {
    field: Entity<TextFieldState>,
    style: TextFieldStyle,
}

impl TextFieldContentElement {
    pub(super) fn new(field: Entity<TextFieldState>, style: TextFieldStyle) -> Self {
        Self { field, style }
    }
}

pub(super) struct TextFieldPrepaintState {
    line: ShapedLine,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    /// The horizontal scroll offset this frame's geometry was computed
    /// against, so `paint` shifts the shaped line's paint origin to match
    /// the already-offset cursor and selection quads.
    scroll_offset: Pixels,
}

impl IntoElement for TextFieldContentElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextFieldContentElement {
    type RequestLayoutState = ();
    type PrepaintState = TextFieldPrepaintState;

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
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = self.style.line_height.into();
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
        let theme_colors = cx.theme().colors;
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let font = text_style.font();

        // Pulled into owned/copied locals up front so the field's read
        // borrow ends before this frame's new scroll offset is written back
        // to it below.
        let field = self.field.read(cx);
        let content = field.model.text().to_owned();
        let showing_placeholder = should_show_placeholder(&content);
        let masked = field.masked;
        let disabled = field.disabled;
        let placeholder = field.placeholder.clone();
        let marked_range = field.marked_range.clone();
        let focused = field.focus_handle.is_focused(window);
        let blink_visible = field.blink.visible();
        let cursor_offset = field.model.cursor();
        let active_selection = field.model.selection();
        let old_scroll_offset = field.scroll_offset;
        let cursor_moved = field.last_followed_cursor != Some(cursor_offset);

        let (display_text, runs): (SharedString, Vec<TextRun>) = if showing_placeholder {
            let run = TextRun {
                len: placeholder.len(),
                font: font.clone(),
                color: rgb(theme_colors.text_secondary).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            (placeholder, vec![run])
        } else if masked {
            let text = SharedString::from(MASK_GLYPH.to_string().repeat(content.chars().count()));
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color: text_style.color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            (text, vec![run])
        } else {
            let text = SharedString::from(content.clone());
            let runs = build_runs(marked_range.as_ref(), &text, &font, text_style.color);
            (text, runs)
        };

        // The display index into `display_text`, which is `content` itself
        // unless masked (in which case it is content's char count, since
        // `MASK_GLYPH` is one ASCII byte per char -- see `char_count_before`).
        let display_index = |byte_offset: usize| {
            if masked {
                char_count_before(&content, byte_offset)
            } else {
                byte_offset
            }
        };

        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        // Re-clamped every paint against the content just shaped and this
        // frame's own layout bounds (already resolved by the time
        // `prepaint` runs, so no measurement lag across frames) so an edit
        // or a resize never leaves a stale offset. Only nudged to keep the
        // caret visible when the cursor has actually moved since the last
        // paint that did so -- otherwise a manual wheel-scroll would be
        // re-snapped back to the caret on its very next repaint.
        let caret_x = line.x_for_index(display_index(cursor_offset));
        let mut scroll_offset =
            scroll::clamp_scroll_offset(old_scroll_offset, line.width, bounds.size.width);
        if cursor_moved {
            scroll_offset =
                scroll::follow_caret(scroll_offset, caret_x, line.width, bounds.size.width);
        }
        self.field.update(cx, |field, _cx| {
            field.scroll_offset = scroll_offset;
            field.last_followed_cursor = Some(cursor_offset);
        });

        let cursor = (focused && blink_visible && !disabled).then(|| {
            text_input::caret_quad(
                bounds,
                caret_x - scroll_offset,
                0,
                self.style.line_height,
                self.style.cursor_width,
                rgb(theme_colors.accent),
            )
        });

        let selection = active_selection.map(|range| {
            let span = text_input::SelectionLineSpan {
                line_index: 0,
                start_x: line.x_for_index(display_index(range.start)) - scroll_offset,
                end_x: line.x_for_index(display_index(range.end)) - scroll_offset,
            };
            text_input::selection_quad(
                &span,
                bounds,
                self.style.line_height,
                theme_colors.accent_wash_hover(),
            )
        });

        TextFieldPrepaintState {
            line,
            cursor,
            selection,
            scroll_offset,
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
        let focus_handle = self.field.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.field.clone()),
            cx,
        );

        if let Some(quad) = prepaint.selection.take() {
            window.paint_quad(quad);
        }

        let origin = point(bounds.left() - prepaint.scroll_offset, bounds.top());
        prepaint
            .line
            .paint(origin, theme::FIELD_LINE_HEIGHT, window, cx)
            .expect("shaped text-field line should paint");

        if let Some(cursor) = prepaint.cursor.take() {
            window.paint_quad(cursor);
        }

        let line = std::mem::take(&mut prepaint.line);
        self.field.update(cx, |field, _cx| {
            field.last_line = Some(line);
            field.last_bounds = Some(bounds);
        });
    }
}

/// Style runs for the field's displayed text: an underlined run for the
/// active IME composition (if any), plain runs either side of it.
fn build_runs(
    marked_range: Option<&Range<usize>>,
    text: &str,
    font: &Font,
    color: Hsla,
) -> Vec<TextRun> {
    let base_run = |len: usize| TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };

    let Some(marked_range) = marked_range else {
        return vec![base_run(text.len())];
    };

    let start = marked_range.start.min(text.len());
    let end = marked_range.end.min(text.len());
    if start >= end {
        return vec![base_run(text.len())];
    }

    let mut runs = Vec::with_capacity(3);
    if start > 0 {
        runs.push(base_run(start));
    }
    runs.push(TextRun {
        underline: Some(UnderlineStyle {
            color: Some(color),
            thickness: MARKED_TEXT_UNDERLINE_WIDTH,
            wavy: false,
        }),
        ..base_run(end - start)
    });
    if end < text.len() {
        runs.push(base_run(text.len() - end));
    }
    runs
}
