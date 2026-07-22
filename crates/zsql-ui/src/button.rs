use gpui::{
    App, ClickEvent, Div, ElementId, Entity, FontWeight, SharedString, Window, div, prelude::*, px,
    rems, rgb, rgba,
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

/// A button switch - allowing toggling between multiple options. Only one option can
/// be selected at a time.
#[derive(IntoElement)]
pub struct ButtonSwitch {
    selected: Option<ElementId>,
    options: Vec<(ElementId, gpui::Stateful<Div>, ButtonClickHandler)>,
    is_disabled: bool,
}

type ButtonClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

impl Default for ButtonSwitch {
    fn default() -> Self {
        Self::new()
    }
}

impl ButtonSwitch {
    #[must_use]
    pub fn new() -> Self {
        Self {
            selected: None,
            options: Vec::new(),
            is_disabled: false,
        }
    }

    #[must_use]
    pub fn selected(mut self, selected: impl Into<ElementId>) -> Self {
        self.selected = Some(selected.into());
        self
    }

    #[must_use]
    pub fn disabled(mut self, is_disabled: bool) -> Self {
        self.is_disabled = is_disabled;
        self
    }

    #[must_use]
    pub fn add_option<T>(
        mut self,
        window: &mut Window,
        cx: &mut Context<T>,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        let id = id.into();
        let label = label.into();
        // Tag each segment with its id as a debug selector so render tests can
        // look up its painted bounds via `VisualTestContext::debug_bounds`
        // (a no-op in release builds).
        let selector = id.to_string();
        let (btn, hovered) = button_base(id.clone(), window, cx);
        let btn = btn
            .font_family(cx.theme().fonts.data.clone())
            .font_weight(FontWeight::NORMAL)
            .text_size(rems(0.625))
            .py(rems(0.1))
            .px(rems(0.75))
            .child(label.to_ascii_uppercase())
            .text_color(rgb(cx.theme().colors.text_tertiary))
            .border_color(rgb(cx.theme().colors.border))
            .when(*hovered.read(cx), |b| {
                b.text_color(rgb(cx.theme().colors.text_primary))
            })
            .debug_selector(move || selector);
        self.options.push((id, btn, Box::new(on_click)));
        self
    }
}

impl RenderOnce for ButtonSwitch {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let mut parent = div()
            .flex()
            .flex_row()
            .rounded(px(7.0))
            .border_color(rgb(theme.colors.border));
        let num_options = self.options.len();
        for (i, (id, option, on_click)) in self.options.into_iter().enumerate() {
            let is_first = i == 0;
            let is_last = i == num_options - 1;
            let is_selected = self.selected.as_ref() == Some(&id);
            let option = match (is_selected, self.is_disabled) {
                (true, disabled) => option
                    .bg(rgba(theme.colors.accent_wash()))
                    .border_color(rgba(theme.colors.accent_wash()))
                    .text_color(rgb(theme.colors.accent))
                    .when(disabled, gpui::Styled::cursor_not_allowed),
                (false, false) => option,
                (false, true) => option
                    .opacity(0.4)
                    .text_color(rgb(cx.theme().colors.text_tertiary))
                    .cursor_not_allowed(),
            };
            let option = match (is_first, is_last) {
                (false, false) => option.border_l_0().rounded_l(px(0.0)).rounded_r(px(0.0)),
                (true, false) => option.border_r_0().rounded_l(px(7.0)).rounded_r(px(0.0)),
                (false, true) => option.border_l_0().rounded_l(px(0.0)).rounded_r(px(7.0)),
                (true, true) => option.rounded(px(7.0)),
            };
            let option = option.when(!self.is_disabled, |b| b.on_click(on_click));
            parent = parent.child(option);
        }

        parent
    }
}
