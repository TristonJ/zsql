//! Hosting the value panel: its resize divider drag and its open/close/
//! follow-selection wiring against the grid's focused cell.

use gpui::{
    Context, Div, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Stateful, Window, div,
    prelude::*, rgb,
};
use zsql_ui::theme::ActiveTheme;

use super::{CloseValuePanel, FocusValuePanel, ResultsView, ToggleValuePanel};
use crate::ui::value_panel::ValuePanel;
use crate::ui::value_panel::data::ValuePanelContent;

impl ResultsView {
    // ---- value panel divider drag -------------------------------------

    /// The value panel's resize divider: a draggable strip on the panel's
    /// left edge, clamped between `value_panel_min_width`/`max_width`.
    pub(super) fn render_value_panel_divider(&self, cx: &Context<Self>) -> Stateful<Div> {
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

    pub(super) fn end_value_panel_drag(
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
    pub(super) fn value_panel_drag_move(
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

    // ---- value panel: open/close/pin/follow-selection ----------------

    /// Toggle the value panel for the focused cell (the grid's `space`
    /// binding). A no-op while nothing is selected.
    #[tracing::instrument(name = "results_toggle_value_panel", skip_all)]
    pub(super) fn toggle_value_panel(
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
    pub(super) fn close_value_panel(
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
    pub(super) fn focus_value_panel(
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
    pub(super) fn open_value_panel_for(
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
    pub(super) fn sync_value_panel_content(
        &mut self,
        cx: &mut Context<Self>,
        cell: Option<(usize, usize)>,
    ) {
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
}
