//! Presentational building blocks for a virtualized tree view (e.g. a
//! schema sidebar): row chrome, disclosure glyphs, and label/meta text.
//! Each function takes only primitives, so the caller owns all row data and
//! click behavior.

use gpui::{Div, SharedString, div, prelude::*, px, rgb};

use crate::icon::{IconName, icon};
use crate::theme::Theme;

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

/// Shared chrome for a tree row: height, indent, gap, monospace text.
#[must_use]
pub fn row_shell(indent: f32, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(ROW_GAP))
        .h(ROW_HEIGHT)
        .pl(px(indent))
        .pr_6()
        .flex_shrink_0()
        .w_full()
        .font_family(&theme.fonts.data)
        .text_size(px(ROW_TEXT_SIZE))
        .text_color(rgb(theme.colors.text_primary))
}

/// The icon a disclosure row shows for `expanded`: chevron-down when
/// expanded, chevron-right when collapsed.
#[must_use]
fn disclosure_icon_name(expanded: bool) -> IconName {
    if expanded {
        IconName::ChevronDown
    } else {
        IconName::ChevronRight
    }
}

/// The tree disclosure glyph: a chevron-down icon expanded, chevron-right
/// collapsed, tinted with the theme's tertiary text color and sized to fill
/// its [`DISCLOSURE_WIDTH`] slot.
#[must_use]
pub fn disclosure_glyph(expanded: bool, theme: &Theme) -> Div {
    div().flex_shrink_0().w(px(DISCLOSURE_WIDTH)).child(icon(
        disclosure_icon_name(expanded),
        px(DISCLOSURE_WIDTH),
        theme.colors.text_tertiary,
    ))
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
pub fn row_meta(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .flex_shrink_0()
        .ml_auto()
        .pl_2()
        .text_size(px(META_TEXT_SIZE))
        .text_color(rgb(theme.colors.text_tertiary))
        .font_family(&theme.fonts.data)
        .child(text.into())
}

/// A row's trailing count, following the label in normal flow.
#[must_use]
pub fn row_count(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .flex_shrink_0()
        .pl_2()
        .text_size(px(META_TEXT_SIZE))
        .text_color(rgb(theme.colors.text_tertiary))
        .font_family(&theme.fonts.data)
        .child(text.into())
}

#[cfg(test)]
mod tests {
    use super::{
        Theme, disclosure_glyph, disclosure_icon_name, disclosure_spacer, row_count, row_label,
        row_meta, row_shell,
    };
    use crate::icon::IconName;

    #[test]
    fn row_shell_and_disclosure_helpers_build_for_any_indent_or_state() {
        let theme = Theme::default();
        let _shell = row_shell(24.0, &theme);
        let _expanded = disclosure_glyph(true, &theme);
        let _collapsed = disclosure_glyph(false, &theme);
        let _spacer = disclosure_spacer();
    }

    #[test]
    fn disclosure_icon_name_maps_both_states_to_distinct_chevrons() {
        assert_eq!(disclosure_icon_name(true), IconName::ChevronDown);
        assert_eq!(disclosure_icon_name(false), IconName::ChevronRight);
    }

    #[test]
    fn label_meta_and_count_helpers_build_for_text() {
        let theme = Theme::default();
        let _label = row_label("orders");
        let _meta = row_meta("4 cols", &theme);
        let _count = row_count("4 cols", &theme);
    }
}
