//! The sidebar's pane switcher: the "SCHEMA"/"SCRIPTS" tabs that occupy the
//! sidebar's header row and select which full-height pane renders below it.

use gpui::{ClickEvent, Context, Div, Stateful, Window, div, prelude::*, px, rgb};
use zsql_ui::icon::IconName;
use zsql_ui::icon_button::icon_button_secondary;
use zsql_ui::tabs::active_underline_solid;
use zsql_ui::theme::{ActiveTheme, Theme};

use super::SidebarView;
use super::model::{SidebarPane, scripts_count};
use crate::ui::theme;

/// The pane-switcher header row: the "SCHEMA"/"SCRIPTS" tabs, plus the
/// refresh button while the schema pane is active.
pub(super) fn render_pane_tabs(
    view: &SidebarView,
    window: &mut Window,
    cx: &mut Context<SidebarView>,
) -> Div {
    let active_theme = cx.theme();
    let active_pane = view.active_pane;
    let script_count = scripts_count(&view.script_rows);

    div()
        .flex()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .h(theme::SIDEBAR_HEADER_HEIGHT)
        .border_b_1()
        .border_color(rgb(active_theme.colors.border_soft))
        .child(render_pane_tab(
            "sidebar-pane-tab-schema",
            "SCHEMA",
            None,
            active_pane == SidebarPane::Schema,
            SidebarPane::Schema,
            active_theme,
            cx,
        ))
        .child(render_pane_tab(
            "sidebar-pane-tab-scripts",
            "SCRIPTS",
            Some(script_count),
            active_pane == SidebarPane::Scripts,
            SidebarPane::Scripts,
            active_theme,
            cx,
        ))
        .child(
            div()
                .flex_1()
                .h_full()
                .flex()
                .flex_row()
                .items_center()
                .justify_end()
                .pr(theme::SIDEBAR_PANE_TAB_PADDING_X)
                .when(active_pane == SidebarPane::Schema, |tail| {
                    tail.child(
                        icon_button_secondary(
                            "sidebar-refresh-schema",
                            window,
                            cx,
                            IconName::Refresh,
                        )
                        .on_click(cx.listener(|view, _evt, _window, cx| view.refresh_schema(cx))),
                    )
                }),
        )
}

#[allow(clippy::too_many_arguments)]
fn render_pane_tab(
    id: &'static str,
    label: &'static str,
    count: Option<usize>,
    active: bool,
    pane: SidebarPane,
    theme: &Theme,
    cx: &Context<SidebarView>,
) -> Stateful<Div> {
    let mut tab = div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .h_full()
        .gap(theme::SIDEBAR_PANE_TAB_GAP)
        .px(theme::SIDEBAR_PANE_TAB_PADDING_X)
        .cursor_pointer()
        .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(if active {
            theme.colors.text_primary
        } else {
            theme.colors.text_tertiary
        }))
        .on_click(cx.listener(move |view, _evt: &ClickEvent, _window, cx| {
            view.switch_pane(pane, cx);
        }))
        .child(label);

    if let Some(count) = count {
        tab = tab.child(
            div()
                .font_family(&theme.fonts.data)
                .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
                .text_color(rgb(theme.colors.text_tertiary))
                .child(count.to_string()),
        );
    }
    if active {
        tab = tab.child(active_underline_solid(theme));
    }
    tab
}
