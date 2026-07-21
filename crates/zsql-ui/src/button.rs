use gpui::{
    Div, ElementId, Entity, FontWeight, SharedString, Window, div, prelude::*, px, rems, rgb, rgba,
};

use crate::theme::ActiveTheme;

/// A common base for all buttons, which handles hover state and basic styling.
/// Hover state is manually implemented because GPUI does not support (currently)
/// updating text styles on hover.
fn button_base<T>(
    id: impl Into<ElementId>,
    window: &mut Window,
    cx: &mut Context<T>,
) -> (gpui::Stateful<Div>, Entity<bool>) {
    let id = id.into();
    let hover_id = SharedString::new(format!("{id}-hovered"));
    let hovered = window.use_keyed_state(hover_id, cx, |_w, _c| false);

    let div = div()
        .id(id)
        .cursor_pointer()
        .px(rems(1.0))
        .py(rems(0.15))
        .rounded(px(7.0))
        .border_1()
        .font_weight(FontWeight::SEMIBOLD)
        .on_hover({
            let hovered = hovered.clone();
            move |now, _w, cx| {
                hovered.update(cx, |h, cx| {
                    *h = *now;
                    cx.notify();
                });
            }
        });
    (div, hovered)
}

/// A primary button is the most important button on a page
pub fn primary_button<T>(
    id: impl Into<ElementId>,
    window: &mut Window,
    cx: &mut Context<T>,
) -> gpui::Stateful<Div> {
    let (btn, hovered) = button_base(id, window, cx);
    let theme = cx.theme();

    btn.border_color(rgb(theme.colors.accent_dim()))
        .text_color(rgb(theme.colors.accent_strong()))
        .when(*hovered.read(cx), |b| {
            b.bg(rgba(theme.colors.accent_wash_hover()))
        })
}

/// A secondary button is less important than a primary button, but still important
pub fn secondary_button<T>(
    id: impl Into<ElementId>,
    window: &mut Window,
    cx: &mut Context<T>,
) -> gpui::Stateful<Div> {
    let (btn, hovered) = button_base(id, window, cx);
    let theme = cx.theme();

    btn.border_color(rgb(theme.colors.border))
        .text_color(rgb(theme.colors.text_secondary))
        .when(*hovered.read(cx), |b| {
            b.text_color(rgb(theme.colors.text_primary))
                .border_color(rgb(theme.colors.text_primary))
        })
}

/// A destructive button is a button that performs a destructive action
pub fn destructive_button<T>(
    id: impl Into<ElementId>,
    window: &mut Window,
    cx: &mut Context<T>,
) -> gpui::Stateful<Div> {
    let (btn, hovered) = button_base(id, window, cx);
    let theme = cx.theme();

    btn.border_color(rgb(theme.colors.error_outline()))
        .text_color(rgb(theme.colors.status_error))
        .when(*hovered.read(cx), |b| b.bg(rgba(theme.colors.error_wash())))
}
