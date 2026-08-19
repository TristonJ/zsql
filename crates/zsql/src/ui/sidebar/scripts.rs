//! Render logic for the sidebar's Scripts pane

use gpui::{ClickEvent, Context, Div, Stateful, Window, div, prelude::*, px, rgb, rgba};
use zsql_ui::icon::{IconName, icon};
use zsql_ui::scrollable::{Axis, ScrollSource, WithScrollbars};
use zsql_ui::theme::ActiveTheme;
use zsql_ui::tree::{row_meta, row_shell};
use zsql_ui::utils::OnHoverState;

use super::SidebarView;
use super::model::{ScriptRow, ScriptRowKind, library_row_is_open, scripts_pane_shows_empty_state};
use crate::ui::tabs::TabModel;
use crate::ui::theme;

/// The glyph every script row leads with
const SCRIPT_ROW_GLYPH: &str = "\u{2261}";
/// The middle-dot separator between a group label and its trailing suffix.
const GROUP_LABEL_SEPARATOR: &str = "\u{b7}";

/// The Scripts pane - a "This connection" group, then a "Library" group,
/// filling the sidebar's full remaining height below the pane tabs/database
/// row
pub(super) fn render_scripts_pane(
    view: &SidebarView,
    window: &mut Window,
    cx: &mut Context<SidebarView>,
) -> Div {
    let session_rows: Vec<(usize, &ScriptRow)> = view
        .script_rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.kind == ScriptRowKind::Session)
        .collect();
    let library_rows: Vec<(usize, &ScriptRow)> = view
        .script_rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.kind == ScriptRowKind::Library)
        .collect();
    let shows_empty_state = scripts_pane_shows_empty_state(&view.script_rows);
    let library_is_empty = library_rows.is_empty();

    let mut pane = div()
        .id("sidebar-scripts-pane")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(&view.scripts_scroll_handle)
        .py(px(theme::SIDEBAR_TREE_PADDING_Y))
        .child(render_connection_group_label(&view.connection_name, cx));

    pane = if shows_empty_state {
        pane.child(render_empty_state(cx))
    } else {
        pane.children(
            session_rows
                .into_iter()
                .map(|(index, row)| render_script_row(index, row, cx)),
        )
    };

    pane = pane.child(render_library_group_label(cx));
    pane = if library_is_empty {
        pane.child(render_library_empty_state(cx))
    } else {
        pane.children(
            library_rows
                .into_iter()
                .map(|(index, row)| render_script_row(index, row, cx)),
        )
    };

    view.scripts_scroll.update(cx, |scroll, _cx| {
        scroll.vertical(Axis::measured(ScrollSource::Container(
            view.scripts_scroll_handle.clone(),
        )));
    });
    let scroll_area = pane.with_scrollbars(
        &view.scripts_scroll,
        SidebarView::tree_scrollbar_style(cx.theme()),
        cx,
    );

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(scroll_area)
        .child(render_scripts_footer(window, cx))
}

/// The pane's pinned footer: a full-width button row, outside the
/// scrollable list, that starts the platform open-file dialog.
fn render_scripts_footer(window: &mut Window, cx: &mut Context<SidebarView>) -> Div {
    let colors = cx.theme().colors;
    let data_font = cx.theme().fonts.data.clone();
    let row = div()
        .id("sidebar-scripts-open-external")
        .debug_selector(|| "sidebar-scripts-open-external".to_owned())
        .flex_1()
        .flex()
        .flex_row()
        .items_center()
        .gap(theme::SIDEBAR_SCRIPTS_FOOTER_GAP)
        .h(theme::SIDEBAR_SCRIPTS_FOOTER_ROW_HEIGHT)
        .px(theme::SIDEBAR_SCRIPTS_FOOTER_PADDING_X)
        .rounded(px(theme::SIDEBAR_SCRIPTS_FOOTER_ROW_RADIUS))
        .border_1()
        .border_color(rgba(theme::COLOR_TRANSPARENT))
        .cursor_pointer()
        .text_color(rgb(colors.text_secondary))
        .hover(|el| {
            el.bg(rgb(colors.bg_raised))
                .border_color(rgb(colors.border))
        })
        .on_hover_state(window, cx, |el| el.text_color(rgb(colors.text_primary)))
        .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
            view.tabs.update(cx, TabModel::request_browse);
        }))
        .child(icon(
            IconName::FolderOpen,
            theme::SIDEBAR_SCRIPTS_FOOTER_ICON_SIZE,
            colors.text_tertiary,
        ))
        .child(div().flex_1().child("Open external file..."))
        .child(
            div()
                .flex_shrink_0()
                .font_family(&data_font)
                .text_size(px(theme::SIDEBAR_SCRIPTS_FOOTER_SHORTCUT_TEXT_SIZE))
                .text_color(rgb(colors.text_tertiary))
                .px(theme::SIDEBAR_SCRIPTS_FOOTER_SHORTCUT_PADDING_X)
                .rounded(px(theme::SIDEBAR_SCRIPTS_FOOTER_SHORTCUT_RADIUS))
                .border_1()
                .border_color(rgb(colors.border))
                .child("Ctrl+Shift+O"),
        );
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .h(theme::SIDEBAR_SCRIPTS_FOOTER_HEIGHT)
        .px(theme::SIDEBAR_SCRIPTS_FOOTER_PADDING_X)
        .border_t_1()
        .border_color(rgb(colors.border_soft))
        .child(row)
}

/// The Library group's own empty-state line
fn render_library_empty_state(cx: &Context<SidebarView>) -> Div {
    div()
        .flex_shrink_0()
        .px(theme::SIDEBAR_SCRIPT_GROUP_PADDING_X)
        .text_size(px(theme::SIDEBAR_SCRIPTS_EMPTY_DETAIL_TEXT_SIZE))
        .text_color(rgb(cx.theme().colors.text_tertiary))
        .child("No library scripts yet")
}

