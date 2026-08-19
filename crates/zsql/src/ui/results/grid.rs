//! The virtualized data grid itself: column headers (delegating to
//! [`super::pager::sortable_column_header`] for sort/funnel affordances),
//! resizable columns, and the data-cell rows, built by composing
//! `zsql_ui::table::Table`. Split out of [`super::ResultsView`]'s own module
//! since column-width measurement and cell rendering are a self-contained
//! concern independent of the results bar, filter bar, or status bar beside
//! the grid.

use std::ops::Range;

use gpui::{
    AnyElement, Context, Div, Pixels, StrikethroughStyle, Window, div, prelude::*, px, rgb,
};
use zsql_core::ColumnMeta;
use zsql_ui::grid::CELL_PADDING_X;
use zsql_ui::table::{Gutter, Table, TableColumn, TableRow, measure};
use zsql_ui::theme::{ActiveTheme, Theme};

use crate::ui::theme::HEADER_EXTRA_PADDING_CHARS;

use super::pager;
use super::quick_find::QuickFindHighlight;
use super::{ResultsView, ValueKind, format_value, theme};

impl ResultsView {
    /// The two-pane virtualized grid (pinned row numbers + horizontally
    /// scrolling data columns), built by composing `zsql_ui::table::Table`.
    pub(super) fn render_grid(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
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
            .gutter(Self::row_number_gutter(row_count, &active_theme))
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

    /// The row-number gutter. A `Gutter::Custom` (rather than the built-in
    /// `RowNumbers`) since only this view knows which rows are staged.
    fn row_number_gutter(row_count: usize, active_theme: &Theme) -> Gutter<Self> {
        let style = Self::table_style(active_theme);
        let width = measure::row_number_column_width(
            row_count,
            &style,
            theme::CELL_CHAR_WIDTH,
            theme::ROW_NUMBER_MIN_WIDTH,
        );
        let header = div()
            .flex_1()
            .flex()
            .justify_end()
            .text_color(rgb(style.row_number_color))
            .child("#")
            .into_any_element();
        Gutter::Custom {
            width,
            header,
            render: Box::new(Self::render_row_number_cells),
        }
    }

    /// Batch renderer for [`ResultsView::row_number_gutter`]'s body cells.
    fn render_row_number_cells(
        &mut self,
        range: std::ops::Range<usize>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let active_theme = cx.theme().clone();
        let colors = active_theme.colors;
        range
            .map(|ix| {
                let staged = self.staged_id_for_row(cx, ix).is_some();
                let mut cell = div().flex_1().flex().justify_end().px(px(CELL_PADDING_X));
                cell = if staged {
                    cell.bg(theme::staged_delete_wash(&active_theme))
                        .text_color(rgb(colors.status_error))
                        .child(format!("- {}", ix + 1))
                } else {
                    cell.text_color(rgb(colors.text_tertiary))
                        .child((ix + 1).to_string())
                };
                cell.into_any_element()
            })
            .collect()
    }

    /// [`zsql_ui::table::Table::resizable_columns`]'s live-resize callback:
    /// stores `column`'s new `width` and marks it so a later
    /// [`ResultsView::sync_dimensions`] call leaves it alone, mirroring
    /// [`ResultsView::value_panel_drag_move`]'s per-move update. Never
    /// touches the grid's focused cell or keyboard focus.
    pub(super) fn resize_column(
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
    /// [`column_width_from_parts`]'s width estimate use, so a column's
    /// measured width can never drift from the padding it is actually
    /// rendered with.
    pub(super) fn table_style(active_theme: &Theme) -> zsql_ui::table::TableStyle {
        zsql_ui::table::TableStyle {
            gutter_cell_padding_x: px(0.0), // we use internal padding to make sure bg colors work
            ..zsql_ui::table::TableStyle::themed(active_theme)
        }
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
                let staged = self.staged_id_for_row(cx, ix).is_some();
                let cells = rows
                    .get(ix)
                    .map(|row| {
                        row.0
                            .iter()
                            .enumerate()
                            .map(|(col, value)| {
                                let formatted = format_value(value);
                                let is_null = formatted.kind == ValueKind::Null;
                                let highlight = self.quick_find_highlight(ix, col);
                                let mut cell = div()
                                    .flex()
                                    .flex_col()
                                    .justify_start()
                                    .items_start()
                                    .h_full()
                                    .overflow_y_hidden()
                                    .text_color(rgb(formatted.kind.color(active_theme)))
                                    .when(is_null, gpui::prelude::Styled::italic);
                                if staged {
                                    cell = cell.text_color(rgb(active_theme.colors.text_tertiary));
                                    // Set directly: line_through() leaves the
                                    // strike at the text color, and gpui's
                                    // text_decoration_color styles only
                                    // underlines.
                                    cell.text_style()
                                        .get_or_insert_with(Default::default)
                                        .strikethrough = Some(StrikethroughStyle {
                                        thickness: px(1.0),
                                        color: Some(rgb(active_theme.colors.status_error).into()),
                                    });
                                } else {
                                    cell = match highlight {
                                        QuickFindHighlight::Current => cell
                                            .rounded(px(theme::QUICK_FIND_MATCH_RADIUS))
                                            .bg(theme::quick_find_current_match_bg(active_theme))
                                            .text_color(rgb(active_theme.colors.accent_contrast)),
                                        QuickFindHighlight::Match => cell
                                            .rounded(px(theme::QUICK_FIND_MATCH_RADIUS))
                                            .bg(theme::quick_find_match_bg(active_theme)),
                                        QuickFindHighlight::None => cell,
                                    };
                                }
                                cell.child(formatted.text).into_any_element()
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let mut table_row = TableRow::new(cells);
                if staged {
                    table_row = table_row.background(theme::staged_delete_wash(active_theme));
                }
                table_row
            })
            .collect()
    }
}

/// Estimate a column's pixel width from its header (name + type tag) and
/// `max_body_chars`, using `style`'s cell padding -- the same `TableStyle`
/// the live grid renders with, so the estimate and the render never drift.
pub(super) fn column_width_from_parts(
    column: &ColumnMeta,
    max_body_chars: usize,
    style: &zsql_ui::table::TableStyle,
) -> Pixels {
    let header_chars =
        column.name.chars().count() + column.type_name.chars().count() + HEADER_EXTRA_PADDING_CHARS;

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
