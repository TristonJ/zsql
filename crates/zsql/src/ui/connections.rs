//! The connection manager: a centered modal (opened from
//! [`super::footer::ConnectionFooterView`]) that lists persisted
//! connections and offers a sectioned add/edit form: the URL on top, its
//! parsed-out driver-specific fields below, both always visible and kept in
//! sync in both directions ([`ConnectionManagerView::sync_fields_from_url`]/
//! the per-field `on_*_field_changed` handlers). The URL stays the single
//! source of truth -- [`StoredConnection::url`] is the only thing persisted;
//! the fields are a parse layer over it, built by [`zsql_core::ConnectionUrl`].
//!
//! Every text input uses the reusable [`zsql_ui::text_field::TextFieldState`]
//! widget: a bordered field with a teal focus ring, blinking caret, muted
//! placeholder, selection, clipboard, and IME. Each field is its own entity.

use std::time::Duration;

use gpui::{
    ClickEvent, Context, Entity, FocusHandle, Focusable, KeyDownEvent, Render, Window, div,
    prelude::*, px, rgb, rgba,
};
use zsql_core::ConnectionUrl;
use zsql_ui::text_field::{TextFieldEvent, TextFieldState};
use zsql_ui::theme::ActiveTheme;

use crate::connections::{ConnectionStore, StoredConnection};
use crate::session::Session;
use crate::tab_session::ConnectionKey;

mod active;
pub use active::{ActiveConnection, FooterDisplay, active_connection_for_url, footer_display};

mod form;

mod actions;
use actions::build_rows;

mod render_list;

mod render_form;

mod style;

/// Which panel the connection-manager modal currently shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerView {
    /// The saved-connections list, with add/edit/delete/switch affordances.
    List,
    /// The "new connection" form: an empty form, offering Connect/Save.
    AddForm,
    /// The "edit connection" form, pre-filled from the [`StoredConnection`]
    /// at this row index, offering only Save changes.
    EditForm {
        /// The row index (into [`ConnectionManagerView::connections`]) being
        /// edited.
        index: usize,
    },
}

/// One persisted connection as shown in the manager, with its auto-detected
/// driver id (or the detection failure's message, surfaced inline rather
/// than hidden) derived fresh from the URL every time the row list is built
/// -- never stored, so it can never go stale relative to the registered
/// drivers.
#[derive(Debug, Clone)]
pub struct ConnectionRow {
    /// The persisted name/url pair this row renders.
    pub connection: StoredConnection,
    /// `Ok(driver id)` if the URL's scheme resolved to a registered driver,
    /// `Err(message)` otherwise.
    pub driver_id: Result<&'static str, String>,
}

/// The Test button's most recent (or in-flight) outcome, shown inline in the
/// form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestOutcome {
    /// A connect+ping attempt is in flight.
    Pending,
    /// The connection opened and answered the ping within the configured
    /// timeout.
    Connected {
        /// Total wall-clock time for the connect-and-ping attempt.
        elapsed_ms: u64,
    },
    /// The connect or ping attempt failed. Carries the driver's own error
    /// text verbatim.
    Failed(String),
}

