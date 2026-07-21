//! The connection footer: a lower-left, status-bar-height row shown
//! directly below the schema sidebar's tree that names the currently active
//! connection (or invites connecting one). Clicking it opens the
//! [`ConnectionManagerView`] modal.

use gpui::{ClickEvent, Context, Entity, Render, Window, div, prelude::*, px, rgb};
use zsql_ui::grid;
use zsql_ui::theme::ActiveTheme;

use super::connections::{ConnectionManagerView, FooterDisplay, footer_display};
use super::theme;
use crate::session::Session;

/// The connection footer view.
pub struct ConnectionFooterView {
    session: Entity<Session>,
    connections: Entity<ConnectionManagerView>,
}

impl ConnectionFooterView {
    /// Build a footer over `session` and the `connections` modal it opens.
    #[must_use]
    pub fn new(
        session: Entity<Session>,
        connections: Entity<ConnectionManagerView>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&session, |_view: &mut Self, _session, cx| cx.notify())
            .detach();
        cx.observe(&connections, |_view: &mut Self, _connections, cx| {
            cx.notify();
        })
        .detach();

        Self {
            session,
            connections,
        }
    }

    /// Open the connection-manager modal and focus it, so `Escape` closes it
    /// immediately without an extra click.
    fn open_modal(&mut self, _event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let focus_handle = self.connections.read(cx).modal_focus_handle();
        self.connections.update(cx, ConnectionManagerView::open);
        window.focus(&focus_handle);
    }
}

impl Render for ConnectionFooterView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let display = footer_display(
            self.session.read(cx).is_connected(),
            self.connections.read(cx).active(),
        );
        let colors = cx.theme().colors;

        let row = div()
            .id("connection-footer")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .flex_shrink_0()
            .w_full()
            .h(theme::STATUS_BAR_HEIGHT)
            .px_3()
            .border_t_1()
            .border_color(rgb(colors.border))
            .cursor_pointer()
            .hover(|el| el.bg(rgb(colors.bg_raised)))
            .font_family(&cx.theme().fonts.data)
            .text_size(px(theme::STATUS_BAR_TEXT_SIZE))
            .on_click(cx.listener(Self::open_modal));

        match display {
            FooterDisplay::Connected { name, host } => row
                .child(grid::status_dot(colors.accent))
                .child(
                    div()
                        .flex_shrink_0()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(colors.text_primary))
                        .child(name),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(colors.text_tertiary))
                        .child(host),
                ),
            FooterDisplay::Disconnected => row
                .child(grid::status_dot_outline(colors.text_tertiary))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(rgb(colors.text_secondary))
                        .child("Not connected"),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(rgb(colors.accent))
                        .child(". click to connect"),
                ),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};

    use super::ConnectionFooterView;
    use crate::connections::ConnectionStore;
    use crate::session::Session;
    use crate::ui::connections::ConnectionManagerView;

    fn empty_store_for_test(label: &str) -> ConnectionStore {
        let path = std::env::temp_dir().join(format!(
            "zsql-footer-render-test-{label}-{}.toml",
            std::process::id()
        ));
        ConnectionStore::load(&path).expect("loading a nonexistent path must succeed empty")
    }

    #[gpui::test]
    fn renders_without_panicking_when_connected_and_when_disconnected(cx: &mut TestAppContext) {
        let session = cx.new(|_cx| Session::new(&crate::config::Config::default()));
        let session_for_connections = session.clone();
        let (_footer, vcx) = cx.add_window_view(|_window, cx| {
            let connections = cx.new(|cx| {
                ConnectionManagerView::new(
                    session_for_connections,
                    empty_store_for_test("render"),
                    cx,
                )
            });
            ConnectionFooterView::new(session, connections, cx)
        });
        vcx.run_until_parked();
    }
}
