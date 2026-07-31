use std::rc::Rc;

use gpui::{
    Div, ElementId, FocusHandle, IntoElement, Pixels, RenderOnce, Stateful, div, prelude::*, px,
    rgb, rgba,
};

use crate::{
    icon::{IconName, icon},
    theme::ActiveTheme,
};

pub const MODAL_CLOSE_ICON_SIZE: Pixels = px(13.0);
const MODAL_CLOSE_HOVER_GROUP: &str = "modal-close-hover";

type ModalCloseHandler = Rc<dyn Fn(&(), &mut gpui::Window, &mut gpui::App)>;

#[derive(IntoElement)]
pub struct Modal<H: IntoElement + 'static, B: IntoElement + 'static> {
    id: ElementId,
    size: ModalSize,
    head: H,
    body: B,
    has_close_icon: bool,
    on_close: ModalCloseHandler,
    focus_handle: Option<FocusHandle>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalSize {
    Small,
    /// A modal wide enough for two side-by-side content columns instead of
    /// one -- e.g. the connection form once its SSH tunnel section opens a
    /// second column beside the base connection fields.
    Wide,
    /// A wider panel for content that needs room to breathe: a card grid or
    /// a multi-column form, with a taller header that fits a subtitle line
    /// under the title.
    Large,
}

impl ModalSize {
    #[must_use]
    pub fn width(&self) -> Pixels {
        match self {
            ModalSize::Small => px(468.0),
            ModalSize::Wide => px(760.0),
            ModalSize::Large => px(880.0),
        }
    }

    #[must_use]
    pub fn radius(&self) -> Pixels {
        match self {
            ModalSize::Small | ModalSize::Wide => px(10.0),
            ModalSize::Large => px(12.0),
        }
    }

    #[must_use]
    pub fn head_height(&self) -> Pixels {
        match self {
            ModalSize::Small | ModalSize::Wide => px(44.0),
            ModalSize::Large => px(66.0),
        }
    }
}

impl<H: IntoElement + 'static, B: IntoElement + 'static> Modal<H, B> {
    pub fn new(id: impl Into<ElementId>) -> Modal<Div, Div> {
        let id = id.into();
        Modal {
            id: id.clone(),
            size: ModalSize::Small,
            head: div(),
            body: div(),
            has_close_icon: true,
            on_close: Rc::new(move |(), _, _cx| {}),
            focus_handle: None,
        }
    }

    #[must_use]
    pub fn size(mut self, size: ModalSize) -> Self {
        self.size = size;
        self
    }

    /// Track `focus_handle` on the modal's key-dispatch container so its
    /// keyboard handlers fire while it is focused. The caller is responsible
    /// for focusing this same handle when the modal opens
    #[must_use]
    pub fn track_focus(mut self, focus_handle: &FocusHandle) -> Self {
        self.focus_handle = Some(focus_handle.clone());
        self
    }

    #[must_use]
    pub fn on_close(
        mut self,
        on_close: impl Fn(&(), &mut gpui::Window, &mut gpui::App) + 'static,
    ) -> Self {
        self.on_close = Rc::new(on_close);
        self
    }

    #[must_use]
    pub fn has_close_icon(mut self, has_close_icon: bool) -> Self {
        self.has_close_icon = has_close_icon;
        self
    }

    pub fn body<C: IntoElement>(self, child: C) -> Modal<H, C> {
        Modal {
            id: self.id,
            size: self.size,
            head: self.head,
            body: child,
            has_close_icon: self.has_close_icon,
            on_close: self.on_close,
            focus_handle: self.focus_handle,
        }
    }

    pub fn head<C: IntoElement>(self, child: C) -> Modal<C, B> {
        Modal {
            id: self.id,
            size: self.size,
            head: child,
            body: self.body,
            has_close_icon: self.has_close_icon,
            on_close: self.on_close,
            focus_handle: self.focus_handle,
        }
    }
}

impl<H: IntoElement, B: IntoElement> Modal<H, B> {
    fn render_head_container(&self, cx: &mut gpui::App) -> Div {
        let colors = cx.theme().colors;
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .h(self.size.head_height())
            .px_3()
            .border_b_1()
            .border_color(rgb(colors.border_soft))
            .font_family(cx.theme().fonts.ui.clone())
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(colors.text_primary))
    }

