//! The single-line `TextField`: a `gpui` state entity wiring OS text/IME
//! input into [`FieldModel`], plus the custom-paint element that shapes and
//! paints the field's text, cursor, selection highlight, placeholder, and
//! focus ring.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, Font, GlobalElementId, Hsla,
    InspectorElementId, IsZero, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, Point, Render, ScrollWheelEvent, ShapedLine, SharedString,
    Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, point, prelude::*, px,
    relative, rgb,
};

use crate::text_field::model::{
    BlinkState, CURSOR_BLINK_INTERVAL, FieldModel, byte_offset_for_char_count, char_count_before,
    should_show_placeholder,
};
use crate::text_field::scroll;
use crate::text_field::theme;
use crate::text_input::{self, TextSource};
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
    /// How far the content is scrolled left, in pixels, so text and the
    /// caret past the field's right edge stay reachable. Zero whenever the
    /// content fits the field; re-clamped every paint against the field's
    /// current content and viewport widths, so it never goes stale.
    scroll_offset: Pixels,
    /// The cursor byte offset as of the last paint that nudged
    /// `scroll_offset` to keep it visible. Lets the caret-follow nudge run
    /// only when the cursor actually moved since then, rather than on every
    /// repaint -- otherwise it would immediately re-snap a wheel-scrolled
    /// offset back to the caret on the very next frame.
    last_followed_cursor: Option<usize>,
    /// Style options for the text field
    style: TextFieldStyle,
}

#[derive(Clone, Copy, Debug)]
pub struct TextFieldStyle {
    pub height: Pixels,
    pub padding_x: Pixels,
    pub padding_y: Pixels,
    pub text_size: Pixels,
    pub border_radius: Pixels,
    pub border_w: Pixels,
    pub line_height: Pixels,
    pub cursor_width: Pixels,
}

impl Default for TextFieldStyle {
    fn default() -> Self {
        Self {
            height: theme::FIELD_HEIGHT,
            padding_x: px(theme::FIELD_PADDING_X),
            padding_y: px(theme::FIELD_PADDING_Y),
            text_size: px(theme::FIELD_TEXT_SIZE),
            border_radius: px(theme::FIELD_RADIUS),
            border_w: px(1.0),
            line_height: theme::FIELD_LINE_HEIGHT,
            cursor_width: theme::FIELD_CURSOR_WIDTH,
        }
    }
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
            scroll_offset: Pixels::ZERO,
            last_followed_cursor: None,
            style: TextFieldStyle::default(),
        };
        Self::spawn_blink_loop(cx);
        state
    }

    /// Update field's style options, e.g. padding.
    #[must_use]
    pub fn style(mut self, style: TextFieldStyle) -> Self {
        self.style = style;
        self
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

    /// Replace the placeholder shown while the field is empty, without
    /// notifying observers: for a caller whose own notify already covers
    /// this render (e.g. switching what a shared find input filters).
    pub fn set_placeholder_quiet(&mut self, placeholder: impl Into<SharedString>) {
        self.placeholder = placeholder.into();
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
        // An explicit keystroke always re-follows the caret, even when the
        // cursor's offset did not change (End while already at the end, Home
        // at offset 0): a prior wheel scroll may have moved the viewport
        // away, and the keystroke's intent is to look at the caret.
        self.last_followed_cursor = None;
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
    /// recent paint's shaped line and bounds, corrected for the field's
    /// current scroll offset so a scrolled field's visible pixel positions
    /// resolve to the character actually under the pointer. `None` before
    /// the first paint. When masked, the shaped line is the masked display
    /// string, so its hit-tested index is converted back to a content byte
    /// offset via [`byte_offset_for_char_count`].
    fn byte_offset_for_point(&self, point: Point<Pixels>) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line = self.last_line.as_ref()?;
        let x = (point.x - bounds.left() + self.scroll_offset).max(Pixels::ZERO);
        let display_index = line.closest_index_for_x(x);
        Some(if self.masked {
            byte_offset_for_char_count(self.model.text(), display_index)
        } else {
            display_index
        })
    }

    // -- wheel -------------------------------------------------------------

    /// The horizontal component of a wheel gesture over the field: a native
    /// horizontal delta (trackpad swipe) always pans it; a plain vertical
    /// delta only does when shift is held, so a bare wheel notch over a
    /// field embedded in a scrollable page still scrolls the page.
    fn wheel_delta_x(event: &ScrollWheelEvent, window: &Window) -> Pixels {
        let delta = event.delta.pixel_delta(window.line_height());
        if !delta.x.is_zero() {
            delta.x
        } else if event.modifiers.shift {
            delta.y
        } else {
            Pixels::ZERO
        }
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(bounds) = self.last_bounds else {
            return;
        };
        let Some(line) = self.last_line.as_ref() else {
            return;
        };

        let delta_x = Self::wheel_delta_x(event, window);
        if delta_x.is_zero() {
            return;
        }

        let max_offset = scroll::max_scroll_offset(line.width, bounds.size.width);
        if max_offset <= Pixels::ZERO {
            return;
        }

        let new_offset = scroll::clamp_scroll_offset(
            self.scroll_offset - delta_x,
            line.width,
            bounds.size.width,
        );
        if new_offset == self.scroll_offset {
            return;
        }

        self.scroll_offset = new_offset;
        cx.notify();
    }
}

