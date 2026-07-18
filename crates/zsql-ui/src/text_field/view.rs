//! The single-line `TextField`: a `gpui` state entity wiring OS text/IME
//! input into [`FieldModel`], plus the custom-paint element that shapes and
//! paints the field's text, cursor, selection highlight, placeholder, and
//! focus ring.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font, GlobalElementId, Hsla,
    InspectorElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, Point, Render, ShapedLine, SharedString, Style, TextRun,
    UTF16Selection, UnderlineStyle, Window, actions, div, fill, point, prelude::*, px, relative,
    rgb, rgba, size,
};

use crate::colors;
use crate::text_field::model::{
    BlinkState, CURSOR_BLINK_INTERVAL, FieldModel, byte_offset_to_utf16, byte_range_from_utf16,
    byte_range_to_utf16, should_show_placeholder,
};
use crate::text_field::theme;

/// Underline thickness for the IME marked-text (composition) span.
const MARKED_TEXT_UNDERLINE_WIDTH: Pixels = px(1.0);

/// The key context the field's own key bindings are scoped to, so they only
/// fire while a `TextField` is focused. Distinct from `zsql_editor`'s
/// `KEY_CONTEXT`, so the two never contend for the same bindings.
pub const KEY_CONTEXT: &str = "TextField";

actions!(
    zsql_ui_text_field,
    [
        MoveLeft,
        MoveRight,
        MoveHome,
        MoveEnd,
        SelectLeft,
        SelectRight,
        SelectHome,
        SelectEnd,
        SelectAll,
        Backspace,
        DeleteForward,
        Submit,
        Copy,
        Cut,
        Paste,
    ]
);

/// Register the field's actions and key bindings. Call once at startup,
/// before any window that hosts a `TextField` is opened.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("left", MoveLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("right", MoveRight, Some(KEY_CONTEXT)),
        KeyBinding::new("home", MoveHome, Some(KEY_CONTEXT)),
        KeyBinding::new("end", MoveEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-left", SelectLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-right", SelectRight, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-home", SelectHome, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-end", SelectEnd, Some(KEY_CONTEXT)),
        // "secondary-" is gpui's cross-platform primary-modifier prefix: cmd
        // on macOS, ctrl elsewhere.
        KeyBinding::new("secondary-a", SelectAll, Some(KEY_CONTEXT)),
        KeyBinding::new("backspace", Backspace, Some(KEY_CONTEXT)),
        KeyBinding::new("delete", DeleteForward, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", Submit, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-c", Copy, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-x", Cut, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-v", Paste, Some(KEY_CONTEXT)),
    ]);
}

/// Events a `TextField` emits. Mirrors the shape of `gpui-component`'s
/// `InputEvent::PressEnter`: a caller subscribes via `cx.subscribe` to learn
/// when the user pressed Enter, e.g. to submit a form.
pub enum TextFieldEvent {
    Submit,
}

/// A single-line, interactive text input: a bordered field with a teal focus
/// ring, a blinking caret, a muted placeholder, click/drag selection,
/// keyboard editing, clipboard, and IME support. Owns only primitives --
/// strings, byte offsets, colors, and pixel sizes -- so it has no reason to
/// know about any app, driver, or session type.
pub struct TextFieldState {
    model: FieldModel,
    placeholder: SharedString,
    focus_handle: FocusHandle,
    /// The IME composition range, as byte offsets into `model.text()`.
    /// `None` when there is no composition in progress.
    marked_range: Option<Range<usize>>,
    /// Whether a mouse-down is currently dragging out a selection.
    is_selecting: bool,
    blink: BlinkState,
    /// Whether the field held focus as of the most recent render. The blink
    /// loop reads this to skip ticking (and repainting) an unfocused field,
    /// whose caret is never painted anyway.
    focused: bool,
    /// The shaped line from the most recent paint. Used to answer
    /// `EntityInputHandler`'s pixel <-> offset queries and to hit-test mouse
    /// events between frames.
    last_line: Option<ShapedLine>,
    /// The content element's bounds from the most recent paint.
    last_bounds: Option<Bounds<Pixels>>,
}

