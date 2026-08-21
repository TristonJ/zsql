//! The connection footer: a lower-left, status-bar-height row shown
//! directly below the schema sidebar's tree that names the currently active
//! connection (or invites connecting one). Clicking it opens the
//! [`ConnectionManagerView`] modal.

use gpui::{
    App, ClickEvent, Context, Entity, FontWeight, Render, SharedString, Window, div, prelude::*,
    px, rgb,
};
use zsql_core::{RowCount, group_thousands};
use zsql_ui::grid::{self, status_dot_outline};
use zsql_ui::theme::{ActiveTheme, Theme};

use super::connections::{ActiveConnection, ConnectionManagerView};
use super::theme;
use crate::session::{LivenessState, Session, SessionState};
use crate::ui::appearance::AppearanceModalView;
use crate::ui::format::host_label;
use crate::ui::tabs::ResultsSnapshot;

mod appearance_trigger;

/// Element id and debug selector for the status bar's version label, so
/// tests can locate its painted bounds.
const CONNECTION_FOOTER_VERSION_ID: &str = "connection-footer-version";

/// Element id and debug selector for the transient save confirmation, so
/// tests can locate its painted bounds.
const CONNECTION_FOOTER_SAVE_CONFIRMATION_ID: &str = "connection-footer-save-confirmation";

/// The status bar's version label text, compiled from the zsql crate's own
/// `Cargo.toml` version.
const FOOTER_VERSION_TEXT: &str = concat!("v", env!("CARGO_PKG_VERSION"));

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
    /// The Appearance modal the status bar's theme trigger opens
    appearance_modal: Entity<AppearanceModalView>,
    /// Force a certain result rather than reading from the current session
    result_snapshot: Option<ResultsSnapshot>,
    /// The row count to display in the footer
    row_count: Option<RowCount>,
    /// The transient save confirmation or error shown after an explicit or
    /// automatic script save, if one is currently visible
    save_confirmation: Option<SaveStatus>,
    /// The detected parameter count to show in place of the normal query
    /// message, while the "Run with parameters" modal is open for the
    /// active tab's run.
    waiting_params_count: Option<usize>,
}

/// The footer's transient post-save message
#[derive(Debug, Clone, PartialEq, Eq)]
enum SaveStatus {
    Saved(SharedString),
    Error(SharedString),
}

