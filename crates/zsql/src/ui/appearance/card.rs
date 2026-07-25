//! One Appearance-modal card: a mini zsql preview painted from an explicit
//! [`Colors`] value, never the global [`zsql_ui::theme::Theme`] -- this is
//! what lets an inactive theme's card show its own real colors while every
//! other view in the window keeps rendering with whatever theme is actually
//! active.

use gpui::{ElementId, FocusHandle, Stateful, div, prelude::*, px, rgb, rgba};
use zsql_ui::theme::Colors;

use crate::theme_resolve::{ThemeEntry, Tone};
use crate::ui::theme;

/// Render one theme card: its mini preview (painted in `entry.colors`) plus
/// a name/tone (or ACTIVE pill) row below it. `id` must be unique among
/// every card in the grid -- callers pass the theme's own name, which is
/// already unique by construction.
#[must_use]
pub(super) fn render_card(
    id: impl Into<ElementId>,
    entry: &ThemeEntry,
    is_active: bool,
    focus_handle: &FocusHandle,
    chrome: &Colors,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> Stateful<gpui::Div> {
    let ring_color = if is_active {
        chrome.accent
    } else {
        chrome.border
    };

    div()
        .id(id)
        .track_focus(focus_handle)
        .flex()
        .flex_col()
        .w(theme::APPEARANCE_CARD_WIDTH)
        .cursor_pointer()
        .child(
            div()
                .rounded(px(theme::APPEARANCE_CARD_PREVIEW_RADIUS))
                .border_2()
                .border_color(rgb(ring_color))
                .overflow_hidden()
                .child(render_mini_preview(entry.colors)),
        )
        .child(render_meta(entry, is_active, chrome))
        .on_click(on_click)
        .focus(move |style| {
            style
                .border_color(rgba(Colors::wash(chrome.accent, 0x66)))
                .bg(rgba(Colors::wash(chrome.accent, 0x0f)))
        })
}

/// The name/tone row below a card's preview: the theme's display name, plus
/// either its tone label (DARK/LIGHT/CUSTOM) or, when it is the active
/// theme, an ACTIVE pill in its place.
fn render_meta(entry: &ThemeEntry, is_active: bool, chrome: &Colors) -> impl IntoElement {
    let tone_label = match entry.tone {
        Tone::Dark => "DARK",
        Tone::Light => "LIGHT",
        Tone::Custom => "CUSTOM",
    };

    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .mt(theme::APPEARANCE_CARD_META_GAP)
        .child(
            div()
                .text_size(px(theme::APPEARANCE_CARD_NAME_TEXT_SIZE))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(rgb(chrome.text_primary))
                .child(entry.display_name.clone()),
        )
        .child(if is_active {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .text_size(px(theme::APPEARANCE_CARD_TONE_TEXT_SIZE))
                .text_color(rgb(chrome.accent))
                .child(zsql_ui::grid::status_dot(chrome.accent))
                .child("ACTIVE")
                .into_any_element()
        } else {
            div()
                .text_size(px(theme::APPEARANCE_CARD_TONE_TEXT_SIZE))
                .text_color(rgb(chrome.text_tertiary))
                .child(tone_label)
                .into_any_element()
        })
}

/// The mini zsql preview itself: a compact editor line, a type-tagged
/// results grid, and a status strip, every color drawn straight from
/// `colors` -- the parameter, never a global lookup, is what lets this paint
/// a theme other than whichever one the rest of the window is currently
/// rendering with.
fn render_mini_preview(colors: Colors) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .bg(rgb(colors.bg_app))
        .font_family("monospace")
        .child(render_mini_editor(colors))
        .child(render_mini_grid(colors))
        .child(render_mini_status(colors))
}

