//! The SQL editor pane: a `gpui` view over [`crate::TextBuffer`]. Wires OS
//! keyboard/IME input into the buffer via `EntityInputHandler`, paints the
//! buffer's lines with a line-number gutter, a blinking-free cursor, and a
//! selection highlight, and runs the buffer's query text through the
//! caller-supplied [`QueryRunner`] seam on cmd/ctrl-enter or a call to
//! [`EditorView::run_current_query`] from the embedding app.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Div, Element, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, FocusHandle, Focusable, Font, GlobalElementId, Hsla,
    InspectorElementId, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, Point, Render, ScrollHandle, ShapedLine, SharedString, Style,
    TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill, point, prelude::*, px,
    relative, rgb, rgba, size,
};
use zsql_ui::colors;

use crate::theme;
use crate::{Highlighter, Position, Selection, SqlHighlighter, TextBuffer};

/// The key context the editor's own key bindings are scoped to, so they only
/// fire while the editor pane is focused.
pub const KEY_CONTEXT: &str = "SqlEditor";

/// Runs the SQL text an [`EditorView`] is asked to run: the selection if
/// there is one, otherwise the whole buffer. Invoked with the entity's own
/// `Context` so the closure can drive whatever app state the embedding
/// binary owns (running the query through a session, relabeling a results
/// view) via the same `cx.update` machinery `EditorView` itself uses. This
/// is the seam that keeps this crate free of any app, driver, or session
/// type.
pub type QueryRunner = Box<dyn Fn(String, &mut Context<EditorView>)>;

/// Invoked after a manual edit to the buffer's text -- typing, paste,
/// backspace/delete, or an IME commit -- but not after cursor movement,
/// selection changes alone, or a programmatic [`EditorView::set_text`]. Lets
/// an embedding app react to the buffer's first real edit (e.g. converting
/// an auto-generated tab into a normal one) without this crate knowing
/// anything about tabs, relations, or sessions.
pub type EditListener = Box<dyn Fn(&mut Context<EditorView>)>;

actions!(
    zsql_editor,
    [
        RunQuery,
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        MoveLineStart,
        MoveLineEnd,
        MoveDocumentStart,
        MoveDocumentEnd,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectLineStart,
        SelectLineEnd,
        SelectDocumentStart,
        SelectDocumentEnd,
        SelectAll,
        Backspace,
        DeleteForward,
        Newline,
        Copy,
        Cut,
        Paste,
    ]
);

/// Register the editor's actions and key bindings. Call once at startup,
/// before any window that hosts an [`EditorView`] is opened.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("left", MoveLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("right", MoveRight, Some(KEY_CONTEXT)),
        KeyBinding::new("up", MoveUp, Some(KEY_CONTEXT)),
        KeyBinding::new("down", MoveDown, Some(KEY_CONTEXT)),
        KeyBinding::new("home", MoveLineStart, Some(KEY_CONTEXT)),
        KeyBinding::new("end", MoveLineEnd, Some(KEY_CONTEXT)),
        // "secondary-" is gpui's cross-platform primary-modifier prefix: cmd
        // on macOS, ctrl elsewhere (see `Modifiers::secondary`). Using it
        // here means these bindings work as Ctrl+<key> on Linux without a
        // separate ctrl- binding, unlike `RunQuery` below which deliberately
        // dual-binds cmd-enter and ctrl-enter explicitly.
        KeyBinding::new("secondary-up", MoveDocumentStart, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-down", MoveDocumentEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-left", SelectLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-right", SelectRight, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-up", SelectUp, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-down", SelectDown, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-home", SelectLineStart, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-end", SelectLineEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-secondary-up", SelectDocumentStart, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-secondary-down", SelectDocumentEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-a", SelectAll, Some(KEY_CONTEXT)),
        KeyBinding::new("backspace", Backspace, Some(KEY_CONTEXT)),
        KeyBinding::new("delete", DeleteForward, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", Newline, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-c", Copy, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-x", Cut, Some(KEY_CONTEXT)),
        KeyBinding::new("secondary-v", Paste, Some(KEY_CONTEXT)),
        KeyBinding::new("cmd-enter", RunQuery, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-enter", RunQuery, Some(KEY_CONTEXT)),
    ]);
}

/// The SQL editor pane: owns the buffer, the OS input handler state, and the
/// last frame's shaped lines/bounds (needed to translate between pixel
/// positions and buffer positions for the cursor, selection, and mouse).
pub struct EditorView {
    buffer: TextBuffer,
    highlighter: Box<dyn Highlighter>,
    focus_handle: FocusHandle,
    run_query: QueryRunner,
    /// Invoked after every manual text edit; see [`EditListener`].
    on_edit: Option<EditListener>,
    /// Whether this editor renders as a compact, single-line strip -- no
    /// line-number gutter -- instead of the full multi-line pane.
    compact: bool,
    /// The IME composition range, as flat character offsets into
    /// `buffer.text()`. `None` when there is no composition in progress.
    marked_range: Option<Range<usize>>,
    /// Whether a mouse-down is currently dragging out a selection.
    is_selecting: bool,
    /// Shaped lines from the most recent paint, one per buffer line. Used to
    /// answer `EntityInputHandler`'s pixel <-> position queries and to hit
    /// test mouse events between frames.
    last_lines: Vec<ShapedLine>,
    /// The content element's bounds from the most recent paint.
    last_bounds: Option<Bounds<Pixels>>,
    /// Scroll position of the code pane, used to keep the cursor in view.
    scroll_handle: ScrollHandle,
    /// Cursor position at the last autoscroll. Autoscroll only fires when the
    /// cursor moves, so it never fights a manual scroll.
    last_autoscroll_cursor: Option<Position>,
}

impl EditorView {
    /// Build an editor over an empty buffer. Running the current query (the
    /// selection if there is one, else the whole buffer) is delegated to
    /// `run_query`.
    #[must_use]
    pub fn new(run_query: QueryRunner, cx: &mut Context<Self>) -> Self {
        Self {
            buffer: TextBuffer::new(),
            highlighter: Box::new(SqlHighlighter::new()),
            focus_handle: cx.focus_handle(),
            run_query,
            on_edit: None,
            compact: false,
            marked_range: None,
            is_selecting: false,
            last_lines: Vec::new(),
            last_bounds: None,
            scroll_handle: ScrollHandle::new(),
            last_autoscroll_cursor: None,
        }
    }