impl ConnectionFooterView {
    /// Build a footer over `session` and the `connections` modal it opens.
    #[must_use]
    pub fn new(
        session: Entity<Session>,
        connections: Entity<ConnectionManagerView>,
        appearance_modal: Entity<AppearanceModalView>,
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
            appearance_modal,
            result_snapshot: None,
            row_count: None,
            save_confirmation: None,
            waiting_params_count: None,
        }
    }

    /// Show (or, with `None`, clear) the detected parameter count while the
    /// "Run with parameters" modal is open for the active tab's run.
    pub fn set_waiting_params_count(&mut self, count: Option<usize>, cx: &mut Context<Self>) {
        self.waiting_params_count = count;
        cx.notify();
    }

    /// Show the transient "saved <file>" confirmation, replacing any
    /// currently shown. The caller is responsible for clearing it again
    /// (see [`Self::clear_saved_confirmation`]) after its own timer.
    pub fn show_saved_confirmation(&mut self, file_name: &str, cx: &mut Context<Self>) {
        self.save_confirmation = Some(SaveStatus::Saved(SharedString::from(format!(
            "saved {file_name}"
        ))));
        cx.notify();
    }

    /// Show a transient save-failure message, replacing any currently
    /// shown. The caller is responsible for clearing it again (see
    /// [`Self::clear_saved_confirmation`]) after its own timer
    pub fn show_save_error(&mut self, reason: &str, cx: &mut Context<Self>) {
        self.save_confirmation = Some(SaveStatus::Error(SharedString::from(reason.to_owned())));
        cx.notify();
    }

    /// Clear the transient save confirmation or error, if one is showing. A
    /// no-op otherwise
    pub fn clear_saved_confirmation(&mut self, cx: &mut Context<Self>) {
        if self.save_confirmation.take().is_some() {
            cx.notify();
        }
    }

    /// The save confirmation text currently shown, if any (success or
    /// error, indistinguishable to this accessor). Test-only: the render
    /// path reads `self.save_confirmation` directly.
    #[cfg(test)]
    pub(crate) fn save_confirmation_for_test(&self) -> Option<String> {
        match self.save_confirmation.as_ref()? {
            SaveStatus::Saved(text) | SaveStatus::Error(text) => Some(text.to_string()),
        }
    }

    /// Whether the currently shown transient message (if any) is an error
    /// rather than a success. Test-only.
    #[cfg(test)]
    pub(crate) fn save_error_showing_for_test(&self) -> bool {
        matches!(self.save_confirmation, Some(SaveStatus::Error(_)))
    }

    #[cfg(test)]
    pub(crate) fn waiting_params_count_for_test(&self) -> Option<usize> {
        self.waiting_params_count
    }

    /// Force the footer to render a certain result rather than reading from the
    /// current session. `None` to stop forcing and return to the session's
    /// current state.
    pub fn set_result_snapshot(
        &mut self,
        snapshot: Option<ResultsSnapshot>,
        cx: &mut Context<Self>,
    ) {
        self.result_snapshot = snapshot;
        cx.notify();
    }

    /// Set the row count to display, if any.
    pub fn set_row_count(&mut self, row_count: Option<RowCount>, cx: &mut Context<Self>) {
        self.row_count = row_count;
        cx.notify();
    }

    /// Open the connection-manager modal and focus it, so `Escape` closes it
    /// immediately without an extra click.
    fn open_modal(&mut self, _event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let focus_handle = self.connections.read(cx).modal_focus_handle();
        self.connections.update(cx, ConnectionManagerView::open);
        window.focus(&focus_handle);
    }

    /// The current state or the frozen state from `result_snapshot`, if any
    fn effective_state<'a>(&'a self, cx: &'a Context<Self>) -> &'a SessionState {
        match &self.result_snapshot {
            Some(snapshot) => &snapshot.state,
            None => self.session.read(cx).state(),
        }
    }

    /// The current result or the frozen result from `result_snapshot`, if any
    fn effective_result<'a>(&'a self, cx: &'a Context<Self>) -> &'a zsql_core::ResultSet {
        match &self.result_snapshot {
            Some(snapshot) => &snapshot.result,
            None => self.session.read(cx).result(),
        }
    }

    fn render_connection_status(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let session = self.session.read(cx);
        let display = footer_display(
            session.state(),
            session.liveness(),
            session.is_connected(),
            self.connections.read(cx).active(),
        );
        let dot_color = status_dot_color(session.state(), session.liveness(), cx.theme());
        let colors = cx.theme().colors;

        let mut row = div()
            .id("connection-footer-status")
            .flex()
            .flex_row()
            .items_center()
            .h_full()
            .max_w(theme::STATUS_BAR_CONNECTION_MAX_WIDTH)
            .gap_2()
            .px_3()
            .cursor_pointer()
            .hover(|el| el.bg(rgb(colors.bg_raised)))
            .font_family(&cx.theme().fonts.data)
            .text_size(px(theme::STATUS_BAR_TEXT_SIZE))
            .on_click(cx.listener(Self::open_modal));

        row = match display {
            FooterDisplay::Disconnected => row.child(status_dot_outline(dot_color)),
            _ => row.child(grid::status_dot(dot_color)),
        };

        match display {
            FooterDisplay::Connected { name, host } => row
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
            FooterDisplay::Connecting => row.child(
                div()
                    .flex_shrink_0()
                    .text_color(rgb(colors.text_secondary))
                    .child("Connecting..."),
            ),
            FooterDisplay::Disconnected => row
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
                        .child("\u{2022} click to connect"),
                ),
        }
    }

    /// The transient "saved <file>" confirmation with the accent check
    /// glyph, shown between the query result summary and the appearance
    /// trigger. `None` while nothing was just saved
    fn render_save_confirmation(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        let status = self.save_confirmation.clone()?;
        let colors = cx.theme().colors;
        let (glyph, text, color) = match status {
            SaveStatus::Saved(text) => ("\u{2713}", text, colors.accent),
            SaveStatus::Error(text) => ("\u{2717}", text, colors.status_error),
        };
        Some(
            div()
                .id(CONNECTION_FOOTER_SAVE_CONFIRMATION_ID)
                .debug_selector(|| CONNECTION_FOOTER_SAVE_CONFIRMATION_ID.to_owned())
                .flex()
                .flex_row()
                .flex_shrink_0()
                .items_center()
                .gap_1()
                .px_3()
                .font_family(&cx.theme().fonts.data)
                .text_size(px(theme::STATUS_BAR_TEXT_SIZE))
                .text_color(rgb(color))
                .child(glyph)
                .child(text),
        )
    }

    fn render_appearance_trigger(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = cx.theme().colors;

        let active_display_name = self.appearance_modal.read(cx).active_theme_display_name();

        appearance_trigger::render_theme_trigger(
            self.appearance_modal.clone(),
            colors,
            active_display_name.into(),
            cx,
        )
    }

    fn render_current_query_result(
        &self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut view = div()
            .flex()
            .flex_row()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_4()
            .font_family(&cx.theme().fonts.data)
            .text_size(px(theme::STATUS_BAR_TEXT_SIZE))
            .text_color(rgb(cx.theme().colors.text_secondary));
        if let Some(count) = self.waiting_params_count {
            return view.child(format!(
                "{count} parameter{}",
                if count == 1 { "" } else { "s" }
            ));
        }
        let state = self.effective_state(cx);
        let result = self.effective_result(cx);
        let error_message = match state {
            SessionState::Error(message) => Some(message.clone()),
            _ => None,
        };

        if let Some(message) = error_message {
            view = view.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(cx.theme().colors.status_error))
                    .child(message),
            );
            return view;
        }

        let mut status_area = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_4()
            .w(theme::STATUS_BAR_QUERY_MESSAGE_WIDTH);
        for message in query_messages(state, result.rows.len(), "rows") {
            status_area = status_area.child(message);
        }
        view = view.child(status_area);

        if let Some(total_row_count_text) = format_total_row_count(self.row_count) {
            view = view.child(total_row_count_text);
        }

        view
    }
}

