//! The sidebar's database row: a full-width row directly under the pane
//! tabs, shown only in the schema pane when the active connection reports
//! more than one selectable database. Opens
//! [`SidebarView::render_db_switcher_menu`] on click.

use gpui::{ClickEvent, Context, Div, div, prelude::*, px, rgb};
use zsql_ui::icon::{IconName, icon};
use zsql_ui::theme::ActiveTheme;

use super::SidebarView;
use super::model::db_row_visible;
use crate::session::SessionState;
use crate::ui::theme;

/// The database row, or `None` when [`db_row_visible`] says it should not
/// render at all (zero height, not merely hidden).
pub(super) fn render_db_row(view: &SidebarView, cx: &Context<SidebarView>) -> Option<Div> {
    let session = view.session.read(cx);
    if !db_row_visible(view.active_pane, session.available_databases().len()) {
        return None;
    }
    let active_theme = cx.theme();
    let current_text = db_row_current_text(session.state(), session.current_database());

    Some(
        div()
            .relative()
            .flex_shrink_0()
            .child(
                div()
                    .id("sidebar-db-row")
                    .flex()
                    .flex_row()
                    .items_center()
                    .w_full()
                    .gap(theme::SIDEBAR_DB_ROW_GAP)
                    .h(theme::SIDEBAR_DB_ROW_HEIGHT)
                    .px(theme::SIDEBAR_DB_ROW_PADDING_X)
                    .border_b_1()
                    .border_color(rgb(active_theme.colors.border_soft))
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(active_theme.colors.bg_raised)))
                    .on_click(cx.listener(|view, _evt: &ClickEvent, _window, cx| {
                        view.toggle_db_switcher(cx);
                    }))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(theme::SIDEBAR_DB_ROW_EYEBROW_TEXT_SIZE))
                            .text_color(rgb(active_theme.colors.text_tertiary))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("DB"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_x_hidden()
                            .text_ellipsis()
                            .text_size(px(theme::SIDEBAR_DB_ROW_NAME_TEXT_SIZE))
                            .font_family(&active_theme.fonts.data)
                            .text_color(rgb(active_theme.colors.text_secondary))
                            .child(current_text),
                    )
                    .child(icon(
                        IconName::ChevronDown,
                        theme::SIDEBAR_DB_ROW_CHEVRON_ICON_SIZE,
                        active_theme.colors.text_tertiary,
                    )),
            )
            .children(view.render_db_switcher_menu(cx)),
    )
}

/// The database row's current-database label
fn db_row_current_text(state: &SessionState, current_database: Option<&str>) -> String {
    if *state == SessionState::Connecting {
        "Connecting...".to_owned()
    } else {
        current_database.unwrap_or("").to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::db_row_current_text;
    use crate::session::SessionState;

    #[test]
    fn connecting_shows_the_placeholder_regardless_of_current_database() {
        assert_eq!(
            db_row_current_text(&SessionState::Connecting, Some("alpha")),
            "Connecting...",
            "a database switch in flight must show the placeholder, not the stale name"
        );
        assert_eq!(
            db_row_current_text(&SessionState::Connecting, None),
            "Connecting..."
        );
    }

    #[test]
    fn a_settled_state_shows_the_current_database_name() {
        assert_eq!(
            db_row_current_text(&SessionState::Connected, Some("alpha")),
            "alpha"
        );
    }

    #[test]
    fn a_settled_state_with_no_known_database_shows_an_empty_string() {
        assert_eq!(db_row_current_text(&SessionState::Connected, None), "");
    }
}