/// The connection-manager modal's state: a saved-connections list, an add/
/// edit form (name/url plus driver-specific fields kept in sync with the
/// URL), and whether the modal is currently open at all.
pub struct ConnectionManagerView {
    session: Entity<Session>,
    store: ConnectionStore,
    rows: Vec<ConnectionRow>,
    /// Per-row focus handles, rebuilt alongside `rows` so `Enter` on a
    /// focused row can connect-and-close the same as clicking it.
    row_focus_handles: Vec<FocusHandle>,
    /// The form's name field.
    name_field: Entity<TextFieldState>,
    /// The form's URL field: the single source of truth every driver field
    /// below is parsed from and reserialized into.
    url_field: Entity<TextFieldState>,
    host_field: Entity<TextFieldState>,
    port_field: Entity<TextFieldState>,
    user_field: Entity<TextFieldState>,
    /// Masked by default; see [`ConnectionManagerView::toggle_password_visible`].
    password_field: Entity<TextFieldState>,
    database_field: Entity<TextFieldState>,
    /// The driver's TLS query-parameter value (`sslmode` for postgres,
    /// `trustServerCertificate` for mssql).
    tls_field: Entity<TextFieldState>,
    /// `SQLite`'s single field: a file path (or `:memory:`).
    sqlite_path_field: Entity<TextFieldState>,
    /// The URL field's current text, parsed -- `None` while it does not
    /// parse, in which case [`Self::dim_reason`] carries why. The driver
    /// fields mutate this in place and reserialize it back into `url_field`,
    /// rather than each owning separate state.
    parsed_url: Option<ConnectionUrl>,
    /// The URL field's current text's detected driver id, from its scheme
    /// alone (see [`detect_driver_id`]) -- this picks which field layout to
    /// show even while [`Self::parsed_url`] is `None` (e.g. a partially
    /// typed `postgres://` URL still shows the Postgres field shapes,
    /// dimmed).
    driver_id: Result<&'static str, String>,
    /// Why the driver-field section is currently dimmed, if it is. `None`
    /// once the URL parses and its scheme resolves to a registered driver.
    dim_reason: Option<String>,
    /// Focus target for the modal overlay itself, so an `Escape` keystroke
    /// reaches [`Self::handle_modal_key_down`] once the caller that opens
    /// the modal focuses it (see `ui::footer::ConnectionFooterView`).
    modal_focus: FocusHandle,
    cancel_focus: FocusHandle,
    test_focus: FocusHandle,
    connect_focus: FocusHandle,
    save_focus: FocusHandle,
    /// Whether the modal overlay is currently mounted/visible.
    open: bool,
    /// Which panel the open modal shows.
    view: ManagerView,
    /// The connection the session is currently pointed at, if any.
    active: Option<ActiveConnection>,
    /// The most recent add/connect/delete/save attempt's outcome, shown
    /// inline.
    status: Option<String>,
    /// The Test button's most recent (or in-flight) outcome, if any has run
    /// since the form was last opened.
    test_outcome: Option<TestOutcome>,
    /// Timeout a Test attempt's ping races against, taken from
    /// [`crate::config::Config::liveness`] so Test and the footer's live
    /// indicator agree on what "unreachable" means.
    probe_timeout: Duration,
}

impl ConnectionManagerView {
    /// Build a manager over `session`, listing whatever `store` already
    /// holds. Starts closed, on the list panel, with no tracked active
    /// connection. `probe_timeout` is the Test button's connect+ping
    /// timeout (typically [`crate::config::Config::liveness`]'s
    /// `probe_timeout()`).
    #[must_use]
    pub fn new(
        session: Entity<Session>,
        store: ConnectionStore,
        probe_timeout: Duration,
        cx: &mut Context<Self>,
    ) -> Self {
        let rows = build_rows(store.connections());
        let name_field = cx.new(|cx| TextFieldState::new("name", None, cx));
        let url_field =
            cx.new(|cx| TextFieldState::new("postgres://... or sqlite://...", None, cx));
        let host_field = cx.new(|cx| TextFieldState::new("host", None, cx));
        let port_field = cx.new(|cx| TextFieldState::new("port", None, cx));
        let user_field = cx.new(|cx| TextFieldState::new("user", None, cx));
        let password_field = cx.new(|cx| {
            let mut field = TextFieldState::new("password", None, cx);
            field.set_masked(true, cx);
            field
        });
        let database_field = cx.new(|cx| TextFieldState::new("database", None, cx));
        let tls_field = cx.new(|cx| TextFieldState::new("", None, cx));
        let sqlite_path_field =
            cx.new(|cx| TextFieldState::new("/path/to.db or :memory:", None, cx));

        // Enter in the name/url fields submits the form (add or save-edit).
        cx.subscribe(&name_field, |view, _field, _event: &TextFieldEvent, cx| {
            view.submit_form(cx);
        })
        .detach();
        cx.subscribe(&url_field, |view, _field, _event: &TextFieldEvent, cx| {
            view.submit_form(cx);
        })
        .detach();

        cx.observe(&url_field, |view, _field, cx| view.on_url_field_changed(cx))
            .detach();
        cx.observe(&host_field, |view, _field, cx| {
            view.on_host_field_changed(cx);
        })
        .detach();
        cx.observe(&port_field, |view, _field, cx| {
            view.on_port_field_changed(cx);
        })
        .detach();
        cx.observe(&user_field, |view, _field, cx| {
            view.on_user_field_changed(cx);
        })
        .detach();
        cx.observe(&password_field, |view, _field, cx| {
            view.on_password_field_changed(cx);
        })
        .detach();
        cx.observe(&database_field, |view, _field, cx| {
            view.on_database_field_changed(cx);
        })
        .detach();
        cx.observe(&tls_field, |view, _field, cx| view.on_tls_field_changed(cx))
            .detach();
        cx.observe(&sqlite_path_field, |view, _field, cx| {
            view.on_sqlite_path_field_changed(cx);
        })
        .detach();

        Self {
            session,
            store,
            row_focus_handles: rows.iter().map(|_| cx.focus_handle()).collect(),
            rows,
            name_field,
            url_field,
            host_field,
            port_field,
            user_field,
            password_field,
            database_field,
            tls_field,
            sqlite_path_field,
            parsed_url: None,
            driver_id: Err("empty URL".to_owned()),
            dim_reason: Some("empty URL".to_owned()),
            modal_focus: cx.focus_handle(),
            cancel_focus: cx.focus_handle(),
            test_focus: cx.focus_handle(),
            connect_focus: cx.focus_handle(),
            save_focus: cx.focus_handle(),
            open: false,
            view: ManagerView::List,
            active: None,
            status: None,
            test_outcome: None,
            probe_timeout,
        }
    }