fn group_label_shell(cx: &Context<SidebarView>) -> Div {
    div()
        .flex_shrink_0()
        .h(theme::SIDEBAR_SCRIPT_GROUP_HEIGHT)
        .flex()
        .flex_row()
        .items_center()
        .px(theme::SIDEBAR_SCRIPT_GROUP_PADDING_X)
        .gap(theme::SIDEBAR_SCRIPT_GROUP_SUFFIX_GAP)
        .text_size(px(theme::SIDEBAR_SCRIPT_GROUP_TEXT_SIZE))
        .text_color(rgb(cx.theme().colors.text_tertiary))
        .font_weight(gpui::FontWeight::SEMIBOLD)
}

fn render_connection_group_label(connection_name: &str, cx: &Context<SidebarView>) -> Div {
    let data_font = cx.theme().fonts.data.clone();
    let text_tertiary = cx.theme().colors.text_tertiary;
    group_label_shell(cx).child("THIS CONNECTION").child(
        div()
            .font_family(&data_font)
            .font_weight(gpui::FontWeight::NORMAL)
            .text_color(rgb(text_tertiary))
            .child(format!("{GROUP_LABEL_SEPARATOR} {connection_name}")),
    )
}

fn render_library_group_label(cx: &Context<SidebarView>) -> Div {
    group_label_shell(cx)
        .mt(theme::SIDEBAR_SCRIPT_GROUP_MARGIN_TOP)
        .child("LIBRARY")
}

fn render_empty_state(cx: &Context<SidebarView>) -> Div {
    let colors = cx.theme().colors;
    let data_font = cx.theme().fonts.data.clone();
    div()
        .flex_shrink_0()
        .mx(theme::SIDEBAR_SCRIPTS_EMPTY_MARGIN_X)
        .mt(theme::SIDEBAR_SCRIPTS_EMPTY_MARGIN_TOP)
        .mb(theme::SIDEBAR_SCRIPTS_EMPTY_MARGIN_BOTTOM)
        .px(theme::SIDEBAR_SCRIPTS_EMPTY_PADDING_X)
        .py(theme::SIDEBAR_SCRIPTS_EMPTY_PADDING_Y)
        .border_1()
        .border_dashed()
        .border_color(rgb(colors.border_soft))
        .rounded(px(theme::SIDEBAR_SCRIPTS_EMPTY_RADIUS))
        .flex()
        .flex_col()
        .items_center()
        .text_center()
        .gap(theme::SIDEBAR_SCRIPTS_EMPTY_GAP)
        .child(
            div()
                .text_size(px(theme::SIDEBAR_SCRIPTS_EMPTY_TITLE_TEXT_SIZE))
                .text_color(rgb(colors.text_secondary))
                .child("No saved scripts yet"),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .justify_center()
                .gap(theme::SIDEBAR_SCRIPTS_EMPTY_KBD_GAP)
                .text_size(px(theme::SIDEBAR_SCRIPTS_EMPTY_DETAIL_TEXT_SIZE))
                .text_color(rgb(colors.text_tertiary))
                .child(
                    div()
                        .font_family(&data_font)
                        .text_size(px(theme::SIDEBAR_SCRIPTS_EMPTY_KBD_TEXT_SIZE))
                        .text_color(rgb(colors.text_secondary))
                        .border_1()
                        .border_color(rgb(colors.border_soft))
                        .rounded(px(theme::SIDEBAR_SCRIPTS_EMPTY_KBD_RADIUS))
                        .px(theme::SIDEBAR_SCRIPTS_EMPTY_KBD_PADDING_X)
                        .child(theme::save_shortcut_label()),
                )
                .child("names the open tab and keeps it here."),
        )
}

/// One script/library row
fn render_script_row(index: usize, row: &ScriptRow, cx: &Context<SidebarView>) -> Stateful<Div> {
    let active_theme = cx.theme();
    let target = row.target.clone();
    let mut shell = row_shell(theme::SIDEBAR_SCRIPT_ROW_INDENT, active_theme)
        .id(("sidebar-script-row", index))
        .cursor_pointer()
        .hover(|this| this.bg(rgb(active_theme.colors.bg_raised)))
        .on_click(cx.listener(move |view, _event: &ClickEvent, window, cx| {
            view.open_script_row(target.clone(), window, cx);
        }))
        .child(
            div()
                .flex_shrink_0()
                .text_color(rgb(active_theme.colors.text_tertiary))
                .child(SCRIPT_ROW_GLYPH),
        );

    // The label group is the greedy flex item, but the label itself is not,
    // so the open-dot hugs the file name instead of being pushed to the far
    // right beside the time meta.
    let mut label_group = div()
        .flex()
        .flex_row()
        .items_center()
        .flex_1()
        .min_w_0()
        .child(div().min_w_0().truncate().child(row.label.clone()));
    if library_row_is_open(row) {
        label_group = label_group.child(
            div()
                .flex_shrink_0()
                .ml(theme::SIDEBAR_LIBRARY_OPEN_DOT_GAP)
                .w(theme::SIDEBAR_LIBRARY_OPEN_DOT_SIZE)
                .h(theme::SIDEBAR_LIBRARY_OPEN_DOT_SIZE)
                .rounded_full()
                .bg(rgb(active_theme.colors.accent)),
        );
    }
    shell = shell
        .child(label_group)
        .child(row_meta(row.relative_time.clone(), active_theme));

    if row.selected {
        shell = shell
            .bg(theme::sidebar_selected_bg(active_theme))
            .border_l_2()
            .border_color(rgb(active_theme.colors.accent));
    }
    shell
}
