//! Presentational building blocks for a closable tab bar (e.g. the editor's
//! query tabs): the bar's own chrome, one tab's shell, its active-state
//! underline, and the trailing close/new-tab glyphs. Each function takes
//! only primitives, so the caller owns all tab data, click behavior, and any
//! domain-specific styling layered on top (e.g. a "generated" tint).

use gpui::{Div, div, prelude::*, px, rgb};

use crate::colors;

/// Height of the tab bar and every tab within it.
pub const TAB_BAR_HEIGHT: gpui::Pixels = px(36.0);
/// Horizontal gap between a tab's leading affordance(s), label, and close
/// glyph.
pub const TAB_GAP: f32 = 7.0;
/// Horizontal padding inside a tab.
pub const TAB_PADDING_X: f32 = 12.0;
/// Text size of a tab's label.
pub const TAB_TEXT_SIZE: f32 = 12.0;
/// Thickness of the active-tab underline, solid or dashed.
pub const TAB_UNDERLINE_THICKNESS: gpui::Pixels = px(2.0);
/// Text size of a tab's trailing close glyph.
pub const TAB_CLOSE_TEXT_SIZE: f32 = 11.0;
/// Width of the trailing "+" new-tab affordance.
pub const NEW_TAB_WIDTH: gpui::Pixels = px(30.0);
/// Text size of the trailing "+" new-tab glyph.
pub const NEW_TAB_TEXT_SIZE: f32 = 15.0;
/// Width of one segment of a dashed active-tab underline.
const DASH_SEGMENT_WIDTH: f32 = 5.0;
/// Gap between two segments of a dashed active-tab underline.
const DASH_SEGMENT_GAP: f32 = 4.0;
/// How many dash segments to lay out before the underline's `overflow_hidden`
/// clips the rest. Sized well past any realistic tab width so the dashing
/// never visibly runs out before the tab's right edge.
const DASH_SEGMENT_COUNT: usize = 48;

/// The tab bar's own chrome: a fixed-height row with a bottom hairline,
/// holding one child per tab plus a trailing new-tab affordance.
#[must_use]
pub fn tab_bar_shell() -> Div {
    div()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .h(TAB_BAR_HEIGHT)
        .bg(rgb(colors::PANEL))
        .border_b_1()
        .border_color(rgb(colors::LINE))
}

/// Shared chrome for one tab: height, padding, gap, a trailing hairline
/// separating it from the next tab, and the active/inactive text color. The
/// caller adds an active-state underline separately via
/// [`active_underline_solid`] or [`active_underline_dashed`], since which
/// (if either) applies depends on the tab's own kind.
#[must_use]
pub fn tab_shell(active: bool) -> Div {
    div()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .gap(px(TAB_GAP))
        .h_full()
        .px(px(TAB_PADDING_X))
        .border_r_1()
        .border_color(rgb(colors::LINE_SOFT))
        .text_size(px(TAB_TEXT_SIZE))
        .text_color(rgb(if active { colors::TEXT } else { colors::MUTED }))
        .when(active, |el| el.bg(rgb(colors::INK)))
}

/// A solid teal underline pinned to a tab's bottom edge, marking a script
/// tab active.
#[must_use]
pub fn active_underline_solid() -> Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(TAB_UNDERLINE_THICKNESS)
        .bg(rgb(colors::TEAL))
}

/// A dashed teal underline pinned to a tab's bottom edge, marking an active
/// generated tab in place of [`active_underline_solid`]. `gpui` has no
/// native repeating-pattern fill, so this approximates one with a row of
/// small teal segments spaced by [`DASH_SEGMENT_GAP`], clipped to the tab's
/// own width by `overflow_hidden`.
#[must_use]
pub fn active_underline_dashed() -> Div {
    let mut track = div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(TAB_UNDERLINE_THICKNESS)
        .flex()
        .flex_row()
        .gap(px(DASH_SEGMENT_GAP))
        .overflow_hidden();
    for _ in 0..DASH_SEGMENT_COUNT {
        track = track.child(
            div()
                .flex_shrink_0()
                .w(px(DASH_SEGMENT_WIDTH))
                .h_full()
                .bg(rgb(colors::TEAL)),
        );
    }
    track
}

/// A tab's trailing close ("x") affordance.
#[must_use]
pub fn close_glyph() -> Div {
    div()
        .flex_shrink_0()
        .text_size(px(TAB_CLOSE_TEXT_SIZE))
        .text_color(rgb(colors::FAINT))
        .child("x")
}

/// The trailing "+" affordance that opens a new script tab.
#[must_use]
pub fn new_tab_glyph() -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .flex_shrink_0()
        .w(NEW_TAB_WIDTH)
        .h_full()
        .text_size(px(NEW_TAB_TEXT_SIZE))
        .text_color(rgb(colors::FAINT))
        .child("+")
}

#[cfg(test)]
mod tests {
    use super::{
        active_underline_dashed, active_underline_solid, close_glyph, new_tab_glyph, tab_bar_shell,
        tab_shell,
    };

    #[test]
    fn tab_bar_and_tab_shells_build_for_either_active_state() {
        let _bar = tab_bar_shell();
        let _active = tab_shell(true);
        let _inactive = tab_shell(false);
    }

    #[test]
    fn active_underline_variants_build() {
        let _solid = active_underline_solid();
        let _dashed = active_underline_dashed();
    }

    #[test]
    fn close_and_new_tab_glyphs_build() {
        let _close = close_glyph();
        let _new_tab = new_tab_glyph();
    }
}
