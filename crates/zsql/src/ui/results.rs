//! The results grid: a virtualized table view over a `Session`'s current
//! [`SessionState`] and accumulated result set

use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, Div, Entity, FocusHandle, Focusable, Font,
    Hsla, KeyBinding, ListSizingBehavior, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Render, ScrollHandle, ScrollStrategy, SharedString, Stateful,
    StyledText, TextRun, UniformListScrollHandle, Window, actions, anchored, deferred, div, font,
    point, prelude::*, px, rgb, rgba, uniform_list,
};
use zsql_core::{ColumnMeta, ResultSet, Row, RowCount, Value};
use zsql_editor::{Highlighter, SqlHighlighter, StyleSpan, syntax_color};
use zsql_ui::button::ButtonSwitch;
use zsql_ui::grid;
use zsql_ui::scrollable::{
    Axis, ScrollSource, ScrollableState, ScrollbarStyle, WithScrollbars, restrict_wheel_to_own_axis,
};
use zsql_ui::table::{
    Gutter, RowNumberStyle, Table, TableColumn, TableRow, TableState, measure,
    row_number_cell_shell,
};
use zsql_ui::theme::{ActiveTheme, Theme};

use super::format::{ValueKind, format_value};
use super::theme;
use crate::config::{LayoutConfig, ValuePanelConfig};
use crate::session::{LivenessState, Session, SessionState};
use crate::ui::format::{self, format_value_for_clipboard, group_thousands};
use crate::ui::value_panel::data::ValuePanelContent;
use crate::ui::value_panel::{self, ValuePanel};

/// The key context the results grid's own key bindings are scoped to, so
/// they only fire while the grid is focused.
pub const KEY_CONTEXT: &str = "ResultsGrid";

actions!(
    zsql_results_grid,
    [
        Copy,
        CellUp,
        CellDown,
        CellLeft,
        CellRight,
        ToggleValuePanel,
        CloseValuePanel,
        FocusValuePanel,
    ]
);

/// Register the results grid's and value panel's key bindings. Call once at
/// startup, before any window that hosts a [`ResultsView`] is opened.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-c", Copy, Some(KEY_CONTEXT)),
        KeyBinding::new("up", CellUp, Some(KEY_CONTEXT)),
        KeyBinding::new("down", CellDown, Some(KEY_CONTEXT)),
        KeyBinding::new("left", CellLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("right", CellRight, Some(KEY_CONTEXT)),
        KeyBinding::new("space", ToggleValuePanel, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", CloseValuePanel, Some(KEY_CONTEXT)),
        KeyBinding::new("tab", FocusValuePanel, Some(KEY_CONTEXT)),
    ]);
    value_panel::init(cx);
}

/// A tab's captured query outcome: the label, lifecycle state, and result
/// set a [`ResultsView`] shows while that tab (rather than the live
/// `Session`) is what it is displaying. Captured once a tab's own run
/// reaches a terminal state, so switching back to that tab later restores
/// exactly what it last produced instead of whatever a different tab most
/// recently ran.
#[derive(Debug, Clone)]
pub struct ResultsSnapshot {
    pub source_label: SharedString,
    pub state: SessionState,
    pub result: ResultSet,
}

/// Which layout the results pane renders a result with: the virtualized grid
/// (a column per result column), or the read-only document viewer joining a
/// document-shaped result's rows into one highlighted, line-numbered text.
/// Always user-choosable via the results bar's Grid|Text switch, whatever the
/// current result's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewMode {
    Grid,
    Text,
}

/// A virtualized results grid, driven by a `Session` entity.
pub struct ResultsView {
    session: Entity<Session>,
    source_label: SharedString,
    /// `Some` while this view is frozen to a specific tab's captured
    /// [`ResultsSnapshot`] instead of following `session` live -- e.g. the
    /// active tab is not the one `session` is currently running a query
    /// for. `None` (the default) means every render reads straight off
    /// `session`.
    frozen: Option<ResultsSnapshot>,
    column_widths: Vec<Pixels>,
    /// Per-column max formatted-text char count seen so far
    column_max_body_chars: Vec<usize>,
    /// How many of `session.result().rows` have already been folded into
    /// `column_max_body_chars`
    folded_row_count: usize,
    /// The grid's mechanical (scroll/drag) state, composed via
    /// `zsql_ui::table`.
    table_state: Entity<TableState>,
    /// Focus target for the grid, so a click on a data cell can focus it and
    /// a subsequent Cmd/Ctrl-C is captured by [`ResultsView::copy_focused_cell`].
    focus_handle: FocusHandle,
    /// The right-click cell context menu, if one is open.
    cell_context_menu: Option<CellContextMenuState>,
    /// The value panel
    value_panel: Entity<ValuePanel>,
    /// The panel's current dock width, draggable between
    /// `value_panel_min_width`/`value_panel_max_width`.
    value_panel_width: Pixels,
    value_panel_min_width: Pixels,
    value_panel_max_width: Pixels,
    /// Width of the panel's own resize divider, from
    /// [`LayoutConfig::divider_thickness`] -- the same tunable the sidebar
    /// and editor/results dividers use, so no dock in the app hardcodes its
    /// own divider width.
    value_panel_divider_thickness: Pixels,
    /// `Some` while the panel's resize divider is being dragged: the
    /// pointer's x position and the panel's own width when the drag began.
    value_panel_drag: Option<(Pixels, Pixels)>,
    /// Which layout the pane currently renders the result with.
    view_mode: ViewMode,
    /// Whether `view_mode` has already been set to its computed default for
    /// the result currently in flight. Cleared alongside the dimension cache
    /// (see [`ResultsView::reset_for_new_result`]) so a freshly-arrived
    /// result recomputes its default exactly once, and left `true`
    /// afterward so a later re-render (more rows folding in, an unrelated
    /// notify) never overrides a manual Grid/Text choice.
    view_mode_defaulted: bool,
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

/// A results grid cell's open right-click context menu: the triggering
/// click position it anchors to. Which cell it targets is not tracked here
/// -- opening the menu already selects that cell (see
/// [`ResultsView::open_cell_context_menu`]), so every menu item acts on the
/// grid's own focused cell, the same target `Copy` (Cmd/Ctrl-C) uses.
#[derive(Debug, Clone)]
struct CellContextMenuState {
    position: Point<Pixels>,
}

/// One position within the Text view's assembled document: a 0-based source
/// line index and a byte offset into that line's own text. Ordered
/// lexicographically by `(line, byte)`, matching reading order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct TextCaret {
    line: usize,
    byte: usize,
}

impl ResultsView {
    /// Build a view over `session`. `source_label` names where the rows came
    /// from (a relation like `public.orders`, or a query kind) and is shown
    /// in the results header bar next to the row count.
    #[must_use]
    pub fn new(
        session: Entity<Session>,
        source_label: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&session, |view: &mut Self, _session, cx| {
            view.sync_dimensions(cx);
            cx.notify();
        })
        .detach();

        let focus_handle = cx.focus_handle();
        let value_panel =
            cx.new(|cx| ValuePanel::new(focus_handle.clone(), ValuePanelConfig::default(), cx));