impl Render for ConnectionFooterView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("connection-footer")
            .flex()
            .flex_row()
            .w_full()
            .h(theme::STATUS_BAR_HEIGHT)
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(rgb(cx.theme().colors.border))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .h_full()
                    .child(self.render_connection_status(window, cx))
                    .child(self.render_current_query_result(window, cx)),
            )
            .children(self.render_save_confirmation(cx))
            .child(render_version(cx))
            .child(self.render_appearance_trigger(window, cx))
    }
}

/// The status bar's crate version label. Non-interactive.
fn render_version(cx: &App) -> impl IntoElement {
    div()
        .id(CONNECTION_FOOTER_VERSION_ID)
        .debug_selector(|| CONNECTION_FOOTER_VERSION_ID.to_owned())
        .flex_shrink_0()
        .px_3()
        .font_family(&cx.theme().fonts.data)
        .text_size(px(theme::STATUS_BAR_TEXT_SIZE))
        .text_color(rgb(cx.theme().colors.text_tertiary))
        .child(FOOTER_VERSION_TEXT)
}

fn status_dot_color(state: &SessionState, liveness: &LivenessState, active_theme: &Theme) -> u32 {
    let colors = active_theme.colors;
    if matches!(liveness, LivenessState::Unreachable(_)) {
        return theme::status_disconnected(active_theme);
    }
    match state {
        SessionState::Empty => colors.text_tertiary,
        SessionState::Connecting => colors.status_warn,
        SessionState::Connected | SessionState::Results(_) | SessionState::Running => colors.accent,
        SessionState::Truncating { .. } | SessionState::Truncated { .. } => colors.status_limited,
        SessionState::Error(_) => colors.status_error,
    }
}

