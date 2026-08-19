//! Visual chrome for a [`super::Table`]: cell padding, row/header heights,
//! per-edge borders, and background/border colors.

use gpui::{Pixels, px};

use crate::grid;
use crate::theme::Theme;

/// Height of a table's header row.
pub const DEFAULT_HEADER_HEIGHT: Pixels = px(28.0);
/// Height of each of a table's body rows.
pub const DEFAULT_ROW_HEIGHT: Pixels = px(24.0);
/// Width of a [`super::Table::resizable_columns`] resize handle's hit
/// target, straddling a header cell's trailing border so a hover near the
/// border -- not only exactly on the hairline -- picks it up.
pub(super) const COLUMN_RESIZE_HANDLE_WIDTH: Pixels = px(6.0);

/// Which structural hairlines a table draws. Each edge is independent, so a
/// caller can drop e.g. column separators while keeping row separators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableBorders {
    /// The horizontal hairline under the header row and under every body
    /// row.
    pub row: bool,
    /// The vertical hairline to the right of every cell.
    pub column: bool,
    /// The hairline separating a pinned gutter pane from the data pane.
    pub outer: bool,
}

impl Default for TableBorders {
    fn default() -> Self {
        Self {
            row: true,
            column: true,
            outer: true,
        }
    }
}

/// A table's cell chrome: padding, row/header heights, which structural
/// hairlines are drawn, and the colors they and the header/gutter
/// backgrounds paint with.
#[derive(Debug, Clone, Copy)]
pub struct TableStyle {
    /// Horizontal padding inside every cell.
    pub cell_padding_x: Pixels,
    /// Horizontal padding inside the gutter cells
    pub gutter_cell_padding_x: Pixels,
    /// Height of the header row.
    pub header_height: Pixels,
    /// Height of each body row.
    pub row_height: Pixels,
    /// Which structural hairlines are drawn.
    pub borders: TableBorders,
    /// Background of the header row and a `RowNumbers`/`Custom` gutter's
    /// header cell.
    pub header_bg: u32,
    /// Background of a `RowNumbers`/`Custom` gutter's body cells.
    pub gutter_bg: u32,
    /// Color of the header row's bottom hairline.
    pub header_border: u32,
    /// Color of every other structural hairline: body row separators,
    /// column separators, and the gutter/data-pane divider.
    pub row_border: u32,
    /// Text color of a [`super::Gutter::RowNumbers`] pane's numbers.
    pub row_number_color: u32,
    /// Background of a data cell holding the table's currently selected
    /// cell.
    pub selection_wash: gpui::Rgba,
    /// Border color of a data cell holding the table's currently selected
    /// cell.
    pub selection_ring: gpui::Rgba,
}

impl Default for TableStyle {
    /// The default theme's chrome -- a caller that needs the live theme's
    /// colors instead should build via [`TableStyle::themed`].
    fn default() -> Self {
        Self::themed(&Theme::default())
    }
}

impl TableStyle {
    /// This table's chrome, painted with `theme`'s colors.
    #[must_use]
    pub fn themed(theme: &Theme) -> Self {
        Self {
            cell_padding_x: px(grid::CELL_PADDING_X),
            gutter_cell_padding_x: px(grid::CELL_PADDING_X),
            header_height: DEFAULT_HEADER_HEIGHT,
            row_height: DEFAULT_ROW_HEIGHT,
            borders: TableBorders::default(),
            header_bg: theme.colors.bg_raised,
            gutter_bg: theme.colors.bg_panel,
            header_border: theme.colors.border,
            row_border: theme.colors.border_soft,
            row_number_color: theme.colors.text_tertiary,
            selection_wash: theme.colors.accent_wash(),
            selection_ring: theme.colors.accent_ring(),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::px;

    use super::{DEFAULT_HEADER_HEIGHT, DEFAULT_ROW_HEIGHT, TableBorders, TableStyle};
    use crate::grid;
    use crate::theme::Theme;

    #[test]
    fn default_style_uses_the_shared_grid_dimensions_and_colors() {
        let theme = Theme::default();
        let style = TableStyle::default();
        assert_eq!(style.header_height, DEFAULT_HEADER_HEIGHT);
        assert_eq!(style.header_height, px(28.0));
        assert_eq!(style.row_height, DEFAULT_ROW_HEIGHT);
        assert_eq!(style.row_height, px(24.0));
        assert_eq!(style.cell_padding_x, px(grid::CELL_PADDING_X));
        assert_eq!(style.header_bg, theme.colors.bg_raised);
        assert_eq!(style.gutter_bg, theme.colors.bg_panel);
        assert_eq!(style.header_border, theme.colors.border);
        assert_eq!(style.row_border, theme.colors.border_soft);
        assert_eq!(style.row_number_color, theme.colors.text_tertiary);
        assert_eq!(style.selection_wash, theme.colors.accent_wash());
        assert_eq!(style.selection_ring, theme.colors.accent_ring());
        assert_eq!(style.borders, TableBorders::default());
    }

    #[test]
    fn default_borders_draw_every_edge() {
        let borders = TableBorders::default();
        assert!(borders.row);
        assert!(borders.column);
        assert!(borders.outer);
    }
}