impl TextSource for TextFieldState {
    fn text(&self) -> String {
        self.model.text().to_owned()
    }

    fn cursor_offset(&self) -> usize {
        self.model.cursor()
    }

    fn selection_range(&self) -> Option<Range<usize>> {
        self.model.selection()
    }

    fn selection_reversed(&self) -> bool {
        self.model.selection_reversed()
    }

    fn set_selection(&mut self, anchor: usize, cursor: usize) {
        self.model.set_selection(anchor, cursor);
    }

    fn marked_range(&self) -> Option<Range<usize>> {
        self.marked_range.clone()
    }

    fn set_marked_range(&mut self, range: Option<Range<usize>>) {
        self.marked_range = range;
    }

    fn replace_range(&mut self, range: Range<usize>, text: &str) {
        self.model.replace_range(range, text);
    }

    fn line_position(&self, offset: usize) -> (usize, usize) {
        (0, offset.min(self.model.text().len()))
    }

    fn offset_for_line_position(&self, _line: usize, in_line_offset: usize) -> usize {
        in_line_offset.min(self.model.text().len())
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
            .h(self.style.height)
            .px(self.style.padding_x)
            .rounded(self.style.border_radius)
            .border(self.style.border_w)
            .border_color(rgb(border_color))
            .bg(rgb(colors.bg_app))
            .text_size(self.style.text_size)
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
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .child(TextFieldContentElement {
                field: cx.entity(),
                style: self.style,
            })
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
        let (text, actual) = text_input::text_for_range(self, range_utf16);
        actual_range.replace(actual);
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(text_input::selected_text_range(self))
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        text_input::marked_text_range(self)
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        text_input::unmark_text(self);
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        text_input::replace_text_in_range(self, range_utf16, new_text);
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
        text_input::replace_and_mark_text_in_range(
            self,
            range_utf16,
            new_text,
            new_selected_range_utf16,
        );
        self.note_keystroke(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let lines = self.last_line.as_slice();
        let bounds = text_input::bounds_for_range(
            self,
            range_utf16,
            element_bounds,
            lines,
            self.style.line_height,
        )?;
        // Shifted by the scroll offset so the IME candidate window anchors
        // to the caret's actual on-screen position on a scrolled field.
        Some(Bounds::new(
            point(bounds.origin.x - self.scroll_offset, bounds.origin.y),
            bounds.size,
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let offset = self.byte_offset_for_point(point)?;
        Some(text_input::character_index_for_point(self, offset))
    }
}

/// The custom `Element` that shapes and paints the field's text, cursor, and
/// selection, and wires OS text input into `TextFieldState` via
/// `window.handle_input`.
struct TextFieldContentElement {
    field: Entity<TextFieldState>,
    style: TextFieldStyle,
}

struct TextFieldPrepaintState {
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

        let cursor = (focused && blink_visible).then(|| {
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

#[cfg(test)]
mod tests;
