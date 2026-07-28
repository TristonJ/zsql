//! The workspace tab bar's render logic: one entry per open tab, each styled
//! per its kind, plus the trailing "+" affordance that opens a new script
//! tab.

use gpui::{ClickEvent, Context, IntoElement, div, prelude::*, px, rgb};
use zsql_ui::icon::{IconName, icon};
use zsql_ui::theme::{ActiveTheme, Theme};

use super::tabs::{Tab, TabId, TabKind};
use super::theme as workspace_theme;
use super::workspace::WorkspaceView;

/// The tab bar: one entry per open tab, in order, plus the trailing "+"
/// affordance that opens a new script tab.
#[must_use]
pub fn render_tab_bar(
    active_id: Option<TabId>,
    tabs: &[Tab],
    cx: &Context<WorkspaceView>,
) -> impl IntoElement {
    let theme = cx.theme();
    let mut bar = zsql_ui::tabs::tab_bar_shell(theme);
    for tab in tabs {
        let active = active_id == Some(tab.id());
        bar = bar.child(render_tab(tab, active, theme, cx));
    }
    bar.child(
        zsql_ui::tabs::new_tab_glyph(theme)
            .id("workspace-new-tab")
            .cursor_pointer()
            .on_click(cx.listener(|view, _event: &ClickEvent, window, cx| {
                view.open_new_script_tab(window, cx);
            })),
    )
}

/// One tab-bar entry for `tab`, marked active when `active` and closable.
fn render_tab(
    tab: &Tab,
    active: bool,
    theme: &Theme,
    cx: &Context<WorkspaceView>,
) -> impl IntoElement {
    let id = tab.id();
    let mut shell = zsql_ui::tabs::tab_shell(active, theme).id(("workspace-tab", id));

    shell = match tab.kind() {
        TabKind::Generated { .. } => {
            shell = shell
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(workspace_theme::TAB_ICON_TEXT_SIZE))
                        .text_color(rgb(theme.colors.accent))
                        .child("#"),
                )
                .child(div().italic().child(tab.title().to_owned()));
            if active {
                shell = shell.child(zsql_ui::tabs::active_underline_solid(theme));
            }
            shell
        }
        TabKind::Script => {
            let mut label = tab.title().to_owned();
            if tab.dirty() {
                label.push('*');
            }
            shell = shell.child(div().child(label));
            if active {
                shell = shell.child(zsql_ui::tabs::active_underline_solid(theme));
            }
            shell
        }
        TabKind::Schema { .. } => {
            shell = shell
                .child(icon(
                    IconName::Table,
                    px(workspace_theme::TAB_ICON_TEXT_SIZE),
                    theme.colors.accent,
                ))
                .child(div().child(tab.title().to_owned()));
            if active {
                shell = shell.child(zsql_ui::tabs::active_underline_solid(theme));
            }
            shell
        }
    };

    shell
        .cursor_pointer()
        .on_click(cx.listener(move |view, _event: &ClickEvent, window, cx| {
            view.activate_tab(id, window, cx);
        }))
        .child(
            zsql_ui::tabs::close_glyph(format!("close-icon-{id}"), theme)
                .id(("workspace-tab-close", id))
                .cursor_pointer()
                .on_click(cx.listener(move |view, _event: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    view.close_tab(id, window, cx);
                })),
        )
}
