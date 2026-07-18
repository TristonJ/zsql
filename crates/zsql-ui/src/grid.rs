//! Presentational building blocks for a virtualized data grid: cell chrome,
//! a type-name badge, and a small status dot. Each function takes only
//! primitives (a pixel width, a color, a string) and returns an `Element`,
//! so the caller owns all row/column data.

use gpui::{Div, Pixels, div, prelude::*, px, rgb, rgba};

use crate::colors;

/// Horizontal padding inside every grid cell.
pub const CELL_PADDING_X: f32 = 11.0;
/// Text size of a column header's type-name badge.
pub const TYPE_TAG_TEXT_SIZE: f32 = 9.5;
/// Horizontal padding inside a type-name badge.
pub const TYPE_TAG_PADDING_X: f32 = 4.0;
/// Type-tag badge border: teal at low opacity (`0x33c2ac` at ~28% alpha).
pub const TYPE_TAG_BORDER: u32 = 0x33_c2_ac_47;
/// Corner radius of a type-name badge.
pub const TYPE_TAG_RADIUS: f32 = 4.0;
/// Diameter of a status dot.
pub const STATUS_DOT_SIZE: f32 = 6.0;

/// Shared chrome for a header cell: fixed width, vertical centering, a
/// trailing hairline border, and truncation for overflowing content.
#[must_use]
pub fn header_cell_shell(width: Pixels) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .w(width)
        .h_full()
        .px(px(CELL_PADDING_X))
        .truncate()
        .border_r_1()
        .border_color(rgb(colors::LINE_SOFT))
}

/// Shared chrome for a body cell. Kept as a distinct function from
/// [`header_cell_shell`] even though they currently render identically, so
/// header and body chrome can diverge later without one call site
/// accidentally affecting the other.
#[must_use]
pub fn body_cell_shell(width: Pixels) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .w(width)
        .h_full()
        .px(px(CELL_PADDING_X))
        .truncate()
        .border_r_1()
        .border_color(rgb(colors::LINE_SOFT))
}

/// A small type-name badge, e.g. shown next to a column's name in a header.
#[must_use]
pub fn type_tag(type_name: &str) -> Div {
    div()
        .text_size(px(TYPE_TAG_TEXT_SIZE))
        .text_color(rgb(colors::TEAL))
        .px(px(TYPE_TAG_PADDING_X))
        .border_1()
        .border_color(rgba(TYPE_TAG_BORDER))
        .rounded(px(TYPE_TAG_RADIUS))
        .child(type_name.to_owned())
}

/// A small round indicator dot filled with `color`.
#[must_use]
pub fn status_dot(color: u32) -> Div {
    div()
        .flex_shrink_0()
        .w(px(STATUS_DOT_SIZE))
        .h(px(STATUS_DOT_SIZE))
        .rounded(px(STATUS_DOT_SIZE / 2.0))
        .bg(rgb(color))
}

#[cfg(test)]
mod tests {
    use gpui::px;

    use super::{body_cell_shell, colors, header_cell_shell, status_dot, type_tag};

    #[test]
    fn header_and_body_cell_shells_build_for_a_width() {
        let _header = header_cell_shell(px(80.0));
        let _body = body_cell_shell(px(80.0));
    }

    #[test]
    fn type_tag_builds_for_a_type_name() {
        let _tag = type_tag("int8");
    }

    #[test]
    fn status_dot_builds_for_a_color() {
        let _dot = status_dot(colors::TEAL);
    }
}