impl TextFieldState {
    /// Build a field with `placeholder` shown when empty, and `initial_value`
    /// as its starting content (cursor placed at the end).
    #[must_use]
    pub fn new(
        placeholder: impl Into<SharedString>,
        initial_value: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Self {
        let model = initial_value.map_or_else(FieldModel::new, FieldModel::from_text);
        let state = Self {
            model,
            placeholder: placeholder.into(),
            focus_handle: cx.focus_handle(),
            marked_range: None,
            is_selecting: false,
            blink: BlinkState::new(),
            focused: false,
            last_line: None,
            last_bounds: None,
        };
        Self::spawn_blink_loop(cx);
        state
    }

    /// The field's current content.
    #[must_use]
    pub fn value(&self) -> SharedString {
        SharedString::from(self.model.text().to_owned())
    }

    /// Replace the field's content, cursor placed at the end, and clear any
    /// active IME composition.
    pub fn set_value(&mut self, value: impl AsRef<str>, cx: &mut Context<Self>) {
        self.model = FieldModel::from_text(value.as_ref());
        self.marked_range = None;
        cx.notify();
    }

    /// Start the recurring cursor-blink loop on the `gpui` executor: ticks
    /// [`BlinkState`] every [`CURSOR_BLINK_INTERVAL`] and repaints, but only
    /// while the field is focused -- an unfocused field never paints a
    /// caret, so ticking it would just force a repaint with no visual
    /// effect. Runs for the entity's lifetime; exits once the entity is
    /// dropped (its `update` then fails and the loop breaks).
    fn spawn_blink_loop(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CURSOR_BLINK_INTERVAL).await;
                let alive = this.update(cx, |field, cx| {
                    if !field.focused {
                        return;
                    }
                    field.blink.tick(CURSOR_BLINK_INTERVAL);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Any keydown pauses blinking (forces the caret solid) and repaints.
    fn note_keystroke(&mut self, cx: &mut Context<Self>) {
        self.blink.on_keystroke();
        cx.notify();
    }

    // -- movement actions --------------------------------------------------

    fn move_left(&mut self, _: &MoveLeft, _window: &mut Window, cx: &mut Context<Self>) {
        self.model.move_left();
        self.note_keystroke(cx);
    }

    fn move_right(&mut self, _: &MoveRight, _window: &mut Window, cx: &mut Context<Self>) {
        self.model.move_right();
        self.note_keystroke(cx);
    }

    fn move_home(&mut self, _: &MoveHome, _window: &mut Window, cx: &mut Context<Self>) {
        self.model.move_home();
        self.note_keystroke(cx);
    }

    fn move_end(&mut self, _: &MoveEnd, _window: &mut Window, cx: &mut Context<Self>) {
        self.model.move_end();
        self.note_keystroke(cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _window: &mut Window, cx: &mut Context<Self>) {
        self.model.extend_left();
        self.note_keystroke(cx);
    }

    fn select_right(&mut self, _: &SelectRight, _window: &mut Window, cx: &mut Context<Self>) {
        self.model.extend_right();
        self.note_keystroke(cx);
    }

    fn select_home(&mut self, _: &SelectHome, _window: &mut Window, cx: &mut Context<Self>) {
        self.model.extend_home();
        self.note_keystroke(cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _window: &mut Window, cx: &mut Context<Self>) {
        self.model.extend_end();
        self.note_keystroke(cx);
    }

    fn select_all(&mut self, _: &SelectAll, _window: &mut Window, cx: &mut Context<Self>) {
        self.model.select_all();
        self.note_keystroke(cx);
    }

    // -- editing actions -----------------------------------------------

    fn backspace(&mut self, _: &Backspace, _window: &mut Window, cx: &mut Context<Self>) {
        self.model.backspace();
        self.note_keystroke(cx);
    }

    fn delete_forward(&mut self, _: &DeleteForward, _window: &mut Window, cx: &mut Context<Self>) {
        self.model.delete_forward();
        self.note_keystroke(cx);
    }

    /// Enter submits the field's value; it never inserts a newline, keeping
    /// the single-line invariant.
    fn submit(&mut self, _: &Submit, _window: &mut Window, cx: &mut Context<Self>) {
        self.note_keystroke(cx);
        let _span = tracing::info_span!(
            "text_field_submit",
            chars = self.model.text().chars().count()
        )
        .entered();
        tracing::info!("text field submitted");
        cx.emit(TextFieldEvent::Submit);
    }

    // -- clipboard -----------------------------------------------------

    fn copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        self.note_keystroke(cx);
        if self.model.has_selection() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.model.selected_text().to_owned(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, _window: &mut Window, cx: &mut Context<Self>) {
        if self.model.has_selection() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.model.selected_text().to_owned(),
            ));
            self.model.backspace(); // deletes the active selection
        }
        self.note_keystroke(cx);
    }

    fn paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.model.insert_text(&text);
        }
        self.note_keystroke(cx);
    }

