use gpui::{Div, ElementId, Stateful, Window, prelude::*, px, rgb};

use crate::{
    button::button_base,
    icon::{IconName, icon},
    theme::ActiveTheme,
};

pub fn icon_button_secondary<T>(
    id: impl Into<ElementId>,
    window: &mut Window,
    cx: &mut Context<T>,
    icon_name: IconName,
) -> Stateful<Div> {
    let (btn, hover) = button_base(id, window, cx);
    let theme = cx.theme();
    btn.px(px(4.0)).py(px(4.0)).child(
        icon(icon_name, px(13.0), theme.colors.text_tertiary).when(*hover.read(cx), |style| {
            style.text_color(rgb(theme.colors.text_primary))
        }),
    )
}