    /// Every persisted connection, with its auto-detected driver tag.
    #[must_use]
    pub fn connections(&self) -> &[ConnectionRow] {
        &self.rows
    }

    /// The most recent add/connect/delete/save attempt's status message, if
    /// any.
    #[must_use]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// The Test button's most recent (or in-flight) outcome, if any.
    #[must_use]
    pub fn test_outcome(&self) -> Option<&TestOutcome> {
        self.test_outcome.as_ref()
    }

    /// Whether the modal overlay is currently mounted/visible.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Which panel the modal currently shows.
    #[must_use]
    pub fn current_view(&self) -> ManagerView {
        self.view
    }

    /// The driver id the form's current URL field detects, purely from its
    /// scheme (see [`detect_driver_id`]) -- this is what picks the visible
    /// field layout, independent of whether the full URL currently parses.
    pub fn pending_driver_id(&self) -> Result<&'static str, String> {
        self.driver_id.clone()
    }

    /// Why the driver-field section is currently dimmed, if it is.
    #[must_use]
    pub fn dim_reason(&self) -> Option<&str> {
        self.dim_reason.as_deref()
    }

    /// The connection currently tracked as active (the session's connected
    /// URL, named), if any.
    #[must_use]
    pub fn active(&self) -> Option<&ActiveConnection> {
        self.active.as_ref()
    }

    /// The stable tab-session key for whichever connection is currently
    /// tracked as active, if any: [`ConnectionKey::Saved`] when its
    /// name/url match a persisted [`StoredConnection`], else
    /// [`ConnectionKey::Unsaved`] for a `DATABASE_URL`/`Config`-fallback
    /// connection with no saved entry behind it.
    #[must_use]
    pub fn active_tab_session_key(&self) -> Option<ConnectionKey> {
        let active = self.active.as_ref()?;
        let is_saved = self
            .store
            .connections()
            .iter()
            .any(|connection| connection.name == active.name && connection.url == active.url);
        Some(if is_saved {
            ConnectionKey::Saved(active.name.clone())
        } else {
            ConnectionKey::Unsaved
        })
    }

    /// The modal overlay's own focus handle, so a caller that opens the
    /// modal (e.g. the footer's click handler) can focus it and make
    /// `Escape` work immediately.
    #[must_use]
    pub fn modal_focus_handle(&self) -> FocusHandle {
        self.modal_focus.clone()
    }

    /// Open the modal on the list panel, clearing any stale status message
    /// from a previous session with it.
    pub fn open(&mut self, cx: &mut Context<Self>) {
        self.open = true;
        self.view = ManagerView::List;
        self.status = None;
        cx.notify();
    }

    /// Close the modal, whichever panel it was showing.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        cx.notify();
    }

    /// `Escape` closes the modal; `Tab`/`Shift-Tab` move focus through the
    /// form and footer buttons in visual order (see [`Self::focus_order`]).
    /// Every other key is ignored here (the text fields handle their own
    /// keys, and an `Enter` in the name/url fields submits via their
    /// `Submit` event).
    pub fn handle_modal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => self.close(cx),
            "tab" => self.move_focus(event.keystroke.modifiers.shift, window, cx),
            _ => {}
        }
    }

    /// The focusable controls in the currently-shown form, in visual
    /// top-to-bottom order: Name, URL, then the driver-specific fields
    /// (whichever set the detected driver picks), then the footer buttons
    /// left-to-right. Empty on the list panel.
    fn focus_order(&self, cx: &Context<Self>) -> Vec<FocusHandle> {
        if matches!(self.view, ManagerView::List) {
            return Vec::new();
        }
        let mut order = vec![
            self.name_field.read(cx).focus_handle(cx),
            self.url_field.read(cx).focus_handle(cx),
        ];
        match self.driver_id.as_deref() {
            Ok("sqlite") => order.push(self.sqlite_path_field.read(cx).focus_handle(cx)),
            Ok("postgres" | "mssql") => {
                order.push(self.host_field.read(cx).focus_handle(cx));
                order.push(self.port_field.read(cx).focus_handle(cx));
                order.push(self.user_field.read(cx).focus_handle(cx));
                order.push(self.password_field.read(cx).focus_handle(cx));
                order.push(self.database_field.read(cx).focus_handle(cx));
                order.push(self.tls_field.read(cx).focus_handle(cx));
            }
            _ => {}
        }
        order.push(self.cancel_focus.clone());
        order.push(self.test_focus.clone());
        match self.view {
            ManagerView::AddForm => {
                order.push(self.connect_focus.clone());
                order.push(self.save_focus.clone());
            }
            ManagerView::EditForm { .. } => order.push(self.save_focus.clone()),
            ManagerView::List => {}
        }
        order
    }

    /// Move focus to the next (or, if `backward`, previous) control in
    /// [`Self::focus_order`], wrapping past either end. A no-op if nothing
    /// in the form currently holds focus and the list to search is empty.
    fn move_focus(&self, backward: bool, window: &mut Window, cx: &Context<Self>) {
        let order = self.focus_order(cx);
        if order.is_empty() {
            return;
        }
        let current = window.focused(cx);
        let current_index = current.and_then(|handle| order.iter().position(|f| *f == handle));
        let next_index = match current_index {
            Some(index) if backward => (index + order.len() - 1) % order.len(),
            Some(index) => (index + 1) % order.len(),
            None => 0,
        };
        window.focus(&order[next_index]);
    }

    /// Replace the tracked active connection, e.g. after a successful
    /// connect (see [`Self::connect_index`]) or at startup once
    /// [`Session::connect`]'s fallback URL resolves (see
    /// [`active_connection_for_url`]).
    pub fn set_active(&mut self, active: Option<ActiveConnection>, cx: &mut Context<Self>) {
        self.active = active;
        cx.notify();
    }
}