        let mut view = Self {
            session,
            source_label: source_label.into(),
            frozen: None,
            column_widths: Vec::new(),
            column_max_body_chars: Vec::new(),
            folded_row_count: 0,
            table_state: cx.new(TableState::new),
            focus_handle,
            cell_context_menu: None,
            value_panel,
            // A view built without `configure_value_panel` (any caller other
            // than `WorkspaceView::new`, e.g. a test) still opens to a
            // sensible dock size/threshold set rather than zero-sized panes,
            // by seeding from `LayoutConfig`/`ValuePanelConfig`'s own
            // documented defaults instead of duplicating their numbers here.
            value_panel_width: LayoutConfig::default().value_panel.default_width,
            value_panel_min_width: LayoutConfig::default().value_panel.min_width,
            value_panel_max_width: LayoutConfig::default().value_panel.max_width,
            value_panel_divider_thickness: LayoutConfig::default().divider_thickness,
            value_panel_drag: None,
            view_mode: ViewMode::Grid,
            view_mode_defaulted: false,
            text_selection: None,
            text_selecting: false,
            text_highlighter: SqlHighlighter::new(),
            text_row_scroll_handle: UniformListScrollHandle::new(),
            text_col_scroll_handle: ScrollHandle::new(),
            text_wrap_scroll_handle: ScrollHandle::new(),
            text_scroll_state: cx.new(ScrollableState::new),
            text_content_extent: None,
        };
        view.sync_dimensions(cx);
        view
    }

    /// Size the value panel dock from `layout` and configure its parse
    /// thresholds/hex layout from `cfg`. Called once by
    /// [`crate::ui::workspace::WorkspaceView::new`] right after
    /// construction; a view built directly (e.g. in a test) keeps the
    /// built-in defaults until this is called.
    pub fn configure_value_panel(
        &mut self,
        cx: &mut Context<Self>,
        layout: &LayoutConfig,
        cfg: ValuePanelConfig,
    ) {
        self.value_panel.update(cx, move |p, _cx| {
            p.set_config(cfg);
        });
        self.value_panel_width = layout.value_panel.default_width;
        self.value_panel_min_width = layout.value_panel.min_width;
        self.value_panel_max_width = layout.value_panel.max_width;
        self.value_panel_divider_thickness = layout.divider_thickness;
    }

    /// Follow `session`'s state/result live under `source_label`, e.g. for
    /// the tab that `session` is currently running a query for. Every
    /// render reads straight off `session` until the next
    /// [`ResultsView::show_snapshot`] or `show_live` call.
    pub fn show_live(&mut self, source_label: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.source_label = source_label.into();
        self.frozen = None;
        self.reset_for_new_result();
        self.sync_dimensions(cx);
        cx.notify();
    }

    /// Freeze the grid to `snapshot` instead of following `session` live,
    /// e.g. when switching to a tab that is not the one `session` is
    /// currently running a query for.
    pub fn show_snapshot(&mut self, snapshot: ResultsSnapshot, cx: &mut Context<Self>) {
        self.source_label = snapshot.source_label.clone();
        self.frozen = Some(snapshot);
        self.reset_for_new_result();
        self.sync_dimensions(cx);
        cx.notify();
    }

    /// Clear every piece of state derived from the previous result, right
    /// before a new result becomes current: the incrementally-folded
    /// column-width cache (so the next `sync_dimensions` call recomputes
    /// widths from the new result's own columns/rows rather than folding
    /// onto stale per-column maxima), and the Text view's default/wrap/
    /// selection/scroll position, so nothing carries forward onto a
    /// different result.
    fn reset_for_new_result(&mut self) {
        self.column_widths = Vec::new();
        self.column_max_body_chars = Vec::new();
        self.folded_row_count = 0;
        self.view_mode = ViewMode::Grid;
        self.view_mode_defaulted = false;
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

    /// The result set this view currently renders: `session`'s live result
    /// while [`ResultsView::frozen`] is `None`, else the frozen snapshot's.
    fn effective_result<'a>(&'a self, cx: &'a App) -> &'a ResultSet {
        match &self.frozen {
            Some(snapshot) => &snapshot.result,
            None => self.session.read(cx).result(),
        }
    }

    /// The lifecycle state this view currently renders: `session`'s live
    /// state while [`ResultsView::frozen`] is `None`, else the frozen
    /// snapshot's.
    fn effective_state<'a>(&'a self, cx: &'a App) -> &'a SessionState {
        match &self.frozen {
            Some(snapshot) => &snapshot.state,
            None => self.session.read(cx).state(),
        }
    }

    /// Bring `column_widths` up to date with the session's current result
    /// set, folding only the rows that streamed in since the last call.
    #[tracing::instrument(name = "results_sync_dimensions", skip_all)]
    fn sync_dimensions(&mut self, cx: &mut Context<Self>) {
        // A selection recorded against a previous, larger result set would
        // otherwise point past the new one's rows/columns: clearing it here
        // is simpler than clamping, and loses nothing a fresh click can't
        // restore. Computed in its own scope (rather than reusing the
        // `result` binding below) so this mutable `table_state` update does
        // not fight the immutable borrow of `cx` that binding would still
        // be holding.
        let (row_count, col_count) = {
            let result: &ResultSet = match &self.frozen {
                Some(snapshot) => &snapshot.result,
                None => self.session.read(cx).result(),
            };
            (result.rows.len(), result.columns.len())
        };
        if let Some((row, col)) = self.table_state.read(cx).focused_cell()
            && (row >= row_count || col >= col_count)
        {
            self.table_state.update(cx, |state, cx| {
                state.clear_focused_cell();
                cx.notify();
            });
            tracing::debug!(
                row,
                col,
                "cleared a results grid selection that no longer fits the current result set"
            );
        }

        // Matched directly on the `frozen` field (rather than through the
        // `effective_result` method) so the borrow checker sees this as
        // borrowing only `self.frozen`, leaving `self.column_widths` and
        // the other fields assigned below free to borrow mutably in the
        // same call -- routing through a `&self` method would borrow all
        // of `self` and block those assignments.
        let result: &ResultSet = match &self.frozen {
            Some(snapshot) => &snapshot.result,
            None => self.session.read(cx).result(),
        };

        if result.columns.len() != self.column_max_body_chars.len() {
            self.column_max_body_chars = vec![0; result.columns.len()];
            self.folded_row_count = 0;
        }

        let rows_folded_this_call = result.rows.len().saturating_sub(self.folded_row_count);
        for row in result.rows.iter().skip(self.folded_row_count) {
            for (index, max_chars) in self.column_max_body_chars.iter_mut().enumerate() {
                if let Some(value) = row.0.get(index) {
                    let chars = format_value(value).text.chars().count();
                    if chars > *max_chars {
                        *max_chars = chars;
                    }
                }
            }
        }
        self.folded_row_count = result.rows.len();

        let table_style = Self::table_style(cx.theme());
        self.column_widths = result
            .columns
            .iter()
            .zip(self.column_max_body_chars.iter())
            .map(|(column, &max_body_chars)| {
                column_width_from_parts(column, max_body_chars, &table_style)
            })
            .collect();

        tracing::debug!(
            column_count = result.columns.len(),
            rows_folded_this_call,
            total_folded_row_count = self.folded_row_count,
            "remeasured results grid column widths"
        );

        self.apply_default_view_mode_if_terminal(cx);
    }

    /// Set `view_mode` to its computed default -- `Text` if the result reads
    /// as a document, `Grid` otherwise -- the first time (since
    /// [`ResultsView::reset_for_new_result`]) the result reaches a
    /// terminal-for-display state (`Results` or `Truncated`). Left alone on
    /// every intermediate `Running`/`Truncating` batch, so the default is
    /// computed once from the whole result rather than from a
    /// still-streaming partial one, and a manual Grid/Text choice made
    /// after that point is never overridden by a later re-render.
    fn apply_default_view_mode_if_terminal(&mut self, cx: &Context<Self>) {
        if self.view_mode_defaulted {
            return;
        }
        let is_terminal = matches!(
            self.effective_state(cx),
            SessionState::Results(_) | SessionState::Truncated { .. }
        );
        if !is_terminal {
            return;
        }
        self.view_mode = if self.effective_result(cx).is_document_shaped() {
            ViewMode::Text
        } else {
            ViewMode::Grid
        };
        self.view_mode_defaulted = true;
        tracing::debug!(view_mode = ?self.view_mode, "defaulted the results pane view for a new result");
    }

    /// The results header bar: row/line count + source/relation label, plus
    /// (always) the Grid|Text view switch and, while Text is active, the
    /// copy and wrap controls. `text_document` is `Some` (Text active) with
    /// the same assembled document [`ResultsView::render_body`] renders,
    /// computed once per render rather than reassembled here.
    fn render_bar(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        text_document: Option<&str>,
    ) -> Div {
        let colors = cx.theme().colors;
        let document_lines = text_document.map(document_line_count);
        let count_text = results_bar_count_text(
            self.effective_state(cx),
            self.effective_result(cx).rows.len(),
            document_lines,
        );

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .flex_shrink_0()
            .gap_3()
            .h(theme::RESULTS_BAR_HEIGHT)
            .px_3()
            .bg(rgb(colors.bg_panel))
            .border_b_1()
            .border_color(rgb(colors.border_soft))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_baseline()
                            .gap_2()
                            .flex_shrink_0()
                            .text_size(px(theme::RESULTS_TAB_TEXT_SIZE))
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(colors.text_primary))
                                    .child("Results"),
                            )
                            .child(
                                div()
                                    .font_family(&cx.theme().fonts.data)
                                    .text_color(rgb(colors.accent))
                                    .child(count_text),
                            ),
                    )
                    .child(
                        div()
                            .font_family(&cx.theme().fonts.data)
                            .text_size(px(theme::RESULTS_META_TEXT_SIZE))
                            .text_color(rgb(colors.text_tertiary))
                            .min_w_0()
                            .truncate()
                            .child(self.source_label.clone()),
                    ),
            )
            .child(self.render_bar_right(window, cx))
    }

    /// The results bar's trailing controls: the copy/wrap buttons while
    /// Text is active, then the Grid|Text switch, which is always rendered
    /// regardless of the current result's shape.
    fn render_bar_right(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let active_theme = cx.theme();
        let switch_enabled = self.effective_result(cx).has_single_text_column();

        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .gap(theme::RESULTS_BAR_RIGHT_GAP);

        if self.view_mode == ViewMode::Text {
            row = row.child(Self::render_icon_button(
                "results-text-copy",
                "copy all",
                false,
                active_theme,
                cx.listener(|view, _: &ClickEvent, _window, cx| view.copy_text_document(cx, true)),
            ));
        }

        row.child(self.render_view_switch(window, cx, switch_enabled))
    }

    /// A small text button for the results bar's trailing controls (copy,
    /// wrap), styled like the plain-text icon affordances elsewhere in the
    /// app. `active` paints it with the view switch's active-segment colors
    /// so a toggle (wrap) shows its on/off state; the copy button, which has
    /// no on/off state, always passes `false`.
    fn render_icon_button(
        id: &'static str,
        label: &'static str,
        active: bool,
        active_theme: &Theme,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<Div> {
        let colors = active_theme.colors;
        let mut button = div()
            .id(id)
            .cursor_pointer()
            .px(theme::RESULTS_ICON_BUTTON_PADDING_X)
            .py(theme::RESULTS_ICON_BUTTON_PADDING_Y)
            .rounded(px(theme::RESULTS_ICON_BUTTON_RADIUS))
            .text_size(px(theme::RESULTS_ICON_BUTTON_TEXT_SIZE))
            .child(label)
            .on_click(on_click);

        if active {
            button = button
                .bg(rgb(theme::view_switch_active_bg(active_theme)))
                .text_color(rgb(theme::view_switch_active_text(active_theme)));
        } else {
            button = button
                .text_color(rgb(colors.text_tertiary))
                .hover(|el| el.text_color(rgb(colors.text_secondary)));
        }
        button
    }

    /// The Grid|Text segmented view switch
    fn render_view_switch(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        text_enabled: bool,
    ) -> impl IntoElement {
        let grid_id = "results-view-grid";
        let text_id = "results-view-text";
        let selected = match self.view_mode {
            ViewMode::Grid => grid_id,
            ViewMode::Text => text_id,
        };

        ButtonSwitch::new()
            .selected(selected)
            .disabled(!text_enabled)
            .add_option(
                window,
                cx,
                grid_id,
                "grid",
                cx.listener(|view, _e, _w, cx| {
                    view.set_view_mode(ViewMode::Grid, cx);
                }),
            )
            .add_option(
                window,
                cx,
                text_id,
                "text",
                cx.listener(|view, _e, _w, cx| {
                    view.set_view_mode(ViewMode::Text, cx);
                }),
            )
    }

    /// The main content area: the virtualized grid when results are
    /// available, or a centered prompt/status message otherwise.
    /// `text_document` is `Some` exactly while `view_mode` is `Text`, so
    /// [`ResultsView::render_grid_or_text`] never reassembles it.
    fn render_body(
        &mut self,
        text_document: Option<Rc<str>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let active_theme = cx.theme();
        let state = self.effective_state(cx).clone();
        let has_columns = !self.effective_result(cx).columns.is_empty();

        let inner = match state {
            SessionState::Results(_)
            | SessionState::Truncating { .. }
            | SessionState::Truncated { .. } => self.render_grid_or_text(text_document, window, cx),
            // Once the streaming query's `Columns` event has arrived there
            // is a real (if partial) result set to paint, so switch to the
            // grid immediately rather than waiting for `Done`
            SessionState::Running if has_columns => {
                self.render_grid_or_text(text_document, window, cx)
            }
            SessionState::Empty => Self::render_placeholder(
                active_theme.colors.text_tertiary,
                "No connection configured",
                "Set DATABASE_URL or connection.default_url in your zsql config, then restart.",
                active_theme,
            ),
            SessionState::Connecting => Self::render_placeholder(
                active_theme.colors.text_tertiary,
                "Connecting…",
                "Establishing a connection to the configured database.",
                active_theme,
            ),
            SessionState::Connected => Self::render_placeholder(
                active_theme.colors.text_tertiary,
                "Connected",
                "Run a query to see results here.",
                active_theme,
            ),
            SessionState::Running => Self::render_placeholder(
                active_theme.colors.text_tertiary,
                "Running query…",
                "Streaming results from the database.",
                active_theme,
            ),
            SessionState::Error(message) => Self::render_placeholder(
                active_theme.colors.status_error,
                "Query failed",
                &message,
                active_theme,
            ),
        };

        let value_panel = self.value_panel.read(cx);
        if !value_panel.is_open() {
            return inner;
        }
        let cell = self.table_state.read(cx).focused_cell();
        self.sync_value_panel_content(cx, cell);

        div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h_0()
            .child(inner.flex_1().min_w_0())
            .child(self.render_value_panel_divider(cx))
            .child(
                div()
                    .flex()
                    .h_full()
                    .w(self.value_panel_width)
                    .child(self.value_panel.clone()),
            )
    }

    /// The value panel's resize divider: a draggable strip on the panel's
    /// left edge, clamped between `value_panel_min_width`/`max_width`.
    fn render_value_panel_divider(&self, cx: &Context<Self>) -> Stateful<Div> {
        let colors = cx.theme().colors;
        div()
            .id("value-panel-divider")
            .debug_selector(|| "value-panel-divider".to_owned())
            .flex_shrink_0()
            .w(self.value_panel_divider_thickness)
            .h_full()
            .cursor(gpui::CursorStyle::ResizeLeftRight)
            .bg(rgb(colors.border))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::start_value_panel_drag))
    }

    fn start_value_panel_drag(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.value_panel_drag = Some((event.position.x, self.value_panel_width));
        cx.notify();
    }

    fn end_value_panel_drag(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.value_panel_drag.take().is_some() {
            cx.notify();
        }
    }

    /// Resize the panel while its divider is being dragged: moving the
    /// pointer left (toward the grid) grows it, since it docks on the right
    /// edge -- clamped to `value_panel_min_width`/`max_width`, a no-op if no
    /// drag is in progress.
    fn value_panel_drag_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((origin_x, start_width)) = self.value_panel_drag else {
            return;
        };
        let delta = origin_x - event.position.x;
        self.value_panel_width =
            (start_width + delta).clamp(self.value_panel_min_width, self.value_panel_max_width);
        cx.notify();
    }

    /// A centered title + detail message shown in place of the grid for any
    /// non-`Results` state.
    fn render_placeholder(
        title_color: u32,
        title: &str,
        detail: &str,
        active_theme: &Theme,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_2()
            .px_6()
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(title_color))
                    .child(title.to_owned()),
            )
            .child(
                div()
                    .text_size(px(theme::RESULTS_META_TEXT_SIZE))
                    .text_color(rgb(active_theme.colors.text_tertiary))
                    .child(detail.to_owned()),
            )
    }

    /// The grid, or the Text view when `view_mode` is `Text` and the result
    /// actually has a single text-typed column to show as a document --
    /// falling back to the grid otherwise, so a stale `Text` selection left
    /// over from a differently-shaped result (or the interval before
    /// `sync_dimensions` next runs) never renders an empty document pane.
    fn render_grid_or_text(
        &mut self,
        text_document: Option<Rc<str>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        if let Some(document) = text_document
            && self.effective_result(cx).has_single_text_column()
        {
            self.render_text(&document, window, cx)
        } else {
            self.render_grid(cx)
        }
    }

    /// The read-only document viewer for a single-text-column result.
    fn render_text(&mut self, document: &str, window: &mut Window, cx: &mut Context<Self>) -> Div {
        self.text_highlighter.set_text(document);
        let lines: Rc<[String]> = document
            .split('\n')
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .into();
        let line_count = lines.len();

        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let style = Self::table_style(active_theme);
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

    /// The two-pane virtualized grid (pinned row numbers + horizontally
    /// scrolling data columns), built by composing `zsql_ui::table::Table`.
    fn render_grid(&mut self, cx: &mut Context<Self>) -> Div {
        let row_count = self.effective_result(cx).rows.len();
        let active_theme = cx.theme();
        let columns = self.build_columns(cx);

        Table::new("results-grid", &self.table_state)
            .style(Self::table_style(active_theme))
            .columns(columns)
            .row_count(row_count)
            .gutter(Gutter::RowNumbers(RowNumberStyle {
                char_width: theme::CELL_CHAR_WIDTH,
                min_width: theme::ROW_NUMBER_MIN_WIDTH,
            }))
            .rows(Self::render_data_row_cells)
            .selectable()
            .focus_on_cell_click(self.focus_handle.clone())
            .on_cell_double_click(Self::open_value_panel_for)
            .on_cell_right_click(|view, row, col, event, _window, cx| {
                view.open_cell_context_menu(row, col, event.position, cx);
            })
            .render(cx)
    }

    /// The one `TableStyle` both `render_grid`'s live `Table` and
    /// `column_width_from_parts`'s width estimate use, so a column's
    /// measured width can never drift from the padding it is actually
    /// rendered with.
    fn table_style(active_theme: &Theme) -> zsql_ui::table::TableStyle {
        zsql_ui::table::TableStyle::themed(active_theme)
    }

    /// The data pane's columns: each column's cached width plus its header
    /// content (name + type-tag badge).
    fn build_columns(&self, cx: &Context<Self>) -> Vec<TableColumn> {
        let active_theme = cx.theme();
        let columns: &[ColumnMeta] = &self.effective_result(cx).columns;
        columns
            .iter()
            .zip(self.column_widths.iter())
            .map(|(column, &width)| TableColumn::new(width, column_header(column, active_theme)))
            .collect()
    }

    /// Render the data-cell rows in `range` for the data pane's virtualized
    /// list
    fn render_data_row_cells(
        &mut self,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<TableRow> {
        let active_theme = cx.theme();
        let rows: &[zsql_core::Row] = &self.effective_result(cx).rows;

        range
            .map(|ix| {
                let cells = rows
                    .get(ix)
                    .map(|row| {
                        row.0
                            .iter()
                            .map(|value| {
                                let formatted = format_value(value);
                                let is_null = formatted.kind == ValueKind::Null;
                                div()
                                    .flex()
                                    .flex_col()
                                    .justify_start()
                                    .items_start()
                                    .h_full()
                                    .overflow_y_hidden()
                                    .text_color(rgb(formatted.kind.color(active_theme)))
                                    .when(is_null, gpui::prelude::Styled::italic)
                                    .child(formatted.text)
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                TableRow::new(cells)
            })
            .collect()
    }

    /// The status bar's connection-lifecycle dot color, label, and trailing
    /// error text (if any), from the session's real state and liveness --
    /// never [`ResultsView::effective_state`]'s frozen tab snapshot. A tab
    /// frozen to an older, successful result must not keep showing
    /// "Connected" while the session itself is `Connecting` to a different
    /// target, has errored, or has gone unreachable; conversely the label
    /// and error text are always computed from the same state, so they can
    /// never disagree.
    ///
    /// [`SessionState::Connected`] alone is enough for the "Connected"
    /// label: every registered driver's `connect()` already performs a real
    /// pool connection plus a synchronous liveness check before resolving,
    /// so `Connected` already implies a verified-reachable connection.
    /// Waiting for the recurring probe's first [`LivenessState::Healthy`]
    /// result on top of that would only add a probe-interval-sized delay
    /// with no correctness benefit.
    fn connection_status(&self, cx: &Context<Self>) -> (u32, &'static str, Option<String>) {
        let session = self.session.read(cx);
        let state = session.state();
        let liveness = session.liveness();
        let (dot_color, label) = status_indicator(state, liveness, cx.theme());
        let error_message = match state {
            SessionState::Error(message) => Some(message.clone()),
            _ => None,
        };
        (dot_color, label, error_message)
    }

    /// The bottom connection/status bar: connection state + label, row
    /// count, and elapsed query time. `text_document` is the same document
    /// [`ResultsView::render_bar`] was given.
    fn render_status_bar(&self, text_document: Option<&str>, cx: &Context<Self>) -> Div {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let (dot_color, label, error_message) = self.connection_status(cx);

        let mut bar = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .gap_4()
            .h(theme::STATUS_BAR_HEIGHT)
            .px_3()
            .bg(rgb(colors.bg_panel))
            .border_t_1()
            .border_color(rgb(colors.border))
            .font_family(&cx.theme().fonts.data)
            .text_size(px(theme::STATUS_BAR_TEXT_SIZE))
            .text_color(rgb(colors.text_secondary))
            .child(grid::status_dot(dot_color))
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(colors.text_primary))
                    .child(label),
            );

        // Query metrics (row/line count, elapsed time) keep coming from the
        // displayed tab's own effective state, frozen or live: a frozen
        // tab's completed-query numbers are unrelated to the session's
        // current connection lifecycle.
        let effective_state = self.effective_state(cx);
        let (metrics_count, metrics_unit) = match text_document {
            Some(document) => (document_line_count(document), "lines"),
            None => (self.effective_result(cx).rows.len(), "rows"),
        };
        if let Some((count_text, elapsed_text)) =
            status_metrics(effective_state, metrics_count, metrics_unit)
        {
            bar = bar.child(count_text).child(elapsed_text);
        }

        if let Some(total_row_count_text) =
            format_total_row_count(self.session.read(cx).row_count())
        {
            bar = bar.child(total_row_count_text);
        }

        if let Some(message) = error_message {
            bar = bar.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(colors.status_error))
                    .child(message),
            );
        }

        bar
    }

    // ---- cell context menu -----------------------------------------

    /// Open the right-click context menu for `(row, col)`, anchored at
    /// `position` (the triggering click, in window coordinates), and select
    /// that cell -- so the menu's `Copy value`/`Copy row as JSON`/`Copy
    /// column name` items, which all act on the focused cell, target the
    /// cell that was actually right-clicked.
    fn open_cell_context_menu(
        &mut self,
        row: usize,
        col: usize,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.table_state.update(cx, |state, cx| {
            state.set_focused_cell(row, col);
            cx.notify();
        });
        self.cell_context_menu = Some(CellContextMenuState { position });
        cx.notify();
    }

    fn close_cell_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.cell_context_menu.take().is_some() {
            cx.notify();
        }
    }

    /// `View value`: open the value panel for the context menu's cell (the
    /// cell was already selected when the menu was opened, see
    /// [`ResultsView::open_cell_context_menu`]), then close the menu.
    #[tracing::instrument(name = "results_view_value_from_menu", skip_all)]
    fn view_value_from_menu(&mut self, cx: &mut Context<Self>) {
        self.value_panel.update(cx, ValuePanel::open);
        let cell = self.table_state.read(cx).focused_cell();
        tracing::debug!(?cell, "opened the value panel from the cell context menu");
        self.close_cell_context_menu(cx);
        cx.notify();
    }

    /// `Copy row as JSON`: serialize every cell of the focused row, each via
    /// its own [`Value`]'s JSON representation (not `format_value`'s display
    /// text) keyed by column name, and write it to the clipboard. A no-op
    /// while nothing is selected.
    #[tracing::instrument(name = "results_copy_row_as_json", skip_all)]
    fn copy_row_as_json(&mut self, cx: &mut Context<Self>) {
        let Some((row, _col)) = self.table_state.read(cx).focused_cell() else {
            tracing::trace!(
                "copy-row-as-json invoked with no results grid selection; nothing to do"
            );
            return;
        };
        let result = self.effective_result(cx);
        let Some(row_data) = result.rows.get(row) else {
            return;
        };
        let text = format::row_as_json_string(row_data, &result.columns);
        tracing::debug!(row, "copied a results grid row as JSON to the clipboard");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// `Copy column name`: write the focused column's exact
    /// [`ColumnMeta::name`] to the clipboard. A no-op while nothing is
    /// selected.
    #[tracing::instrument(name = "results_copy_column_name", skip_all)]
    fn copy_column_name(&mut self, cx: &mut Context<Self>) {
        let Some((_row, col)) = self.table_state.read(cx).focused_cell() else {
            tracing::trace!(
                "copy-column-name invoked with no results grid selection; nothing to do"
            );
            return;
        };
        let Some(name) = self
            .effective_result(cx)
            .columns
            .get(col)
            .map(|column| column.name.clone())
        else {
            return;
        };
        tracing::debug!(col, "copied a results grid column name to the clipboard");
        cx.write_to_clipboard(ClipboardItem::new_string(name));
    }

    /// The right-click cell context menu overlay: `View value`, `Copy
    /// value`, `Copy row as JSON`, a separator, then `Copy column name`,
    /// anchored at the triggering click. A full-window backdrop behind it
    /// absorbs off-menu clicks, mirroring the sidebar's relation-row context
    /// menu. Renders nothing when no menu is open.
    fn render_cell_context_menu(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let active_theme = cx.theme();
        let menu = self.cell_context_menu.clone()?;

        let content = div()
            .id("results-cell-context-menu")
            .occlude()
            .w(theme::CONTEXT_MENU_WIDTH)
            .p(theme::CONTEXT_MENU_PADDING)
            .bg(rgb(active_theme.colors.bg_raised))
            .border_1()
            .border_color(rgb(active_theme.colors.border))
            .rounded(px(theme::CONTEXT_MENU_RADIUS))
            .child(context_menu_item(cx, "View value", |view, _window, cx| {
                view.view_value_from_menu(cx);
            }))
            .child(context_menu_item(cx, "Copy value", |view, window, cx| {
                view.copy_focused_cell(&Copy, window, cx);
                view.close_cell_context_menu(cx);
            }))
            .child(context_menu_item(
                cx,
                "Copy row as JSON",
                |view, _window, cx| {
                    view.copy_row_as_json(cx);
                    view.close_cell_context_menu(cx);
                },
            ))
            .child(context_menu_separator(active_theme))
            .child(context_menu_item(
                cx,
                "Copy column name",
                |view, _window, cx| {
                    view.copy_column_name(cx);
                    view.close_cell_context_menu(cx);
                },
            ));

        let backdrop = div()
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _event: &MouseDownEvent, _window, cx| {
                    view.close_cell_context_menu(cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|view, _event: &MouseDownEvent, _window, cx| {
                    view.close_cell_context_menu(cx);
                }),
            )
            .child(
                anchored()
                    .position(menu.position)
                    .snap_to_window()
                    .child(content),
            );

        Some(deferred(backdrop).with_priority(1).into_any_element())
    }

    // ---- value panel: open/close/pin/follow-selection ----------------

    /// Toggle the value panel for the focused cell (the grid's `space`
    /// binding). A no-op while nothing is selected.
    #[tracing::instrument(name = "results_toggle_value_panel", skip_all)]
    fn toggle_value_panel(
        &mut self,
        _: &ToggleValuePanel,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(cell) = self.table_state.read(cx).focused_cell() else {
            return;
        };
        self.value_panel.update(cx, ValuePanel::toggle);
        self.sync_value_panel_content(cx, Some(cell));
        cx.notify();
    }

    /// `esc` while the grid has focus: close the panel, leaving grid focus
    /// (and the focused cell) untouched.
    fn close_value_panel(
        &mut self,
        _: &CloseValuePanel,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.value_panel.update(cx, |p, cx| {
            if p.is_open() {
                p.close(cx);
            }
        });
    }

    /// `tab` while the grid has focus: move keyboard focus onto the panel,
    /// if it is open.
    fn focus_value_panel(
        &mut self,
        _: &FocusValuePanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = self.value_panel.read(cx);
        if panel.is_open() {
            window.focus(panel.focus_handle());
        }
    }

    /// Double-click on a data cell (see [`ResultsView::render_data_row_cells`]):
    /// select it and open the value panel directly.
    #[tracing::instrument(name = "results_open_value_panel_for", skip_all)]
    fn open_value_panel_for(
        &mut self,
        row: usize,
        col: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.table_state.update(cx, |state, cx| {
            state.set_focused_cell(row, col);
            cx.notify();
        });
        self.value_panel.update(cx, ValuePanel::open);
        tracing::debug!(row, col, "opened the value panel via double-click");
        cx.notify();
    }

    /// Bring `value_panel_json` up to date with the provided cell
    fn sync_value_panel_content(&mut self, cx: &mut Context<Self>, cell: Option<(usize, usize)>) {
        let content = cell.and_then(|(row, col)| {
            let id = 31 * row + col;
            let value = self
                .effective_result(cx)
                .rows
                .get(row)
                .and_then(|r| r.0.get(col))?;
            let column = self.effective_result(cx).columns.get(col)?;
            Some((id, value, column))
        });
        if !self
            .value_panel
            .read(cx)
            .would_update_content(content.map(|c| c.0))
        {
            return;
        }

        let content = content
            .map(|(id, value, column)| ValuePanelContent::new(id, value.clone(), column.clone()));
        self.value_panel
            .update(cx, |p, _cx| p.update_content(content));
    }

    /// Copy the selected cell's full formatted value to the system
    /// clipboard while Grid is active, or the Text view's whole assembled
    /// document while Text is active. A NULL cell copies as an empty string
    /// rather than the literal "NULL" the grid displays, so pasting a NULL
    /// cell elsewhere never inserts visible placeholder text. A no-op in
    /// Grid while nothing is selected.
    #[tracing::instrument(name = "results_copy_focused_cell", skip_all)]
    fn copy_focused_cell(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        tracing::trace!("got a copy request for the results pane");
        if self.view_mode == ViewMode::Text {
            self.copy_text_document(cx, false);
            return;
        }
        let Some((row, col)) = self.table_state.read(cx).focused_cell() else {
            tracing::trace!("copy invoked with no results grid selection; nothing to do");
            return;
        };
        let Some(value) = self
            .effective_result(cx)
            .rows
            .get(row)
            .and_then(|r| r.0.get(col))
        else {
            return;
        };
        let text = format_value_for_clipboard(value);
        tracing::debug!(row, col, "copied a results grid cell to the clipboard");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// Copy the Text view's exact assembled document to the system
    /// clipboard, verbatim -- no tab characters, no per-line quoting, and no
    /// trailing separator added.
    #[tracing::instrument(name = "results_copy_text_document", skip_all)]
    fn copy_text_document(&self, cx: &mut Context<Self>, all: bool) {
        let document = assemble_document(self.effective_result(cx));
        let document = if all {
            document
        } else {
            self.get_selected_text(&document).unwrap_or_default()
        };

        tracing::debug!(
            chars = document.chars().count(),
            "copied the results text view's document"
        );
        cx.write_to_clipboard(ClipboardItem::new_string(document));
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

    /// Switch the results pane's rendering between the grid and the Text
    /// document viewer. A no-op if `mode` is already active.
    fn set_view_mode(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        if self.view_mode == mode {
            return;
        }
        tracing::debug!(?mode, "switched the results pane view");
        self.view_mode = mode;
        cx.notify();
    }

    /// Move the selected cell by `(delta_row, delta_col)`, clamped to the
    /// current result's bounds with no wraparound. Starts from `(0, 0)` when
    /// nothing is selected yet; applying the delta only when a selection
    /// already exists. A no-op for an empty result.
    fn move_focused_cell(&mut self, delta_row: isize, delta_col: isize, cx: &mut Context<Self>) {
        let row_count = self.effective_result(cx).rows.len();
        let col_count = self.effective_result(cx).columns.len();
        if row_count == 0 || col_count == 0 {
            return;
        }

        // If no cell is currently selected, select (0, 0) and return without
        // applying the delta. This ensures the first arrow press in any
        // direction lands on the top-left cell consistently.
        if self.table_state.read(cx).focused_cell().is_none() {
            tracing::trace!("moved the results grid selection to (0, 0)");
            self.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 0);
                cx.notify();
            });
            return;
        }

        let (current_row, current_col) = self.table_state.read(cx).focused_cell().unwrap();
        let new_row = current_row
            .saturating_add_signed(delta_row)
            .min(row_count - 1);
        let new_col = current_col
            .saturating_add_signed(delta_col)
            .min(col_count - 1);
        tracing::trace!(new_row, new_col, "moved the results grid selection");
        self.table_state.update(cx, |state, cx| {
            state.set_focused_cell(new_row, new_col);
            cx.notify();
        });
    }

    fn cell_up(&mut self, _: &CellUp, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_focused_cell(-1, 0, cx);
    }

    fn cell_down(&mut self, _: &CellDown, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_focused_cell(1, 0, cx);
    }

    fn cell_left(&mut self, _: &CellLeft, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_focused_cell(0, -1, cx);
    }

    fn cell_right(&mut self, _: &CellRight, _window: &mut Window, cx: &mut Context<Self>) {
        self.move_focused_cell(0, 1, cx);
    }
}

