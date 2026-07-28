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

use crate::text_field::model::{
    BlinkState, CURSOR_BLINK_INTERVAL, FieldModel, byte_offset_for_char_count,
    byte_offset_to_utf16, byte_range_from_utf16, byte_range_to_utf16, char_count_before,
    should_show_placeholder,
};
use crate::text_field::theme;
use crate::theme::ActiveTheme;

/// Underline thickness for the IME marked-text (composition) span.
const MARKED_TEXT_UNDERLINE_WIDTH: Pixels = px(1.0);

/// The single-byte ASCII glyph a masked field (e.g. a password) shows once
/// per character instead of the real content -- one byte per char keeps the
/// masked display index numerically equal to the content's char count, so
/// [`char_count_before`]/[`byte_offset_for_char_count`] convert between the
/// two without any extra bookkeeping.
const MASK_GLYPH: char = '*';

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
    /// Whether this field's content displays as [`MASK_GLYPH`] repeated
    /// (e.g. a password) rather than as itself. Editing, selection, and
    /// clipboard all still operate on the real content underneath.
    masked: bool,
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
            masked: false,
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

    /// Replace the field's content exactly like [`Self::set_value`], but
    /// without notifying this field's own observers -- for a caller that
    /// owns some other authoritative state this field merely mirrors (e.g.
    /// a parsed-URL field derived from a sibling URL field's text) and is
    /// about to notify on its own behalf, so a repaint still happens but a
    /// redundant "this field changed" reaction does not fire and treat the
    /// refresh as a fresh edit in its own right.
    pub fn set_value_quiet(&mut self, value: impl AsRef<str>) {
        self.model = FieldModel::from_text(value.as_ref());
        self.marked_range = None;
    }

    /// Whether this field currently displays its content masked.
    #[must_use]
    pub fn is_masked(&self) -> bool {
        self.masked
    }

    /// Show ([`MASK_GLYPH`] repeated) or reveal this field's content, e.g. a
    /// password field's "show" toggle. Editing keeps working on the real
    /// content underneath either way.
    pub fn set_masked(&mut self, masked: bool, cx: &mut Context<Self>) {
        self.masked = masked;
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

    /// The byte offset into the real content under `point`, using the most
    /// recent paint's shaped line and bounds. `None` before the first paint.
    /// When masked, the shaped line is the masked display string, so its
    /// hit-tested index is converted back to a content byte offset via
    /// [`byte_offset_for_char_count`].
    fn byte_offset_for_point(&self, point: Point<Pixels>) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line = self.last_line.as_ref()?;
        let x = (point.x - bounds.left()).max(Pixels::ZERO);
        let display_index = line.closest_index_for_x(x);
        Some(if self.masked {
            byte_offset_for_char_count(self.model.text(), display_index)
        } else {
            display_index
        })
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
        let colors = cx.theme().colors;
        let border_color = if self.focused {
            colors.accent
        } else {
            colors.border
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
            // A field placed in a flex row/column defaults to a min-width
            // equal to its content's natural (unwrapped) size, which lets a
            // long value push every ancestor up to a fixed-width container
            // wider than intended -- `min_w_0` lets this field shrink to its
            // parent's available width instead, and `overflow_hidden` then
            // clips whatever no longer fits rather than painting it outside
            // the field's own box.
            .min_w_0()
            .overflow_hidden()
            .h(theme::FIELD_HEIGHT)
            .px(px(theme::FIELD_PADDING_X))
            .rounded(px(theme::FIELD_RADIUS))
            .border_1()
            .border_color(rgb(border_color))
            .bg(rgb(colors.bg_app))
            .text_size(px(theme::FIELD_TEXT_SIZE))
            .text_color(rgb(colors.text_primary))
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
        let theme_colors = cx.theme().colors;
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
                color: rgb(theme_colors.text_secondary).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            (text, vec![run])
        } else if field.masked {
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
            let text = SharedString::from(content.to_owned());
            let runs = build_runs(field, &text, &font, text_style.color);
            (text, runs)
        };

        // The display index into `display_text`, which is `content` itself
        // unless masked (in which case it is content's char count, since
        // `MASK_GLYPH` is one ASCII byte per char -- see `char_count_before`).
        let display_index = |byte_offset: usize| {
            if field.masked {
                char_count_before(content, byte_offset)
            } else {
                byte_offset
            }
        };

        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor = (field.focus_handle.is_focused(window) && field.blink.visible()).then(|| {
            let x = line.x_for_index(display_index(field.model.cursor()));
            fill(
                Bounds::new(
                    point(bounds.left() + x, bounds.top()),
                    size(theme::FIELD_CURSOR_WIDTH, theme::FIELD_LINE_HEIGHT),
                ),
                rgb(theme_colors.accent),
            )
        });

        let selection = field.model.selection().map(|range| {
            let start_x = line.x_for_index(display_index(range.start));
            let end_x = line.x_for_index(display_index(range.end));
            fill(
                Bounds::from_corners(
                    point(bounds.left() + start_x, bounds.top()),
                    point(
                        bounds.left() + end_x,
                        bounds.top() + theme::FIELD_LINE_HEIGHT,
                    ),
                ),
                rgba(theme_colors.accent_wash_hover()),
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
mod tests;