impl Render for ConnectionManagerView {
    /// The modal overlay: a dimmed backdrop (clicking it closes the modal)
    /// centering a panel that shows either the list or the add/edit form.
    /// Only ever mounted while [`Self::is_open`] is true -- the caller
    /// (`ui::workspace::WorkspaceView`) is responsible for conditionally
    /// mounting this entity in the first place, so `render` does not
    /// re-check `open` itself.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        div()
            .id("connection-modal-scrim")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(colors.scrim))
            // Block mouse events from reaching the workspace behind the modal
            // (notably the SQL editor): without this, a click on a field falls
            // through to the editor's own mouse-down, which steals focus back.
            .occlude()
            .track_focus(&self.modal_focus)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.handle_modal_key_down(event, window, cx);
            }))
            .on_click(cx.listener(|_view, _event: &ClickEvent, _window, cx| {
                // This modal is a bit confusing if it closes due to outside
                // click
                cx.stop_propagation();
            }))
            .child(
                div()
                    .id("connection-modal-panel")
                    .debug_selector(|| "connection-modal-panel".to_owned())
                    .w(style::MODAL_WIDTH)
                    .bg(rgb(colors.bg_panel))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .rounded(px(style::MODAL_RADIUS))
                    .overflow_hidden()
                    // Swallows the click before it reaches the scrim's
                    // close-on-click handler above, so interacting with the
                    // panel itself never closes the modal out from under
                    // the user.
                    .on_click(cx.listener(|_view, _event: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                    }))
                    .child(self.render_modal_head(cx))
                    .child(match self.current_view() {
                        ManagerView::List => self.render_modal_list(window, cx).into_any_element(),
                        ManagerView::AddForm | ManagerView::EditForm { .. } => {
                            self.render_modal_form(window, cx).into_any_element()
                        }
                    }),
            )
    }
}

#[cfg(test)]
mod tests;
