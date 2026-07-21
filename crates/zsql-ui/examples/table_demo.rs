//! Visual demo for `zsql_ui::table`: a generated grid of rows and columns,
//! with keys to change the row/column count and cycle `TableStyle`/`Gutter`
//! presets. Run with `cargo run -p zsql-ui --example table_demo`.
//!
//! Keys (once the window has focus):
//! - `r` / `shift-r`: grow / shrink the row count
//! - `c` / `shift-c`: grow / shrink the column count
//! - `b`: cycle borders on/off
//! - `p`: cycle cell-padding presets
//! - `g`: cycle the gutter (none / row numbers / a custom two-letter tag)

use gpui::{
    AnyElement, App, Application, Bounds, Context, Entity, FocusHandle, Focusable, IntoElement,
    KeyDownEvent, Render, Window, WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
};
use zsql_ui::table::{
    Gutter, RowNumberStyle, Table, TableColumn, TableRow, TableState, TableStyle,
};
use zsql_ui::theme::{ActiveTheme, Theme};

const WINDOW_WIDTH: f32 = 900.0;
const WINDOW_HEIGHT: f32 = 560.0;
const PAGE_PADDING: f32 = 16.0;
const HINT_TEXT_SIZE: f32 = 11.0;
const COLUMN_WIDTH: f32 = 160.0;
const ROW_COUNT_STEP: usize = 50;
const MAX_ROW_COUNT: usize = 5_000;
const MAX_COLUMN_COUNT: usize = 24;

const NARROW_PADDING: f32 = 4.0;
const WIDE_PADDING: f32 = 20.0;

/// Which cell-padding preset the demo currently applies, cycled by the `p`
/// key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaddingPreset {
    Default,
    Narrow,
    Wide,
}

impl PaddingPreset {
    fn next(self) -> Self {
        match self {
            Self::Default => Self::Narrow,
            Self::Narrow => Self::Wide,
            Self::Wide => Self::Default,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Narrow => "narrow",
            Self::Wide => "wide",
        }
    }

    fn cell_padding_x(self) -> f32 {
        match self {
            Self::Default => TableStyle::default().cell_padding_x.into(),
            Self::Narrow => NARROW_PADDING,
            Self::Wide => WIDE_PADDING,
        }
    }
}

/// Which gutter preset the demo currently applies, cycled by the `g` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GutterPreset {
    None,
    RowNumbers,
    Custom,
}

impl GutterPreset {
    fn next(self) -> Self {
        match self {
            Self::None => Self::RowNumbers,
            Self::RowNumbers => Self::Custom,
            Self::Custom => Self::None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RowNumbers => "row numbers",
            Self::Custom => "custom (row-kind tag)",
        }
    }
}

struct DemoRoot {
    focus: FocusHandle,
    table_state: Entity<TableState>,
    row_count: usize,
    column_count: usize,
    borders_on: bool,
    padding: PaddingPreset,
    gutter: GutterPreset,
}

impl DemoRoot {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            table_state: cx.new(TableState::new),
            row_count: 400,
            column_count: 6,
            borders_on: true,
            padding: PaddingPreset::Default,
            gutter: GutterPreset::RowNumbers,
        }
    }

    fn style(&self, theme: &Theme) -> TableStyle {
        let mut style = TableStyle::themed(theme);
        style.borders.row = self.borders_on;
        style.borders.column = self.borders_on;
        style.borders.outer = self.borders_on;
        style.cell_padding_x = px(self.padding.cell_padding_x());
        style
    }

    fn columns(&self, theme: &Theme) -> Vec<TableColumn> {
        (0..self.column_count)
            .map(|ix| {
                TableColumn::new(
                    px(COLUMN_WIDTH),
                    div()
                        .text_color(rgb(theme.colors.text_primary))
                        .child(format!("column_{ix}")),
                )
            })
            .collect()
    }

    fn handle_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "r" if event.keystroke.modifiers.shift => {
                self.row_count = self.row_count.saturating_sub(ROW_COUNT_STEP);
            }
            "r" => {
                self.row_count = (self.row_count + ROW_COUNT_STEP).min(MAX_ROW_COUNT);
            }
            "c" if event.keystroke.modifiers.shift => {
                self.column_count = self.column_count.saturating_sub(1).max(1);
            }
            "c" => {
                self.column_count = (self.column_count + 1).min(MAX_COLUMN_COUNT);
            }
            "b" => self.borders_on = !self.borders_on,
            "p" => self.padding = self.padding.next(),
            "g" => self.gutter = self.gutter.next(),
            _ => return,
        }
        cx.notify();
    }

    fn hint_bar(&self, theme: &Theme) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .text_size(px(HINT_TEXT_SIZE))
            .text_color(rgb(theme.colors.text_tertiary))
            .child(format!(
                "rows: {}  columns: {}  borders: {}  padding: {}  gutter: {}",
                self.row_count,
                self.column_count,
                if self.borders_on { "on" } else { "off" },
                self.padding.label(),
                self.gutter.label(),
            ))
            .child("r/shift-r: rows  c/shift-c: columns  b: borders  p: padding  g: gutter")
    }
}

impl Focusable for DemoRoot {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DemoRoot {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let row_count = self.row_count;
        let column_count = self.column_count;
        let gutter = self.gutter;

        let table = Table::new("table-demo", &self.table_state)
            .style(self.style(&theme))
            .columns(self.columns(&theme))
            .row_count(row_count)
            .gutter(match gutter {
                GutterPreset::None => Gutter::None,
                GutterPreset::RowNumbers => Gutter::RowNumbers(RowNumberStyle::default()),
                GutterPreset::Custom => Gutter::Custom {
                    width: px(60.0),
                    header: div().child("KIND").into_any_element(),
                    render: Box::new(move |_this: &mut Self, range, _window, cx| {
                        range
                            .map(|ix| row_kind_tag(ix, cx.theme()).into_any_element())
                            .collect::<Vec<AnyElement>>()
                    }),
                },
            })
            .rows(move |_this: &mut Self, range, _window, _cx| {
                range
                    .map(|ix| {
                        let cells = (0..column_count)
                            .map(|col| {
                                div()
                                    .text_color(rgb(theme.colors.text_primary))
                                    .child(format!("r{ix}c{col}"))
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>();
                        TableRow::new(cells)
                    })
                    .collect::<Vec<_>>()
            })
            .render(cx);

        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _window, cx| {
                view.handle_key_down(event, cx);
            }))
            .flex()
            .flex_col()
            .gap_2()
            .size_full()
            .p(px(PAGE_PADDING))
            .bg(rgb(theme.colors.bg_app))
            .child(self.hint_bar(&theme))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .border_1()
                    .border_color(rgb(theme.colors.border))
                    .child(table),
            )
    }
}

/// A short two-letter tag standing in for arbitrary caller-rendered gutter
/// content, cycling through a few kinds so a custom gutter reads as
/// genuinely per-row rather than a fixed label.
fn row_kind_tag(ix: usize, theme: &Theme) -> gpui::Div {
    const KINDS: [&str; 3] = ["TB", "VW", "MV"];
    div()
        .text_color(rgb(theme.colors.accent))
        .child(KINDS[ix % KINDS.len()])
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(DemoRoot::new);
                window.focus(&view.read(cx).focus);
                view
            },
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