impl Focusable for ResultsView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Test-only accessors used by `ui::sidebar`'s and `ui::tabs`'s tests
#[cfg(test)]
impl ResultsView {
    pub(crate) fn source_label_for_test(&self) -> &str {
        &self.source_label
    }

    /// Whether this view is currently frozen to a captured
    /// [`ResultsSnapshot`] (see [`ResultsView::show_snapshot`]) rather than
    /// following `session` live.
    pub(crate) fn is_frozen_for_test(&self) -> bool {
        self.frozen.is_some()
    }

    pub(crate) fn view_mode_for_test(&self) -> ViewMode {
        self.view_mode
    }

    /// The Text view's current selection as `((anchor_line, anchor_byte),
    /// (cursor_line, cursor_byte))`.
    pub(crate) fn text_selection_for_test(&self) -> Option<((usize, usize), (usize, usize))> {
        self.text_selection
            .map(|(anchor, cursor)| ((anchor.line, anchor.byte), (cursor.line, cursor.byte)))
    }

    pub(crate) fn set_view_mode_for_test(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        self.set_view_mode(mode, cx);
    }

    pub(crate) fn set_text_caret_for_test(
        &mut self,
        line: usize,
        byte: usize,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        self.set_text_caret(line, byte, extend, cx);
    }
}

impl Render for ResultsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Assembled once here (rather than separately by the bar, status
        // bar, and body) so a single render pass never re-joins the same
        // result's rows into a document three times over.
        let text_document: Option<Rc<str>> = (self.view_mode == ViewMode::Text)
            .then(|| Rc::from(assemble_document(self.effective_result(cx))));

        div()
            .id("results-grid-pane")
            .relative()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::copy_focused_cell))
            .on_action(cx.listener(Self::cell_up))
            .on_action(cx.listener(Self::cell_down))
            .on_action(cx.listener(Self::cell_left))
            .on_action(cx.listener(Self::cell_right))
            .on_action(cx.listener(Self::toggle_value_panel))
            .on_action(cx.listener(Self::close_value_panel))
            .on_action(cx.listener(Self::focus_value_panel))
            .on_mouse_move(cx.listener(Self::value_panel_drag_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::end_value_panel_drag))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::end_value_panel_drag))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(cx.theme().colors.bg_app))
            .child(self.render_bar(window, cx, text_document.as_deref()))
            .child(self.render_body(text_document.clone(), window, cx))
            .child(self.render_status_bar(text_document.as_deref(), cx))
            .children(self.render_cell_context_menu(cx))
    }
}

