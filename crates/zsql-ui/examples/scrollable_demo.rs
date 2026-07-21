//! Visual demo for `zsql_ui::scrollable`: three panes -- vertical-only,
//! horizontal-only, and both axes at once -- each built with
//! `with_scrollbars`. Each pane has a "More content"/"Less content" button
//! to toggle whether it overflows (exercising the scrollbar
//! appearing/disappearing) and a "Remount" button that swaps in a fresh,
//! never-laid-out scroll handle (exercising the first-frame-unmeasured
//! nudge path). Run with `cargo run -p zsql-ui --example scrollable_demo`.

use gpui::{
    App, Application, Bounds, ClickEvent, Context, Entity, Render, ScrollHandle,
    UniformListScrollHandle, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, rgba,
    size, uniform_list,
};
use zsql_ui::scrollable::{
    Axis, ScrollSource, ScrollableState, ScrollbarStyle, WithScrollbars, restrict_wheel_to_own_axis,
};
use zsql_ui::theme::{ActiveTheme, Theme};

const WINDOW_WIDTH: f32 = 900.0;
const WINDOW_HEIGHT: f32 = 420.0;
const PAGE_PADDING: f32 = 24.0;
const PANE_GAP: f32 = 20.0;
const PANE_WIDTH: f32 = 260.0;
const PANE_HEIGHT: f32 = 240.0;
const ROW_HEIGHT: f32 = 22.0;
const LABEL_TEXT_SIZE: f32 = 11.0;
const ROW_TEXT_SIZE: f32 = 12.0;
const BUTTON_TEXT_SIZE: f32 = 11.0;
const BUTTON_RADIUS: f32 = 4.0;
const BUTTON_HOVER: u32 = 0x87_8e_9f_33;

const SMALL_ROW_COUNT: usize = 4;
const LARGE_ROW_COUNT: usize = 60;
const SMALL_CONTENT_WIDTH: f32 = 220.0;
const LARGE_CONTENT_WIDTH: f32 = 1_400.0;

/// One demo pane's mechanical state: which axes it exercises, its scroll
/// handles, and whether it is currently showing the "large" (overflowing)
/// or "small" (fitting) content variant.
struct Pane {
    title: &'static str,
    vertical_enabled: bool,
    horizontal_enabled: bool,
    large_content: bool,
    scroll: Entity<ScrollableState>,
    list_handle: UniformListScrollHandle,
    container_handle: ScrollHandle,
}

impl Pane {
    fn new(
        title: &'static str,
        vertical_enabled: bool,
        horizontal_enabled: bool,
        cx: &mut Context<DemoRoot>,
    ) -> Self {
        Self {
            title,
            vertical_enabled,
            horizontal_enabled,
            large_content: true,
            scroll: cx.new(ScrollableState::new),
            list_handle: UniformListScrollHandle::new(),
            container_handle: ScrollHandle::new(),
        }
    }

    fn row_count(&self) -> usize {
        if !self.vertical_enabled {
            1
        } else if self.large_content {
            LARGE_ROW_COUNT
        } else {
            SMALL_ROW_COUNT
        }
    }

    fn content_width(&self) -> f32 {
        if !self.horizontal_enabled {
            0.0
        } else if self.large_content {
            LARGE_CONTENT_WIDTH
        } else {
            SMALL_CONTENT_WIDTH
        }
    }

    /// Replaces both scroll handles with fresh, never-laid-out ones, so the
    /// next render starts with zero measured bounds -- the same "first
    /// frame" state a scrollable pane is in the moment its content first
    /// appears -- to exercise the nudge that brings the scrollbar back
    /// without any further input.
    fn remount(&mut self) {
        self.list_handle = UniformListScrollHandle::new();
        self.container_handle = ScrollHandle::new();
    }

    #[allow(clippy::cast_precision_loss)] // row counts here are always tiny
    /// Point `scroll`'s axes at this pane's handles and current extents,
    /// clearing whichever axis this pane does not demonstrate.
    fn configure_axes(&self, cx: &mut Context<DemoRoot>) {
        let row_count = self.row_count();
        let content_width = self.content_width();
        let vertical_enabled = self.vertical_enabled;
        let horizontal_enabled = self.horizontal_enabled;

        self.scroll.update(cx, |state, _cx| {
            if vertical_enabled {
                state.vertical(Axis::new(
                    ScrollSource::UniformList(self.list_handle.clone()),
                    row_count as f32 * ROW_HEIGHT,
                ));
            } else {
                state.clear_vertical();
            }
            if horizontal_enabled {
                state.horizontal(Axis::new(
                    ScrollSource::Container(self.container_handle.clone()),
                    content_width,
                ));
            } else {
                state.clear_horizontal();
            }
        });
    }

