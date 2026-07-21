//! Visual demo for `TextField`: opens a window with a few fields so a human
//! can check the focus ring, blinking cursor, placeholder, typing, and
//! selection. Run with `cargo run -p zsql-ui --example text_field_demo`.

use gpui::{
    App, Application, Bounds, Context, Entity, Focusable, Render, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, rgb, size,
};
use zsql_ui::text_field::{TextFieldState, init};
use zsql_ui::theme::{ActiveTheme, Theme};

const WINDOW_WIDTH: f32 = 420.0;
const WINDOW_HEIGHT: f32 = 340.0;
const FIELD_GAP: f32 = 18.0;
const PAGE_PADDING: f32 = 28.0;
const LABEL_TEXT_SIZE: f32 = 10.5;

struct DemoRoot {
    name: Entity<TextFieldState>,
    url: Entity<TextFieldState>,
    notes: Entity<TextFieldState>,
}

impl DemoRoot {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            name: cx.new(|cx| TextFieldState::new("Connection name", None, cx)),
            url: cx.new(|cx| {
                TextFieldState::new(
                    "postgres://user@host:5432/db",
                    Some("postgres://app@staging.internal:5432/app"),
                    cx,
                )
            }),
            notes: cx.new(|cx| TextFieldState::new("Optional notes", None, cx)),
        }
    }

    fn labeled_field(
        label: &'static str,
        field: Entity<TextFieldState>,
        theme: &Theme,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .text_size(px(LABEL_TEXT_SIZE))
                    .text_color(rgb(theme.colors.text_tertiary))
                    .child(label),
            )
            .child(field)
    }
}

impl Render for DemoRoot {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .flex_col()
            .gap(px(FIELD_GAP))
            .size_full()
            .p(px(PAGE_PADDING))
            .bg(rgb(theme.colors.bg_panel))
            .child(Self::labeled_field("Name", self.name.clone(), &theme))
            .child(Self::labeled_field("URL", self.url.clone(), &theme))
            .child(Self::labeled_field("Notes", self.notes.clone(), &theme))
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        init(cx);

        let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let root = cx.new(DemoRoot::new);
                window.focus(&root.read(cx).name.read(cx).focus_handle(cx));
                root
            },
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
