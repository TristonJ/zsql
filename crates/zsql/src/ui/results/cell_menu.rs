//! The results grid's right-click cell context menu and its clipboard
//! actions.

use gpui::{ClipboardItem, Context, Pixels, Point, prelude::*};
use zsql_ui::context_menu::{ContextMenu, ContextMenuItem};

use super::{CellContextMenuState, Copy, ResultsView};
use crate::ui::format;
use crate::ui::value_panel::ValuePanel;

impl ResultsView {
    /// Open the right-click context menu for `(row, col)`, anchored at
    /// `position` (the triggering click, in window coordinates), and select
    /// that cell -- so the menu's `Copy value`/`Copy row as JSON`/`Copy
    /// column name` items, which all act on the focused cell, target the
    /// cell that was actually right-clicked.
    pub(super) fn open_cell_context_menu(
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
    pub(super) fn view_value_from_menu(&mut self, cx: &mut Context<Self>) {
        self.value_panel.update(cx, ValuePanel::open);
        let cell = self.table_state.read(cx).focused_cell();
        tracing::debug!(?cell, "opened the value panel from the cell context menu");
        self.close_cell_context_menu(cx);
        cx.notify();
    }

    /// `Copy row`: serialize every cell of the focused row via its own
    /// [`Value`]'s display text (not its JSON representation), joined by
    /// comma, and write it to the clipboard. A no-op while nothing is selected.
    #[tracing::instrument(name = "results_copy_row", skip_all)]
    pub(super) fn copy_row(&mut self, cx: &mut Context<Self>) {
        let Some((row, _col)) = self.table_state.read(cx).focused_cell() else {
            tracing::trace!("copy-row invoked with no results grid selection; nothing to do");
            return;
        };
        let result = self.effective_result(cx);
        let Some(row_data) = result.rows.get(row) else {
            return;
        };
        let text = format::row_as_csv_string(row_data);
        tracing::debug!(row, "copied a results grid row to the clipboard");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// `Copy row as JSON`: serialize every cell of the focused row, each via
    /// its own [`Value`]'s JSON representation (not `format_value`'s display
    /// text) keyed by column name, and write it to the clipboard. A no-op
    /// while nothing is selected.
    #[tracing::instrument(name = "results_copy_row_as_json", skip_all)]
    pub(super) fn copy_row_as_json(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn copy_column_name(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn render_cell_context_menu(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let menu_state = self.cell_context_menu.as_ref()?;
        let menu = ContextMenu::new("results-cell-context-menu")
            .position(menu_state.position)
            .on_close(cx.listener(|view, _event, _window, cx| {
                view.close_cell_context_menu(cx);
            }))
            .add_item(ContextMenuItem::new("View value").on_click(cx.listener(
                |view, _event, _window, cx| {
                    view.view_value_from_menu(cx);
                },
            )))
            .add_item(ContextMenuItem::new("Copy value").on_click(cx.listener(
                |view, _event, window, cx| {
                    view.copy_focused_cell(&Copy, window, cx);
                    view.close_cell_context_menu(cx);
                },
            )))
            .add_item(ContextMenuItem::new("Copy row").on_click(cx.listener(
                |view, _event, _window, cx| {
                    view.copy_row(cx);
                    view.close_cell_context_menu(cx);
                },
            )))
            .add_item(
                ContextMenuItem::new("Copy row as JSON").on_click(cx.listener(
                    |view, _event, _window, cx| {
                        view.copy_row_as_json(cx);
                        view.close_cell_context_menu(cx);
                    },
                )),
            )
            .add_separator()
            .add_item(
                ContextMenuItem::new("Copy column name").on_click(cx.listener(
                    |view, _event, _window, cx| {
                        view.copy_column_name(cx);
                        view.close_cell_context_menu(cx);
                    },
                )),
            );

        Some(menu.into_any_element())
    }
}
