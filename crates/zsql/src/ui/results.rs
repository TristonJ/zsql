//! The results grid: a virtualized table view over a `Session`'s current
//! [`SessionState`] and accumulated result set

use std::ops::Range;

use gpui::{
    AnyElement, App, ClipboardItem, Context, Div, Entity, FocusHandle, Focusable, KeyBinding,
    MouseButton, Pixels, Point, Render, SharedString, Window, actions, div, prelude::*, px, rgb,
};
use zsql_core::{ColumnMeta, ResultSet, RowCount, group_thousands};
use zsql_ui::table::{Gutter, RowNumberStyle, Table, TableColumn, TableRow, TableState, measure};
use zsql_ui::theme::{ActiveTheme, Theme};

use super::appearance::AppearanceModalView;
use super::connections::ConnectionManagerView;
use super::format::{ValueKind, format_value};
use super::tabs::ResultsSnapshot;
use super::theme;
use crate::config::{LayoutConfig, ValuePanelConfig};
use crate::session::{LivenessState, Session, SessionState};
use crate::ui::format::format_value_for_clipboard;
use crate::ui::results::pager::PreviewControls;
use crate::ui::results::text_view::TextView;
use crate::ui::value_panel::{self, ValuePanel};

mod appearance_trigger;
mod cell_menu;
mod empty_state;
pub(crate) mod pager;
mod panel_host;
mod status_bar;
mod text_view;
mod toolbar;

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
        PrevPage,
        NextPage,
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
        KeyBinding::new("ctrl-[", PrevPage, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-]", NextPage, Some(KEY_CONTEXT)),
    ]);
    value_panel::init(cx);
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
    /// Parallel to `column_widths`: `true` for a column whose width came
    /// from a manual header-border drag rather than the auto-fit estimate,
    /// so [`ResultsView::sync_dimensions`] leaves it alone as further rows
    /// stream in. Cleared alongside `column_widths` by
    /// [`ResultsView::reset_for_new_result`].
    column_width_overrides: Vec<bool>,
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
    /// The connection-manager modal the Empty-state "Add connection" button
    /// opens
    connections_modal: Option<Entity<ConnectionManagerView>>,
    /// The Appearance modal the status bar's theme trigger opens
    appearance_modal: Option<Entity<AppearanceModalView>>,
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
    /// The text view for results when they're displayed as a single text doc
    text_view: Entity<TextView>,
    /// The active tab's sort/page state and control dispatcher, while it is
    /// a live, unedited generated preview. `None` otherwise -- a script
    /// tab, a schema tab, or a generated tab that has been edited -- which
    /// is what renders the header sort affordance and pager bar inert
    /// without hiding the grid itself. Set by
    /// [`ResultsView::set_preview_controls`], called from
    /// `ui::tabs::TabModel` whenever the active tab (or its dirty/kind
    /// state) changes.
    preview: Option<PreviewControls>,
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
        let text_view = cx.new(TextView::new);

        let mut view = Self {
            session,
            source_label: source_label.into(),
            frozen: None,
            column_widths: Vec::new(),
            column_width_overrides: Vec::new(),
            column_max_body_chars: Vec::new(),
            folded_row_count: 0,
            table_state: cx.new(TableState::new),
            focus_handle,
            cell_context_menu: None,
            connections_modal: None,
            appearance_modal: None,
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
            text_view,
            preview: None,
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

    /// Wire the connection-manager modal the Empty-state "Add connection"
    /// button opens, so clicking it opens the same shared instance
    pub fn set_connections_modal(&mut self, connections: Entity<ConnectionManagerView>) {
        self.connections_modal = Some(connections);
    }

    /// Wire the Appearance modal the status bar's theme trigger opens, so
    /// clicking it opens this workspace's shared instance rather than
    /// constructing a new one on each click.
    pub fn set_appearance_modal(&mut self, appearance: Entity<AppearanceModalView>) {
        self.appearance_modal = Some(appearance);
    }

    /// Set (or clear) the active tab's sort/page controls. Called by
    /// `ui::tabs::TabModel` every time the active tab, or that tab's
    /// dirty/kind state, changes -- see [`ResultsView::preview`]'s own doc
    /// comment for what `None` renders.
    pub fn set_preview_controls(
        &mut self,
        preview: Option<PreviewControls>,
        cx: &mut Context<Self>,
    ) {
        self.preview = preview;
        cx.notify();
    }

    /// The last-page number the currently rendered pager snapshot would show,
    /// or `None` when there are no active preview controls or the total is not
    /// yet known. Lets a test assert the rendered snapshot -- not just the
    /// owning tab -- reflects an asynchronously fetched row count.
    #[cfg(test)]
    pub(crate) fn preview_last_page_number_for_test(&self) -> Option<u64> {
        self.preview
            .as_ref()
            .and_then(|preview| preview.state.last_page_number())
    }

    /// The active tab's own total row count, once known: the value the
    /// status bar renders via [`format_total_row_count`]. Sourced from
    /// [`ResultsView::preview`] -- the active tab's own frozen sort/page
    /// state -- rather than from `session` directly, so a tab that is not
    /// currently live never shows a different tab's most recently fetched
    /// total. `None` while the active tab is not a live, unedited generated
    /// preview, or that preview's own count fetch has not resolved yet.
    fn active_total_row_count(&self) -> Option<RowCount> {
        self.preview
            .as_ref()
            .and_then(|preview| preview.state.total_rows())
    }

    /// Test-only mirror of [`ResultsView::active_total_row_count`], for
    /// asserting on the exact value the status bar would render.
    #[cfg(test)]
    pub(crate) fn active_total_row_count_for_test(&self) -> Option<RowCount> {
        self.active_total_row_count()
    }

    /// [`PrevPage`]'s handler: step the active preview's pager back one
    /// page. A no-op with no visible side effect while `preview` is `None`
    /// (the active tab is not a live generated preview) or already on page
    /// 1, matching [`NextPage`]'s own symmetric behavior.
    fn prev_page(&mut self, _action: &PrevPage, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(preview) = &self.preview {
            (preview.dispatch.clone())(pager::PreviewAction::PrevPage, window, cx);
        }
    }

    /// [`NextPage`]'s handler; see [`ResultsView::prev_page`].
    fn next_page(&mut self, _action: &NextPage, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(preview) = &self.preview {
            (preview.dispatch.clone())(pager::PreviewAction::NextPage, window, cx);
        }
    }

    /// Follow `session`'s state/result live under `source_label`, e.g. for
    /// the tab that `session` is currently running a query for. Every
    /// render reads straight off `session` until the next
    /// [`ResultsView::show_snapshot`] or `show_live` call.
    pub fn show_live(&mut self, source_label: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.source_label = source_label.into();
        self.frozen = None;
        self.reset_for_new_result(cx);
        self.sync_dimensions(cx);
        cx.notify();
    }

    /// Freeze the grid to `snapshot` instead of following `session` live,
    /// e.g. when switching to a tab that is not the one `session` is
    /// currently running a query for.
    pub fn show_snapshot(&mut self, snapshot: ResultsSnapshot, cx: &mut Context<Self>) {
        self.source_label = snapshot.source_label.clone();
        self.frozen = Some(snapshot);
        self.reset_for_new_result(cx);
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
    fn reset_for_new_result(&mut self, cx: &mut Context<Self>) {
        self.column_widths = Vec::new();
        self.column_width_overrides = Vec::new();
        self.column_max_body_chars = Vec::new();
        self.folded_row_count = 0;
        self.view_mode = ViewMode::Grid;
        self.view_mode_defaulted = false;
        self.text_view.update(cx, |tv, _c| tv.reset());
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
            self.column_width_overrides = vec![false; result.columns.len()];
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

        // A column flagged in `column_width_overrides` was set by a manual
        // header-border drag rather than this auto-fit estimate, so its
        // prior width is carried forward untouched rather than
        // recomputed here.
        let table_style = Self::table_style(cx.theme());
        let previous_widths = self.column_widths.clone();
        self.column_widths = result
            .columns
            .iter()
            .zip(self.column_max_body_chars.iter())
            .enumerate()
            .map(|(index, (column, &max_body_chars))| {
                let auto_fit = || column_width_from_parts(column, max_body_chars, &table_style);
                if self
                    .column_width_overrides
                    .get(index)
                    .copied()
                    .unwrap_or(false)
                {
                    previous_widths.get(index).copied().unwrap_or_else(auto_fit)
                } else {
                    auto_fit()
                }
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
    fn apply_default_view_mode_if_terminal(&mut self, cx: &mut Context<Self>) {
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

    /// The main content area: the virtualized grid when results are
    /// available, or a centered prompt/status message otherwise.
    /// `text_document` is `Some` exactly while `view_mode` is `Text`, so
    /// [`ResultsView::render_grid_or_text`] never reassembles it.
    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let active_theme = cx.theme();
        let state = self.effective_state(cx).clone();
        let has_columns = !self.effective_result(cx).columns.is_empty();

        let inner = match state {
            SessionState::Results(_)
            | SessionState::Truncating { .. }
            | SessionState::Truncated { .. } => self.render_grid_or_text(window, cx),
            // Once the streaming query's `Columns` event has arrived there
            // is a real (if partial) result set to paint, so switch to the
            // grid immediately rather than waiting for `Done`
            SessionState::Running if has_columns => self.render_grid_or_text(window, cx),
            SessionState::Empty => Self::render_placeholder(
                active_theme.colors.text_tertiary,
                empty_state::TITLE,
                empty_state::DETAIL,
                active_theme,
            )
            .child(empty_state::render_add_connection_cta(
                self.connections_modal.clone(),
                window,
                cx,
            ))
            .into_any_element(),
            SessionState::Connecting => Self::render_placeholder(
                active_theme.colors.text_tertiary,
                "Connecting...",
                "Establishing a connection to the configured database.",
                active_theme,
            )
            .into_any_element(),
            SessionState::Connected => Self::render_placeholder(
                active_theme.colors.text_tertiary,
                "Connected",
                "Run a query to see results here.",
                active_theme,
            )
            .into_any_element(),
            SessionState::Running => Self::render_placeholder(
                active_theme.colors.text_tertiary,
                "Running query...",
                "Streaming results from the database.",
                active_theme,
            )
            .into_any_element(),
            SessionState::Error(message) => Self::render_placeholder(
                active_theme.colors.status_error,
                "Query failed",
                &message,
                active_theme,
            )
            .into_any_element(),
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
            .child(inner)
            .child(self.render_value_panel_divider(cx))
            .child(
                div()
                    .flex()
                    .h_full()
                    .w(self.value_panel_width)
                    .child(self.value_panel.clone()),
            )
            .into_any_element()
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
            .flex_1()
            .min_w_0()
    }

    /// The grid, or the Text view when `view_mode` is `Text` and the result
    /// actually has a single text-typed column to show as a document --
    /// falling back to the grid otherwise, so a stale `Text` selection left
    /// over from a differently-shaped result (or the interval before
    /// `sync_dimensions` next runs) never renders an empty document pane.
    fn render_grid_or_text(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.view_mode == ViewMode::Text && self.text_view.read(cx).has_document() {
            self.text_view.clone().into_any_element()
        } else {
            self.render_grid(window, cx)
                .flex_1()
                .min_w_0()
                .into_any_element()
        }
    }

    /// The two-pane virtualized grid (pinned row numbers + horizontally
    /// scrolling data columns), built by composing `zsql_ui::table::Table`.
    fn render_grid(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let row_count = self.effective_result(cx).rows.len();
        // Detached from `cx`'s borrow (rather than the usual `&Theme`) so it
        // can still be read after `build_columns` needs `cx` mutably for its
        // own header hover-state bookkeeping.
        let active_theme = cx.theme().clone();
        let columns = self.build_columns(window, cx);

        Table::new("results-grid", &self.table_state)
            .style(Self::table_style(&active_theme))
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
            .resizable_columns(px(theme::MIN_COLUMN_WIDTH), Self::resize_column)
            .render(cx)
    }

    /// [`zsql_ui::table::Table::resizable_columns`]'s live-resize callback:
    /// stores `column`'s new `width` and marks it so a later
    /// [`ResultsView::sync_dimensions`] call leaves it alone, mirroring
    /// [`ResultsView::value_panel_drag_move`]'s per-move update. Never
    /// touches the grid's focused cell or keyboard focus.
    fn resize_column(
        &mut self,
        column: usize,
        width: Pixels,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _span = tracing::trace_span!("results_resize_column", column).entered();
        if let Some(slot) = self.column_widths.get_mut(column) {
            *slot = width;
        }
        if let Some(overridden) = self.column_width_overrides.get_mut(column) {
            *overridden = true;
        }
        cx.notify();
    }

    /// The one `TableStyle` both `render_grid`'s live `Table` and
    /// `column_width_from_parts`'s width estimate use, so a column's
    /// measured width can never drift from the padding it is actually
    /// rendered with.
    fn table_style(active_theme: &Theme) -> zsql_ui::table::TableStyle {
        zsql_ui::table::TableStyle::themed(active_theme)
    }

    /// The data pane's columns: each column's cached width plus its header
    /// content (name, type-tag badge, and the sort affordance from
    /// [`ResultsView::preview`]).
    fn build_columns(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<TableColumn> {
        let active_theme = cx.theme().clone();
        let preview = self.preview.as_ref();
        let columns: Vec<ColumnMeta> = self.effective_result(cx).columns.clone();
        columns
            .iter()
            .zip(self.column_widths.iter())
            .map(|(column, &width)| {
                let header =
                    pager::sortable_column_header(column, &active_theme, preview, window, cx);
                TableColumn::new(width, header)
            })
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
        let text_view = self.text_view.read(cx);
        let document = if all {
            text_view.document().map(str::to_owned)
        } else {
            text_view
                .document()
                .map(|_| text_view.selected_text().unwrap_or_default())
        };
        let Some(document) = document else {
            tracing::trace!("copy invoked with no results text document; nothing to do");
            return;
        };

        tracing::debug!(
            chars = document.chars().count(),
            "copied the results text view's document"
        );
        cx.write_to_clipboard(ClipboardItem::new_string(document));
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
        let Some((current_row, current_col)) = self.table_state.read(cx).focused_cell() else {
            tracing::trace!("moved the results grid selection to (0, 0)");
            self.table_state.update(cx, |state, cx| {
                state.set_focused_cell(0, 0);
                cx.notify();
            });
            return;
        };
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

impl Render for ResultsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.text_view.update(cx, |text_view, cx| {
            text_view.update_document(self.effective_result(cx))
        });

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
            .on_action(cx.listener(Self::prev_page))
            .on_action(cx.listener(Self::next_page))
            .on_mouse_move(cx.listener(Self::value_panel_drag_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::end_value_panel_drag))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::end_value_panel_drag))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(cx.theme().colors.bg_app))
            .child(self.render_bar(window, cx))
            .child(self.render_body(window, cx))
            .child(self.render_status_bar(cx))
            .children(self.render_cell_context_menu(cx))
    }
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
        SessionState::Connecting => (colors.status_warn, "Connecting..."),
        SessionState::Connected | SessionState::Results(_) => (colors.accent, "Connected"),
        SessionState::Running => (colors.accent, "Running..."),
        SessionState::Truncating { .. } => (colors.status_limited, "Running... (truncated)"),
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
mod tests;
