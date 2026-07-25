//! [`AppearanceModalView`]'s rendering: the modal shell, title/subtitle
//! header, the card grid (see [`super::card`]), and the footer hint.

use std::path::Path;

use gpui::{Context, Div, KeyDownEvent, Render, Window, div, prelude::*, px, rgb};
use zsql_ui::modal::{Modal, ModalSize};
use zsql_ui::theme::ActiveTheme;

use super::AppearanceModalView;
use super::card::render_card;
use crate::ui::theme;

impl Render for AppearanceModalView {
    /// The Appearance modal. The caller (`ui::workspace::WorkspaceView`) is
    /// responsible for conditionally mounting this entity, so `render` does
    /// not re-check `open` itself.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = div()
            .id("appearance-modal-body")
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.handle_key_down(event, window, cx);
            }))
            .child(self.render_grid(window, cx))
            .child(render_footer(self.themes_dir.as_deref(), cx));

        Modal::<Div, Div>::new("appearance-modal")
            .size(ModalSize::Large)
            .track_focus(&self.modal_focus)
            .on_close(cx.listener(|view, (), _w, cx| view.close(cx)))
            .head(render_head(cx))
            .body(body)
    }
}

impl AppearanceModalView {
    /// The scrollable grid of theme cards. Every card is a radio in this
    /// modal's radiogroup: exactly one (the active theme's) is checked at a
    /// time, arrow keys move which one is checked (see
    /// [`AppearanceModalView::handle_key_down`]), and a click checks and
    /// applies whichever one was clicked.
    fn render_grid(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let mut grid = div()
            .id("appearance-modal-grid")
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(theme::APPEARANCE_MODAL_GRID_GAP)
            .p(theme::APPEARANCE_MODAL_GRID_PADDING)
            .max_h(theme::APPEARANCE_MODAL_GRID_MAX_HEIGHT)
            .overflow_y_scroll();

        for (index, entry) in self.themes.iter().enumerate() {
            let is_active = entry.name == self.active_name;
            let Some(focus_handle) = self.card_focus_handles.get(index).cloned() else {
                continue;
            };
            let card_id = gpui::SharedString::from(format!("appearance-card-{}", entry.name));
            grid = grid.child(render_card(
                card_id,
                entry,
                is_active,
                &focus_handle,
                &colors,
                cx.listener(move |view, _event, window, cx| {
                    view.select(index, window, cx);
                }),
            ));
        }
        grid
    }
}

/// The modal's title/subtitle header.
fn render_head(cx: &Context<AppearanceModalView>) -> Div {
    let colors = cx.theme().colors;
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(px(theme::APPEARANCE_MODAL_TITLE_TEXT_SIZE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(colors.text_primary))
                .child("Appearance"),
        )
        .child(
            div()
                .text_size(px(theme::APPEARANCE_MODAL_SUBTITLE_TEXT_SIZE))
                .text_color(rgb(colors.text_secondary))
                .child("Pick a theme. It applies to every window right away."),
        )
}

/// The footer: a hint naming the actual themes directory files are scanned
/// from (so it is correct on every platform, not just Linux's `~/.config`),
/// and a Done button that closes the modal.
fn render_footer(themes_dir: Option<&Path>, cx: &Context<AppearanceModalView>) -> Div {
    let colors = cx.theme().colors;
    let hint = match themes_dir {
        Some(dir) => format!("Add your own -- drop a theme.json into {}", dir.display()),
        None => "Add your own -- drop a theme.json into your zsql themes directory".to_owned(),
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_4()
        .px(theme::APPEARANCE_FOOTER_PADDING_X)
        .py(theme::APPEARANCE_FOOTER_PADDING_Y)
        .border_t_1()
        .border_color(rgb(colors.border_soft))
        .child(
            div()
                .text_size(px(theme::APPEARANCE_FOOTER_HINT_TEXT_SIZE))
                .text_color(rgb(colors.text_secondary))
                .child(hint),
        )
        .child(
            div()
                .id("appearance-modal-done")
                .flex_shrink_0()
                .px(theme::APPEARANCE_DONE_BUTTON_PADDING_X)
                .py(theme::APPEARANCE_DONE_BUTTON_PADDING_Y)
                .rounded(px(theme::APPEARANCE_DONE_BUTTON_RADIUS))
                .cursor_pointer()
                .bg(rgb(colors.accent))
                .text_color(rgb(colors.accent_contrast))
                .text_size(px(theme::APPEARANCE_DONE_BUTTON_TEXT_SIZE))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Done")
                .on_click(cx.listener(|view, _event, _window, cx| view.close(cx))),
        )
}