/// One results grid context menu row, mirroring `sidebar.rs`'s
/// `context_menu_item`/`ContextMenuState` pattern for the sidebar's
/// relation-row context menu.
fn context_menu_item(
    cx: &Context<ResultsView>,
    label: &'static str,
    on_click: impl Fn(&mut ResultsView, &mut Window, &mut Context<ResultsView>) + 'static,
) -> Stateful<Div> {
    let active_theme = cx.theme();
    div()
        .id(label)
        .flex()
        .flex_row()
        .items_center()
        .h(theme::CONTEXT_MENU_ITEM_HEIGHT)
        .px(theme::CONTEXT_MENU_ITEM_PADDING_X)
        .rounded(px(theme::CONTEXT_MENU_ITEM_RADIUS))
        .cursor_pointer()
        .text_size(px(theme::CONTEXT_MENU_ITEM_TEXT_SIZE))
        .text_color(rgb(active_theme.colors.text_primary))
        .hover(|el| el.bg(rgba(theme::sidebar_selected_bg(active_theme))))
        .child(label)
        .on_click(cx.listener(move |view, _event: &ClickEvent, window, cx| {
            on_click(view, window, cx);
        }))
}

/// A thin horizontal divider between context menu item groups.
fn context_menu_separator(active_theme: &Theme) -> Div {
    div()
        .h(theme::CONTEXT_MENU_SEPARATOR_HEIGHT)
        .my(theme::CONTEXT_MENU_SEPARATOR_MARGIN_Y)
        .bg(rgb(active_theme.colors.border_soft))
}

/// A data column's header content: its name plus a type-name badge.
fn column_header(column: &ColumnMeta, active_theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_color(rgb(active_theme.colors.text_primary))
                .child(column.name.clone()),
        )
        .child(grid::type_tag_tertiary(&column.type_name, active_theme))
        .into_any_element()
}

/// The bottom status bar's dot color and label for `state`. A `liveness` of
/// [`LivenessState::Unreachable`] overrides every state's normal indicator
/// with a distinct "Disconnected" one, since the probe result is
/// independent of (and can contradict) whatever `state` currently holds -
/// for instance a query can still be `Running` against a connection the
/// probe has just found unreachable.
fn status_indicator(
    state: &SessionState,
    liveness: &LivenessState,
    active_theme: &Theme,
) -> (u32, &'static str) {
    let colors = active_theme.colors;
    if matches!(liveness, LivenessState::Unreachable(_)) {
        return (theme::status_disconnected(active_theme), "Disconnected");
    }
    match state {
        SessionState::Empty => (colors.text_tertiary, "Not connected"),
        SessionState::Connecting => (colors.status_warn, "Connecting…"),
        SessionState::Connected | SessionState::Results(_) => (colors.accent, "Connected"),
        SessionState::Running => (colors.accent, "Running…"),
        SessionState::Truncating { .. } => (colors.status_limited, "Running… (truncated)"),
        SessionState::Truncated { .. } => (colors.status_limited, "Truncated"),
        SessionState::Error(_) => (colors.status_error, "Error"),
    }
}

/// The results bar's row/line count text for `state` and `row_count` (the
/// effective result's row count). `document_lines` is `Some(line_count)`
/// while the Text view is active (reading "N lines" instead of "N rows",
/// including in the truncated form) and `None` while the grid is active.
fn results_bar_count_text(
    state: &SessionState,
    row_count: usize,
    document_lines: Option<usize>,
) -> String {
    match state {
        SessionState::Results(_) | SessionState::Running => match document_lines {
            Some(lines) => format!("{lines} lines"),
            None => row_count.to_string(),
        },
        SessionState::Truncating { rows } | SessionState::Truncated { rows, .. } => {
            match document_lines {
                Some(lines) => format!("{lines} lines (truncated at {row_count})"),
                None => format!("{rows} (truncated at {row_count})"),
            }
        }
        SessionState::Empty
        | SessionState::Connecting
        | SessionState::Connected
        | SessionState::Error(_) => "-".to_owned(),
    }
}