    fn render(&self, cx: &mut Context<DemoRoot>) -> gpui::AnyElement {
        let theme = cx.theme();
        let row_count = self.row_count();
        let content_width = self.content_width();
        let horizontal_enabled = self.horizontal_enabled;

        self.configure_axes(cx);

        let list = restrict_wheel_to_own_axis(
            uniform_list(
                gpui::SharedString::from(format!("scrollable-demo-rows-{}", self.title)),
                row_count,
                move |range: std::ops::Range<usize>, _window, _cx| {
                    range
                        .map(|ix| {
                            div()
                                .flex_shrink_0()
                                .h(px(ROW_HEIGHT))
                                .px_2()
                                .flex()
                                .items_center()
                                .border_b_1()
                                .border_color(rgb(theme.colors.border_soft))
                                .text_size(px(ROW_TEXT_SIZE))
                                .text_color(rgb(theme.colors.text_primary))
                                .child(format!("row {ix}"))
                                .into_any_element()
                        })
                        .collect::<Vec<_>>()
                },
            )
            .flex_1()
            .track_scroll(self.list_handle.clone()),
        );

        let scroll = self.scroll.clone();
        let viewport: gpui::Div = if horizontal_enabled {
            div()
                .id(gpui::SharedString::from(format!(
                    "scrollable-demo-h-scroll-{}",
                    self.title
                )))
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .h_full()
                .overflow_x_hidden()
                .track_scroll(&self.container_handle)
                .on_scroll_wheel(ScrollableState::wheel_handler(&self.scroll))
                .child(
                    div()
                        .min_w(px(content_width))
                        .flex()
                        .flex_col()
                        .flex_1()
                        .child(list),
                )
                .with_scrollbars(&scroll, ScrollbarStyle::default(), cx)
        } else {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .h_full()
                .child(list)
                .with_scrollbars(&scroll, ScrollbarStyle::default(), cx)
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(LABEL_TEXT_SIZE))
                    .text_color(rgb(theme.colors.text_tertiary))
                    .child(self.title),
            )
            .child(
                div()
                    .w(px(PANE_WIDTH))
                    .h(px(PANE_HEIGHT))
                    .border_1()
                    .border_color(rgb(theme.colors.border))
                    .bg(rgb(theme.colors.bg_panel))
                    .child(viewport),
            )
            .into_any_element()
    }
}

struct DemoRoot {
    vertical_only: Pane,
    horizontal_only: Pane,
    both_axes: Pane,
}

impl DemoRoot {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            vertical_only: Pane::new("Vertical only", true, false, cx),
            horizontal_only: Pane::new("Horizontal only", false, true, cx),
            both_axes: Pane::new("Both axes", true, true, cx),
        }
    }

    fn button(
        label: &'static str,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        theme: &Theme,
    ) -> impl IntoElement {
        div()
            .id(label)
            .px_2()
            .py_1()
            .rounded(px(BUTTON_RADIUS))
            .border_1()
            .border_color(rgb(theme.colors.border))
            .bg(rgb(theme.colors.bg_raised))
            .text_size(px(BUTTON_TEXT_SIZE))
            .text_color(rgb(theme.colors.text_primary))
            .cursor_pointer()
            .hover(|el| el.bg(rgba(BUTTON_HOVER)))
            .child(label)
            .on_click(on_click)
    }

    fn pane_column(&mut self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let pane = match index {
            0 => &self.vertical_only,
            1 => &self.horizontal_only,
            _ => &self.both_axes,
        };
        let content = pane.render(cx);

        let toggle_label: &'static str = if pane.large_content {
            "Less content"
        } else {
            "More content"
        };

        let toggle_click = cx.listener(move |view: &mut Self, _event: &ClickEvent, _window, cx| {
            let pane = view.pane_mut(index);
            pane.large_content = !pane.large_content;
            cx.notify();
        });
        let remount_click =
            cx.listener(move |view: &mut Self, _event: &ClickEvent, _window, cx| {
                view.pane_mut(index).remount();
                cx.notify();
            });

        div().flex().flex_col().gap_2().child(content).child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(Self::button(toggle_label, toggle_click, &theme))
                .child(Self::button("Remount", remount_click, &theme)),
        )
    }

    fn pane_mut(&mut self, index: usize) -> &mut Pane {
        match index {
            0 => &mut self.vertical_only,
            1 => &mut self.horizontal_only,
            _ => &mut self.both_axes,
        }
    }
}

impl Render for DemoRoot {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .gap(px(PANE_GAP))
            .size_full()
            .p(px(PAGE_PADDING))
            .bg(rgb(theme.colors.bg_app))
            .child(
                div()
                    .text_size(px(LABEL_TEXT_SIZE))
                    .text_color(rgb(theme.colors.text_tertiary))
                    .child(
                        "Each pane's scrollbar appears once its content overflows; toggle \
                         content size or remount a pane to exercise the first-frame path.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(PANE_GAP))
                    .child(self.pane_column(0, cx))
                    .child(self.pane_column(1, cx))
                    .child(self.pane_column(2, cx)),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| cx.new(DemoRoot::new),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
