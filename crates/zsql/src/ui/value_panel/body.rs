//! The value panel's plain-text/mono body renderers.

use std::ops::Range;

use gpui::{App, ClickEvent, Div, Entity, Hsla, Window, div, font, prelude::*, px, rgb};
use zsql_ui::selectable_text::{SelectableTextStyle, with_selectable_text};
use zsql_ui::theme::{ActiveTheme, Theme};

use super::view::ValuePanel;
use crate::ui::theme;

/// Prose body for the Text renderer: padded, scrollable, selectable.
#[derive(IntoElement)]
pub(super) struct TextBody {
    panel: Entity<ValuePanel>,
    text: String,
    selection: Option<Range<usize>>,
}

impl TextBody {
    pub(super) fn new(
        panel: Entity<ValuePanel>,
        text: &str,
        selection: Option<Range<usize>>,
    ) -> Self {
        Self {
            panel,
            text: text.to_owned(),
            selection,
        }
    }
}

impl RenderOnce for TextBody {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let container = div()
            .flex_1()
            .min_h_0()
            .p(theme::VALUE_PANEL_PADDING_X)
            .text_size(px(theme::VALUE_PANEL_TEXT_SIZE))
            .text_color(rgb(colors.text_primary))
            .font_family(&active_theme.fonts.data);
        selectable(
            container,
            &self.text,
            self.selection,
            &self.panel,
            active_theme,
        )
        .id("value-panel-text-body")
        .debug_selector(|| "value-panel-text-body".to_owned())
        .overflow_y_scroll()
    }
}

/// Monospace body for every raw/encoded value text; optionally scrollable.
#[derive(IntoElement)]
pub(super) struct MonoBody {
    panel: Entity<ValuePanel>,
    text: String,
    selection: Option<Range<usize>>,
    scroll: bool,
}

impl MonoBody {
    pub(super) fn new(
        panel: Entity<ValuePanel>,
        text: &str,
        selection: Option<Range<usize>>,
    ) -> Self {
        Self {
            panel,
            text: text.to_owned(),
            selection,
            scroll: false,
        }
    }

    #[must_use]
    pub(super) fn scrollable(mut self) -> Self {
        self.scroll = true;
        self
    }
}

impl RenderOnce for MonoBody {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let active_theme = cx.theme();
        let body = mono_text(&self.panel, &self.text, self.selection, active_theme)
            .flex_1()
            .min_h_0()
            .p(theme::VALUE_PANEL_PADDING_X);
        if self.scroll {
            body.id("value-panel-scroll-body")
                .overflow_y_scroll()
                .into_any_element()
        } else {
            body.into_any_element()
        }
    }
}

/// Centered placeholder body for a NULL value.
#[derive(IntoElement)]
pub(super) struct NullBody;

impl RenderOnce for NullBody {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors;
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .text_color(rgb(colors.value_null))
            .child("NULL")
    }
}

/// Body for a JSON value past the eager-parse threshold: a truncation
/// notice, the selectable preview text, and a load-the-rest action.
#[derive(IntoElement)]
pub(super) struct OversizedJsonBody {
    panel: Entity<ValuePanel>,
    preview: String,
    total_bytes: usize,
    selection: Option<Range<usize>>,
}

impl OversizedJsonBody {
    pub(super) fn new(
        panel: Entity<ValuePanel>,
        preview: &str,
        total_bytes: usize,
        selection: Option<Range<usize>>,
    ) -> Self {
        Self {
            panel,
            preview: preview.to_owned(),
            total_bytes,
            selection,
        }
    }
}

impl RenderOnce for OversizedJsonBody {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let total_bytes = self.total_bytes;
        let load_panel = self.panel.clone();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_2()
            .p(theme::VALUE_PANEL_PADDING_X)
            .child(
                div()
                    .text_size(px(theme::VALUE_PANEL_LABEL_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(format!(
                        "{total_bytes} bytes -- past the eager-parse threshold; showing the \
                             first {} bytes",
                        self.preview.len()
                    )),
            )
            .child(
                mono_text(&self.panel, &self.preview, self.selection, active_theme)
                    .id("value-panel-oversized-preview")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll(),
            )
            .child(
                div()
                    .id("value-panel-load-full")
                    .cursor_pointer()
                    .flex_shrink_0()
                    .px_2()
                    .h(theme::VALUE_PANEL_BUTTON_HEIGHT)
                    .flex()
                    .items_center()
                    .rounded(px(theme::VALUE_PANEL_BUTTON_RADIUS))
                    .bg(theme::sidebar_selected_bg(active_theme))
                    .text_color(rgb(colors.accent))
                    .text_size(px(theme::VALUE_PANEL_LABEL_TEXT_SIZE))
                    .child("Load full value")
                    .on_click(move |_event: &ClickEvent, _window, app| {
                        load_panel.update(app, ValuePanel::load_full_json_value);
                    }),
            )
    }
}

/// Unpadded monospace selectable text block shared by the mono bodies.
fn mono_text(
    panel: &Entity<ValuePanel>,
    text: &str,
    selection: Option<Range<usize>>,
    active_theme: &Theme,
) -> Div {
    let container = div()
        .font_family(&active_theme.fonts.data)
        .text_size(px(theme::VALUE_PANEL_TEXT_SIZE))
        .text_color(rgb(active_theme.colors.text_primary));
    selectable(container, text, selection, panel, active_theme)
}

/// Wire mouse selection for `text` onto `container`, reporting selection
/// changes back to `panel`.
fn selectable(
    container: Div,
    text: &str,
    selection: Option<Range<usize>>,
    panel: &Entity<ValuePanel>,
    active_theme: &Theme,
) -> Div {
    let style = SelectableTextStyle {
        font: font(active_theme.fonts.data.clone()),
        color: Hsla::from(rgb(active_theme.colors.text_primary)),
        selection_bg: Hsla::from(theme::text_selection_bg(active_theme)),
    };
    let down_panel = panel.clone();
    let drag_panel = panel.clone();
    with_selectable_text(
        container,
        text,
        &style,
        selection,
        move |offset, extend, _window, app| {
            down_panel.update(app, |view, cx| {
                view.state.text_selection_mut().begin(offset, extend);
                cx.notify();
            });
        },
        move |offset, _window, app| {
            drag_panel.update(app, |view, cx| {
                if view
                    .state
                    .text_selection_mut()
                    .extend_while_dragging(offset)
                {
                    cx.notify();
                }
            });
        },
    )
}
