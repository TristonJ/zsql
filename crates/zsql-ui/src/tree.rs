//! Presentational building blocks for a virtualized tree view (e.g. a
//! schema sidebar): row chrome, disclosure glyphs, and label/meta text.
//! Each function takes only primitives, so the caller owns all row data and
//! click behavior.

use gpui::{Div, SharedString, div, prelude::*, px, rgb};

use crate::colors;

/// Height of each row in a tree view.
pub const ROW_HEIGHT: gpui::Pixels = px(26.0);
/// Horizontal gap between a tree row's disclosure glyph, label, and
/// trailing affordances.
pub const ROW_GAP: f32 = 7.0;
/// Text size of a tree row's label and disclosure glyph.
pub const ROW_TEXT_SIZE: f32 = 12.5;
/// Width reserved for a row's disclosure glyph (`v`/`>`) or, for a
/// non-disclosable row, the equivalent blank space that keeps its label
/// aligned with its parent's children.
pub const DISCLOSURE_WIDTH: f32 = 10.0;
/// Text size of a row's trailing count affordance.
pub const META_TEXT_SIZE: f32 = 10.0;
/// Text size of a row's kind label.
pub const KIND_TEXT_SIZE: f32 = 9.0;

/// Shared chrome for a tree row: height, indent, gap, monospace text.
#[must_use]
pub fn row_shell(indent: f32) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(ROW_GAP))
        .h(ROW_HEIGHT)
        .pl(px(indent))
        .pr_3()
        .flex_shrink_0()
        .w_full()
        .font_family("monospace")
        .text_size(px(ROW_TEXT_SIZE))
        .text_color(rgb(colors::TEXT))
}

/// The ASCII disclosure glyph: `v` expanded, `>` collapsed.
#[must_use]
pub fn disclosure_glyph(expanded: bool) -> Div {
    div()
        .flex_shrink_0()
        .w(px(DISCLOSURE_WIDTH))
        .text_color(rgb(colors::FAINT))
        .child(if expanded { "v" } else { ">" })
}

/// Blank space the width of a disclosure glyph, for a row that cannot be
/// disclosed but still needs its label aligned with its siblings.
#[must_use]
pub fn disclosure_spacer() -> Div {
    div().flex_shrink_0().w(px(DISCLOSURE_WIDTH))
}

/// A row's primary label.
#[must_use]
pub fn row_label(text: impl Into<SharedString>) -> Div {
    div().flex_1().min_w_0().truncate().child(text.into())
}

/// A row's trailing affordance (e.g. a relation/column count).
#[must_use]
pub fn row_meta(text: impl Into<SharedString>) -> Div {
    div()
        .flex_shrink_0()
        .ml_auto()
        .pl_2()
        .text_size(px(META_TEXT_SIZE))
        .text_color(rgb(colors::FAINT))
        .font_family("monospace")
        .child(text.into())
}

/// A row's kind label (e.g. table/view/matview/partitioned).
#[must_use]
pub fn row_kind(text: impl Into<SharedString>) -> Div {
    div()
        .flex_shrink_0()
        .ml_auto()
        .pl_2()
        .text_size(px(KIND_TEXT_SIZE))
        .text_color(rgb(colors::FAINT))
        .font_family("monospace")
        .child(text.into())
}

/// A row's trailing count, following [`row_kind`] in normal flow.
#[must_use]
pub fn row_count(text: impl Into<SharedString>) -> Div {
    div()
        .flex_shrink_0()
        .pl_2()
        .text_size(px(META_TEXT_SIZE))
        .text_color(rgb(colors::FAINT))
        .font_family("monospace")
        .child(text.into())
}

#[cfg(test)]
mod tests {
    use super::{
        disclosure_glyph, disclosure_spacer, row_count, row_kind, row_label, row_meta, row_shell,
    };

    #[test]
    fn row_shell_and_disclosure_helpers_build_for_any_indent_or_state() {
        let _shell = row_shell(24.0);
        let _expanded = disclosure_glyph(true);
        let _collapsed = disclosure_glyph(false);
        let _spacer = disclosure_spacer();
    }

    #[test]
    fn label_meta_kind_and_count_helpers_build_for_text() {
        let _label = row_label("orders");
        let _meta = row_meta("4 cols");
        let _kind = row_kind("table");
        let _count = row_count("4 cols");
    }
}
