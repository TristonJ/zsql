//! The SQL editor pane: a `gpui` view over [`crate::TextBuffer`]. Wires OS
//! keyboard/IME input into the buffer via `EntityInputHandler`, paints the
//! buffer's lines with a line-number gutter, a blinking-free cursor, and a
//! selection highlight, and runs the buffer's query text through the
//! caller-supplied [`QueryRunner`] seam on cmd/ctrl-enter or a call to
//! [`EditorView::run_current_query`] from the embedding app.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Div, FocusHandle, Focusable, KeyBinding,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollHandle,
    ShapedLine, Window, actions, div, point, prelude::*, px, rgb,
};
use zsql_ui::theme::{ActiveTheme, Theme};

use crate::theme;
use crate::{Highlighter, Position, SqlHighlighter, TextBuffer};

use element::EditorContentElement;

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
        Undo,
        Redo,
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
        KeyBinding::new("secondary-z", Undo, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-secondary-z", Redo, Some(KEY_CONTEXT)),
        // Ctrl-Y is a common redo shortcut in its own right, distinct from
        // secondary-y's cross-platform cmd-y -- both are bound explicitly,
        // the same dual-bind pattern cmd-enter/ctrl-enter use above.
        KeyBinding::new("secondary-y", Redo, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-y", Redo, Some(KEY_CONTEXT)),
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

    // Line numbers here are always small (an SQL editor pane, not a huge
    // document), so the `usize -> f32` conversion below cannot lose
    // meaningful precision.
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

    // -- undo/redo -------------------------------------------------------

    fn undo(&mut self, _: &Undo, _window: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.undo() {
            self.notify_edit(cx);
        }
    }

    fn redo(&mut self, _: &Redo, _window: &mut Window, cx: &mut Context<Self>) {
        if self.buffer.redo() {
            self.notify_edit(cx);
        }
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
            self.buffer.insert_pasted_text(&text);
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
            if event.click_count >= 3 {
                self.buffer.select_line(position.line);
            } else if event.click_count == 2 {
                self.buffer.select_word(position);
            } else if event.modifiers.shift {
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
    fn render_gutter(
        line_count: usize,
        cursor_line: usize,
        active_theme: &Theme,
    ) -> gpui::Stateful<Div> {
        let colors = &active_theme.colors;
        let mut gutter = div()
            .id("sql-editor-gutter")
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(theme::EDITOR_GUTTER_WIDTH)
            .py(px(theme::EDITOR_PADDING_Y))
            .border_r_1()
            .border_color(rgb(colors.border_soft))
            .bg(rgb(colors.bg_app));

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
                        colors.text_secondary
                    } else {
                        colors.text_tertiary
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
        let active_theme = cx.theme();

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
            .bg(rgb(active_theme.colors.bg_app))
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
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
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
                    .font_family(&cx.theme().fonts.data)
                    .text_size(px(theme::EDITOR_TEXT_SIZE))
                    .text_color(rgb(active_theme.colors.text_primary))
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
                            .h(element::editor_content_height(line_count))
                            .when(!compact, |el| {
                                el.child(Self::render_gutter(line_count, cursor_line, active_theme))
                            })
                            .child(
                                div()
                                    .id("sql-editor-text")
                                    .flex_1()
                                    .min_w_0()
                                    .px(px(theme::EDITOR_PADDING_X))
                                    .py(px(theme::EDITOR_PADDING_Y))
                                    .child(EditorContentElement::new(cx.entity())),
                            ),
                    ),
            )
    }
}

/// Test-only accessors used by this module's own tests, and by consumer
/// crates' tests that drive an [`EditorView`] end to end (see the
/// `test-support` feature).
#[cfg(any(test, feature = "test-support"))]
use zsql_ui::text_input;

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
            text_input::line_top(bounds.top(), px(theme::EDITOR_LINE_HEIGHT), position.line),
        )
    }
}

mod element;
mod input;

#[cfg(test)]
mod tests;
