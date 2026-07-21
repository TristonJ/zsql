//! The table's pinned left pane: row numbers by default, or a
//! caller-supplied renderer.

use std::ops::Range;

use gpui::{AnyElement, Context, Div, Pixels, Render, Window, div, prelude::*, rgb};

use super::measure;
use super::style::TableStyle;

/// A `Gutter::Custom` pane's batch cell renderer: same shape as
/// [`super::Table::rows`], keeping `&mut V` access for cell rendering.
type GutterRenderer<V> =
    Box<dyn Fn(&mut V, Range<usize>, &mut Window, &mut Context<V>) -> Vec<AnyElement>>;

/// Sizing inputs for a [`Gutter::RowNumbers`] pane: how wide one digit
/// renders at the table's text size, and the narrowest the pane is ever
/// allowed to shrink to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowNumberStyle {
    pub char_width: f32,
    pub min_width: f32,
}

impl Default for RowNumberStyle {
    fn default() -> Self {
        Self {
            char_width: measure::DEFAULT_ROW_NUMBER_CHAR_WIDTH,
            min_width: measure::DEFAULT_ROW_NUMBER_MIN_WIDTH,
        }
    }
}

/// The table's pinned left pane. Absent entirely (`None`), the default
/// right-aligned row-number column (`RowNumbers`), or a caller-supplied
/// width and batch renderer (`Custom`).
pub enum Gutter<V: Render> {
    /// No pinned pane at all: the data pane fills the table's full width.
    None,
    /// A right-aligned row-number column, one-indexed, sized by the given
    /// [`RowNumberStyle`].
    RowNumbers(RowNumberStyle),
    /// A caller-defined pinned pane: its own width, header content, and a
    /// batch cell renderer wired through `cx.processor`. `render` must
    /// return exactly one element per index in the range it is given: a
    /// debug build panics if the returned count does not match the range's
    /// length, since a short or long batch would put the pinned pane's rows
    /// out of alignment with the data pane's.
    Custom {
        width: Pixels,
        header: AnyElement,
        render: GutterRenderer<V>,
    },
}

/// The gutter's header cell chrome shared by every gutter kind: sizing,
/// background, and the header row's bottom hairline. Carries no forced
/// text alignment or color, so a [`Gutter::Custom`] header renders exactly
/// the content its caller builds.
pub(super) fn gutter_header_shell(style: &TableStyle) -> Div {
    let mut cell = div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .h(style.header_height)
        .px(style.cell_padding_x)
        .bg(rgb(style.header_bg));
    if style.borders.row {
        cell = cell.border_b_1().border_color(rgb(style.header_border));
    }
    cell
}

/// One gutter body cell's chrome shared by every gutter kind: sizing,
/// background, and the row's bottom hairline. Carries no forced text
/// alignment or color, so a [`Gutter::Custom`] cell renders exactly the
/// content its caller builds.
pub(super) fn gutter_cell_shell(width: Pixels, style: &TableStyle) -> Div {
    let mut cell = div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .w(width)
        .h(style.row_height)
        .px(style.cell_padding_x)
        .bg(rgb(style.gutter_bg));
    if style.borders.row {
        cell = cell.border_b_1().border_color(rgb(style.row_border));
    }
    cell
}

/// [`gutter_header_shell`] plus [`Gutter::RowNumbers`]' own right-alignment
/// and faint text color.
pub(super) fn row_number_header_shell(style: &TableStyle) -> Div {
    gutter_header_shell(style)
        .justify_end()
        .text_color(rgb(style.row_number_color))
}

/// [`gutter_cell_shell`] plus [`Gutter::RowNumbers`]' own right-alignment
/// and faint text color.
pub(super) fn row_number_cell_shell(width: Pixels, style: &TableStyle) -> Div {
    gutter_cell_shell(width, style)
        .justify_end()
        .text_color(rgb(style.row_number_color))
}
