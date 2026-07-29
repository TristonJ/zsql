//! The status bar's theme control: a 3-chip swatch of the active theme's
//! own colors, its display name, and a caret. Clicking it opens this
//! workspace's [`AppearanceModalView`] instance, wired in once by
//! [`ResultsView::set_appearance_modal`] rather than constructed fresh on
//! each click.

use gpui::{Context, Div, Entity, SharedString, Stateful, div, prelude::*, px, rgb};
use zsql_ui::theme::Colors;

use crate::ui::appearance::AppearanceModalView;
use crate::ui::theme;

use super::ResultsView;

/// Element id for [`render_theme_trigger`], so tests can locate its painted
/// bounds.
pub(super) const THEME_TRIGGER_ID: &str = "results-theme-trigger";

/// The three swatch chip colors the status-bar theme trigger paints, in the
/// trigger's own left-to-right order: accent, numeric value, boolean value.
#[must_use]
pub(super) fn swatch_colors(colors: &Colors) -> [u32; 3] {
    [colors.accent, colors.value_number, colors.value_bool]
}

/// The status-bar theme control. `active_colors` is the currently-applied
/// global palette (so the swatch always matches what the rest of the window
/// is painted with); `active_display_name` names it. Clicking opens (and
/// focuses) `appearance_modal` -- a no-op click while it is `None`, e.g. a
/// [`ResultsView`] built without [`ResultsView::set_appearance_modal`].
pub(super) fn render_theme_trigger(
    appearance_modal: Option<Entity<AppearanceModalView>>,
    active_colors: Colors,
    active_display_name: SharedString,
    cx: &Context<ResultsView>,
) -> Stateful<Div> {
    let chips = swatch_colors(&active_colors);

    div()
        .id(THEME_TRIGGER_ID)
        .debug_selector(|| THEME_TRIGGER_ID.to_owned())
        .flex_shrink_0()
        .flex()
        .flex_row()
        .items_center()
        .gap(theme::APPEARANCE_TRIGGER_GAP)
        .px(theme::APPEARANCE_TRIGGER_PADDING_X)
        .py(theme::APPEARANCE_TRIGGER_PADDING_Y)
        .rounded(px(theme::APPEARANCE_TRIGGER_RADIUS))
        .cursor_pointer()
        .hover(|el| el.text_color(rgb(active_colors.accent)))
        .child(render_swatch(chips))
        .child(
            div()
                .text_size(px(theme::APPEARANCE_TRIGGER_TEXT_SIZE))
                .text_color(rgb(active_colors.text_primary))
                .child(active_display_name),
        )
        .child(
            div()
                .text_size(px(theme::APPEARANCE_TRIGGER_TEXT_SIZE))
                .text_color(rgb(active_colors.text_secondary))
                .child("\u{25be}"),
        )
        .on_click(cx.listener(move |_view, _event, window, cx| {
            let Some(modal) = appearance_modal.as_ref() else {
                return;
            };
            modal.update(cx, AppearanceModalView::open);
            // Focus the checked card itself, not the modal overlay: arrow
            // keys are handled by a listener on the card grid, a descendant
            // of the overlay, so key events only reach it once a card
            // actually holds focus. The overlay stays on the dispatch path
            // either way (it is an ancestor of every card), so Escape still
            // closes the modal from here.
            let focus_handle = modal.read(cx).focused_card_handle();
            window.focus(&focus_handle);
        }))
}

/// The trigger's 3-chip swatch: one small square per color in `chips`.
fn render_swatch(chips: [u32; 3]) -> Div {
    let mut row = div()
        .flex()
        .flex_row()
        .gap(theme::APPEARANCE_TRIGGER_SWATCH_GAP);
    for color in chips {
        row = row.child(
            div()
                .flex_shrink_0()
                .w(theme::APPEARANCE_TRIGGER_SWATCH_SIZE)
                .h(theme::APPEARANCE_TRIGGER_SWATCH_SIZE)
                .rounded(px(theme::APPEARANCE_TRIGGER_SWATCH_RADIUS))
                .bg(rgb(color)),
        );
    }
    row
}

#[cfg(test)]
mod tests {
    use zsql_ui::theme::Colors;

    use super::swatch_colors;

    #[test]
    fn swatch_colors_reads_accent_value_number_and_value_bool_in_order() {
        let colors = Colors {
            accent: 0x11_11_11,
            value_number: 0x22_22_22,
            value_bool: 0x33_33_33,
            ..Colors::default()
        };
        assert_eq!(swatch_colors(&colors), [0x11_11_11, 0x22_22_22, 0x33_33_33]);
    }

    #[test]
    fn swatch_colors_matches_the_default_palettes_own_role_values() {
        let colors = Colors::default();
        assert_eq!(
            swatch_colors(&colors),
            [colors.accent, colors.value_number, colors.value_bool]
        );
    }
}