    /// Replace the buffer's entire text, e.g. to seed a freshly-opened tab
    /// with auto-generated SQL. Does not invoke the [`EditListener`]: this
    /// is a programmatic write, not a manual edit.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buffer = TextBuffer::from_text(text);
        self.sync_highlighter();
        cx.notify();
    }

    /// The buffer's current text.
    #[must_use]
    pub fn text(&self) -> String {
        self.buffer.text()
    }

    /// Install the listener invoked after every manual text edit, replacing
    /// any previously-installed listener.
    pub fn set_on_edit(&mut self, listener: EditListener) {
        self.on_edit = Some(listener);
    }

    /// Whether this editor currently renders in its compact, single-line
    /// style (see [`EditorView::set_compact`]).
    #[must_use]
    pub fn is_compact(&self) -> bool {
        self.compact
    }

    /// Switch between the compact, single-line strip style and the normal
    /// full multi-line pane.
    pub fn set_compact(&mut self, compact: bool) {
        self.compact = compact;
    }

    /// Notify the view that the buffer's text just changed from a manual
    /// edit -- as opposed to cursor movement, a selection change alone, or
    /// [`EditorView::set_text`] -- and, if one is installed, run the
    /// [`EditListener`].
    fn notify_edit(&mut self, cx: &mut Context<Self>) {
        self.sync_highlighter();
        cx.notify();
        if let Some(on_edit) = &self.on_edit {
            on_edit(cx);
        }
    }

    /// Re-derive the highlighter's cached spans from the buffer's current
    /// text. Called by every path that can change the buffer's text.
    fn sync_highlighter(&mut self) {
        self.highlighter.set_text(&self.buffer.text());
    }

    #[allow(clippy::cast_precision_loss)]
    fn autoscroll_to_cursor(&mut self) {
        let cursor = self.buffer.cursor();
        if self.last_autoscroll_cursor == Some(cursor) {
            return;
        }
        let viewport = self.scroll_handle.bounds().size.height;
        if viewport <= px(0.0) {
            return;
        }
        let line_height = px(theme::EDITOR_LINE_HEIGHT);
        let cursor_top = px(theme::EDITOR_PADDING_Y) + line_height * cursor.line as f32;
        let cursor_bottom = cursor_top + line_height;
        let scroll = -self.scroll_handle.offset().y;
        let new_scroll = if cursor_top < scroll {
            cursor_top
        } else if cursor_bottom > scroll + viewport {
            cursor_bottom - viewport
        } else {
            scroll
        };
        if new_scroll != scroll {
            self.scroll_handle.set_offset(point(px(0.0), -new_scroll));
        }
        self.last_autoscroll_cursor = Some(cursor);
    }

    // -- movement actions --------------------------------------------------

    fn move_left(&mut self, _: &MoveLeft, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_left();
        cx.notify();
    }

    fn move_right(&mut self, _: &MoveRight, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_right();
        cx.notify();
    }

    fn move_up(&mut self, _: &MoveUp, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_up();
        cx.notify();
    }

    fn move_down(&mut self, _: &MoveDown, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_down();
        cx.notify();
    }

    fn move_line_start(&mut self, _: &MoveLineStart, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_line_start();
        cx.notify();
    }

    fn move_line_end(&mut self, _: &MoveLineEnd, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.move_line_end();
        cx.notify();
    }

    fn move_document_start(
        &mut self,
        _: &MoveDocumentStart,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.move_document_start();
        cx.notify();
    }

    fn move_document_end(
        &mut self,
        _: &MoveDocumentEnd,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.move_document_end();
        cx.notify();
    }

    // -- shift-extend movement actions --------------------------------------

    fn select_left(&mut self, _: &SelectLeft, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.extend_left();
        cx.notify();
    }

    fn select_right(&mut self, _: &SelectRight, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.extend_right();
        cx.notify();
    }

    fn select_up(&mut self, _: &SelectUp, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.extend_up();
        cx.notify();
    }

    fn select_down(&mut self, _: &SelectDown, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.extend_down();
        cx.notify();
    }

    fn select_line_start(
        &mut self,
        _: &SelectLineStart,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.extend_line_start();
        cx.notify();
    }

    fn select_line_end(&mut self, _: &SelectLineEnd, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.extend_line_end();
        cx.notify();
    }

    fn select_document_start(
        &mut self,
        _: &SelectDocumentStart,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.extend_document_start();
        cx.notify();
    }

    fn select_document_end(
        &mut self,
        _: &SelectDocumentEnd,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.buffer.extend_document_end();
        cx.notify();
    }

    fn select_all(&mut self, _: &SelectAll, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.select_all();
        cx.notify();
    }

    // -- editing actions -----------------------------------------------

    fn backspace(&mut self, _: &Backspace, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.backspace();
        self.notify_edit(cx);
    }

    fn delete_forward(&mut self, _: &DeleteForward, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.delete_forward();
        self.notify_edit(cx);
    }

    fn newline(&mut self, _: &Newline, _window: &mut Window, cx: &mut Context<Self>) {
        self.buffer.insert_newline();
        self.notify_edit(cx);
    }

    // -- clipboard -----------------------------------------------------

    fn copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.has_selection() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.buffer.selected_text()));
        }
    }

    fn cut(&mut self, _: &Cut, _window: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.has_selection() {
            cx.write_to_clipboard(ClipboardItem::new_string(self.buffer.selected_text()));
            self.buffer.backspace(); // deletes the active selection
            self.notify_edit(cx);
        }
    }

    fn paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.buffer.insert_text(&text);
            self.notify_edit(cx);
        }
    }

    // -- run -------------------------------------------------------------

    fn run_query(&mut self, _: &RunQuery, _window: &mut Window, cx: &mut Context<Self>) {
        self.run_current_query(cx);
    }

    /// Run the buffer's query text (the selection if there is one,
    /// otherwise the whole document) through the `run_query` seam. A blank
    /// query is a no-op. The single implementation the `RunQuery` keybinding
    /// and any embedding app's own Run affordance both call, so running the
    /// current query never has more than one code path.
    pub fn run_current_query(&mut self, cx: &mut Context<Self>) {
        let sql = self.buffer.query_text();
        if sql.trim().is_empty() {
            return;
        }

        tracing::info!(chars = sql.chars().count(), "editor running query");
        (self.run_query)(sql, cx);
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
        if let Some(position) = self.position_for_point(event.position) {
            if event.modifiers.shift {
                let anchor = self
                    .buffer
                    .selection()
                    .map_or(self.buffer.cursor(), |s| s.anchor);
                self.buffer.set_selection(anchor, position);
            } else {
                self.buffer.set_cursor(position);
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
        if let Some(position) = self.position_for_point(event.position) {
            let anchor = self
                .buffer
                .selection()
                .map_or(self.buffer.cursor(), |s| s.anchor);
            self.buffer.set_selection(anchor, position);
            cx.notify();
        }
    }

    /// The buffer position under `point`, using the most recent paint's
    /// shaped lines and bounds. `None` before the first paint.
    fn position_for_point(&self, point: Point<Pixels>) -> Option<Position> {
        let bounds = self.last_bounds?;
        if self.last_lines.is_empty() {
            return None;
        }

        let relative_y = point.y - bounds.top();
        let row = if relative_y <= Pixels::ZERO {
            0
        } else {
            let mut row = 0;
            let mut row_top = Pixels::ZERO;
            let line_height = px(theme::EDITOR_LINE_HEIGHT);
            while row + 1 < self.last_lines.len() && relative_y >= row_top + line_height {
                row += 1;
                row_top += line_height;
            }
            row
        };

        let line = self.last_lines.get(row)?;
        let byte_index = line.closest_index_for_x(point.x - bounds.left());
        let raw_line = self.buffer.lines().get(row)?;
        let column = raw_line
            .get(..byte_index)
            .map_or(0, |prefix| prefix.chars().count());
        Some(Position::new(row, column))
    }

    // -- rendering helpers -------------------------------------------------

    /// The line-number gutter, one row per buffer line, matching the
    /// content element's row spacing so the two stay aligned.
    fn render_gutter(line_count: usize, cursor_line: usize) -> gpui::Stateful<Div> {
        let mut gutter = div()
            .id("sql-editor-gutter")
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(theme::EDITOR_GUTTER_WIDTH)
            .py(px(theme::EDITOR_PADDING_Y))
            .border_r_1()
            .border_color(rgb(colors::LINE_SOFT))
            .bg(rgb(colors::INK));

        for line_index in 0..line_count {
            let is_current = line_index == cursor_line;
            gutter = gutter.child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .justify_end()
                    .h(px(theme::EDITOR_LINE_HEIGHT))
                    .px(px(theme::EDITOR_GUTTER_PADDING_X))
                    .text_color(rgb(if is_current {
                        colors::MUTED
                    } else {
                        colors::FAINT
                    }))
                    .child((line_index + 1).to_string()),
            );
        }
        gutter
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let line_count = self.buffer.lines().len();
        let cursor_line = self.buffer.cursor().line;
        let compact = self.compact;

        div()
            .id("sql-editor")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .flex()
            .flex_col()
            // Height comes from the parent workspace column: that wrapper
            // gives this pane an explicit, resizable pixel height (see
            // `workspace.rs`), and `h_full()` fills exactly that rather than
            // this view hardcoding its own fixed size.
            .h_full()
            .bg(rgb(colors::INK))
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::move_line_start))
            .on_action(cx.listener(Self::move_line_end))
            .on_action(cx.listener(Self::move_document_start))
            .on_action(cx.listener(Self::move_document_end))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_line_start))
            .on_action(cx.listener(Self::select_line_end))
            .on_action(cx.listener(Self::select_document_start))
            .on_action(cx.listener(Self::select_document_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete_forward))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::run_query))
            .child(
                div()
                    .id("sql-editor-code")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .font_family("monospace")
                    .text_size(px(theme::EDITOR_TEXT_SIZE))
                    .text_color(rgb(colors::TEXT))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_move(cx.listener(Self::on_mouse_move))
                    .child(
                        // A single content row with a definite height equal to
                        // all the lines plus padding. The scroll container
                        // measures its scrollable extent from this child's
                        // height, so it must be the real content height rather
                        // than stretch to the viewport (which would leave
                        // nothing to scroll).
                        div()
                            .flex()
                            .flex_row()
                            .flex_none()
                            .h(editor_content_height(line_count))
                            .when(!compact, |el| {
                                el.child(Self::render_gutter(line_count, cursor_line))
                            })
                            .child(
                                div()
                                    .id("sql-editor-text")
                                    .flex_1()
                                    .min_w_0()
                                    .px(px(theme::EDITOR_PADDING_X))
                                    .py(px(theme::EDITOR_PADDING_Y))
                                    .child(EditorContentElement {
                                        editor: cx.entity(),
                                    }),
                            ),
                    ),
            )
    }
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.buffer.text();
        let char_range = char_range_from_utf16(&text, range_utf16);
        actual_range.replace(char_range_to_utf16(&text, char_range.clone()));
        Some(
            text.chars()
                .skip(char_range.start)
                .take(char_range.len())
                .collect(),
        )
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let text = self.buffer.text();
        let cursor_offset = self.buffer.char_offset_for_position(self.buffer.cursor());
        let (start, end, reversed) = match self.buffer.selection() {
            Some(selection) => {
                let (start_pos, end_pos) = selection.ordered();
                let start = self.buffer.char_offset_for_position(start_pos);
                let end = self.buffer.char_offset_for_position(end_pos);
                (start, end, selection.cursor == start_pos)
            }
            None => (cursor_offset, cursor_offset, false),
        };
        Some(UTF16Selection {
            range: char_range_to_utf16(&text, start..end),
            reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let text = self.buffer.text();
        self.marked_range
            .clone()
            .map(|range| char_range_to_utf16(&text, range))
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
        let char_range = self.resolve_replace_range(range_utf16);
        let start_pos = self.buffer.position_for_char_offset(char_range.start);
        let end_pos = self.buffer.position_for_char_offset(char_range.end);
        self.buffer.set_selection(start_pos, end_pos);
        self.buffer.insert_text(new_text);
        self.marked_range = None;
        self.notify_edit(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let char_range = self.resolve_replace_range(range_utf16);
        let start_pos = self.buffer.position_for_char_offset(char_range.start);
        let end_pos = self.buffer.position_for_char_offset(char_range.end);
        self.buffer.set_selection(start_pos, end_pos);
        self.buffer.insert_text(new_text);

        let inserted_start = char_range.start;
        let inserted_len = new_text.chars().count();
        self.marked_range = if new_text.is_empty() {
            None
        } else {
            Some(inserted_start..inserted_start + inserted_len)
        };

        if let Some(relative_utf16) = new_selected_range_utf16 {
            // `new_selected_range_utf16` is UTF-16-relative to `new_text` itself
            // (NSTextInputClient's `setMarkedText:selectedRange:` semantics), not
            // to the document as a whole, so it must be resolved against
            // `new_text` before adding `inserted_start`.
            let relative = char_range_from_utf16(new_text, relative_utf16);
            let selection_start = self
                .buffer
                .position_for_char_offset(inserted_start + relative.start);
            let selection_end = self
                .buffer
                .position_for_char_offset(inserted_start + relative.end);
            self.buffer.set_selection(selection_start, selection_end);
        }

        self.notify_edit(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let text = self.buffer.text();
        let char_range = char_range_from_utf16(&text, range_utf16);
        let start = self.buffer.position_for_char_offset(char_range.start);
        let end = self.buffer.position_for_char_offset(char_range.end);
        if start.line != end.line {
            // A marked/queried range spanning multiple lines is rare for SQL
            // IME input; keep the OS-facing geometry simple and decline it
            // rather than paint a misleading single-line box.
            return None;
        }

        let line = self.last_lines.get(start.line)?;
        let line_height = px(theme::EDITOR_LINE_HEIGHT);
        let top = line_top(element_bounds.top(), line_height, start.line);
        Some(Bounds::from_corners(
            point(
                element_bounds.left() + line.x_for_index(self.buffer.line_byte_offset(start)),
                top,
            ),
            point(
                element_bounds.left() + line.x_for_index(self.buffer.line_byte_offset(end)),
                top + line_height,
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let position = self.position_for_point(point)?;
        let text = self.buffer.text();
        let char_offset = self.buffer.char_offset_for_position(position);
        Some(char_offset_to_utf16(&text, char_offset))
    }
}

impl EditorView {
    /// The flat char range a `replace_text_in_range`-style call should
    /// operate on: the given UTF-16 range if present, else the active IME
    /// composition range, else the current selection (collapsed to the
    /// cursor if there is none).
    fn resolve_replace_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        if let Some(range_utf16) = range_utf16 {
            let text = self.buffer.text();
            return char_range_from_utf16(&text, range_utf16);
        }
        if let Some(marked) = self.marked_range.clone() {
            return marked;
        }
        let cursor = self.buffer.char_offset_for_position(self.buffer.cursor());
        match self.buffer.selection() {
            Some(selection) => {
                let (start, end) = selection.ordered();
                self.buffer.char_offset_for_position(start)
                    ..self.buffer.char_offset_for_position(end)
            }
            None => cursor..cursor,
        }
    }
}

/// The custom `Element` that shapes and paints the buffer's lines, cursor,
/// and selection, and wires OS text input into `EditorView` via
/// `window.handle_input`.
struct EditorContentElement {
    editor: Entity<EditorView>,
}

struct EditorPrepaintState {
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
                let runs = build_runs(editor, line_index, raw_line, &font, color);
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
                    rgb(colors::TEAL),
                ))
            })
            .flatten();

        let selection_quads = selection.map_or_else(Vec::new, |selection| {
            selection_highlight_quads(selection, &lines, &editor.buffer, bounds)
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
fn line_top(origin: Pixels, line_height: Pixels, line_index: usize) -> Pixels {
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
fn editor_content_height(line_count: usize) -> Pixels {
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
                rgba(theme::EDITOR_SELECTION_BG),
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
fn build_runs(
    editor: &EditorView,
    line_index: usize,
    raw_line: &str,
    font: &Font,
    color: Hsla,
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
                Hsla::from(rgb(theme::syntax_color(span.kind)))
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

// -- UTF-16 <-> flat char offset conversions --------------------------------
//
// `EntityInputHandler` deals in UTF-16 code unit offsets into "the text" the
// OS was told about, which here is the whole document (`buffer.text()`).
// `TextBuffer` itself only knows char offsets, so these free functions
// bridge the two at the gpui boundary -- they stay out of the buffer module
// so it has no reason to know what an OS text input API counts in.

fn char_offset_from_utf16(text: &str, offset_utf16: usize) -> usize {
    let mut utf16_count = 0;
    for (char_index, ch) in text.chars().enumerate() {
        if utf16_count >= offset_utf16 {
            return char_index;
        }
        utf16_count += ch.len_utf16();
    }
    text.chars().count()
}

fn char_offset_to_utf16(text: &str, offset: usize) -> usize {
    text.chars().take(offset).map(char::len_utf16).sum()
}

fn char_range_from_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    char_offset_from_utf16(text, range.start)..char_offset_from_utf16(text, range.end)
}

fn char_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    char_offset_to_utf16(text, range.start)..char_offset_to_utf16(text, range.end)
}

/// Test-only accessors used by this module's own tests, and by consumer
/// crates' tests that drive an [`EditorView`] end to end (see the
/// `test-support` feature).
#[cfg(any(test, feature = "test-support"))]
impl EditorView {
    #[must_use]
    pub fn buffer_for_test(&self) -> &TextBuffer {
        &self.buffer
    }

    pub fn set_text_for_test(&mut self, text: &str) {
        self.buffer = TextBuffer::from_text(text);
        self.sync_highlighter();
    }

    /// Insert `text` at the cursor as a manual edit -- i.e. this fires the
    /// `EditListener` like real typed input would, unlike
    /// [`EditorView::set_text_for_test`]. Lets a test simulate "the user
    /// typed something" without a focused window's real input handler.
    pub fn insert_text_for_test(&mut self, text: &str, cx: &mut Context<Self>) {
        self.buffer.insert_text(text);
        self.notify_edit(cx);
    }

    /// The pixel point that hit-tests back to `position`, computed from the
    /// most recent paint's shaped lines and bounds. Lets mouse-handling
    /// tests drive `on_mouse_down`/`on_mouse_move` with real, paint-derived
    /// coordinates instead of guessed pixel offsets.
    ///
    /// # Panics
    ///
    /// Panics if no paint has run yet, or if `position.line` is past the
    /// last painted line.
    #[must_use]
    pub fn point_for_position_for_test(&self, position: Position) -> Point<Pixels> {
        let bounds = self
            .last_bounds
            .expect("a paint must run before computing a point for a position");
        let line = self
            .last_lines
            .get(position.line)
            .expect("position.line must be within the painted lines");
        let byte_index = self.buffer.line_byte_offset(position);
        point(
            bounds.left() + line.x_for_index(byte_index),
            line_top(bounds.top(), px(theme::EDITOR_LINE_HEIGHT), position.line),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use gpui::{
        Entity, EntityInputHandler, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
        MouseUpEvent, TestAppContext, VisualTestContext,
    };

    use gpui::{Hsla, UnderlineStyle, rgb};

    use super::{
        Backspace, Copy, Cut, DeleteForward, EditorView, MoveDocumentEnd, MoveDocumentStart,
        MoveDown, MoveLeft, MoveLineEnd, MoveLineStart, MoveRight, MoveUp, Newline, Paste,
        Position, QueryRunner, RunQuery, SelectAll, SelectDocumentEnd, SelectDocumentStart,
        SelectDown, SelectLeft, SelectLineEnd, SelectLineStart, SelectRight, SelectUp, build_runs,
    };
    use crate::HighlightKind;
    use crate::theme::syntax_color;

    /// A `QueryRunner` double that records every SQL string it was asked to
    /// run instead of running anything, in place of a real session/database.
    fn recording_query_runner() -> (QueryRunner, Arc<Mutex<Vec<String>>>) {
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorded = queries.clone();
        let runner: QueryRunner = Box::new(move |sql, _cx| {
            recorded.lock().expect("queries lock poisoned").push(sql);
        });
        (runner, queries)
    }

    /// The entity under test plus the SQL text its `QueryRunner` recorded.
    struct Harness {
        editor: Entity<EditorView>,
        queries: Arc<Mutex<Vec<String>>>,
    }

    /// Build an [`EditorView`] as a window's root view, focused, wired to a
    /// recording `QueryRunner` so `RunQuery` can be asserted against without
    /// a real session or database.
    fn build_harness(cx: &mut TestAppContext) -> (Harness, &mut VisualTestContext) {
        let (runner, queries) = recording_query_runner();

        let (editor, vcx) = cx.add_window_view(|window, cx| {
            let view = EditorView::new(runner, cx);
            window.focus(&view.focus_handle);
            view
        });

        (Harness { editor, queries }, vcx)
    }

    // -- viewport / paint coverage -----------------------------------------

    #[gpui::test]
    #[allow(clippy::cast_precision_loss)]
    fn moving_the_cursor_below_the_fold_scrolls_it_into_view(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, _cx| {
            let many_lines = (0..60)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n");
            view.set_text_for_test(&many_lines);
        });
        vcx.dispatch_action(MoveDocumentEnd);
        vcx.run_until_parked();

        harness.editor.update(vcx, |view, _cx| {
            let viewport = view.scroll_handle.bounds().size.height;
            assert!(
                viewport > gpui::px(0.0),
                "the pane has a measured height after paint"
            );
            let scroll = -view.scroll_handle.offset().y;
            let line_height = gpui::px(crate::theme::EDITOR_LINE_HEIGHT);
            let cursor_top = gpui::px(crate::theme::EDITOR_PADDING_Y)
                + line_height * view.buffer_for_test().cursor().line as f32;
            assert!(
                cursor_top >= scroll
                    && cursor_top + line_height <= scroll + viewport + gpui::px(1.0),
                "the cursor line must be within the viewport after autoscroll"
            );
        });
    }

    #[gpui::test]
    fn painting_with_an_active_ime_composition_marks_the_span(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        vcx.update(|window, cx| {
            harness.editor.update(cx, |view, cx| {
                view.replace_and_mark_text_in_range(None, "ab", Some(0..2), window, cx);
            });
        });
        vcx.run_until_parked();
        harness.editor.update(vcx, |view, _cx| {
            assert!(
                view.marked_range.is_some(),
                "the composition stays marked across a paint"
            );
        });
    }

    #[gpui::test]
    fn unmark_text_clears_the_ime_composition(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        vcx.update(|window, cx| {
            harness.editor.update(cx, |view, cx| {
                view.replace_and_mark_text_in_range(None, "ab", Some(0..2), window, cx);
                assert!(view.marked_range.is_some());
                view.unmark_text(window, cx);
            });
        });
        harness.editor.update(vcx, |view, _cx| {
            assert!(
                view.marked_range.is_none(),
                "unmark_text clears the composition"
            );
        });
    }

    #[gpui::test]
    fn bounds_for_range_has_geometry_for_one_line_and_none_across_lines(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness
            .editor
            .update(vcx, |view, _cx| view.set_text_for_test("select\nfrom"));
        vcx.run_until_parked();
        vcx.update(|window, cx| {
            harness.editor.update(cx, |view, cx| {
                let element_bounds = view.last_bounds.expect("a paint has run");
                assert!(
                    view.bounds_for_range(0..3, element_bounds, window, cx)
                        .is_some(),
                    "a single-line range has geometry"
                );
                assert!(
                    view.bounds_for_range(0..8, element_bounds, window, cx)
                        .is_none(),
                    "a range spanning two lines declines geometry"
                );
            });
        });
    }

    #[gpui::test]
    fn painting_a_selection_spanning_interior_lines_does_not_panic(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness
            .editor
            .update(vcx, |view, _cx| view.set_text_for_test("aaa\nbbb\nccc"));
        vcx.dispatch_action(SelectAll);
        vcx.run_until_parked();
        harness.editor.update(vcx, |view, _cx| {
            assert!(
                view.buffer_for_test().selection().is_some(),
                "select-all leaves a selection spanning all three lines"
            );
        });
    }

    // -- typed / IME input --------------------------------------------------

    #[gpui::test]
    fn typing_inserts_characters_into_the_buffer_via_the_input_handler(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        vcx.simulate_input("select 1");
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().text(), "select 1");
        });
    }

    #[gpui::test]
    fn ime_composition_marks_replaces_and_commits_text(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, _cx| {
            view.set_text_for_test("select ");
            view.buffer.move_document_end();
        });

        // The IME starts composing "n" and proposes the cursor sit right
        // after it, as if more candidate keystrokes are still coming.
        vcx.update(|window, cx| {
            harness.editor.update(cx, |view, cx| {
                view.replace_and_mark_text_in_range(None, "n", Some(1..1), window, cx);
            });
        });
        let marked_range_utf16 = vcx.update(|window, cx| {
            harness
                .editor
                .update(cx, |view, cx| view.marked_text_range(window, cx))
        });
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().text(), "select n");
            assert_eq!(
                view.buffer_for_test().cursor(),
                Position::new(0, 8),
                "the proposed selection should follow the composed text"
            );
        });
        assert_eq!(marked_range_utf16, Some(7..8));

        // The composition continues, replacing the marked text with the
        // next candidate.
        vcx.update(|window, cx| {
            harness.editor.update(cx, |view, cx| {
                view.replace_and_mark_text_in_range(None, "now", None, window, cx);
            });
        });
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().text(), "select now");
            assert_eq!(view.marked_range, Some(7..10));
        });

        // Commit: the OS replaces the marked range with no explicit range
        // argument, and the composition ends.
        vcx.update(|window, cx| {
            harness.editor.update(cx, |view, cx| {
                view.replace_text_in_range(None, "now", window, cx);
            });
        });
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().text(), "select now");
            assert!(
                view.marked_range.is_none(),
                "committing text must clear the composition range"
            );
        });
    }

    #[gpui::test]
    fn utf16_offsets_round_trip_through_a_surrogate_pair(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, cx| {
            // U+1F600 sits outside the BMP, so it is one `char` but two
            // UTF-16 code units -- exactly the case a naive char-count
            // implementation of the UTF-16 boundary math would get wrong.
            view.set_text_for_test("a\u{1F600}b");
            view.buffer
                .set_selection(Position::new(0, 1), Position::new(0, 2));
            cx.notify();
        });
        vcx.run_until_parked();

        let (selection, emoji_text, actual_range_utf16, hit_char_index) =
            vcx.update(|window, cx| {
                harness.editor.update(cx, |view, cx| {
                    let selection = view
                        .selected_text_range(false, window, cx)
                        .expect("a selection should report a UTF-16 range");
                    let mut actual_range = None;
                    let emoji_text = view
                        .text_for_range(1..3, &mut actual_range, window, cx)
                        .expect("range 1..3 should resolve to the emoji");
                    let click_point = view.point_for_position_for_test(Position::new(0, 1));
                    let hit_char_index = view.character_index_for_point(click_point, window, cx);
                    (selection, emoji_text, actual_range, hit_char_index)
                })
            });

        assert_eq!(
            selection.range,
            1..3,
            "the emoji occupies UTF-16 code units 1..3"
        );
        assert!(!selection.reversed);
        assert_eq!(emoji_text, "\u{1F600}");
        assert_eq!(actual_range_utf16, Some(1..3));
        assert_eq!(
            hit_char_index,
            Some(1),
            "a point at the emoji's leading edge should hit UTF-16 offset 1"
        );
    }

    #[gpui::test]
    fn ime_selected_range_is_resolved_against_the_inserted_text_not_the_document(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, _cx| {
            // A leading astral character (two UTF-16 code units, one char)
            // is exactly the case where resolving `new_selected_range_utf16`
            // against the whole post-insert document -- instead of against
            // `new_text` alone, as NSTextInputClient's `setMarkedText:
            // selectedRange:` specifies -- misaligns the UTF-16 count and
            // produces the wrong selection.
            view.set_text_for_test("\u{1F600}");
            view.buffer.move_document_end();
        });

        // The IME composes "ab" and asks for UTF-16 units 1..2 of that
        // composed text selected, i.e. just the "b".
        vcx.update(|window, cx| {
            harness.editor.update(cx, |view, cx| {
                view.replace_and_mark_text_in_range(None, "ab", Some(1..2), window, cx);
            });
        });

        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().text(), "\u{1F600}ab");
            assert_eq!(
                view.buffer_for_test().selected_text(),
                "b",
                "the selected range is relative to the inserted text, not the \
                 whole document's UTF-16 offsets"
            );
        });
    }

    // -- movement / editing actions ------------------------------------

    #[gpui::test]
    fn move_right_and_backspace_actions_delegate_to_the_buffer(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness
            .editor
            .update(vcx, |view, _cx| view.set_text_for_test("abc"));

        vcx.dispatch_action(MoveRight);
        vcx.dispatch_action(MoveRight);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().cursor(), Position::new(0, 2));
        });

        vcx.dispatch_action(Backspace);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().text(), "ac");
            assert_eq!(view.buffer_for_test().cursor(), Position::new(0, 1));
        });
    }

    #[gpui::test]
    fn move_left_and_delete_forward_actions_delegate_to_the_buffer(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness
            .editor
            .update(vcx, |view, _cx| view.set_text_for_test("abc"));
        vcx.dispatch_action(MoveRight);
        vcx.dispatch_action(MoveRight);
        vcx.dispatch_action(MoveRight);

        vcx.dispatch_action(MoveLeft);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().cursor(), Position::new(0, 2));
        });

        vcx.dispatch_action(DeleteForward);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().text(), "ab");
            assert_eq!(
                view.buffer_for_test().cursor(),
                Position::new(0, 2),
                "delete-forward removes the next character without moving the cursor"
            );
        });
    }

    #[gpui::test]
    fn move_up_down_line_start_and_line_end_actions_delegate_to_the_buffer(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) = build_harness(cx);
        harness
            .editor
            .update(vcx, |view, _cx| view.set_text_for_test("abc\nde"));

        vcx.dispatch_action(MoveLineEnd);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().cursor(), Position::new(0, 3));
        });

        vcx.dispatch_action(MoveDown);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(
                view.buffer_for_test().cursor(),
                Position::new(1, 2),
                "the desired column (3) clamps to the shorter line's length"
            );
        });

        vcx.dispatch_action(MoveLineStart);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().cursor(), Position::new(1, 0));
        });

        vcx.dispatch_action(MoveUp);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().cursor(), Position::new(0, 0));
        });
    }

    #[gpui::test]
    fn move_document_start_and_end_actions_delegate_to_the_buffer(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, _cx| {
            view.set_text_for_test("select 1;\nfrom orders\nwhere true;");
            view.buffer.set_cursor(Position::new(1, 2));
        });

        vcx.dispatch_action(MoveDocumentEnd);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(
                view.buffer_for_test().cursor(),
                Position::new(2, "where true;".chars().count())
            );
        });

        vcx.dispatch_action(MoveDocumentStart);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().cursor(), Position::new(0, 0));
        });
    }

    #[gpui::test]
    fn select_up_down_and_line_boundary_actions_extend_the_selection(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, _cx| {
            view.set_text_for_test("select 1;\nfrom orders");
            view.buffer.set_cursor(Position::new(1, 4));
        });

        vcx.dispatch_action(SelectLineEnd);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().selected_text(), " orders");
        });

        vcx.dispatch_action(SelectLineStart);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().selected_text(), "from");
        });

        vcx.dispatch_action(SelectUp);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().selected_text(), "select 1;\nfrom");
        });

        vcx.dispatch_action(SelectDown);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().selected_text(), "from");
        });
    }

    #[gpui::test]
    fn select_document_start_and_end_actions_extend_the_selection(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, _cx| {
            view.set_text_for_test("select 1;\nfrom orders\nwhere true;");
        });

        vcx.dispatch_action(SelectDocumentEnd);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(
                view.buffer_for_test().selected_text(),
                "select 1;\nfrom orders\nwhere true;"
            );
        });

        vcx.dispatch_action(SelectDocumentStart);
        harness.editor.update(vcx, |view, _cx| {
            assert!(
                !view.buffer_for_test().has_selection(),
                "extending back to the anchor's own position collapses the selection"
            );
        });
    }

    #[gpui::test]
    fn shift_right_actions_extend_a_selection_from_the_cursor(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness
            .editor
            .update(vcx, |view, _cx| view.set_text_for_test("hello"));

        vcx.dispatch_action(SelectRight);
        vcx.dispatch_action(SelectRight);
        vcx.dispatch_action(SelectRight);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().selected_text(), "hel");
        });
    }

    #[gpui::test]
    fn shift_left_actions_extend_a_selection_from_the_cursor(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness
            .editor
            .update(vcx, |view, _cx| view.set_text_for_test("hello"));
        for _ in 0..5 {
            vcx.dispatch_action(MoveRight);
        }

        vcx.dispatch_action(SelectLeft);
        vcx.dispatch_action(SelectLeft);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().selected_text(), "lo");
        });
    }

    #[gpui::test]
    fn select_all_action_selects_the_whole_document(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, _cx| {
            view.set_text_for_test("select 1;\nselect 2;");
        });

        vcx.dispatch_action(SelectAll);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(
                view.buffer_for_test().selected_text(),
                "select 1;\nselect 2;"
            );
        });
    }

    #[gpui::test]
    fn newline_action_splits_the_current_line_at_the_cursor(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness
            .editor
            .update(vcx, |view, _cx| view.set_text_for_test("helloworld"));
        for _ in 0..5 {
            vcx.dispatch_action(MoveRight);
        }

        vcx.dispatch_action(Newline);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(
                view.buffer_for_test().lines(),
                &["hello".to_owned(), "world".to_owned()]
            );
        });
    }

    // -- clipboard -------------------------------------------------------

    #[gpui::test]
    fn copy_cut_and_paste_round_trip_through_the_gpui_clipboard(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness
            .editor
            .update(vcx, |view, _cx| view.set_text_for_test("hello world"));
        for _ in 0..5 {
            vcx.dispatch_action(SelectRight);
        }

        vcx.dispatch_action(Copy);
        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some("hello"));
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(
                view.buffer_for_test().text(),
                "hello world",
                "copy must not modify the buffer"
            );
        });

        vcx.dispatch_action(Cut);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().text(), " world");
        });

        vcx.dispatch_action(Paste);
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().text(), "hello world");
        });
    }

    // -- run query ---------------------------------------------------------

    #[gpui::test]
    fn run_query_with_no_selection_runs_the_whole_buffer(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, _cx| {
            view.set_text_for_test("select * from orders");
        });

        vcx.dispatch_action(RunQuery);

        assert_eq!(
            harness
                .queries
                .lock()
                .expect("queries lock poisoned")
                .as_slice(),
            ["select * from orders"]
        );
    }

    #[gpui::test]
    fn run_query_with_a_selection_runs_only_the_selected_text(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, _cx| {
            view.set_text_for_test("select 1;\nselect 2;");
            view.buffer.move_line_start();
            view.buffer.move_down();
            for _ in 0.."select 2;".chars().count() {
                view.buffer.extend_right();
            }
        });

        vcx.dispatch_action(RunQuery);

        assert_eq!(
            harness
                .queries
                .lock()
                .expect("queries lock poisoned")
                .as_slice(),
            ["select 2;"]
        );
    }

    #[gpui::test]
    fn run_query_on_an_empty_buffer_is_a_noop(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        vcx.dispatch_action(RunQuery);
        assert!(
            harness
                .queries
                .lock()
                .expect("queries lock poisoned")
                .is_empty()
        );
    }

    #[gpui::test]
    fn run_query_on_a_whitespace_only_buffer_is_a_noop(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness
            .editor
            .update(vcx, |view, _cx| view.set_text_for_test("   \n  \t"));
        vcx.dispatch_action(RunQuery);
        assert!(
            harness
                .queries
                .lock()
                .expect("queries lock poisoned")
                .is_empty()
        );
    }

    #[gpui::test]
    fn cmd_enter_and_ctrl_enter_keystrokes_both_dispatch_run_query(cx: &mut TestAppContext) {
        cx.update(super::init);
        let (harness, vcx) = build_harness(cx);
        harness
            .editor
            .update(vcx, |view, _cx| view.set_text_for_test("select 1"));

        vcx.simulate_keystrokes("cmd-enter");
        assert_eq!(
            harness
                .queries
                .lock()
                .expect("queries lock poisoned")
                .as_slice(),
            ["select 1"],
            "cmd-enter should dispatch RunQuery"
        );

        vcx.simulate_keystrokes("ctrl-enter");
        assert_eq!(
            harness
                .queries
                .lock()
                .expect("queries lock poisoned")
                .as_slice(),
            ["select 1", "select 1"],
            "ctrl-enter should also dispatch RunQuery"
        );
    }

    #[gpui::test]
    fn run_current_query_runs_the_buffer_without_dispatching_the_run_query_action(
        cx: &mut TestAppContext,
    ) {
        // An embedding app's own Run affordance (e.g. a workspace header's
        // button) calls this method directly rather than dispatching the
        // `RunQuery` action, since it must work regardless of which element
        // holds keyboard focus. Pin that this public method alone -- with no
        // action dispatch and no focused window -- reaches the same
        // `QueryRunner` seam.
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, _cx| {
            view.set_text_for_test("select * from orders");
        });

        harness.editor.update(vcx, EditorView::run_current_query);

        assert_eq!(
            harness
                .queries
                .lock()
                .expect("queries lock poisoned")
                .as_slice(),
            ["select * from orders"]
        );
    }

    // -- rendering -----------------------------------------------------

    #[gpui::test]
    fn renders_a_multiline_buffer_with_a_selection_without_panicking(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, cx| {
            view.set_text_for_test("select *\nfrom orders\nwhere status = 'paid'");
            view.buffer.move_document_end();
            for _ in 0..6 {
                view.buffer.extend_left();
            }
            cx.notify();
        });
        vcx.run_until_parked();
    }

    /// The default `EditorView` paints with the real SQL highlighter, not
    /// `PlainHighlighter`; this drives a full paint over SQL text exercising
    /// keywords, a string, a number, and both comment forms, asserting only
    /// that painting a frame does not panic (gpui cannot render headlessly
    /// here, so pixel colors are the human's visual pass via `cargo run`).
    #[gpui::test]
    fn rendering_a_multiline_sql_buffer_with_the_sql_highlighter_does_not_panic(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, cx| {
            view.set_text_for_test(
                "-- top comment\nSELECT id, 'paid' AS status /* inline */\nFROM orders WHERE total > 42.5",
            );
            cx.notify();
        });
        vcx.run_until_parked();
    }

    // -- highlighting --------------------------------------------------

    #[gpui::test]
    fn build_runs_keeps_the_highlight_color_and_gains_the_underline_where_they_overlap(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, _cx| {
            view.set_text_for_test("SELECT 1");
            // Marks columns 2..6 ("LECT"), overlapping part of the "SELECT"
            // keyword span (0..6) but not all of it, and not touching the
            // "1" literal's span (7..8) at all.
            view.marked_range = Some(2..6);
        });

        let (font, base_color) = vcx.update(|window, _cx| {
            let style = window.text_style();
            (style.font(), style.color)
        });

        harness.editor.update(vcx, |view, _cx| {
            let runs = build_runs(view, 0, "SELECT 1", &font, base_color);

            let keyword_color = Hsla::from(rgb(syntax_color(HighlightKind::Keyword)));
            let number_color = Hsla::from(rgb(syntax_color(HighlightKind::Number)));
            let underline = UnderlineStyle {
                color: Some(base_color),
                thickness: gpui::px(1.0),
                wavy: false,
            };

            assert_eq!(
                runs.len(),
                4,
                "expected: unmarked keyword head, marked+underlined keyword \
                 tail, unstyled space, number literal"
            );
            assert_eq!(runs[0].color, keyword_color);
            assert_eq!(runs[0].underline, None);

            assert_eq!(
                runs[1].color, keyword_color,
                "the overlapping run keeps the keyword's highlight color"
            );
            assert_eq!(
                runs[1].underline,
                Some(underline),
                "the overlapping run also gains the IME underline"
            );

            assert_eq!(runs[2].color, base_color);
            assert_eq!(runs[2].underline, None);

            assert_eq!(runs[3].color, number_color);
            assert_eq!(runs[3].underline, None);
        });
    }

    #[gpui::test]
    fn typing_a_keyword_highlights_it_on_the_next_render(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);

        let (font, base_color) = vcx.update(|window, _cx| {
            let style = window.text_style();
            (style.font(), style.color)
        });

        harness.editor.update(vcx, |view, _cx| {
            let runs = build_runs(view, 0, "", &font, base_color);
            assert_eq!(
                runs.first().map(|run| run.color),
                Some(base_color),
                "an empty buffer has nothing highlighted yet"
            );
        });

        // `insert_text_for_test` goes through the same manual-edit path real
        // typing does (it fires the `EditListener`), unlike
        // `set_text_for_test`.
        harness.editor.update(vcx, |view, cx| {
            view.insert_text_for_test("SELECT", cx);
        });

        harness.editor.update(vcx, |view, _cx| {
            let runs = build_runs(view, 0, "SELECT", &font, base_color);
            let keyword_color = Hsla::from(rgb(syntax_color(HighlightKind::Keyword)));
            assert_eq!(
                runs.first().map(|run| run.color),
                Some(keyword_color),
                "the keyword just typed is highlighted without any extra step"
            );
        });
    }

    // -- mouse -----------------------------------------------------------

    #[gpui::test]
    fn mouse_down_places_the_cursor_and_dragging_extends_a_selection(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, cx| {
            view.set_text_for_test("select 1\nfrom orders");
            cx.notify();
        });
        vcx.run_until_parked();

        // Click right after "from " on the second line.
        let click_target = Position::new(1, 5);
        let click_point = harness.editor.read_with(vcx, |view, _cx| {
            view.point_for_position_for_test(click_target)
        });
        vcx.update(|window, cx| {
            harness.editor.update(cx, |view, cx| {
                view.on_mouse_down(
                    &MouseDownEvent {
                        button: MouseButton::Left,
                        position: click_point,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    },
                    window,
                    cx,
                );
            });
        });
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().cursor(), click_target);
            assert!(!view.buffer_for_test().has_selection());
            assert!(view.is_selecting);
        });

        // Drag to the end of "orders" to extend a selection.
        let drag_target = Position::new(1, "from orders".chars().count());
        let drag_point = harness.editor.read_with(vcx, |view, _cx| {
            view.point_for_position_for_test(drag_target)
        });
        vcx.update(|window, cx| {
            harness.editor.update(cx, |view, cx| {
                view.on_mouse_move(
                    &MouseMoveEvent {
                        position: drag_point,
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Modifiers::default(),
                    },
                    window,
                    cx,
                );
            });
        });
        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.buffer_for_test().selected_text(), "orders");
        });

        vcx.update(|window, cx| {
            harness.editor.update(cx, |view, cx| {
                view.on_mouse_up(
                    &MouseUpEvent {
                        button: MouseButton::Left,
                        position: drag_point,
                        modifiers: Modifiers::default(),
                        click_count: 1,
                    },
                    window,
                    cx,
                );
            });
        });
        harness.editor.update(vcx, |view, _cx| {
            assert!(!view.is_selecting, "mouse-up should end the drag");
        });
    }

    // -- on_edit / set_text / compact ---------------------------------------

    /// An `on_edit` listener double that counts how many times it fired.
    fn counting_edit_listener() -> (crate::EditListener, Arc<Mutex<usize>>) {
        let count = Arc::new(Mutex::new(0));
        let counted = count.clone();
        let listener: crate::EditListener = Box::new(move |_cx| {
            *counted.lock().expect("edit count lock poisoned") += 1;
        });
        (listener, count)
    }

    #[gpui::test]
    fn typing_fires_the_on_edit_listener(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        let (listener, count) = counting_edit_listener();
        harness
            .editor
            .update(vcx, |view, _cx| view.set_on_edit(listener));

        vcx.simulate_input("ab");

        assert_eq!(*count.lock().expect("edit count lock poisoned"), 2);
    }

    #[gpui::test]
    fn backspace_delete_newline_cut_and_paste_all_fire_the_on_edit_listener(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) = build_harness(cx);
        let (listener, count) = counting_edit_listener();
        harness.editor.update(vcx, |view, _cx| {
            view.set_text_for_test("ab");
            view.set_on_edit(listener);
            view.buffer.move_document_end();
        });

        vcx.dispatch_action(Backspace);
        vcx.dispatch_action(DeleteForward);
        vcx.dispatch_action(Newline);
        vcx.dispatch_action(SelectAll);
        vcx.dispatch_action(Cut);
        vcx.dispatch_action(Paste);

        assert_eq!(*count.lock().expect("edit count lock poisoned"), 5);
    }

    #[gpui::test]
    fn cursor_movement_and_selection_alone_do_not_fire_the_on_edit_listener(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) = build_harness(cx);
        let (listener, count) = counting_edit_listener();
        harness.editor.update(vcx, |view, _cx| {
            view.set_text_for_test("select 1");
            view.set_on_edit(listener);
        });

        vcx.dispatch_action(MoveRight);
        vcx.dispatch_action(MoveLineEnd);
        vcx.dispatch_action(MoveLineStart);
        vcx.dispatch_action(SelectRight);
        vcx.dispatch_action(SelectAll);

        assert_eq!(*count.lock().expect("edit count lock poisoned"), 0);
    }

    #[gpui::test]
    fn set_text_replaces_the_buffer_without_firing_the_on_edit_listener(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        let (listener, count) = counting_edit_listener();
        harness.editor.update(vcx, |view, cx| {
            view.set_on_edit(listener);
            view.set_text("select * from orders", cx);
        });

        harness.editor.update(vcx, |view, _cx| {
            assert_eq!(view.text(), "select * from orders");
        });
        assert_eq!(
            *count.lock().expect("edit count lock poisoned"),
            0,
            "a programmatic set_text is not a manual edit"
        );
    }

    #[gpui::test]
    fn compact_mode_toggles_and_renders_without_a_gutter(cx: &mut TestAppContext) {
        let (harness, vcx) = build_harness(cx);
        harness.editor.update(vcx, |view, _cx| {
            assert!(
                !view.is_compact(),
                "editors start in full, non-compact mode"
            );
            view.set_text_for_test("select * from orders limit 200");
            view.set_compact(true);
            assert!(view.is_compact());
        });
        vcx.run_until_parked();
    }
}