    // -- mouse -----------------------------------------------------------

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        self.is_selecting = true;
        if let Some(offset) = self.byte_offset_for_point(event.position) {
            if event.modifiers.shift {
                let anchor = self.model.anchor();
                self.model.set_selection(anchor, offset);
            } else {
                self.model.set_cursor(offset);
            }
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_selecting {
            return;
        }
        if let Some(offset) = self.byte_offset_for_point(event.position) {
            let anchor = self.model.anchor();
            self.model.set_selection(anchor, offset);
            cx.notify();
        }
    }

    /// The byte offset under `point`, using the most recent paint's shaped
    /// line and bounds. `None` before the first paint.
    fn byte_offset_for_point(&self, point: Point<Pixels>) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line = self.last_line.as_ref()?;
        let x = (point.x - bounds.left()).max(Pixels::ZERO);
        Some(line.closest_index_for_x(x))
    }

    /// The flat byte range a `replace_text_in_range`-style call should
    /// operate on: the given UTF-16 range if present, else the active IME
    /// composition range, else the current selection (collapsed to the
    /// cursor if there is none).
    fn resolve_replace_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        if let Some(range_utf16) = range_utf16 {
            return byte_range_from_utf16(self.model.text(), range_utf16);
        }
        if let Some(marked) = self.marked_range.clone() {
            return marked;
        }
        self.model
            .selection()
            .unwrap_or(self.model.cursor()..self.model.cursor())
    }
}

impl Focusable for TextFieldState {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<TextFieldEvent> for TextFieldState {}

impl Render for TextFieldState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.focused = self.focus_handle.is_focused(window);
        let border_color = if self.focused {
            colors::TEAL
        } else {
            colors::LINE
        };

        div()
            // Unique per instance: two fields on the same screen (e.g. a
            // name + URL form) must not share an element id, or gpui conflates
            // their interactivity state and mouse hit-testing so clicks stop
            // reaching one of them.
            .id(("text-field", cx.entity_id()))
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .flex()
            .items_center()
            .w_full()
            .h(theme::FIELD_HEIGHT)
            .px(px(theme::FIELD_PADDING_X))
            .rounded(px(theme::FIELD_RADIUS))
            .border_1()
            .border_color(rgb(border_color))
            .bg(rgb(colors::INK))
            .text_size(px(theme::FIELD_TEXT_SIZE))
            .text_color(rgb(colors::TEXT))
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::move_home))
            .on_action(cx.listener(Self::move_end))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete_forward))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextFieldContentElement { field: cx.entity() })
    }
}

