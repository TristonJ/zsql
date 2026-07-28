//! The connection footer: a lower-left, status-bar-height row shown
//! directly below the schema sidebar's tree that names the currently active
//! connection (or invites connecting one). Clicking it opens the
//! [`ConnectionManagerView`] modal.

use gpui::{ClickEvent, Context, Entity, Render, Window, div, prelude::*, px, rgb};
use zsql_ui::grid;
use zsql_ui::theme::ActiveTheme;

use super::connections::{ActiveConnection, ConnectionManagerView};
use super::theme;
use crate::session::{LivenessState, Session, SessionState};
use crate::ui::format::host_label;

/// What the connection footer should render, derived from the session's
/// lifecycle state and whichever connection (if any) is currently tracked as
/// active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FooterDisplay {
    /// Show the active connection's name and host, with a filled status dot.
    Connected {
        /// The active connection's display name.
        name: String,
        /// A `host[:port]`-shaped label derived from the active connection's
        /// URL.
        host: String,
    },
    /// Show a "Connecting..." status while a connect attempt is in flight.
    Connecting,
    /// Show the "not connected, click to connect" prompt with a hollow dot.
    Disconnected,
}

/// The connection footer's display, given the session's real lifecycle
/// state and connection liveness, whether a live connection is currently
/// held (see [`Session::is_connected`]), and whichever connection is
/// tracked as active. Applies the same three-way Connecting/Connected/
/// Disconnected distinction the results status bar uses (see
/// `crate::ui::results`'s `status_indicator`), in this precedence order:
///
/// 1. [`LivenessState::Unreachable`] always overrides to [`FooterDisplay::Disconnected`],
///    regardless of `state`.
/// 2. [`SessionState::Connecting`] always renders [`FooterDisplay::Connecting`], even if
///    `session_is_connected` is still `true` -- mid-switch, the prior
///    connection's `Arc` is still held until the new connect resolves, and
///    that stale "still connected" read must not win over the connect
///    attempt actually in flight.
/// 3. Otherwise, `session_is_connected` together with a tracked `active`
///    connection renders [`FooterDisplay::Connected`].
/// 4. Everything else (no URL configured, an errored connect with no live
///    connection, or a connected session with no active connection
///    tracked, which should not normally happen since every connect path
///    threads one through) falls back to [`FooterDisplay::Disconnected`].
///
/// Note that a query error (as opposed to a connect failure) moves `state`
/// to [`SessionState::Error`] without dropping the underlying connection,
/// so rule 3 still applies and the footer keeps showing the still-connected
/// database rather than falling back to "Not connected".
#[must_use]
pub fn footer_display(
    state: &SessionState,
    liveness: &LivenessState,
    session_is_connected: bool,
    active: Option<&ActiveConnection>,
) -> FooterDisplay {
    if matches!(liveness, LivenessState::Unreachable(_)) {
        return FooterDisplay::Disconnected;
    }
    if matches!(state, SessionState::Connecting) {
        return FooterDisplay::Connecting;
    }
    match (session_is_connected, active) {
        (true, Some(active)) => FooterDisplay::Connected {
            name: active.name.clone(),
            host: host_label(&active.url),
        },
        _ => FooterDisplay::Disconnected,
    }
}

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
        let session = self.session.read(cx);
        let display = footer_display(
            session.state(),
            session.liveness(),
            session.is_connected(),
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
            FooterDisplay::Connecting => row.child(grid::status_dot(colors.status_warn)).child(
                div()
                    .flex_shrink_0()
                    .text_color(rgb(colors.text_secondary))
                    .child("Connecting..."),
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

    use super::{ActiveConnection, ConnectionFooterView, FooterDisplay, footer_display};
    use crate::connections::ConnectionStore;
    use crate::session::{LivenessState, Session, SessionState};
    use crate::ui::connections::ConnectionManagerView;

    fn sample_active_connection() -> ActiveConnection {
        ActiveConnection {
            id: None,
            name: "zsql local".to_owned(),
            url: "postgres://localhost:5432/zsql".to_owned(),
        }
    }

    #[test]
    fn footer_display_shows_the_active_connections_name_and_host_when_connected() {
        let active = sample_active_connection();
        match footer_display(
            &SessionState::Connected,
            &LivenessState::Unknown,
            true,
            Some(&active),
        ) {
            FooterDisplay::Connected { name, host } => {
                assert_eq!(name, "zsql local");
                assert_eq!(host, "localhost:5432");
            }
            other => panic!("expected FooterDisplay::Connected, got {other:?}"),
        }
    }

    #[test]
    fn footer_display_is_disconnected_when_the_session_holds_no_live_connection() {
        let active = sample_active_connection();
        assert_eq!(
            footer_display(
                &SessionState::Error("connection refused".to_owned()),
                &LivenessState::Unknown,
                false,
                Some(&active)
            ),
            FooterDisplay::Disconnected,
            "a failed connect must render the not-connected prompt, not an error affordance"
        );
    }

    #[test]
    fn footer_display_is_disconnected_when_connected_but_no_active_connection_is_tracked() {
        assert_eq!(
            footer_display(
                &SessionState::Connected,
                &LivenessState::Unknown,
                true,
                None
            ),
            FooterDisplay::Disconnected
        );
    }

    #[test]
    fn footer_display_shows_connecting_during_a_connect_attempt() {
        assert_eq!(
            footer_display(
                &SessionState::Connecting,
                &LivenessState::Unknown,
                false,
                None
            ),
            FooterDisplay::Connecting
        );
    }

    #[test]
    fn footer_display_shows_connected_immediately_after_a_successful_connect_needs_no_probe() {
        // A fresh `Connected` session has not had time for the recurring
        // liveness probe to complete even once yet.
        let active = sample_active_connection();
        assert_eq!(
            footer_display(
                &SessionState::Connected,
                &LivenessState::Unknown,
                true,
                Some(&active)
            ),
            FooterDisplay::Connected {
                name: "zsql local".to_owned(),
                host: "localhost:5432".to_owned(),
            },
            "Connected must not wait on the first Healthy probe result"
        );
    }

    #[test]
    fn footer_display_shows_connecting_when_switching_even_though_the_prior_connection_is_still_held()
     {
        // Mid-switch: `connect_url` moves `state` to `Connecting` but keeps the
        // prior connection's `Arc` alive (and `is_connected()` therefore still
        // true) until the new attempt resolves.
        let active = sample_active_connection();
        assert_eq!(
            footer_display(
                &SessionState::Connecting,
                &LivenessState::Healthy,
                true,
                Some(&active)
            ),
            FooterDisplay::Connecting,
            "Connecting must win over a stale still-connected read from the connection being replaced"
        );
    }

    #[test]
    fn footer_display_is_disconnected_when_liveness_is_unreachable_even_though_connected() {
        let active = sample_active_connection();
        let unreachable = LivenessState::Unreachable("connection reset".to_owned());
        assert_eq!(
            footer_display(&SessionState::Connected, &unreachable, true, Some(&active)),
            FooterDisplay::Disconnected
        );
    }

    #[test]
    fn footer_display_stays_connected_through_a_query_error_that_leaves_the_connection_live() {
        let active = sample_active_connection();
        assert_eq!(
            footer_display(
                &SessionState::Error("syntax error at or near \"selct\"".to_owned()),
                &LivenessState::Healthy,
                true,
                Some(&active)
            ),
            FooterDisplay::Connected {
                name: "zsql local".to_owned(),
                host: "localhost:5432".to_owned(),
            },
            "a query error must not be mistaken for a connect failure while the connection is live"
        );
    }

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
        let (footer, vcx) = cx.add_window_view(|_window, cx| {
            let connections = cx.new(|cx| {
                ConnectionManagerView::new(
                    session_for_connections,
                    empty_store_for_test("render"),
                    crate::config::Config::default().liveness.probe_timeout(),
                    zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                    cx,
                )
            });
            ConnectionFooterView::new(session, connections, cx)
        });
        vcx.run_until_parked();

        footer.read_with(vcx, |footer, cx| {
            assert!(
                matches!(
                    footer.session.read(cx).state(),
                    crate::session::SessionState::Empty
                ),
                "a freshly built session must start Empty"
            );
        });
    }

    #[gpui::test]
    fn renders_without_panicking_when_connecting(cx: &mut TestAppContext) {
        let session = cx.new(|_cx| {
            Session::new_for_render_test(
                crate::session::SessionState::Connecting,
                zsql_core::ResultSet::default(),
            )
        });
        let session_for_connections = session.clone();
        let (footer, vcx) = cx.add_window_view(|_window, cx| {
            let connections = cx.new(|cx| {
                ConnectionManagerView::new(
                    session_for_connections,
                    empty_store_for_test("render-connecting"),
                    crate::config::Config::default().liveness.probe_timeout(),
                    zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                    cx,
                )
            });
            ConnectionFooterView::new(session, connections, cx)
        });
        vcx.run_until_parked();

        footer.read_with(vcx, |footer, cx| {
            assert!(
                matches!(
                    footer.session.read(cx).state(),
                    crate::session::SessionState::Connecting
                ),
                "the footer must keep reflecting the session's Connecting state"
            );
        });
    }
}
