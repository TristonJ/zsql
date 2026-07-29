use std::rc::Rc;

use gpui::{
    App, ClickEvent, Corner, Div, MouseButton, Pixels, Point, RenderOnce, SharedString, Window,
    anchored, deferred, div, prelude::*, px, rgb, rgba,
};

use crate::theme::{ActiveTheme, Colors, Theme};

#[derive(Debug, Clone, Copy)]
pub struct ContextMenuStyle {
    /// Width of a right-click context menu.
    pub width: Pixels,
    /// Padding around a context menu's items.
    pub padding: Pixels,
    /// Corner radius of a context menu.
    pub radius: f32,
    /// Height of one context menu item.
    pub item_height: Pixels,
    /// Horizontal padding inside a context menu item.
    pub item_padding_x: Pixels,
    /// Corner radius of a context menu item.
    pub item_radius: f32,
    /// Text size of a context menu item's label.
    pub item_text_size: f32,
    /// Height of a context menu's separator line.
    pub separator_height: Pixels,
    /// Vertical margin around a context menu separator.
    pub separator_margin_y: Pixels,
}

impl Default for ContextMenuStyle {
    fn default() -> Self {
        Self {
            width: px(210.0),
            padding: px(5.0),
            radius: 8.0,
            item_height: px(28.0),
            item_padding_x: px(9.0),
            item_radius: 5.0,
            item_text_size: 12.0,
            separator_height: px(1.0),
            separator_margin_y: px(5.0),
        }
    }
}

pub type ContextMenuOnCloseFn = Rc<dyn Fn(&(), &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct ContextMenu {
    id: SharedString,
    style: ContextMenuStyle,
    items: Vec<ContextMenuItem>,
    on_close: Option<ContextMenuOnCloseFn>,
    position: Option<Point<Pixels>>,
    anchor: Option<Corner>,
    offset: Option<Point<Pixels>>,
    separators: Vec<usize>,
}

impl ContextMenu {
    pub fn new(id: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            style: ContextMenuStyle::default(),
            items: Vec::new(),
            on_close: None,
            position: None,
            anchor: None,
            offset: None,
            separators: Vec::new(),
        }
    }

    #[must_use]
    pub fn style(mut self, style: ContextMenuStyle) -> Self {
        self.style = style;
        self
    }

    #[must_use]
    pub fn add_item(mut self, item: ContextMenuItem) -> Self {
        self.items.push(item);
        self
    }

    #[must_use]
    pub fn add_separator(mut self) -> Self {
        self.separators.push(self.items.len());
        self
    }

    #[must_use]
    pub fn position(mut self, position: Point<Pixels>) -> Self {
        self.position = Some(position);
        self
    }

    #[must_use]
    pub fn on_close(mut self, f: impl Fn(&(), &mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(f));
        self
    }

    #[must_use]
    pub fn anchor(mut self, anchor: Corner) -> Self {
        self.anchor = Some(anchor);
        self
    }

    #[must_use]
    pub fn offset(mut self, offset: Point<Pixels>) -> Self {
        self.offset = Some(offset);
        self
    }

    fn render_separator(style: &ContextMenuStyle, theme: &Theme, selector: String) -> Div {
        div()
            .h(style.separator_height)
            .my(style.separator_margin_y)
            .bg(rgb(theme.colors.border_soft))
            .debug_selector(move || selector)
    }
}

pub type ContextMenuOnClickFn = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct ContextMenuItem {
    id: SharedString,
    label: String,
    style: ContextMenuStyle,
    on_click: Option<ContextMenuOnClickFn>,
}

impl ContextMenuItem {
    #[must_use]
    pub fn new(label: &'static str) -> Self {
        Self::with_id(SharedString::from(label), label)
    }

    pub fn with_id(id: impl Into<SharedString>, label: impl AsRef<str>) -> Self {
        let label = label.as_ref();
        Self {
            id: id.into(),
            label: label.to_string(),
            style: ContextMenuStyle::default(),
            on_click: None,
        }
    }

    /// Override the default id of the context menu item. By default, the id is set to the label of the item.
    #[must_use]
    pub fn id(mut self, id: impl Into<SharedString>) -> Self {
        self.id = id.into();
        self
    }

    /// Set the callback function to be called when the context menu item is clicked.
    #[must_use]
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }

    #[must_use]
    pub(crate) fn style(mut self, style: ContextMenuStyle) -> Self {
        self.style = style;
        self
    }
}

impl RenderOnce for ContextMenu {
    fn render(self, window: &mut gpui::Window, cx: &mut gpui::App) -> impl IntoElement {
        let theme = cx.theme();
        let menu_selector = self.id.to_string();
        let mut menu = div()
            .id(self.id)
            .debug_selector({
                let menu_selector = menu_selector.clone();
                move || menu_selector
            })
            .occlude()
            .w(self.style.width)
            .p(self.style.padding)
            .bg(rgb(theme.colors.bg_raised))
            .border_1()
            .border_color(rgb(theme.colors.border))
            .rounded(px(self.style.radius));
        let style = self.style;
        for (i, item) in self.items.into_iter().enumerate() {
            for separator_index in &self.separators {
                if *separator_index == i {
                    let separator_selector = format!("{menu_selector}-separator-{i}");
                    menu = menu.child(Self::render_separator(&style, theme, separator_selector));
                }
            }
            menu = menu.child(item.style(self.style));
        }

        let viewport_size = window.viewport_size();
        let on_mouse_left_close = self.on_close.clone();
        let on_mouse_right_close = self.on_close.clone();
        let backdrop = div()
            .absolute()
            .inset_0()
            .occlude()
            .w(viewport_size.width)
            .h(viewport_size.height)
            .when_some(on_mouse_left_close, |el, on_close| {
                el.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    on_close(&(), window, cx);
                })
            })
            .when_some(on_mouse_right_close, |el, on_close| {
                el.on_mouse_down(MouseButton::Right, move |_, window, cx| {
                    on_close(&(), window, cx);
                })
            });

        let container = div()
            .absolute()
            .child(anchored().position(Point::default()).child(backdrop))
            .child(
                anchored()
                    .when_some(self.position, gpui::Anchored::position)
                    .when_some(self.anchor, gpui::Anchored::anchor)
                    .when_some(self.offset, gpui::Anchored::offset)
                    .snap_to_window()
                    .child(menu),
            );

        deferred(container).with_priority(1)
    }
}

impl RenderOnce for ContextMenuItem {
    fn render(self, _window: &mut gpui::Window, cx: &mut gpui::App) -> impl IntoElement {
        let theme = cx.theme();
        let selector = self.id.to_string();
        div()
            .id(self.id)
            .debug_selector(move || selector)
            .flex()
            .flex_row()
            .items_center()
            .h(self.style.item_height)
            .px(self.style.item_padding_x)
            .rounded(px(self.style.item_radius))
            .cursor_pointer()
            .text_size(px(self.style.item_text_size))
            .text_color(rgb(theme.colors.text_primary))
            .hover(|el| el.bg(rgba(Colors::wash(theme.colors.accent, 0x1a))))
            .child(self.label)
            .when_some(self.on_click, |el, on_click| el.on_click(on_click))
    }
}

#[cfg(test)]
mod tests;