fn render_mini_editor(colors: Colors) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap_2()
        .h(theme::MINI_EDITOR_HEIGHT)
        .px(theme::MINI_PADDING_X)
        .bg(rgb(colors.bg_panel))
        .border_b_1()
        .border_color(rgb(colors.border))
        .text_size(px(theme::MINI_TEXT_SIZE))
        .child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                .overflow_hidden()
                .child(div().text_color(rgb(colors.syntax_keyword)).child("select"))
                .child(div().text_color(rgb(colors.text_primary)).child("*"))
                .child(div().text_color(rgb(colors.syntax_keyword)).child("from"))
                .child(div().text_color(rgb(colors.text_primary)).child("users")),
        )
        .child(
            div()
                .flex_shrink_0()
                .px(theme::MINI_RUN_CHIP_PADDING_X)
                .rounded(px(theme::MINI_RUN_CHIP_RADIUS))
                .bg(rgb(colors.accent))
                .text_color(rgb(colors.accent_contrast))
                .text_size(px(theme::MINI_RUN_CHIP_TEXT_SIZE))
                .font_weight(gpui::FontWeight::BOLD)
                .child("Run"),
        )
}

/// One mini-grid row's three columns: id/email/active. Shared by the header
/// (as plain labels plus type tags) and each data row.
const MINI_COLUMNS: [&str; 3] = ["id", "email", "active"];
const MINI_TYPE_TAGS: [&str; 3] = ["int8", "text", "bool"];

fn render_mini_grid(colors: Colors) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .child(render_mini_header(colors))
        .child(render_mini_data_row(
            colors,
            ["1", "ada@sql.io", "true"],
            [false, false, false],
        ))
        .child(render_mini_data_row(
            colors,
            ["2", "lin@sql.io", "false"],
            [false, false, false],
        ))
        .child(render_mini_data_row(
            colors,
            ["3", "NULL", "true"],
            [false, true, false],
        ))
}

fn render_mini_header(colors: Colors) -> impl IntoElement {
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .h(theme::MINI_GRID_ROW_HEIGHT)
        .px(theme::MINI_PADDING_X)
        .bg(rgb(colors.bg_raised))
        .border_b_1()
        .border_color(rgb(colors.border))
        .text_size(px(theme::MINI_TEXT_SIZE))
        .text_color(rgb(colors.text_primary));
    for (name, tag) in MINI_COLUMNS.into_iter().zip(MINI_TYPE_TAGS) {
        row = row.child(
            div()
                .flex_1()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .min_w_0()
                .overflow_hidden()
                .child(name)
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(theme::MINI_TAG_TEXT_SIZE))
                        .text_color(rgb(colors.accent))
                        .border_1()
                        .border_color(rgba(Colors::wash(colors.accent, 0x6b)))
                        .px(theme::MINI_TAG_PADDING_X)
                        .rounded(px(theme::MINI_TAG_RADIUS))
                        .child(tag),
                ),
        );
    }
    row
}

/// One mini-grid data row. `is_null` marks each column whose value should
/// render as the NULL placeholder rather than its given text, exercising the
/// same number/text/bool/NULL coloring the real results grid uses.
fn render_mini_data_row(colors: Colors, values: [&str; 3], is_null: [bool; 3]) -> impl IntoElement {
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .h(theme::MINI_GRID_ROW_HEIGHT)
        .px(theme::MINI_PADDING_X)
        .border_b_1()
        .border_color(rgb(colors.border_soft))
        .text_size(px(theme::MINI_TEXT_SIZE));
    for (index, value) in values.into_iter().enumerate() {
        let color = if is_null[index] {
            colors.value_null
        } else {
            match index {
                0 => colors.value_number,
                2 => colors.value_bool,
                _ => colors.value_text,
            }
        };
        let text = if is_null[index] { "NULL" } else { value };
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(rgb(color))
                .child(text.to_owned()),
        );
    }
    row
}

fn render_mini_status(colors: Colors) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .h(theme::MINI_STATUS_HEIGHT)
        .px(theme::MINI_PADDING_X)
        .bg(rgb(colors.bg_panel))
        .border_t_1()
        .border_color(rgb(colors.border))
        .text_size(px(theme::MINI_STATUS_TEXT_SIZE))
        .text_color(rgb(colors.text_secondary))
        .child(zsql_ui::grid::status_dot(colors.accent))
        .child("connected")
        .child(div().text_color(rgb(colors.text_tertiary)).child("."))
        .child("3 rows")
        .child(div().text_color(rgb(colors.text_tertiary)).child("."))
        .child("8 ms")
}