impl EntityInputHandler for TextFieldState {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.model.text();
        let byte_range = byte_range_from_utf16(text, range_utf16);
        actual_range.replace(byte_range_to_utf16(text, byte_range.clone()));
        Some(text[byte_range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let text = self.model.text();
        let range = self
            .model
            .selection()
            .unwrap_or(self.model.cursor()..self.model.cursor());
        Some(UTF16Selection {
            range: byte_range_to_utf16(text, range),
            reversed: self.model.selection_reversed(),
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let text = self.model.text();
        self.marked_range
            .clone()
            .map(|range| byte_range_to_utf16(text, range))
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
        let range = self.resolve_replace_range(range_utf16);
        self.model.replace_range(range, new_text);
        self.marked_range = None;
        self.note_keystroke(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.resolve_replace_range(range_utf16);
        let inserted_start = range.start;
        self.model.replace_range(range, new_text);
        let inserted_len = self.model.cursor() - inserted_start;

        self.marked_range = if inserted_len == 0 {
            None
        } else {
            Some(inserted_start..inserted_start + inserted_len)
        };

        if let Some(relative_utf16) = new_selected_range_utf16 {
            // `new_selected_range_utf16` is UTF-16-relative to `new_text`
            // itself (NSTextInputClient's `setMarkedText:selectedRange:`
            // semantics), not to the field's whole content, so it must be
            // resolved against `new_text` before adding `inserted_start`.
            let relative = byte_range_from_utf16(new_text, relative_utf16);
            let selection_start = inserted_start + relative.start;
            let selection_end = inserted_start + relative.end;
            self.model.set_selection(selection_start, selection_end);
        }

        self.note_keystroke(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let text = self.model.text();
        let byte_range = byte_range_from_utf16(text, range_utf16);
        let line = self.last_line.as_ref()?;
        Some(Bounds::from_corners(
            point(
                element_bounds.left() + line.x_for_index(byte_range.start),
                element_bounds.top(),
            ),
            point(
                element_bounds.left() + line.x_for_index(byte_range.end),
                element_bounds.top() + theme::FIELD_LINE_HEIGHT,
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let offset = self.byte_offset_for_point(point)?;
        Some(byte_offset_to_utf16(self.model.text(), offset))
    }
}

/// The custom `Element` that shapes and paints the field's text, cursor, and
/// selection, and wires OS text input into `TextFieldState` via
/// `window.handle_input`.
struct TextFieldContentElement {
    field: Entity<TextFieldState>,
}

struct TextFieldPrepaintState {
    line: ShapedLine,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
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
        style.size.height = theme::FIELD_LINE_HEIGHT.into();
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
        let field = self.field.read(cx);
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let font = text_style.font();

        let content = field.model.text();
        let showing_placeholder = should_show_placeholder(content);

        let (display_text, runs): (SharedString, Vec<TextRun>) = if showing_placeholder {
            let text = field.placeholder.clone();
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color: rgb(colors::MUTED).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            (text, vec![run])
        } else {
            let text = SharedString::from(content.to_owned());
            let runs = build_runs(field, &text, &font, text_style.color);
            (text, runs)
        };

        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor = (field.focus_handle.is_focused(window) && field.blink.visible()).then(|| {
            let x = line.x_for_index(field.model.cursor());
            fill(
                Bounds::new(
                    point(bounds.left() + x, bounds.top()),
                    size(theme::FIELD_CURSOR_WIDTH, theme::FIELD_LINE_HEIGHT),
                ),
                rgb(colors::TEAL),
            )
        });

        let selection = field.model.selection().map(|range| {
            let start_x = line.x_for_index(range.start);
            let end_x = line.x_for_index(range.end);
            fill(
                Bounds::from_corners(
                    point(bounds.left() + start_x, bounds.top()),
                    point(
                        bounds.left() + end_x,
                        bounds.top() + theme::FIELD_LINE_HEIGHT,
                    ),
                ),
                rgba(theme::FIELD_SELECTION_BG),
            )
        });

        TextFieldPrepaintState {
            line,
            cursor,
            selection,
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

        let origin = point(bounds.left(), bounds.top());
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
fn build_runs(field: &TextFieldState, text: &str, font: &Font, color: Hsla) -> Vec<TextRun> {
    let base_run = |len: usize| TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };

    let Some(marked_range) = field.marked_range.as_ref() else {
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

#[cfg(test)]
impl TextFieldState {
    /// The pixel point that hit-tests back to `offset`, computed from the
    /// most recent paint's shaped line and bounds. Lets mouse-handling tests
    /// drive `on_mouse_down`/`on_mouse_move` with real, paint-derived
    /// coordinates instead of guessed pixel offsets.
    ///
    /// # Panics
    ///
    /// Panics if no paint has run yet.
    fn point_for_offset_for_test(&self, offset: usize) -> Point<Pixels> {
        let bounds = self
            .last_bounds
            .expect("a paint must run before computing a point for an offset");
        let line = self
            .last_line
            .as_ref()
            .expect("a paint must run before computing a point for an offset");
        point(bounds.left() + line.x_for_index(offset), bounds.top())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use gpui::{Bounds, EntityInputHandler, Modifiers, TestAppContext, point};

    use super::{
        Backspace, Copy, Cut, DeleteForward, MoveEnd, MoveLeft, MoveRight, Paste, SelectAll,
        SelectLeft, SelectRight, Submit, TextFieldEvent, TextFieldState,
    };
    use crate::text_field::theme;

    fn build_field<'a>(
        cx: &'a mut TestAppContext,
        initial_value: Option<&str>,
    ) -> (
        gpui::Entity<TextFieldState>,
        &'a mut gpui::VisualTestContext,
    ) {
        cx.add_window_view(|window, cx| {
            let state = TextFieldState::new("Search", initial_value, cx);
            window.focus(&state.focus_handle);
            state
        })
    }

    // -- render smoke --------------------------------------------------

    #[gpui::test]
    fn field_renders_one_frame_without_panicking_when_unfocused(cx: &mut TestAppContext) {
        let (_field, vcx) = cx.add_window_view(|_window, cx| {
            TextFieldState::new("Search connections", Some("staging"), cx)
        });
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn field_renders_one_frame_without_panicking_when_focused(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("select 1"));
        vcx.run_until_parked();
        // Exercise the selection-quad and cursor-quad paint paths too.
        field.update(vcx, |field, cx| {
            field.model.set_selection(0, 3);
            cx.notify();
        });
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn field_with_empty_content_renders_the_placeholder_without_panicking(cx: &mut TestAppContext) {
        let (_field, vcx) = build_field(cx, None);
        vcx.run_until_parked();
    }

    // -- value / round-trip ----------------------------------------------

    #[gpui::test]
    fn value_after_set_value_round_trips(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("initial"));
        field.update(vcx, |field, cx| field.set_value("replaced", cx));
        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.value().as_ref(), "replaced");
        });
    }

    // -- submit ----------------------------------------------------------

    #[gpui::test]
    fn enter_emits_submit_and_no_other_key_does(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("select 1"));

        let submits: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let recorded = submits.clone();
        vcx.update(|_window, cx| {
            cx.subscribe(&field, move |_field, event, _cx| {
                if matches!(event, TextFieldEvent::Submit) {
                    *recorded.lock().expect("submit count lock poisoned") += 1;
                }
            })
            .detach();
        });

        vcx.dispatch_action(MoveRight);
        vcx.dispatch_action(Backspace);
        vcx.dispatch_action(SelectAll);
        vcx.dispatch_action(Copy);
        vcx.run_until_parked();
        assert_eq!(
            *submits.lock().expect("submit count lock poisoned"),
            0,
            "no non-Enter key should submit"
        );

        vcx.dispatch_action(Submit);
        vcx.run_until_parked();
        assert_eq!(
            *submits.lock().expect("submit count lock poisoned"),
            1,
            "Enter should submit exactly once"
        );
    }

    #[gpui::test]
    fn enter_never_inserts_a_newline(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("select 1"));
        vcx.dispatch_action(MoveEnd);
        vcx.dispatch_action(Submit);
        vcx.run_until_parked();
        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.value().as_ref(), "select 1");
        });
    }

    // -- movement / editing actions ------------------------------------

    #[gpui::test]
    fn move_right_and_backspace_actions_delegate_to_the_model(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("abc"));
        field.update(vcx, |field, _cx| field.model.set_cursor(0));

        vcx.dispatch_action(MoveRight);
        vcx.dispatch_action(MoveRight);
        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.model.cursor(), 2);
        });