    fn render_close_icon(&self, cx: &mut gpui::App) -> Stateful<Div> {
        let colors = cx.theme().colors;
        let close_listener = self.on_close.clone();
        let id = self.id.clone();
        div()
            .id((self.id.clone(), "close-icon"))
            .debug_selector(move || format!("{id}-close-icon"))
            .group(MODAL_CLOSE_HOVER_GROUP)
            .ml_auto()
            .cursor_pointer()
            .child(
                icon(IconName::Close, MODAL_CLOSE_ICON_SIZE, colors.text_tertiary)
                    .group_hover(MODAL_CLOSE_HOVER_GROUP, |style| {
                        style.text_color(rgb(colors.text_primary))
                    }),
            )
            .on_click(move |_e, w, cx| close_listener(&(), w, cx))
    }

    fn render_scrim(&self, cx: &mut gpui::App) -> Stateful<Div> {
        let colors = cx.theme().colors;
        let close_listener = self.on_close.clone();
        div()
            .id((self.id.clone(), "scrim"))
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(colors.scrim))
            // Block mouse events from reaching the workspace behind the modal
            .occlude()
            .when_some(self.focus_handle.clone(), |el, handle| {
                el.track_focus(&handle)
            })
            .on_key_down(move |event, window, cx| {
                if event.keystroke.key.as_str() == "escape" {
                    cx.stop_propagation();
                    close_listener(&(), window, cx);
                }
            })
            // For now, we don't allow "click out" of modals
            .on_click(|_event, _window, cx| {
                cx.stop_propagation();
            })
    }
}

impl<H: IntoElement + 'static, B: IntoElement + 'static> RenderOnce for Modal<H, B> {
    fn render(self, _window: &mut gpui::Window, cx: &mut gpui::App) -> impl IntoElement {
        let id = self.id.clone();
        let colors = cx.theme().colors;
        let scrim = self.render_scrim(cx);
        let close_icon = self.has_close_icon.then(|| self.render_close_icon(cx));
        let mut head = self.render_head_container(cx).child(self.head);
        if let Some(close_icon) = close_icon {
            head = head.child(close_icon);
        }

        let panel = div()
            .id((id.clone(), "panel"))
            .debug_selector(|| format!("{}-{}", id, "panel"))
            .w(self.size.width())
            .bg(rgb(colors.bg_panel))
            .border_1()
            .border_color(rgb(colors.border))
            .rounded(self.size.radius())
            .overflow_hidden()
            // Swallows the click before it reaches the scrim's close-on-click
            .on_click(|_event, _window, cx| {
                cx.stop_propagation();
            })
            .child(head)
            .child(self.body);

        scrim.child(panel)
    }
}

/// A small uppercase modal section label ("NAME", "WHERE IT LIVES"),
/// optionally suffixed with contextual data
pub fn section_label(label: &'static str, suffix: Option<String>, cx: &gpui::App) -> Div {
    let colors = cx.theme().colors;
    let mut header = div().flex().flex_row().items_baseline().gap_1().child(
        div()
            .text_size(px(10.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(colors.text_tertiary))
            .child(label.to_uppercase()),
    );
    if let Some(suffix) = suffix {
        header = header.child(
            div()
                .font_family(cx.theme().fonts.data.clone())
                .text_size(px(10.0))
                .text_color(rgb(colors.text_secondary))
                .child(suffix),
        );
    }
    header
}

/// The styled shell of a modal footer bar
#[must_use]
pub fn footer_bar(cx: &gpui::App) -> Div {
    let colors = cx.theme().colors;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_4()
        .py(px(10.0))
        .border_t_1()
        .border_color(rgb(colors.border_soft))
}

/// A modal head with `title` on the left and an `esc` chip on the right,
/// for a modal that dismisses on Escape and opts out of the default close
/// icon (`has_close_icon(false)`).
pub fn head_with_esc_hint(title: &'static str, cx: &gpui::App) -> Div {
    let colors = cx.theme().colors;
    div()
        .flex_1()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .child(title)
        .child(
            div()
                .font_family(cx.theme().fonts.data.clone())
                .text_size(px(10.0))
                .text_color(rgb(colors.text_tertiary))
                .border_1()
                .border_color(rgb(colors.border))
                .rounded(px(4.0))
                .px(px(5.0))
                .child("esc"),
        )
}

#[cfg(test)]
mod tests;