/// The bottom status bar's "N <unit>" / "N ms" text for `state`, given
/// `count` and its `unit` word (`"rows"` for the grid, `"lines"` while the
/// Text view is active). `None` for any state with no completed query to
/// report timing/count for.
fn status_metrics(state: &SessionState, count: usize, unit: &str) -> Option<(String, String)> {
    match state {
        SessionState::Results(elapsed) => Some((
            format!("{count} {unit}"),
            format!("{} ms", elapsed.as_millis()),
        )),
        SessionState::Truncated { elapsed, rows } => Some((
            format!("Result limited to {count} {unit} ({rows} total)"),
            format!("{} ms", elapsed.as_millis()),
        )),
        _ => None,
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

/// Join a single-text-column result's rows into one document: a lone row's
/// value verbatim, or multiple rows joined with `'\n'`. If _every_ row
/// in the result is terminated with a newline, and there is more than one
/// row, we _don't_ join with an extra newline. This is to make displaying
/// things like `sp_helptext` a bit more natural.
fn assemble_document(result: &ResultSet) -> String {
    match result.rows.as_slice() {
        [] => String::new(),
        [row] => document_cell_text(row).to_owned(),
        rows => {
            let iter = rows.iter().map(document_cell_text);
            join_document_lines(iter)
        }
    }
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

/// Line count of `document`, matching the same `'\n'`-split convention
/// `TextBuffer`/`SqlHighlighter` use: an empty document counts as 1 line,
/// and a trailing newline yields one extra empty final line.
fn document_line_count(document: &str) -> usize {
    document.split('\n').count()
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

/// The byte offset of `line`'s `char_index`-th character, or `line`'s full
/// byte length if `char_index` is at or past its end.
fn char_byte_offset(line: &str, char_index: usize) -> usize {
    line.char_indices()
        .nth(char_index)
        .map_or(line.len(), |(byte_index, _)| byte_index)
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

/// One line of the Text view's body: `line`'s text painted with `runs`,
/// wrapped or not per `wrap`, and draggable/clickable to set the Text view's
/// selection (shift-click extends from the existing anchor; a plain
/// click-and-drag extends continuously as the mouse moves).
fn render_text_view_line(
    index: usize,
    line: &str,
    runs: Vec<TextRun>,
    wrap: bool,
    cx: &Context<ResultsView>,
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

/// Appended after the number when a total row count is a planner estimate
/// rather than an exact count, so the distinction reads clearly even without
/// color.
const ESTIMATED_ROW_COUNT_SUFFIX: &str = " (estimated)";

/// Labels the whole-relation total so it never reads as the streamed-rows
/// metric beside it. That metric renders as `"200 rows"` (capped at the
/// preview limit), so the total drops the bare `"rows"` word for `"total"`
/// and the two can no longer be confused.
const TOTAL_ROW_COUNT_LABEL: &str = " total";

/// The previewed relation's total row count, for the status bar: e.g.
/// `"1,234 total"` for an exact count, or `"~1,234,567 total (estimated)"`
/// when the driver could only provide a planner estimate. `None` when no
/// count has been fetched (no preview yet, still fetching, or the fetch
/// failed), so the caller can omit the segment entirely.
fn format_total_row_count(row_count: Option<RowCount>) -> Option<String> {
    let row_count = row_count?;
    let grouped = group_thousands(row_count.value());
    Some(if row_count.is_estimated() {
        format!(
            "{}{grouped}{TOTAL_ROW_COUNT_LABEL}{ESTIMATED_ROW_COUNT_SUFFIX}",
            zsql_core::ESTIMATE_MARKER
        )
    } else {
        format!("{grouped}{TOTAL_ROW_COUNT_LABEL}")
    })
}

/// Estimate a column's pixel width from its header (name + type tag) and
/// `max_body_chars`, using `style`'s cell padding -- the same `TableStyle`
/// the live grid renders with, so the estimate and the render never drift.
fn column_width_from_parts(
    column: &ColumnMeta,
    max_body_chars: usize,
    style: &zsql_ui::table::TableStyle,
) -> Pixels {
    let header_chars = column.name.chars().count() + column.type_name.chars().count();
    measure::column_width(
        header_chars,
        max_body_chars,
        style,
        measure::ColumnWidthLimits {
            char_width: theme::CELL_CHAR_WIDTH,
            header_extra_width: theme::TYPE_TAG_EXTRA_WIDTH,
            min_width: theme::MIN_COLUMN_WIDTH,
            max_width: theme::MAX_COLUMN_WIDTH,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::{
        AppContext as _, Focusable as _, Hsla, Modifiers, MouseButton, MouseDownEvent,
        MouseUpEvent, font, px, rgb,
    };
    use zsql_core::{ColumnMeta, ResultSet, Row, RowCount, Value};

    use super::{
        CellDown, CellLeft, CellRight, CellUp, Copy, ResultsView, SessionState, ViewMode,
        assemble_document, column_width_from_parts, document_line_count, format_total_row_count,
        results_bar_count_text, status_indicator, status_metrics, text_view_line_runs,
    };

    use crate::session::{LivenessState, Session};
    use crate::ui::theme;
    use zsql_editor::{HighlightKind, StyleSpan, syntax_color};
    use zsql_ui::table::body_first_cell_debug_selector;
    use zsql_ui::theme::Theme;

    fn column(name: &str, type_name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            nullable: false,
        }
    }

    #[test]
    fn column_width_from_parts_grows_for_a_longer_type_name() {
        let style = ResultsView::table_style(&Theme::default());
        let short_type = column("id", "int8");
        let long_type = column("id", "timestamp with time zone");

        let narrow = column_width_from_parts(&short_type, 0, &style);
        let wide = column_width_from_parts(&long_type, 0, &style);

        assert!(
            f32::from(wide) > f32::from(narrow),
            "a longer type_name must widen the column even with an identical name and no body \
             content: narrow={narrow:?} wide={wide:?}"
        );
    }

    #[test]
    fn column_width_from_parts_clamps_at_the_configured_minimum() {
        let style = ResultsView::table_style(&Theme::default());
        let width = column_width_from_parts(&column("a", "b"), 0, &style);
        assert!(f32::from(width) >= theme::MIN_COLUMN_WIDTH);
    }

    #[test]
    fn column_width_from_parts_clamps_at_the_configured_maximum() {
        let style = ResultsView::table_style(&Theme::default());
        let width =
            column_width_from_parts(&column(&"x".repeat(500), &"y".repeat(500)), 5_000, &style);
        assert!((f32::from(width) - theme::MAX_COLUMN_WIDTH).abs() < f32::EPSILON);
    }

    fn sample_result() -> ResultSet {
        ResultSet {
            columns: vec![
                ColumnMeta {
                    name: "id".to_owned(),
                    type_name: "int8".to_owned(),
                    nullable: false,
                },
                ColumnMeta {
                    name: "status".to_owned(),
                    type_name: "text".to_owned(),
                    nullable: true,
                },
            ],
            rows: vec![
                Row(vec![Value::Int(1), Value::Text("paid".to_owned())]),
                Row(vec![
                    Value::Int(2),
                    Value::Text("a-very-long-status-value".to_owned()),
                ]),
            ],
            affected: None,
            notices: Vec::new(),
        }
    }

    #[test]
    fn status_indicator_maps_each_state_to_its_dot_color_and_label() {
        let active_theme = Theme::default();
        let colors = active_theme.colors;
        assert_eq!(
            status_indicator(&SessionState::Empty, &LivenessState::Unknown, &active_theme),
            (colors.text_tertiary, "Not connected")
        );
        assert_eq!(
            status_indicator(
                &SessionState::Connecting,
                &LivenessState::Unknown,
                &active_theme
            ),
            (colors.status_warn, "Connecting…")
        );
        assert_eq!(
            status_indicator(
                &SessionState::Connected,
                &LivenessState::Healthy,
                &active_theme
            ),
            (colors.accent, "Connected")
        );
        assert_eq!(
            status_indicator(
                &SessionState::Running,
                &LivenessState::Healthy,
                &active_theme
            ),
            (colors.accent, "Running…")
        );
        assert_eq!(
            status_indicator(
                &SessionState::Results(Duration::from_millis(1)),
                &LivenessState::Healthy,
                &active_theme
            ),
            (colors.accent, "Connected")
        );
        assert_eq!(
            status_indicator(
                &SessionState::Error("boom".to_owned()),
                &LivenessState::Unknown,
                &active_theme
            ),
            (colors.status_error, "Error")
        );
        let limited = status_indicator(
            &SessionState::Truncated {
                elapsed: Duration::from_millis(1),
                rows: 100,
            },
            &LivenessState::Healthy,
            &active_theme,
        );
        assert_eq!(limited, (colors.status_limited, "Truncated"));
        assert_ne!(
            limited,
            (colors.accent, "Connected"),
            "Limited must not be indistinguishable from a normal completed result"
        );
        assert_ne!(
            limited,
            (colors.status_error, "Error"),
            "Limited must not be indistinguishable from a query error"
        );
    }

    #[test]
    fn status_indicator_shows_disconnected_regardless_of_session_state_when_liveness_is_unreachable()
     {
        let active_theme = Theme::default();
        let unreachable = LivenessState::Unreachable("connection reset".to_owned());
        for state in [
            SessionState::Connected,
            SessionState::Running,
            SessionState::Results(Duration::from_millis(1)),
        ] {
            assert_eq!(
                status_indicator(&state, &unreachable, &active_theme),
                (theme::status_disconnected(&active_theme), "Disconnected"),
                "expected a Disconnected indicator regardless of state {state:?}"
            );
        }
    }

    #[test]
    fn status_indicator_treats_a_healthy_or_unknown_liveness_as_no_override() {
        let active_theme = Theme::default();
        assert_eq!(
            status_indicator(
                &SessionState::Connected,
                &LivenessState::Healthy,
                &active_theme
            ),
            status_indicator(
                &SessionState::Connected,
                &LivenessState::Unknown,
                &active_theme
            ),
            "Healthy and Unknown liveness must not change a state's own indicator"
        );
    }

    #[test]
    fn status_metrics_reports_rows_and_elapsed_ms_only_for_results() {
        let state = SessionState::Results(Duration::from_millis(42));
        assert_eq!(
            status_metrics(&state, 1, "rows"),
            Some(("1 rows".to_owned(), "42 ms".to_owned()))
        );

        for state in [
            SessionState::Empty,
            SessionState::Connecting,
            SessionState::Connected,
            SessionState::Running,
            SessionState::Error("boom".to_owned()),
        ] {
            assert_eq!(
                status_metrics(&state, 5, "rows"),
                None,
                "expected no fabricated rows/ms text for {state:?}"
            );
        }
    }

    #[test]
    fn status_metrics_reads_as_truncated_for_a_limited_result() {
        let state = SessionState::Truncated {
            elapsed: Duration::from_millis(7),
            rows: 5_000,
        };
        assert_eq!(
            status_metrics(&state, 100, "rows"),
            Some((
                "Result limited to 100 rows (5000 total)".to_owned(),
                "7 ms".to_owned()
            )),
            "the row count shown must be the actual number streamed, with the limit accurate"
        );
    }

    #[test]
    fn status_metrics_uses_the_given_unit_word_for_the_count_text() {
        let state = SessionState::Results(Duration::from_millis(3));
        assert_eq!(
            status_metrics(&state, 17, "lines"),
            Some(("17 lines".to_owned(), "3 ms".to_owned())),
            "the Text view's status metric must read lines, not rows"
        );
    }

    // ---- connection_status: the status bar's real-session-state wiring --

    #[gpui::test]
    fn connection_status_shows_connecting_for_a_frozen_tab_while_the_session_is_switching(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = cx.new(|_cx| {
            Session::new_for_render_test(SessionState::Connecting, ResultSet::default())
        });
        let (view, vcx) =
            cx.add_window_view(|_window, cx| ResultsView::new(session, "public.orders", cx));

        view.update(vcx, |view, cx| {
            view.show_snapshot(
                super::ResultsSnapshot {
                    source_label: "public.orders".into(),
                    state: SessionState::Results(Duration::from_millis(42)),
                    result: sample_result(),
                },
                cx,
            );
        });

        view.update(vcx, |view, cx| {
            let (_dot_color, label, error) = view.connection_status(cx);
            assert_eq!(
                label, "Connecting…",
                "switching sessions must show Connecting, not the frozen tab's stale Connected"
            );
            assert!(error.is_none());

            let metrics = status_metrics(
                view.effective_state(cx),
                view.effective_result(cx).rows.len(),
                "rows",
            );
            assert_eq!(
                metrics,
                Some(("2 rows".to_owned(), "42 ms".to_owned())),
                "the frozen tab's own row/elapsed metrics must be unaffected by the switch"
            );
        });
    }

    #[gpui::test]
    fn connection_status_never_shows_connected_for_a_failed_or_timed_out_connect(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = cx.new(|_cx| {
            Session::new_for_render_test(
                SessionState::Error("liveness probe timed out after 3000ms".to_owned()),
                ResultSet::default(),
            )
        });
        let (view, vcx) =
            cx.add_window_view(|_window, cx| ResultsView::new(session, "public.orders", cx));

        // The tab is still frozen to a prior, successful run: the failed
        // connect must not be masked by it.
        view.update(vcx, |view, cx| {
            view.show_snapshot(
                super::ResultsSnapshot {
                    source_label: "public.orders".into(),
                    state: SessionState::Results(Duration::from_millis(9)),
                    result: sample_result(),
                },
                cx,
            );
        });

        view.update(vcx, |view, cx| {
            let (_dot_color, label, error) = view.connection_status(cx);
            assert_ne!(label, "Connected");
            assert_eq!(label, "Error");
            assert_eq!(
                error,
                Some("liveness probe timed out after 3000ms".to_owned())
            );
        });
    }

    #[gpui::test]
    fn connection_status_shows_connected_immediately_after_connect_without_waiting_for_a_probe(
        cx: &mut gpui::TestAppContext,
    ) {
        // A freshly successful connect: liveness has not had time to reach
        // Healthy yet (no probe has ticked), and there is no frozen tab.
        let session = cx
            .new(|_cx| Session::new_for_render_test(SessionState::Connected, ResultSet::default()));
        let (view, vcx) =
            cx.add_window_view(|_window, cx| ResultsView::new(session, "public.orders", cx));

        view.update(vcx, |view, cx| {
            let (_dot_color, label, error) = view.connection_status(cx);
            assert_eq!(label, "Connected");
            assert!(error.is_none());
        });
    }

    #[gpui::test]
    fn connection_status_shows_disconnected_when_the_session_liveness_is_unreachable(
        cx: &mut gpui::TestAppContext,
    ) {
        // Query results were previously frozen into the tab while the
        // connection was healthy; the session's liveness has since gone
        // Unreachable and must override the stale frozen indicator.
        let session = cx
            .new(|_cx| Session::new_for_render_test(SessionState::Connected, ResultSet::default()));
        session.update(cx, |session, _cx| {
            session.set_liveness_for_test(LivenessState::Unreachable("connection reset".into()));
        });
        let (view, vcx) =
            cx.add_window_view(|_window, cx| ResultsView::new(session, "public.orders", cx));

        view.update(vcx, |view, cx| {
            view.show_snapshot(
                super::ResultsSnapshot {
                    source_label: "public.orders".into(),
                    state: SessionState::Results(Duration::from_millis(9)),
                    result: sample_result(),
                },
                cx,
            );
        });

        view.update(vcx, |view, cx| {
            let (_dot_color, label, error) = view.connection_status(cx);
            assert_eq!(
                label, "Disconnected",
                "an unreachable liveness must override a frozen tab's stale Connected indicator"
            );
            assert!(error.is_none());
        });
    }

    #[test]
    fn format_total_row_count_renders_nothing_when_absent() {
        assert_eq!(format_total_row_count(None), None);
    }

    #[test]
    fn format_total_row_count_renders_an_exact_count_with_thousands_separators() {
        assert_eq!(
            format_total_row_count(Some(RowCount::Exact(1_234))),
            Some("1,234 total".to_owned())
        );
    }

    #[test]
    fn format_total_row_count_renders_an_estimated_count_marked_distinctly() {
        assert_eq!(
            format_total_row_count(Some(RowCount::Estimated(1_234_567))),
            Some("~1,234,567 total (estimated)".to_owned())
        );
    }

    #[test]
    fn format_total_row_count_labels_the_total_distinctly_from_the_streamed_rows_metric() {
        // The streamed-rows metric reads "N rows"; the total must not, or the
        // two segments are indistinguishable in the status bar.
        let exact = format_total_row_count(Some(RowCount::Exact(1_234))).unwrap();
        let estimated = format_total_row_count(Some(RowCount::Estimated(1_234))).unwrap();
        assert!(!exact.ends_with(" rows"));
        assert!(exact.contains("total"));
        assert!(estimated.contains("total"));
    }

    #[test]
    fn format_total_row_count_handles_small_counts_with_no_separator_needed() {
        assert_eq!(
            format_total_row_count(Some(RowCount::Exact(7))),
            Some("7 total".to_owned())
        );
        assert_eq!(
            format_total_row_count(Some(RowCount::Exact(0))),
            Some("0 total".to_owned())
        );
    }

    #[gpui::test]
    fn renders_one_frame_without_panicking(cx: &mut gpui::TestAppContext) {
        let mut result = sample_result();
        result.rows.push(Row(vec![Value::Int(3), Value::Null]));
        result
            .rows
            .push(Row(vec![Value::Bool(true), Value::Float(42.5)]));
        result.rows.push(Row(vec![
            Value::Numeric("123456789.12345".to_owned()),
            Value::Bytes(vec![0xAB, 0xCD, 0xEF]),
        ]));
        result.rows.push(Row(vec![
            Value::Uuid("550e8400-e29b-41d4-a716-446655440000".to_owned()),
            Value::Timestamp("2026-07-14T09:12:31+00:00".to_owned()),
        ]));
        result.rows.push(Row(vec![
            Value::Json(r#"{"key":"value"}"#.to_owned()),
            Value::Array(vec![
                Value::Int(1),
                Value::Text("two".to_owned()),
                Value::Null,
            ]),
        ]));
        result.rows.push(Row(vec![
            Value::Unknown("custom_type".to_owned()),
            Value::Bool(false),
        ]));

        let state = SessionState::Results(Duration::from_millis(8));
        let session = cx.new(|_cx| Session::new_for_render_test(state, result));
        cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
    }

    #[gpui::test]
    fn renders_with_every_row_count_variant_without_panicking(cx: &mut gpui::TestAppContext) {
        for row_count in [
            None,
            Some(RowCount::Exact(1_234)),
            Some(RowCount::Estimated(1_234_567)),
        ] {
            let state = SessionState::Results(Duration::from_millis(8));
            let session = cx.new(|_cx| {
                let mut session = Session::new_for_render_test(state, sample_result());
                session.set_row_count_for_test(row_count);
                session
            });
            cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
        }
    }

    #[gpui::test]
    fn renders_every_non_results_state_without_panicking(cx: &mut gpui::TestAppContext) {
        for state in [
            SessionState::Empty,
            SessionState::Connecting,
            SessionState::Connected,
            // No `Columns` event has arrived yet: the placeholder path,
            // not the grid.
            SessionState::Running,
            SessionState::Error("connection refused".to_owned()),
        ] {
            let session = cx.new(|_cx| Session::new_for_render_test(state, ResultSet::default()));
            cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
        }
    }

    #[gpui::test]
    fn renders_the_grid_for_a_running_query_with_partial_results(cx: &mut gpui::TestAppContext) {
        let mut result = sample_result();
        result.rows.truncate(1);
        let session = cx.new(|_cx| Session::new_for_render_test(SessionState::Running, result));

        cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
    }

    /// The rows streamed before a query was cancelled at the configured
    /// limit must stay visible: `Limited` renders the grid, not a
    /// placeholder, exactly like a normal completed result.
    #[gpui::test]
    fn renders_the_grid_for_a_limited_result_keeping_rows_visible(cx: &mut gpui::TestAppContext) {
        let mut result = sample_result();
        result.rows.truncate(1);
        let state = SessionState::Truncated {
            elapsed: Duration::from_millis(5),
            rows: 1,
        };
        let session = cx.new(|_cx| Session::new_for_render_test(state, result));

        cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
    }

    /// A result set with enough rows to overflow any reasonable viewport must
    /// show its vertical scrollbar without user interaction. This guards the
    /// first-frame regression where the scrollbar stayed hidden because the
    /// scroll viewport's bounds are zero during the first render and nothing
    /// forced the follow-up re-render once they became known.
    #[gpui::test]
    fn vertical_scrollbar_is_shown_after_the_first_frame_when_rows_overflow(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut result = sample_result();
        let template = result.rows[0].clone();
        result.rows = (0..400).map(|_| template.clone()).collect();
        let session = cx.new(|_cx| {
            Session::new_for_render_test(
                SessionState::Results(std::time::Duration::from_millis(1)),
                result,
            )
        });
        let (view, vcx) =
            cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
        vcx.run_until_parked();

        view.read_with(vcx, |v, app| {
            assert!(
                v.table_state
                    .read(app)
                    .scroll()
                    .read(app)
                    .vertical_visible(),
                "the vertical scrollbar must be visible for 400 overflowing rows"
            );
        });
    }

    /// A result set with enough wide columns to overflow any reasonable
    /// viewport must show its horizontal scrollbar without user
    /// interaction, mirroring the equivalent vertical-overflow test.
    #[gpui::test]
    fn horizontal_scrollbar_is_shown_after_the_first_frame_when_columns_overflow(
        cx: &mut gpui::TestAppContext,
    ) {
        let columns: Vec<ColumnMeta> = (0..40)
            .map(|index| ColumnMeta {
                name: format!("a_fairly_long_column_name_{index}"),
                type_name: "text".to_owned(),
                nullable: true,
            })
            .collect();
        let row = Row(columns
            .iter()
            .map(|_| Value::Text("a moderately long cell value".to_owned()))
            .collect());
        let result = ResultSet {
            columns,
            rows: vec![row],
            affected: None,
            notices: Vec::new(),
        };
        let session = cx.new(|_cx| {
            Session::new_for_render_test(
                SessionState::Results(std::time::Duration::from_millis(1)),
                result,
            )
        });
        let (view, vcx) =
            cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
        vcx.run_until_parked();

        view.read_with(vcx, |v, app| {
            assert!(
                v.table_state
                    .read(app)
                    .scroll()
                    .read(app)
                    .horizontal_visible(),
                "the horizontal scrollbar must be visible for 40 overflowing wide columns"
            );
        });
    }

    /// A result set whose columns already fit inside the viewport must not
    /// show a horizontal scrollbar, mirroring the vertical scrollbar's
    /// hidden contract when rows already fit.
    #[gpui::test]
    fn horizontal_scrollbar_is_absent_when_columns_fit_the_viewport(cx: &mut gpui::TestAppContext) {
        let session = cx.new(|_cx| {
            Session::new_for_render_test(
                SessionState::Results(std::time::Duration::from_millis(1)),
                sample_result(),
            )
        });
        let (view, vcx) =
            cx.add_window_view(|_window, cx| super::ResultsView::new(session, "public.orders", cx));
        vcx.run_until_parked();

        view.read_with(vcx, |v, app| {
            assert!(
                !v.table_state
                    .read(app)
                    .scroll()
                    .read(app)
                    .horizontal_visible(),
                "the horizontal scrollbar must be absent when columns already fit the viewport"
            );
        });
    }

    #[gpui::test]
    fn column_widths_grow_incrementally_as_rows_stream_in(cx: &mut gpui::TestAppContext) {
        let columns = vec![ColumnMeta {
            name: "v".to_owned(),
            type_name: "text".to_owned(),
            nullable: true,
        }];
        let first_batch = ResultSet {
            columns: columns.clone(),
            rows: vec![Row(vec![Value::Text("ab".to_owned())])],
            affected: None,
            notices: Vec::new(),
        };

        let session =
            cx.new(|_cx| Session::new_for_render_test(SessionState::Running, first_batch));
        let session_for_view = session.clone();
        let (view, vcx) =
            cx.add_window_view(|_window, cx| super::ResultsView::new(session_for_view, "t", cx));

        let width_after_first_batch = view.update(vcx, |view, _cx| {
            assert_eq!(
                view.folded_row_count, 1,
                "the one row present at construction should already be folded"
            );
            view.column_widths[0]
        });

        // A second batch arrives with a much longer cell in the same
        // column.
        session.update(vcx, |session, _cx| {
            session.set_result_for_test(ResultSet {
                columns,
                rows: vec![
                    Row(vec![Value::Text("ab".to_owned())]),
                    Row(vec![Value::Text(
                        "a much longer value than before".to_owned(),
                    )]),
                ],
                affected: None,
                notices: Vec::new(),
            });
        });
        // `Session::set_result_for_test` bypasses `cx.notify()`, so the view
        // is synced explicitly here rather than relying on the observer
        view.update(vcx, super::ResultsView::sync_dimensions);

        view.update(vcx, |view, _cx| {
            assert_eq!(
                view.folded_row_count, 2,
                "folded_row_count should catch up to the new total row count"
            );
            assert!(
                f32::from(view.column_widths[0]) > f32::from(width_after_first_batch),
                "width should grow once a longer cell streams in"
            );
        });
    }

    // -- cell selection / copy -------------------------------------------

    fn view_with_results(
        cx: &mut gpui::TestAppContext,
        result: ResultSet,
    ) -> (gpui::Entity<ResultsView>, &mut gpui::VisualTestContext) {
        let state = SessionState::Results(Duration::from_millis(1));
        let session = cx.new(|_cx| Session::new_for_render_test(state, result));
        cx.add_window_view(|_window, cx| ResultsView::new(session, "public.orders", cx))
    }

    #[gpui::test]
    fn clicking_a_cell_focuses_the_grid_and_a_following_copy_key_copies_its_value(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, vcx) = view_with_results(cx, sample_result());
        vcx.run_until_parked();

        let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());
        let cell_bounds = vcx
            .debug_bounds(body_first_cell_debug_selector(&table_state))
            .expect("the top-of-viewport body cell must be painted");
        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: gpui::point(
                cell_bounds.origin.x + px(5.0),
                cell_bounds.origin.y + px(5.0),
            ),
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.run_until_parked();

        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            Some((0, 0)),
            "clicking the top-of-viewport body cell must select row 0, column 0"
        );

        vcx.dispatch_action(Copy);
        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some("1"),
            "Cmd/Ctrl-C after a click must copy the clicked cell's value, proving the click \
             also focused the grid (dispatch_action only reaches a focused view's key bindings)"
        );
    }

    #[gpui::test]
    fn copy_with_no_selection_never_writes_to_the_clipboard(cx: &mut gpui::TestAppContext) {
        let (view, vcx) = view_with_results(cx, sample_result());
        vcx.run_until_parked();

        assert_eq!(vcx.read_from_clipboard().and_then(|item| item.text()), None);
        view.update_in(vcx, |view, window, cx| {
            view.copy_focused_cell(&Copy, window, cx);
        });
        assert_eq!(
            vcx.read_from_clipboard().and_then(|item| item.text()),
            None,
            "copying with no selection must not write anything to the clipboard"
        );
    }

    #[gpui::test]
    fn copy_writes_the_full_formatted_value_not_a_truncated_display_string(
        cx: &mut gpui::TestAppContext,
    ) {
        let long_value = "a very long value that would visually truncate in a narrow cell but \
                           must still be copied in full"
            .to_owned();
        let result = ResultSet {
            columns: vec![column("v", "text")],
            rows: vec![Row(vec![Value::Text(long_value.clone())])],
            affected: None,
            notices: Vec::new(),
        };
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.table_state
                .update(cx, |state, _cx| state.set_focused_cell(0, 0));
        });
        view.update_in(vcx, |view, window, cx| {
            view.copy_focused_cell(&Copy, window, cx);
        });

        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some(long_value.as_str()));
    }

    #[gpui::test]
    fn copy_of_a_null_cell_writes_an_empty_string(cx: &mut gpui::TestAppContext) {
        let result = ResultSet {
            columns: vec![column("v", "text")],
            rows: vec![Row(vec![Value::Null])],
            affected: None,
            notices: Vec::new(),
        };
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.table_state
                .update(cx, |state, _cx| state.set_focused_cell(0, 0));
        });
        view.update_in(vcx, |view, window, cx| {
            view.copy_focused_cell(&Copy, window, cx);
        });

        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some(""),
            "a NULL cell must copy as an empty string, not the literal \"NULL\" the grid \
             displays"
        );
    }

    #[gpui::test]
    fn a_selection_outside_a_shrunken_result_is_cleared_and_copy_stays_a_noop(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut result = sample_result();
        result
            .rows
            .push(Row(vec![Value::Int(3), Value::Text("extra".to_owned())]));
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.table_state
                .update(cx, |state, _cx| state.set_focused_cell(2, 1));
        });

        // The session's result shrinks back to `sample_result`'s two rows,
        // taking the just-set selection at row 2 out of bounds.
        let session = view.read_with(vcx, |v, _app| v.session.clone());
        session.update(vcx, |session, _cx| {
            session.set_result_for_test(sample_result());
        });
        // `Session::set_result_for_test` bypasses `cx.notify()`, so the view
        // is synced explicitly here rather than relying on the observer.
        view.update(vcx, super::ResultsView::sync_dimensions);

        let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            None,
            "a selection that no longer fits the shrunken result must be cleared"
        );

        // Re-rendering (the highlight path) and invoking copy (the domain
        // lookup path) must both stay safe with no selection left to act on.
        vcx.run_until_parked();
        view.update_in(vcx, |view, window, cx| {
            view.copy_focused_cell(&Copy, window, cx);
        });
        assert_eq!(vcx.read_from_clipboard().and_then(|item| item.text()), None);
    }

    #[gpui::test]
    fn copy_of_a_selection_past_a_smaller_results_bounds_stays_a_noop(
        cx: &mut gpui::TestAppContext,
    ) {
        // Sets an out-of-bounds selection directly on `TableState` rather
        // than going through `sync_dimensions` (which would clear it): this
        // exercises `copy_focused_cell`'s own `.get()` guard against a
        // `Some` selection whose (row, col) has no matching value, not the
        // no-selection (`None`) path a cleared selection would take
        // instead.
        let (view, vcx) = view_with_results(cx, sample_result());
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.table_state
                .update(cx, |state, _cx| state.set_focused_cell(50, 50));
        });
        view.update_in(vcx, |view, window, cx| {
            view.copy_focused_cell(&Copy, window, cx);
        });

        assert_eq!(
            vcx.read_from_clipboard().and_then(|item| item.text()),
            None,
            "a selection past the result's own rows/columns must not panic and must not write \
             anything to the clipboard"
        );
    }

    #[gpui::test]
    fn an_empty_result_set_selects_nothing_and_copy_is_a_noop(cx: &mut gpui::TestAppContext) {
        let (view, vcx) = view_with_results(cx, ResultSet::default());
        vcx.run_until_parked();

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: gpui::point(px(50.0), px(50.0)),
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.run_until_parked();

        let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            None,
            "an empty result has no cell to select"
        );

        view.update_in(vcx, |view, window, cx| {
            view.copy_focused_cell(&Copy, window, cx);
        });
        assert_eq!(vcx.read_from_clipboard().and_then(|item| item.text()), None);
    }

    #[gpui::test]
    fn arrow_keys_over_an_empty_result_set_select_nothing_and_do_not_panic(
        cx: &mut gpui::TestAppContext,
    ) {
        // `move_focused_cell` computes `row_count - 1`/`col_count - 1` to
        // clamp a new selection: an empty result must return before that
        // subtraction, or it would underflow.
        let (view, vcx) = cx.add_window_view(|window, cx| {
            let state = SessionState::Results(Duration::from_millis(1));
            let session = cx.new(|_cx| Session::new_for_render_test(state, ResultSet::default()));
            let view = ResultsView::new(session, "public.orders", cx);
            window.focus(&view.focus_handle(cx));
            view
        });
        vcx.run_until_parked();

        vcx.dispatch_action(CellDown);
        vcx.dispatch_action(CellRight);

        let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            None,
            "an empty result has no cell for an arrow key to select"
        );
    }

    #[gpui::test]
    fn arrow_keys_move_the_selection_one_cell_at_a_time_and_clamp_at_the_bounds(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, vcx) = cx.add_window_view(|window, cx| {
            let state = SessionState::Results(Duration::from_millis(1));
            let session = cx.new(|_cx| Session::new_for_render_test(state, sample_result()));
            let view = ResultsView::new(session, "public.orders", cx);
            window.focus(&view.focus_handle(cx));
            view
        });
        vcx.run_until_parked();
        let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());

        // No wraparound past the top-left corner.
        vcx.dispatch_action(CellUp);
        vcx.dispatch_action(CellLeft);
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            Some((0, 0)),
            "moving up/left with nothing selected must land on (0, 0), not go negative"
        );

        vcx.dispatch_action(CellDown);
        vcx.dispatch_action(CellRight);
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            Some((1, 1)),
            "CellDown/CellRight must move exactly one row/column at a time"
        );

        // `sample_result` has exactly 2 rows and 2 columns: (1, 1) is
        // already the bottom-right corner, so further Down/Right must not
        // move past it.
        vcx.dispatch_action(CellDown);
        vcx.dispatch_action(CellRight);
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            Some((1, 1)),
            "moving past the last row/column must clamp at the bounds rather than wrap or \
             go out of range"
        );
    }

    // -- Text view: assembling the document -------------------------------

    fn text_column_result(rows: Vec<Row>) -> ResultSet {
        ResultSet {
            columns: vec![column("Text", "nvarchar")],
            rows,
            affected: None,
            notices: Vec::new(),
        }
    }

    #[test]
    fn assemble_document_uses_a_single_rows_value_verbatim() {
        let result = text_column_result(vec![Row(vec![Value::Text(
            "CREATE PROCEDURE p\nAS\nBEGIN\nEND".to_owned(),
        )])]);
        assert_eq!(
            assemble_document(&result),
            "CREATE PROCEDURE p\nAS\nBEGIN\nEND",
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
        assert_eq!(assemble_document(&result), "CREATE PROCEDURE p\nAS\nBEGIN");
    }

    #[test]
    fn assemble_document_renders_a_null_row_as_an_empty_line() {
        let result = text_column_result(vec![
            Row(vec![Value::Text("a".to_owned())]),
            Row(vec![Value::Null]),
            Row(vec![Value::Text("c".to_owned())]),
        ]);
        assert_eq!(assemble_document(&result), "a\n\nc");
    }

    #[test]
    fn assemble_document_is_empty_for_zero_rows() {
        assert_eq!(assemble_document(&text_column_result(Vec::new())), "");
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

    // -- Text view: results bar count text ---------------------------------

    #[test]
    fn results_bar_count_text_reads_rows_for_grid_and_lines_for_text() {
        let state = SessionState::Results(Duration::from_millis(1));
        assert_eq!(results_bar_count_text(&state, 17, None), "17");
        assert_eq!(results_bar_count_text(&state, 17, Some(12)), "12 lines");
    }

    #[test]
    fn results_bar_count_text_reads_lines_for_a_truncated_text_view() {
        let state = SessionState::Truncated {
            elapsed: Duration::from_millis(1),
            rows: 5_000,
        };
        assert_eq!(
            results_bar_count_text(&state, 100, None),
            "5000 (truncated at 100)"
        );
        assert_eq!(
            results_bar_count_text(&state, 100, Some(80)),
            "80 lines (truncated at 100)"
        );
    }

    #[test]
    fn results_bar_count_text_is_a_dash_for_non_result_states() {
        for state in [
            SessionState::Empty,
            SessionState::Connecting,
            SessionState::Connected,
            SessionState::Error("boom".to_owned()),
        ] {
            assert_eq!(results_bar_count_text(&state, 5, None), "-");
        }
    }

    // -- Text view: default selection and reset -----------------------------

    #[gpui::test]
    fn a_document_shaped_result_defaults_to_the_text_view(cx: &mut gpui::TestAppContext) {
        let result = text_column_result(vec![
            Row(vec![Value::Text("line one".to_owned())]),
            Row(vec![Value::Text("line two".to_owned())]),
        ]);
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Text
        );
    }

    #[gpui::test]
    fn a_non_document_shaped_result_defaults_to_the_grid_view(cx: &mut gpui::TestAppContext) {
        let (view, vcx) = view_with_results(cx, sample_result());
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Grid
        );
    }

    #[gpui::test]
    fn a_single_row_with_no_newline_defaults_to_the_grid_view(cx: &mut gpui::TestAppContext) {
        let result = text_column_result(vec![Row(vec![Value::Text("just one line".to_owned())])]);
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Grid
        );
    }

    #[gpui::test]
    fn switching_to_a_new_result_discards_a_manual_view_choice_and_recomputes_the_default(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, vcx) = view_with_results(cx, sample_result());
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Grid
        );

        // A manual choice on a non-document result: forced into Text even
        // though the grid is the computed default.
        view.update(vcx, |view, cx| {
            view.set_view_mode_for_test(ViewMode::Text, cx);
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Text
        );

        // A new document-shaped result becomes current via show_snapshot,
        // one of the two reset points: the manual choice must not survive,
        // and the new result's own default (Text, since it is document
        // shaped) is what actually renders -- not a coincidental repeat of
        // the stale manual choice.
        let document = text_column_result(vec![
            Row(vec![Value::Text("a".to_owned())]),
            Row(vec![Value::Text("b".to_owned())]),
        ]);
        view.update(vcx, |view, cx| {
            view.show_snapshot(
                super::ResultsSnapshot {
                    source_label: "doc".into(),
                    state: SessionState::Results(Duration::from_millis(1)),
                    result: document,
                },
                cx,
            );
        });
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Text
        );

        // Now prove it was actually recomputed, not left over: manually
        // force Grid, then show a NON-document snapshot and confirm the
        // manual choice is discarded in favor of that result's own Grid
        // default.
        view.update(vcx, |view, cx| {
            view.set_view_mode_for_test(ViewMode::Grid, cx);
        });
        view.update(vcx, |view, cx| {
            view.show_snapshot(
                super::ResultsSnapshot {
                    source_label: "doc2".into(),
                    state: SessionState::Results(Duration::from_millis(1)),
                    result: text_column_result(vec![
                        Row(vec![Value::Text("x".to_owned())]),
                        Row(vec![Value::Text("y".to_owned())]),
                    ]),
                },
                cx,
            );
        });
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Text,
            "the freshly-arrived document-shaped result must default to Text again, proving the \
             default was recomputed rather than the prior manual Grid choice leaking through"
        );
    }

    #[gpui::test]
    fn the_default_is_not_computed_while_the_query_is_still_running(cx: &mut gpui::TestAppContext) {
        let result = text_column_result(vec![Row(vec![Value::Text(
            "only one row so far".to_owned(),
        )])]);
        let session = cx.new(|_cx| Session::new_for_render_test(SessionState::Running, result));
        let (view, vcx) =
            cx.add_window_view(|_window, cx| ResultsView::new(session, "public.orders", cx));
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Grid,
            "Grid must keep rendering while Running, exactly as today, with no default computed \
             yet from a still-partial result"
        );
    }
    // -- Text view: line runs ------------------------------------------

    #[test]
    fn text_view_line_runs_with_no_spans_or_selection_is_one_base_colored_run() {
        let theme = Theme::default();
        let run_font = font(theme.fonts.data.clone());
        let base = Hsla::from(rgb(theme.colors.text_primary));
        let selection_bg = Hsla::from(rgb(theme::text_selection_bg(&theme)));

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
        let selection_bg = Hsla::from(rgb(theme::text_selection_bg(&theme)));

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
        let selection_bg = Hsla::from(rgb(theme::text_selection_bg(&theme)));

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
        let selection_bg = Hsla::from(rgb(theme::text_selection_bg(&theme)));

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

    // -- Text view: switch disabled state ------------------------------

    #[gpui::test]
    fn clicking_the_disabled_text_segment_on_a_multi_column_result_does_not_switch_the_view(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, vcx) = view_with_results(cx, sample_result());
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Grid,
            "sample_result has two columns, so it is not document shaped and Grid is its default"
        );

        let text_segment_bounds = vcx
            .debug_bounds("results-view-text")
            .expect("the Text segment must still be painted, only disabled, not omitted");
        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: gpui::point(
                text_segment_bounds.origin.x + px(5.0),
                text_segment_bounds.origin.y + px(5.0),
            ),
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: gpui::point(
                text_segment_bounds.origin.x + px(5.0),
                text_segment_bounds.origin.y + px(5.0),
            ),
            modifiers: Modifiers::default(),
            click_count: 1,
        });
        vcx.run_until_parked();

        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Grid,
            "a disabled Text segment must be inert: clicking it on a multi-column result must \
             not switch the view away from Grid"
        );
    }

    #[gpui::test]
    fn clicking_the_enabled_text_segment_on_a_single_text_column_result_switches_to_text(
        cx: &mut gpui::TestAppContext,
    ) {
        // One text column but a single newline-free row: NOT document shaped, so
        // Grid is the default -- yet the Text segment is enabled (single text
        // column) and must actually switch when clicked, independent of the
        // row-count/newline condition that only governs the default.
        let result = text_column_result(vec![Row(vec![Value::Text("just one line".to_owned())])]);
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Grid,
            "a single newline-free row is not document shaped, so Grid is the default"
        );

        let text_segment_bounds = vcx
            .debug_bounds("results-view-text")
            .expect("the Text segment must be painted");
        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: gpui::point(
                text_segment_bounds.origin.x + px(5.0),
                text_segment_bounds.origin.y + px(5.0),
            ),
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: gpui::point(
                text_segment_bounds.origin.x + px(5.0),
                text_segment_bounds.origin.y + px(5.0),
            ),
            modifiers: Modifiers::default(),
            click_count: 1,
        });
        vcx.run_until_parked();

        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Text,
            "the Text segment is enabled for any single-text-column result and must switch \
             the view even when Grid was the default"
        );
    }

    // -- Text view: copy -----------------------------------------------

    #[gpui::test]
    fn copy_while_the_text_view_is_active_copies_nothing_with_no_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        let result = text_column_result(vec![
            Row(vec![Value::Text("CREATE PROCEDURE p".to_owned())]),
            Row(vec![Value::Text("    AS".to_owned())]),
        ]);
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Text
        );

        view.update_in(vcx, |view, window, cx| {
            view.copy_focused_cell(&Copy, window, cx);
        });

        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some(""),
            "Cmd/Ctrl-C in the Text view must copy the selection - if there is no selection, it must copy an empty string"
        );
    }

    #[gpui::test]
    fn copy_while_the_text_view_is_active_copies_selection(cx: &mut gpui::TestAppContext) {
        let result = text_column_result(vec![
            Row(vec![Value::Text("CREATE PROCEDURE p".to_owned())]),
            Row(vec![Value::Text("    AS".to_owned())]),
        ]);
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Text
        );

        view.update_in(vcx, |view, window, cx| {
            view.text_selection = Some((
                super::TextCaret { line: 0, byte: 7 },
                super::TextCaret { line: 1, byte: 4 },
            ));
            view.copy_focused_cell(&Copy, window, cx);
        });

        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some("PROCEDURE p\n    "),
            "Cmd/Ctrl-C in the Text view must copy the selection"
        );
    }

    #[gpui::test]
    fn copy_while_the_grid_is_active_still_copies_the_focused_cell(cx: &mut gpui::TestAppContext) {
        let (view, vcx) = view_with_results(cx, sample_result());
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Grid
        );

        view.update(vcx, |view, cx| {
            view.table_state
                .update(cx, |state, _cx| state.set_focused_cell(0, 0));
        });
        view.update_in(vcx, |view, window, cx| {
            view.copy_focused_cell(&Copy, window, cx);
        });

        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some("1"),
            "Grid's Cmd/Ctrl-C behavior must be unaffected by the Text view's own copy path"
        );
    }

    // -- Text view: character-granular selection -----------------------------

    #[gpui::test]
    fn clicking_then_shift_clicking_extends_the_selection_from_the_original_anchor(
        cx: &mut gpui::TestAppContext,
    ) {
        let result = text_column_result(vec![
            Row(vec![Value::Text("aaa".to_owned())]),
            Row(vec![Value::Text("bbb".to_owned())]),
            Row(vec![Value::Text("ccc".to_owned())]),
        ]);
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.set_text_caret_for_test(0, 1, false, cx);
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.text_selection_for_test()),
            Some(((0, 1), (0, 1)))
        );

        view.update(vcx, |view, cx| view.set_text_caret_for_test(2, 2, true, cx));
        assert_eq!(
            view.read_with(vcx, |v, _app| v.text_selection_for_test()),
            Some(((0, 1), (2, 2))),
            "a shift-click must extend from the existing anchor rather than starting a new one"
        );

        view.update(vcx, |view, cx| {
            view.set_text_caret_for_test(1, 0, false, cx);
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.text_selection_for_test()),
            Some(((1, 0), (1, 0))),
            "a plain click (no shift) must start a fresh selection at the clicked position"
        );
    }

    #[gpui::test]
    fn dragging_after_a_click_extends_the_selection_but_a_shift_click_does_not_arm_dragging(
        cx: &mut gpui::TestAppContext,
    ) {
        let result = text_column_result(vec![
            Row(vec![Value::Text("aaa".to_owned())]),
            Row(vec![Value::Text("bbb".to_owned())]),
        ]);
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.set_text_caret_for_test(0, 0, false, cx);
        });
        view.update(vcx, |view, cx| {
            view.extend_text_selection_while_dragging(1, 2, cx);
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.text_selection_for_test()),
            Some(((0, 0), (1, 2))),
            "a drag begun by a plain click must extend the live selection as the mouse moves"
        );

        view.update(vcx, ResultsView::end_text_selection_drag);
        view.update(vcx, |view, cx| {
            view.extend_text_selection_while_dragging(0, 1, cx);
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.text_selection_for_test()),
            Some(((0, 0), (1, 2))),
            "extending after the drag has ended must be a no-op"
        );

        view.update(vcx, |view, cx| view.set_text_caret_for_test(0, 2, true, cx));
        view.update(vcx, |view, cx| {
            view.extend_text_selection_while_dragging(1, 1, cx);
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.text_selection_for_test()),
            Some(((0, 0), (0, 2))),
            "a shift-click does not arm dragging, so a subsequent move must not extend it"
        );
    }

    #[test]
    fn line_selection_range_covers_only_the_lines_and_bytes_between_anchor_and_cursor() {
        let selection = (
            super::TextCaret { line: 0, byte: 2 },
            super::TextCaret { line: 2, byte: 1 },
        );
        assert_eq!(
            super::line_selection_range(selection, 0, 5),
            Some(2..5),
            "the anchor's own line is selected from its byte to the line's end"
        );
        assert_eq!(
            super::line_selection_range(selection, 1, 5),
            Some(0..5),
            "a line strictly between anchor and cursor is selected in full"
        );
        assert_eq!(
            super::line_selection_range(selection, 2, 5),
            Some(0..1),
            "the cursor's own line is selected from its start to its byte"
        );
        assert_eq!(
            super::line_selection_range(selection, 3, 5),
            None,
            "a line outside the selection's line range is not selected"
        );
    }

    #[test]
    fn line_selection_range_is_none_for_a_collapsed_selection() {
        let caret = super::TextCaret { line: 1, byte: 3 };
        assert_eq!(
            super::line_selection_range((caret, caret), 1, 10),
            None,
            "a plain click with no drag selects nothing to highlight"
        );
    }

    #[gpui::test]
    fn switching_to_a_new_result_clears_any_text_selection(cx: &mut gpui::TestAppContext) {
        let result = text_column_result(vec![
            Row(vec![Value::Text("a".to_owned())]),
            Row(vec![Value::Text("b".to_owned())]),
        ]);
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();
        view.update(vcx, |view, cx| {
            view.set_text_caret_for_test(0, 0, false, cx);
        });
        assert!(
            view.read_with(vcx, |v, _app| v.text_selection_for_test())
                .is_some()
        );

        view.update(vcx, |view, cx| {
            view.show_snapshot(
                super::ResultsSnapshot {
                    source_label: "doc".into(),
                    state: SessionState::Results(Duration::from_millis(1)),
                    result: sample_result(),
                },
                cx,
            );
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.text_selection_for_test()),
            None
        );
    }

    // -- Text view: rendering smoke tests -----------------------------------

    #[gpui::test]
    fn renders_the_text_view_without_panicking(cx: &mut gpui::TestAppContext) {
        let result = text_column_result(vec![
            Row(vec![Value::Text("CREATE PROCEDURE p".to_owned())]),
            Row(vec![Value::Text("AS".to_owned())]),
            Row(vec![Value::Text("BEGIN".to_owned())]),
            Row(vec![Value::Text("    SELECT 1;".to_owned())]),
            Row(vec![Value::Text("END".to_owned())]),
        ]);
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Text
        );
    }

    #[gpui::test]
    fn renders_the_grid_when_manually_selected_for_a_document_shaped_result(
        cx: &mut gpui::TestAppContext,
    ) {
        let result = text_column_result(vec![
            Row(vec![Value::Text("a".to_owned())]),
            Row(vec![Value::Text("b".to_owned())]),
        ]);
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();
        view.update(vcx, |view, cx| {
            view.set_view_mode_for_test(ViewMode::Grid, cx);
        });
        vcx.run_until_parked();
        assert_eq!(
            view.read_with(vcx, |v, _app| v.view_mode_for_test()),
            ViewMode::Grid
        );
    }
}