        vcx.dispatch_action(Backspace);
        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.value().as_ref(), "ac");
            assert_eq!(field.model.cursor(), 1);
        });
    }

    #[gpui::test]
    fn move_left_and_delete_forward_actions_delegate_to_the_model(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("abc"));

        vcx.dispatch_action(MoveLeft);
        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.model.cursor(), 2);
        });

        vcx.dispatch_action(DeleteForward);
        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.value().as_ref(), "ab");
            assert_eq!(field.model.cursor(), 2);
        });
    }

    #[gpui::test]
    fn shift_extend_and_select_all_actions_delegate_to_the_model(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("hello"));
        field.update(vcx, |field, _cx| field.model.set_cursor(0));

        vcx.dispatch_action(SelectRight);
        vcx.dispatch_action(SelectRight);
        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.model.selected_text(), "he");
        });

        vcx.dispatch_action(SelectLeft);
        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.model.selected_text(), "h");
        });

        vcx.dispatch_action(SelectAll);
        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.model.selected_text(), "hello");
        });
    }

    // -- clipboard -------------------------------------------------------

    #[gpui::test]
    fn cut_copies_the_selection_to_the_clipboard_then_removes_it(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("hello world"));
        field.update(vcx, |field, _cx| field.model.set_selection(6, 11));

        vcx.dispatch_action(Cut);
        vcx.run_until_parked();

        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.value().as_ref(), "hello ");
        });
        vcx.update(|_window, cx| {
            let clipboard = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .expect("cut should have written to the clipboard");
            assert_eq!(clipboard, "world");
        });
    }

    #[gpui::test]
    fn copy_writes_the_selection_to_the_clipboard_and_leaves_content_unchanged(
        cx: &mut TestAppContext,
    ) {
        let (field, vcx) = build_field(cx, Some("hello world"));
        field.update(vcx, |field, _cx| field.model.set_selection(6, 11));

        vcx.dispatch_action(Copy);
        vcx.run_until_parked();

        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.value().as_ref(), "hello world");
            assert_eq!(field.model.selected_text(), "world");
        });
        vcx.update(|_window, cx| {
            let clipboard = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .expect("copy should have written to the clipboard");
            assert_eq!(clipboard, "world");
        });
    }

    #[gpui::test]
    fn copy_with_no_selection_writes_nothing_to_the_clipboard(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("hello world"));
        vcx.update(|_window, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("sentinel".to_owned()));
        });
        field.update(vcx, |field, _cx| field.model.set_cursor(3));
        assert!(!field.read_with(vcx, |field, _cx| field.model.has_selection()));

        vcx.dispatch_action(Copy);
        vcx.run_until_parked();

        vcx.update(|_window, cx| {
            let clipboard = cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .expect("clipboard should still hold the sentinel");
            assert_eq!(
                clipboard, "sentinel",
                "copy with no selection must not touch the clipboard"
            );
        });
    }

    #[gpui::test]
    fn paste_inserts_the_clipboard_text_at_the_cursor(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("select "));
        vcx.update(|_window, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("1".to_owned()));
        });
        field.update(vcx, |field, _cx| field.model.move_end());

        vcx.dispatch_action(Paste);
        vcx.run_until_parked();

        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.value().as_ref(), "select 1");
        });
    }

    // -- typed / IME input --------------------------------------------------

    #[gpui::test]
    fn typing_inserts_characters_into_the_field_via_the_input_handler(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, None);
        vcx.simulate_input("select 1");
        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.value().as_ref(), "select 1");
        });
    }

    #[gpui::test]
    fn ime_composition_marks_then_commits_text(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("select "));
        field.update(vcx, |field, _cx| field.model.move_end());

        vcx.update(|window, cx| {
            field.update(cx, |field, cx| {
                field.replace_and_mark_text_in_range(None, "n", Some(1..1), window, cx);
            });
        });
        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.value().as_ref(), "select n");
            assert_eq!(field.marked_range, Some(7..8));
        });

        vcx.update(|window, cx| {
            field.update(cx, |field, cx| {
                field.replace_text_in_range(None, "now", window, cx);
            });
        });
        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.value().as_ref(), "select now");
            assert!(
                field.marked_range.is_none(),
                "committing text must clear the composition range"
            );
        });
    }

    #[gpui::test]
    fn marked_text_range_converts_byte_range_to_utf16(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, None);
        field.update(vcx, |field, cx| {
            // Use multi-byte emoji to distinguish byte from UTF-16 offsets.
            field.set_value("a\u{1F600}b", cx);
        });

        vcx.update(|window, cx| {
            field.update(cx, |field, cx| {
                // Mark the emoji (bytes 1..5) which is UTF-16 range 1..3.
                field.replace_and_mark_text_in_range(
                    Some(1..3),
                    "\u{1F600}",
                    Some(0..2),
                    window,
                    cx,
                );
            });
        });

        let marked_range = vcx
            .update(|window, cx| field.update(cx, |field, cx| field.marked_text_range(window, cx)));

        assert_eq!(
            marked_range,
            Some(1..3),
            "marked_text_range must convert byte range to UTF-16 range"
        );
    }

    #[gpui::test]
    fn text_for_range_returns_slice_and_writes_actual_range(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, None);
        field.update(vcx, |field, cx| {
            // Use multi-byte emoji to distinguish byte from UTF-16 offsets.
            field.set_value("a\u{1F600}b", cx);
        });
        vcx.run_until_parked();

        let (text, actual_range) = vcx.update(|window, cx| {
            field.update(cx, |field, cx| {
                let mut actual = None;
                let text = field.text_for_range(1..3, &mut actual, window, cx);
                (text, actual)
            })
        });

        assert_eq!(
            text,
            Some("\u{1F600}".to_owned()),
            "text_for_range must return the slice for the UTF-16 range"
        );
        assert_eq!(
            actual_range,
            Some(1..3),
            "text_for_range must write back the UTF-16 range"
        );
    }

    #[gpui::test]
    fn character_index_for_point_converts_byte_offset_to_utf16(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("a\u{1F600}b"));
        vcx.run_until_parked();

        let point = field.read_with(vcx, |field, _cx| {
            // Get the point for byte offset 5 (after the emoji), which is UTF-16 index 3.
            field.point_for_offset_for_test(5)
        });

        let utf16_index = vcx.update(|window, cx| {
            field.update(cx, |field, cx| {
                field.character_index_for_point(point, window, cx)
            })
        });

        assert_eq!(
            utf16_index,
            Some(3),
            "character_index_for_point must convert byte offset to UTF-16 index"
        );
    }

    #[gpui::test]
    fn bounds_for_range_returns_correct_geometry(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("a\u{1F600}b"));
        vcx.run_until_parked();

        let bounds = vcx.update(|window, cx| {
            field.update(cx, |field, cx| {
                let element_bounds = field.last_bounds.expect("paint must have run");
                // Request bounds for the emoji (UTF-16 range 1..3, byte range 1..5).
                field.bounds_for_range(1..3, element_bounds, window, cx)
            })
        });

        field.read_with(vcx, |field, _cx| {
            let line = field
                .last_line
                .as_ref()
                .expect("paint must have run");
            let element_bounds = field
                .last_bounds
                .expect("paint must have run");

            let expected_bounds = Bounds::from_corners(
                point(
                    element_bounds.left() + line.x_for_index(1),
                    element_bounds.top(),
                ),
                point(
                    element_bounds.left() + line.x_for_index(5),
                    element_bounds.top() + theme::FIELD_LINE_HEIGHT,
                ),
            );

            assert_eq!(
                bounds, Some(expected_bounds),
                "bounds_for_range must return the correct Bounds using line.x_for_index and FIELD_LINE_HEIGHT"
            );
        });
    }

    #[gpui::test]
    fn unmark_text_clears_the_ime_composition(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, None);
        vcx.update(|window, cx| {
            field.update(cx, |field, cx| {
                field.replace_and_mark_text_in_range(None, "ab", Some(0..2), window, cx);
                assert!(field.marked_range.is_some());
                field.unmark_text(window, cx);
            });
        });
        field.read_with(vcx, |field, _cx| {
            assert!(field.marked_range.is_none());
        });
    }

    #[gpui::test]
    fn utf16_offsets_round_trip_through_a_surrogate_pair(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, None);
        field.update(vcx, |field, cx| {
            // U+1F600 sits outside the BMP, so it is one `char` but two
            // UTF-16 code units -- exactly the case a naive byte- or
            // char-count implementation of the UTF-16 boundary math would
            // get wrong.
            field.set_value("a\u{1F600}b", cx);
            field.model.set_selection(1, 5);
        });

        let selection = vcx.update(|window, cx| {
            field.update(cx, |field, cx| {
                field
                    .selected_text_range(false, window, cx)
                    .expect("a selection should report a UTF-16 range")
            })
        });
        assert_eq!(
            selection.range,
            1..3,
            "the emoji occupies UTF-16 code units 1..3"
        );
    }

    // -- mouse -------------------------------------------------------------

    #[gpui::test]
    fn click_places_the_cursor_at_the_clicked_offset(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("hello world"));
        vcx.run_until_parked();

        let click_point = field.read_with(vcx, |field, _cx| field.point_for_offset_for_test(5));
        vcx.simulate_click(click_point, Modifiers::default());
        vcx.run_until_parked();

        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.model.cursor(), 5);
            assert!(!field.model.has_selection());
        });
    }

    #[gpui::test]
    fn shift_click_extends_a_selection_from_the_prior_cursor_position(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("hello world"));
        vcx.run_until_parked();

        let first_click = field.read_with(vcx, |field, _cx| field.point_for_offset_for_test(2));
        vcx.simulate_click(first_click, Modifiers::default());
        vcx.run_until_parked();
        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.model.cursor(), 2);
            assert!(!field.model.has_selection());
        });

        let shift_click = field.read_with(vcx, |field, _cx| field.point_for_offset_for_test(7));
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        vcx.simulate_click(shift_click, shift);
        vcx.run_until_parked();

        field.read_with(vcx, |field, _cx| {
            assert_eq!(
                field.model.selection(),
                Some(2..7),
                "shift-click should extend the selection from the fixed anchor at the prior cursor"
            );
        });
    }

    #[gpui::test]
    fn drag_extends_a_selection(cx: &mut TestAppContext) {
        let (field, vcx) = build_field(cx, Some("hello world"));
        vcx.run_until_parked();

        let start = field.read_with(vcx, |field, _cx| field.point_for_offset_for_test(0));
        let end = field.read_with(vcx, |field, _cx| field.point_for_offset_for_test(5));

        vcx.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position: start,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(gpui::MouseMoveEvent {
            position: end,
            pressed_button: Some(gpui::MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        vcx.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position: end,
            modifiers: Modifiers::default(),
            click_count: 1,
        });
        vcx.run_until_parked();

        field.read_with(vcx, |field, _cx| {
            assert_eq!(field.model.selection(), Some(0..5));
        });
    }
}