/// The bottom status bar's "N <unit>" / "N ms" text for `state`, given
/// `count` and its `unit` word (`"rows"` for the grid, `"lines"` while the
/// Text view is active). `None` for any state with no completed query to
/// report timing/count for.
fn query_messages(state: &SessionState, count: usize, unit: &str) -> Vec<String> {
    match state {
        SessionState::Results(elapsed) => vec![
            format!("{count} {unit}"),
            format!("{} ms", elapsed.as_millis()),
        ],
        SessionState::Truncated { elapsed, rows } => vec![
            format!("Result limited to {count} {unit} ({rows} total)"),
            format!("{} ms", elapsed.as_millis()),
        ],
        SessionState::Running | SessionState::Truncating { .. } => {
            vec!["Running...".to_owned()]
        }
        _ => vec![],
    }
}

const ESTIMATED_ROW_COUNT_SUFFIX: &str = " (estimated)";
const TOTAL_ROW_COUNT_LABEL: &str = " total";

/// The previewed relation's total row count, for the status bar: e.g.
/// `"1,234 total"` for an exact count, or `"~1,234,567 total (estimated)"`
/// when the driver could only provide a planner estimate. `None` when no
/// count has been fetched (no preview yet, still fetching, or the fetch
/// failed), so the caller can omit the segment entirely.
fn format_total_row_count(row_count: Option<RowCount>) -> Option<String> {
    let row_count = row_count?;
    let grouped = group_thousands(row_count.value());
    Some(if row_count.is_estimated() {
        format!(
            "{}{grouped}{TOTAL_ROW_COUNT_LABEL}{ESTIMATED_ROW_COUNT_SUFFIX}",
            zsql_core::ESTIMATE_MARKER
        )
    } else {
        format!("{grouped}{TOTAL_ROW_COUNT_LABEL}")
    })
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};
    use zsql_core::{BatchSink, Connection, CoreError, FilterState, QueryHandle};

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

    /// A [`Connection`] double whose methods are never invoked: it exists
    /// only so a session can hold a live connection.
    struct NoopConnection;

    #[async_trait::async_trait]
    impl Connection for NoopConnection {
        fn stream_query(&self, _sql: String, _sink: BatchSink) -> QueryHandle {
            unimplemented!("not exercised by this test")
        }

        async fn introspect(&self) -> Result<zsql_core::SchemaTree, CoreError> {
            unimplemented!("not exercised by this test")
        }

        async fn ping(&self) -> Result<(), CoreError> {
            unimplemented!("not exercised by this test")
        }

        async fn count_rows(
            &self,
            _schema: &str,
            _relation: &str,
            _filters: &FilterState,
        ) -> Result<zsql_core::RowCount, CoreError> {
            unimplemented!("not exercised by this test")
        }

        async fn describe_relation(
            &self,
            _schema: &str,
            _relation: &str,
        ) -> Result<zsql_core::RelationSchema, CoreError> {
            unimplemented!("not exercised by this test")
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

    #[test]
    fn footer_version_text_matches_the_crate_version() {
        assert_eq!(
            super::FOOTER_VERSION_TEXT,
            format!("v{}", env!("CARGO_PKG_VERSION"))
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
    fn renders_the_version_label_when_disconnected(cx: &mut TestAppContext) {
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
            let appearance = cx.new(|cx| {
                crate::ui::appearance::AppearanceModalView::new(
                    "zsql-dark".to_owned(),
                    None,
                    None,
                    cx,
                )
            });
            ConnectionFooterView::new(session, connections, appearance, cx)
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

        assert!(
            vcx.debug_bounds(super::CONNECTION_FOOTER_VERSION_ID)
                .is_some(),
            "the version label must be painted while disconnected"
        );
    }

    #[gpui::test]
    fn renders_the_version_label_when_connecting(cx: &mut TestAppContext) {
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
            let appearance = cx.new(|cx| {
                crate::ui::appearance::AppearanceModalView::new(
                    "zsql-dark".to_owned(),
                    None,
                    None,
                    cx,
                )
            });
            ConnectionFooterView::new(session, connections, appearance, cx)
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

        assert!(
            vcx.debug_bounds(super::CONNECTION_FOOTER_VERSION_ID)
                .is_some(),
            "the version label must be painted while connecting"
        );
    }

    #[gpui::test]
    fn renders_the_version_label_when_connected(cx: &mut TestAppContext) {
        let session = cx
            .new(|_cx| Session::new_connected_for_render_test(std::sync::Arc::new(NoopConnection)));
        let session_for_connections = session.clone();

        let (footer, vcx) = cx.add_window_view(|_window, cx| {
            let connections = cx.new(|cx| {
                let mut connections = ConnectionManagerView::new(
                    session_for_connections,
                    empty_store_for_test("render-connected"),
                    crate::config::Config::default().liveness.probe_timeout(),
                    zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                    cx,
                );
                connections.set_active(Some(sample_active_connection()), cx);
                connections
            });
            let appearance = cx.new(|cx| {
                crate::ui::appearance::AppearanceModalView::new(
                    "zsql-dark".to_owned(),
                    None,
                    None,
                    cx,
                )
            });
            ConnectionFooterView::new(session, connections, appearance, cx)
        });
        vcx.run_until_parked();

        footer.read_with(vcx, |footer, cx| {
            assert!(
                footer.session.read(cx).is_connected(),
                "new_connected_for_render_test must leave the session connected"
            );
        });

        let version_bounds = vcx
            .debug_bounds(super::CONNECTION_FOOTER_VERSION_ID)
            .expect("the version label must be painted while connected");
        let trigger_bounds = vcx
            .debug_bounds(super::appearance_trigger::THEME_TRIGGER_ID)
            .expect("the theme trigger must be painted while connected");
        assert!(
            version_bounds.origin.x < trigger_bounds.origin.x,
            "the version label must sit to the left of the theme trigger"
        );
    }

    #[gpui::test]
    fn the_save_confirmation_paints_left_of_the_version_label_which_paints_left_of_the_theme_trigger(
        cx: &mut TestAppContext,
    ) {
        let session = cx.new(|_cx| Session::new(&crate::config::Config::default()));
        let session_for_connections = session.clone();
        let (footer, vcx) = cx.add_window_view(|_window, cx| {
            let connections = cx.new(|cx| {
                ConnectionManagerView::new(
                    session_for_connections,
                    empty_store_for_test("render-save-confirmation"),
                    crate::config::Config::default().liveness.probe_timeout(),
                    zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                    cx,
                )
            });
            let appearance = cx.new(|cx| {
                crate::ui::appearance::AppearanceModalView::new(
                    "zsql-dark".to_owned(),
                    None,
                    None,
                    cx,
                )
            });
            ConnectionFooterView::new(session, connections, appearance, cx)
        });
        vcx.run_until_parked();

        footer.update(vcx, |footer, cx| {
            footer.show_saved_confirmation("query.sql", cx);
        });
        vcx.run_until_parked();

        let save_bounds = vcx
            .debug_bounds(super::CONNECTION_FOOTER_SAVE_CONFIRMATION_ID)
            .expect("the save confirmation must be painted once shown");
        let version_bounds = vcx
            .debug_bounds(super::CONNECTION_FOOTER_VERSION_ID)
            .expect("the version label must still be painted alongside the save confirmation");
        let trigger_bounds = vcx
            .debug_bounds(super::appearance_trigger::THEME_TRIGGER_ID)
            .expect("the theme trigger must still be painted alongside the save confirmation");

        assert!(
            save_bounds.origin.x < version_bounds.origin.x,
            "the save confirmation must sit left of the version label"
        );
        assert!(
            version_bounds.origin.x < trigger_bounds.origin.x,
            "the version label must sit left of the theme trigger"
        );
    }

    #[gpui::test]
    fn the_version_label_and_theme_trigger_hold_position_when_the_left_cluster_overflows(
        cx: &mut TestAppContext,
    ) {
        let session = cx.new(|_cx| {
            Session::new_for_render_test(
                crate::session::SessionState::Error("x".repeat(500)),
                zsql_core::ResultSet::default(),
            )
        });
        let session_for_connections = session.clone();
        let (_footer, vcx) = cx.add_window_view(|_window, cx| {
            let connections = cx.new(|cx| {
                ConnectionManagerView::new(
                    session_for_connections,
                    empty_store_for_test("render-overflow"),
                    crate::config::Config::default().liveness.probe_timeout(),
                    zsql_core::DEFAULT_QUERY_BATCH_SIZE,
                    cx,
                )
            });
            let appearance = cx.new(|cx| {
                crate::ui::appearance::AppearanceModalView::new(
                    "zsql-dark".to_owned(),
                    None,
                    None,
                    cx,
                )
            });
            ConnectionFooterView::new(session, connections, appearance, cx)
        });
        vcx.run_until_parked();

        let viewport_width = vcx.update(|window, _cx| window.viewport_size().width);

        let version_bounds = vcx
            .debug_bounds(super::CONNECTION_FOOTER_VERSION_ID)
            .expect("the version label must be painted even when the left cluster overflows");
        let trigger_bounds = vcx
            .debug_bounds(super::appearance_trigger::THEME_TRIGGER_ID)
            .expect("the theme trigger must be painted even when the left cluster overflows");

        assert!(
            version_bounds.origin.x < trigger_bounds.origin.x,
            "the version label must stay left of the theme trigger under overflow"
        );
        assert!(
            trigger_bounds.origin.x + trigger_bounds.size.width <= viewport_width,
            "the theme trigger must stay within the window instead of being pushed offscreen"
        );
    }

    #[test]
    fn format_total_row_count_renders_nothing_when_absent() {
        assert_eq!(super::format_total_row_count(None), None);
    }

    #[test]
    fn format_total_row_count_renders_an_exact_count_with_thousands_separators() {
        assert_eq!(
            super::format_total_row_count(Some(zsql_core::RowCount::Exact(1_234))),
            Some("1,234 total".to_owned())
        );
    }

    #[test]
    fn format_total_row_count_renders_an_estimated_count_marked_distinctly() {
        assert_eq!(
            super::format_total_row_count(Some(zsql_core::RowCount::Estimated(1_234_567))),
            Some("~1,234,567 total (estimated)".to_owned())
        );
    }

    #[test]
    fn format_total_row_count_labels_the_total_distinctly_from_the_streamed_rows_metric() {
        // The streamed-rows metric reads "N rows"; the total must not, or the
        // two segments are indistinguishable in the status bar.
        let exact = super::format_total_row_count(Some(zsql_core::RowCount::Exact(1_234))).unwrap();
        let estimated =
            super::format_total_row_count(Some(zsql_core::RowCount::Estimated(1_234))).unwrap();
        assert!(!exact.ends_with(" rows"));
        assert!(exact.contains("total"));
        assert!(estimated.contains("total"));
    }

    #[test]
    fn format_total_row_count_handles_small_counts_with_no_separator_needed() {
        assert_eq!(
            super::format_total_row_count(Some(zsql_core::RowCount::Exact(7))),
            Some("7 total".to_owned())
        );
        assert_eq!(
            super::format_total_row_count(Some(zsql_core::RowCount::Exact(0))),
            Some("0 total".to_owned())
        );
    }
}