#[cfg(test)]
mod value_panel_view_tests {
    use std::time::Duration;

    use gpui::{
        AppContext as _, Focusable as _, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
        MouseUpEvent, point, px,
    };
    use zsql_core::{ColumnMeta, ResultSet, Row, Value};
    use zsql_ui::table::body_first_cell_debug_selector;

    use super::{
        CloseValuePanel, FocusValuePanel, ResultsView, SessionState, ToggleValuePanel, ValuePanel,
    };
    use crate::session::Session;
    use crate::ui::value_panel::data::{JsonLoad, PathSegment};
    use crate::ui::value_panel::view::{
        CopyTreeNodePath, CopyTreeNodeValue, FocusGridFromPanel, TreeCollapse, TreeDown,
        TreeExpand, TreeUp,
    };

    fn column(name: &str, type_name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            nullable: true,
        }
    }

    fn json_result() -> ResultSet {
        ResultSet {
            columns: vec![
                column("id", "int8"),
                column("payload", "jsonb"),
                column("note", "text"),
            ],
            rows: vec![
                Row(vec![
                    Value::Int(1),
                    Value::Json(r#"{"items":[{"sku":"A1"}]}"#.to_owned()),
                    Value::Text("hi".to_owned()),
                ]),
                Row(vec![
                    Value::Int(2),
                    Value::Json(r#"{"items":[{"sku":"B2"}]}"#.to_owned()),
                    Value::Null,
                ]),
            ],
            affected: None,
            notices: Vec::new(),
        }
    }

    fn view_with_results(
        cx: &mut gpui::TestAppContext,
        result: ResultSet,
    ) -> (gpui::Entity<ResultsView>, &mut gpui::VisualTestContext) {
        let state = SessionState::Results(Duration::from_millis(1));
        let session = cx.new(|_cx| Session::new_for_render_test(state, result));
        cx.add_window_view(|window, cx| {
            let view = ResultsView::new(session, "public.orders", cx);
            window.focus(&view.focus_handle(cx));
            view
        })
    }

    #[gpui::test]
    fn space_toggles_the_panel_open_then_closed(cx: &mut gpui::TestAppContext) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();
        view.update(vcx, |view, cx| {
            view.table_state
                .update(cx, |state, _cx| state.set_focused_cell(0, 0));
        });

        vcx.dispatch_action(ToggleValuePanel);
        assert!(view.read_with(vcx, |v, app| v.value_panel.read(app).is_open()));

        vcx.dispatch_action(ToggleValuePanel);
        assert!(!view.read_with(vcx, |v, app| v.value_panel.read(app).is_open()));
    }

    #[gpui::test]
    fn esc_closes_the_panel_and_leaves_the_focused_cell_untouched(cx: &mut gpui::TestAppContext) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();
        view.update(vcx, |view, cx| {
            view.table_state
                .update(cx, |state, _cx| state.set_focused_cell(1, 0));
            view.value_panel.update(cx, ValuePanel::open);
        });

        vcx.dispatch_action(CloseValuePanel);

        view.read_with(vcx, |v, app| {
            assert!(!v.value_panel.read(app).is_open());
            assert_eq!(v.table_state.read(app).focused_cell(), Some((1, 0)));
        });
    }

    #[gpui::test]
    fn double_click_opens_the_panel_for_the_clicked_cell(cx: &mut gpui::TestAppContext) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();

        let table_state = view.read_with(vcx, |v, _app| v.table_state.clone());
        let cell_bounds = vcx
            .debug_bounds(body_first_cell_debug_selector(&table_state))
            .expect("the top-of-viewport body cell must be painted");
        let position = point(
            cell_bounds.origin.x + px(5.0),
            cell_bounds.origin.y + px(5.0),
        );
        let mouse_down = |click_count| MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count,
            first_mouse: false,
        };
        vcx.simulate_event(mouse_down(1));
        vcx.run_until_parked();
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            Some((0, 0)),
            "the first mouse-down of the double click must select the cell, same as a plain click"
        );

        vcx.simulate_event(mouse_down(2));
        vcx.run_until_parked();

        assert!(view.read_with(vcx, |v, app| v.value_panel.read(app).is_open()));
        assert_eq!(
            table_state.read_with(vcx, |s, _app| s.focused_cell()),
            Some((0, 0))
        );
    }

    #[gpui::test]
    fn opening_the_panel_leaves_the_focused_cell_selection_untouched(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();
        view.update(vcx, |view, cx| {
            view.table_state
                .update(cx, |state, _cx| state.set_focused_cell(0, 1));
        });

        vcx.dispatch_action(ToggleValuePanel);
        vcx.run_until_parked();

        view.read_with(vcx, |v, app| {
            assert!(v.value_panel.read(app).is_open());
            assert_eq!(v.table_state.read(app).focused_cell(), Some((0, 1)));
        });
    }

    #[gpui::test]
    fn context_menu_actions_operate_on_the_right_clicked_cell(cx: &mut gpui::TestAppContext) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.open_cell_context_menu(1, 0, point(px(10.0), px(10.0)), cx);
        });
        view.read_with(vcx, |v, app| {
            assert_eq!(v.table_state.read(app).focused_cell(), Some((1, 0)));
            assert!(v.cell_context_menu.is_some());
        });

        view.update(vcx, |view, cx| {
            view.copy_column_name(cx);
        });
        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied.as_deref(), Some("id"));

        view.update(vcx, |view, cx| {
            view.open_cell_context_menu(1, 1, point(px(10.0), px(10.0)), cx);
            view.copy_row_as_json(cx);
        });
        let copied = vcx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&copied).unwrap();
        assert_eq!(parsed["id"], serde_json::json!(2));
        assert_eq!(
            parsed["payload"],
            serde_json::json!({"items": [{"sku": "B2"}]})
        );
        assert_eq!(parsed["note"], serde_json::Value::Null);
    }

    #[gpui::test]
    fn view_value_from_the_context_menu_opens_the_panel_and_closes_the_menu(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.open_cell_context_menu(1, 1, point(px(10.0), px(10.0)), cx);
            view.view_value_from_menu(cx);
        });

        view.read_with(vcx, |v, app| {
            assert!(v.value_panel.read(app).is_open());
            assert!(v.cell_context_menu.is_none());
            assert_eq!(v.table_state.read(app).focused_cell(), Some((1, 1)));
        });
    }

    #[gpui::test]
    fn unpinned_panel_follows_focus_and_pinned_panel_freezes_on_its_cell(
        cx: &mut gpui::TestAppContext,
    ) {
        // The results view keys panel content by `31 * row + col` (see
        // `sync_value_panel_content`): (0, 0) -> 0, (1, 0) -> 31.
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();
        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 0);
                cx.notify();
            });
            view.value_panel.update(cx, ValuePanel::open);
            cx.notify();
        });
        vcx.run_until_parked(); // a render pass syncs the panel's content

        view.read_with(vcx, |v, app| {
            assert_eq!(
                v.value_panel
                    .read(app)
                    .state_for_test()
                    .content()
                    .map(|c| c.id),
                Some(0),
                "an open unpinned panel must target the focused cell"
            );
        });

        // Unpinned: moving the grid's selection re-targets the panel.
        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(1, 0);
                cx.notify();
            });
            cx.notify();
        });
        vcx.run_until_parked();
        view.read_with(vcx, |v, app| {
            assert_eq!(
                v.value_panel
                    .read(app)
                    .state_for_test()
                    .content()
                    .map(|c| c.id),
                Some(31),
                "an unpinned panel must follow the grid's live selection"
            );
        });

        // Pin, then move the grid: the panel must keep its pinned content.
        view.update(vcx, |view, cx| {
            view.value_panel
                .update(cx, |p, _cx| p.state_mut_for_test().pin());
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 0);
                cx.notify();
            });
            cx.notify();
        });
        vcx.run_until_parked();
        view.read_with(vcx, |v, app| {
            assert!(
                v.value_panel.read(app).is_pinned(),
                "the panel must report itself pinned"
            );
            assert_eq!(
                v.table_state.read(app).focused_cell(),
                Some((0, 0)),
                "the grid's own selection must keep moving normally"
            );
            assert_eq!(
                v.value_panel
                    .read(app)
                    .state_for_test()
                    .content()
                    .map(|c| c.id),
                Some(31),
                "a pinned panel must keep showing its pinned content despite the grid \
                 selection moving"
            );
        });
    }

    #[gpui::test]
    fn tab_moves_focus_between_the_grid_and_the_panel(cx: &mut gpui::TestAppContext) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();
        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 1);
                cx.notify();
            });
            view.value_panel.update(cx, ValuePanel::open);
            cx.notify();
        });
        vcx.run_until_parked();

        let panel_focus =
            view.read_with(vcx, |v, app| v.value_panel.read(app).focus_handle().clone());
        // `FocusGridFromPanel` returns focus to the grid pane the panel tabbed
        // in from: the panel's parent handle is the view's own focus handle.
        let grid_focus = view.read_with(vcx, ResultsView::focus_handle);

        vcx.dispatch_action(FocusValuePanel);
        vcx.update(|window, cx| {
            assert_eq!(window.focused(cx).as_ref(), Some(&panel_focus));
        });

        vcx.dispatch_action(FocusGridFromPanel);
        vcx.update(|window, cx| {
            assert_eq!(window.focused(cx).as_ref(), Some(&grid_focus));
        });
    }

    #[gpui::test]
    fn copy_tree_node_value_and_path_target_the_selected_nested_node(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 1); // the "payload" jsonb column
                cx.notify();
            });
            view.value_panel.update(cx, ValuePanel::open);
            cx.notify();
        });
        vcx.run_until_parked(); // a render pass parses the cell's JSON

        view.update(vcx, |view, cx| {
            view.value_panel.update(cx, |p, _cx| {
                p.state_mut_for_test().select_tree_path(vec![
                    PathSegment::Key("items".to_owned()),
                    PathSegment::Index(0),
                    PathSegment::Key("sku".to_owned()),
                ]);
            });
        });

        vcx.dispatch_action(FocusValuePanel);
        vcx.dispatch_action(CopyTreeNodeValue);
        let copied_value = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied_value.as_deref(),
            Some("\"A1\""),
            "Cmd/Ctrl-C must copy the selected node's own value, not the whole document"
        );

        vcx.dispatch_action(CopyTreeNodePath);
        let copied_path = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(copied_path.as_deref(), Some("$.items[0].sku"));
    }

    /// Cmd/Ctrl-C with the panel focused over a non-JSON cell (no parsed
    /// tree to copy a node from) must still copy the panel's own target
    /// cell -- the same text `Copy value`/`copy_focused_cell` produce --
    /// rather than silently doing nothing.
    #[gpui::test]
    fn copy_with_the_panel_focused_over_a_non_json_cell_copies_the_cells_value(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 2); // the "note" text column
                cx.notify();
            });
            view.value_panel.update(cx, ValuePanel::open);
            cx.notify();
        });
        vcx.run_until_parked();

        vcx.dispatch_action(FocusValuePanel);
        vcx.dispatch_action(CopyTreeNodeValue);
        let copied = vcx.read_from_clipboard().and_then(|item| item.text());
        assert_eq!(
            copied.as_deref(),
            Some("hi"),
            "with no parsed JSON tree to copy a node from, Cmd/Ctrl-C must fall back to the \
             panel's own target cell value"
        );
    }

    #[gpui::test]
    fn null_and_empty_text_cells_render_as_distinguishable_panel_states(
        cx: &mut gpui::TestAppContext,
    ) {
        let result = ResultSet {
            columns: vec![column("a", "text"), column("b", "text")],
            rows: vec![Row(vec![Value::Null, Value::Text(String::new())])],
            affected: None,
            notices: Vec::new(),
        };
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();

        for col in [0usize, 1usize] {
            view.update(vcx, |view, cx| {
                view.table_state.update(cx, |state, cx| {
                    state.set_focused_cell(0, col);
                    cx.notify();
                });
                view.value_panel.update(cx, ValuePanel::open);
                cx.notify();
            });
            vcx.run_until_parked();
        }

        view.read_with(vcx, |v, app| {
            let result = v.effective_result(app);
            assert_eq!(
                super::value_panel::data::renderer_for(
                    &result.rows[0].0[0],
                    &result.columns[0].type_name
                ),
                super::value_panel::data::RendererKind::Null,
                "a Null cell must render the explicit NULL state, not the Text renderer"
            );
            assert_eq!(
                super::value_panel::data::renderer_for(
                    &result.rows[0].0[1],
                    &result.columns[1].type_name
                ),
                super::value_panel::data::RendererKind::Text,
                "an empty-string cell must still render as Text, not Null"
            );
        });
    }

    #[gpui::test]
    fn tree_keyboard_actions_move_selection_and_expand_collapse_nodes(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 1); // the "payload" jsonb column
                cx.notify();
            });
            view.value_panel.update(cx, ValuePanel::open);
            cx.notify();
        });
        vcx.run_until_parked(); // a render pass parses the cell's JSON
        view.read_with(vcx, |v, app| {
            assert_eq!(
                v.value_panel
                    .read(app)
                    .state_for_test()
                    .selected_tree_path(),
                &[] as &[PathSegment]
            );
        });

        vcx.dispatch_action(FocusValuePanel);

        vcx.dispatch_action(TreeExpand);
        view.read_with(vcx, |v, app| {
            assert!(
                v.value_panel
                    .read(app)
                    .state_for_test()
                    .is_tree_node_expanded(&[] as &[PathSegment]),
                "TreeExpand on the root object must expand it"
            );
        });

        vcx.dispatch_action(TreeDown);
        view.read_with(vcx, |v, app| {
            assert_eq!(
                v.value_panel
                    .read(app)
                    .state_for_test()
                    .selected_tree_path(),
                [PathSegment::Key("items".to_owned())],
                "TreeDown must move selection to the root's first revealed child"
            );
        });

        vcx.dispatch_action(TreeExpand);
        view.read_with(vcx, |v, app| {
            assert!(
                v.value_panel
                    .read(app)
                    .state_for_test()
                    .is_tree_node_expanded(&[PathSegment::Key("items".to_owned())])
            );
        });

        vcx.dispatch_action(TreeCollapse);
        view.read_with(vcx, |v, app| {
            assert!(
                !v.value_panel
                    .read(app)
                    .state_for_test()
                    .is_tree_node_expanded(&[PathSegment::Key("items".to_owned())]),
                "TreeCollapse must collapse the selected node"
            );
        });

        vcx.dispatch_action(TreeUp);
        view.read_with(vcx, |v, app| {
            assert_eq!(
                v.value_panel
                    .read(app)
                    .state_for_test()
                    .selected_tree_path(),
                &[] as &[PathSegment],
                "TreeUp must move selection back to the previous visible row"
            );
        });
    }

    /// A JSON cell's panel renders in every mode without panicking, covering
    /// the JSON tree/pretty/raw paths, the oversized-preview path, and the
    /// invalid-JSON fallback path together in one render smoke test.
    #[gpui::test]
    fn renders_the_panel_for_every_json_state_without_panicking(cx: &mut gpui::TestAppContext) {
        let result = ResultSet {
            columns: vec![column("payload", "jsonb"), column("bad", "jsonb")],
            rows: vec![Row(vec![
                Value::Json(r#"{"items":[{"sku":"A1"},{"sku":"B2"}]}"#.to_owned()),
                Value::Json("not json".to_owned()),
            ])],
            affected: None,
            notices: Vec::new(),
        };
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();

        for mode in super::value_panel::data::JSON_MODES {
            view.update(vcx, |view, cx| {
                view.table_state.update(cx, |state, cx| {
                    state.set_focused_cell(0, 0);
                    cx.notify();
                });
                view.value_panel.update(cx, ValuePanel::open);
                view.value_panel
                    .update(cx, |p, _cx| p.state_mut_for_test().set_json_mode(mode));
                cx.notify();
            });
            vcx.run_until_parked();
        }

        // The second column's `Value::Json` fails to parse: the panel must
        // still render (Raw fallback) rather than panicking.
        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 1);
                cx.notify();
            });
            cx.notify();
        });
        vcx.run_until_parked();

        view.read_with(vcx, |v, app| {
            assert!(matches!(
                v.value_panel.read(app).json_load_for_test(),
                Some(JsonLoad::Invalid(_))
            ));
        });
    }

    /// A results grid panel for the non-JSON renderers (Bytes, Timestamp,
    /// Bool, Unknown) renders in every mode without panicking, mirroring
    /// [`renders_the_panel_for_every_json_state_without_panicking`]'s
    /// coverage of the JSON renderer.
    #[gpui::test]
    fn renders_the_panel_for_bytes_timestamp_bool_and_unknown_cells_without_panicking(
        cx: &mut gpui::TestAppContext,
    ) {
        let result = ResultSet {
            columns: vec![
                column("blob", "bytea"),
                column("seen_at", "timestamptz"),
                column("active", "bool"),
                column("location", "point"),
            ],
            rows: vec![Row(vec![
                Value::Bytes(vec![0x00, 0x41, 0xff, 0x10]),
                Value::Timestamp("2026-07-14T09:12:31+02:00".to_owned()),
                Value::Bool(true),
                Value::Unknown("(1,2)".to_owned()),
            ])],
            affected: None,
            notices: Vec::new(),
        };
        let (view, vcx) = view_with_results(cx, result);
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 0);
                cx.notify();
            });
            view.value_panel.update(cx, ValuePanel::open);
            cx.notify();
        });
        for mode in super::value_panel::data::BYTES_MODES {
            view.update(vcx, |view, cx| {
                view.value_panel
                    .update(cx, |p, _cx| p.state_mut_for_test().set_bytes_mode(mode));
                cx.notify();
            });
            vcx.run_until_parked();
        }

        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 1);
                cx.notify();
            });
            cx.notify();
        });
        for mode in super::value_panel::data::TIMESTAMP_MODES {
            view.update(vcx, |view, cx| {
                view.value_panel
                    .update(cx, |p, _cx| p.state_mut_for_test().set_timestamp_mode(mode));
                cx.notify();
            });
            vcx.run_until_parked();
        }

        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 2);
                cx.notify();
            });
            cx.notify();
        });
        vcx.run_until_parked();

        view.update(vcx, |view, cx| {
            view.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 3);
                cx.notify();
            });
            cx.notify();
        });
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn dragging_the_divider_resizes_the_panel_clamped_to_configured_bounds(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, vcx) = view_with_results(cx, json_result());
        vcx.run_until_parked();
        view.update(vcx, |view, cx| {
            view.table_state
                .update(cx, |state, _cx| state.set_focused_cell(0, 0));
            view.value_panel.update(cx, ValuePanel::open);
            cx.notify();
        });
        vcx.run_until_parked();

        let (min_width, max_width, start_width) = view.read_with(vcx, |v, _app| {
            (
                v.value_panel_min_width,
                v.value_panel_max_width,
                v.value_panel_width,
            )
        });

        let divider_bounds = vcx
            .debug_bounds("value-panel-divider")
            .expect("the resize divider must be painted while the panel is docked open");
        let origin = divider_bounds.origin;

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: origin,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.value_panel_width),
            start_width,
            "pressing down on the divider must not itself resize the panel"
        );

        // `on_mouse_move` only fires while the pointer sits inside the
        // dragged element's own hitbox, so the drag stays within the test
        // window's bounds (1920x1080) rather than moving off-screen: the
        // panel docks on the right edge, so dragging to the window's left
        // edge grows it (clamped at the configured maximum) and dragging to
        // its right edge shrinks it (clamped at the configured minimum).
        let window_left = px(0.0);
        let window_right = px(1_900.0);

        vcx.simulate_event(MouseMoveEvent {
            position: point(window_left, origin.y),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.value_panel_width),
            max_width,
            "dragging to the window's left edge must clamp the panel at its configured maximum \
             width"
        );

        vcx.simulate_event(MouseMoveEvent {
            position: point(window_right, origin.y),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.value_panel_width),
            min_width,
            "dragging to the window's right edge must clamp the panel at its configured \
             minimum width"
        );

        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: point(window_right, origin.y),
            modifiers: Modifiers::default(),
            click_count: 1,
        });

        // Releasing ends the drag: a further move must not resize the panel.
        vcx.simulate_event(MouseMoveEvent {
            position: origin,
            pressed_button: None,
            modifiers: Modifiers::default(),
        });
        assert_eq!(
            view.read_with(vcx, |v, _app| v.value_panel_width),
            min_width,
            "moving the mouse after mouse-up must not resume resizing the panel"
        );
    }
}
