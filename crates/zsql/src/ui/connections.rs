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

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    ClickEvent, Context, Div, Entity, FocusHandle, Focusable, KeyDownEvent, Render, Stateful, Task,
    Window, div, prelude::*, px, rgb, rgba,
};
use zsql_core::{Connection, ConnectionUrl};
use zsql_ui::grid;
use zsql_ui::icon::{IconName, icon};
use zsql_ui::text_field::{TextFieldEvent, TextFieldState};
use zsql_ui::theme::ActiveTheme;

use super::theme;
use crate::connections::{ConnectionStore, ConnectionStoreError, StoredConnection};
use crate::drivers;
use crate::session::{Session, SessionState, probe_connection};
use crate::tab_session::ConnectionKey;

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

/// The name + URL of whichever connection the session is currently pointed
/// at, tracked independently of [`Session`] (which only knows the connected
/// URL, not which saved [`StoredConnection`] -- if any -- it came from, nor
/// a display name for a `DATABASE_URL` fallback connection that was never
/// saved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveConnection {
    /// The display name shown in the footer and the modal's active row.
    pub name: String,
    /// The connection URL this name was resolved for.
    pub url: String,
}

/// What the connection footer (see [`super::footer`]) should render, derived
/// from the session's lifecycle state and whichever connection (if any) is
/// currently tracked as active.
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
    /// Show the "not connected, click to connect" prompt with a hollow dot.
    Disconnected,
}

/// The connection footer's display, given whether a live connection is
/// currently held (see [`Session::is_connected`]) and whichever connection
/// is tracked as active. Connected only counts when both hold: the session
/// actually holds a live connection *and* an active connection is tracked --
/// a connected session with no tracked active connection (which should not
/// normally happen, since every connect path threads one through) still
/// falls back to the disconnected prompt rather than showing a blank name.
///
/// Deliberately takes connection liveness rather than [`SessionState`]
/// itself: a query error moves `state` to [`SessionState::Error`] without
/// dropping the underlying connection, and the footer must keep showing the
/// still-connected database through that, not fall back to "Not connected".
#[must_use]
pub fn footer_display(
    session_is_connected: bool,
    active: Option<&ActiveConnection>,
) -> FooterDisplay {
    match (session_is_connected, active) {
        (true, Some(active)) => FooterDisplay::Connected {
            name: active.name.clone(),
            host: host_label(&active.url),
        },
        _ => FooterDisplay::Disconnected,
    }
}

/// Determine the active-connection label for a freshly connected `url`: the
/// name of whichever [`StoredConnection`] in `saved` has a matching url
/// (first match wins), or -- when `url` matches no saved connection, e.g. a
/// `DATABASE_URL`/`Config` fallback connection -- a name derived from the
/// url's host via [`host_label`], so the footer always has something
/// sensible to show instead of a blank label.
#[must_use]
pub fn active_connection_for_url(url: &str, saved: &[StoredConnection]) -> ActiveConnection {
    let name = saved
        .iter()
        .find(|connection| connection.url == url)
        .map_or_else(|| host_label(url), |connection| connection.name.clone());
    ActiveConnection {
        name,
        url: url.to_owned(),
    }
}

/// Extract a `host[:port]`-shaped label from a connection URL for display,
/// e.g. `postgres://user:pass@localhost:5432/db` -> `localhost:5432`. Falls
/// back to the scheme-stripped remainder of the URL if no host segment can
/// be isolated (e.g. a `sqlite:` path), so even an unusual URL still renders
/// something instead of an empty label.
#[must_use]
pub fn host_label(url: &str) -> String {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let after_userinfo = after_scheme
        .rsplit_once('@')
        .map_or(after_scheme, |(_, rest)| rest);
    let host = after_userinfo
        .split(['/', '?'])
        .next()
        .unwrap_or(after_userinfo);
    if host.is_empty() {
        after_scheme.to_owned()
    } else {
        host.to_owned()
    }
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

/// The identity and coloring of one list row's trailing icon button (edit or
/// delete); see [`ConnectionManagerView::row_icon_button`].
#[derive(Clone, Copy)]
struct RowIconButton {
    id_name: &'static str,
    index: usize,
    icon_name: IconName,
    icon_size: gpui::Pixels,
    idle_color: u32,
    hover_color: u32,
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

    /// Switch the open modal to the empty add-connection form.
    pub fn show_add_form(&mut self, cx: &mut Context<Self>) {
        self.view = ManagerView::AddForm;
        self.clear_inputs(cx);
        self.status = None;
        self.test_outcome = None;
        cx.notify();
    }

    /// Switch the open modal to the edit form for the connection at `index`,
    /// pre-filled from its stored name/url.
    pub fn show_edit_form(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            tracing::warn!(index, "edit requested for an out-of-range row");
            return;
        };
        let name = row.connection.name.clone();
        let url = row.connection.url.clone();
        self.view = ManagerView::EditForm { index };
        self.status = None;
        self.test_outcome = None;
        self.name_field
            .update(cx, |field, _cx| field.set_value_quiet(name));
        self.url_field
            .update(cx, |field, _cx| field.set_value_quiet(url));
        self.sync_fields_from_url(cx);
        cx.notify();
    }

    /// Cancel out of the form back to the list, discarding whatever was
    /// typed without saving anything.
    pub fn cancel_form(&mut self, cx: &mut Context<Self>) {
        self.view = ManagerView::List;
        self.clear_inputs(cx);
        self.status = None;
        self.test_outcome = None;
        cx.notify();
    }

    /// Empty every form field and reset the parsed-URL/driver-detection
    /// state to the empty-URL baseline. Uses the quiet setter throughout
    /// (see [`TextFieldState::set_value_quiet`]): this view's own
    /// `cx.notify()` below is what repaints the fields with their new
    /// (empty) content, and none of these resets is a "field edited by the
    /// user" event that should feed back into the URL.
    fn clear_inputs(&mut self, cx: &mut Context<Self>) {
        self.name_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.url_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.host_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.port_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.user_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.password_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.database_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.tls_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.sqlite_path_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.parsed_url = None;
        self.driver_id = Err("empty URL".to_owned());
        self.dim_reason = Some("empty URL".to_owned());
        cx.notify();
    }

    /// The form's current name and URL values, read from the fields.
    fn input_values(&self, cx: &Context<Self>) -> (String, String) {
        (
            self.name_field.read(cx).value().to_string(),
            self.url_field.read(cx).value().to_string(),
        )
    }

    /// Replace the tracked active connection, e.g. after a successful
    /// connect (see [`Self::connect_index`]) or at startup once
    /// [`Session::connect`]'s fallback URL resolves (see
    /// [`active_connection_for_url`]).
    pub fn set_active(&mut self, active: Option<ActiveConnection>, cx: &mut Context<Self>) {
        self.active = active;
        cx.notify();
    }

    /// `Enter` in the name/url fields: submits the add form, or saves an
    /// edit, according to which panel is open.
    fn submit_form(&mut self, cx: &mut Context<Self>) {
        match self.view {
            ManagerView::AddForm => {
                let _ = self.add_connection(cx);
            }
            ManagerView::EditForm { index } => {
                let _ = self.save_edit(index, cx);
            }
            ManagerView::List => {}
        }
    }

    /// Set the name field's content. Test helper: users type into the field
    /// directly, so this is only needed to drive the field from tests.
    #[cfg(test)]
    fn set_name_input(&mut self, name: impl AsRef<str>, cx: &mut Context<Self>) {
        self.name_field
            .update(cx, |field, cx| field.set_value(name, cx));
    }

    /// Set the URL field's content. Test helper (see [`Self::set_name_input`]).
    #[cfg(test)]
    fn set_url_input(&mut self, url: impl AsRef<str>, cx: &mut Context<Self>) {
        self.url_field
            .update(cx, |field, cx| field.set_value(url, cx));
    }
}

/// Reject a would-be [`StoredConnection`] before it ever reaches
/// [`ConnectionStore::add`]/[`ConnectionStore::update`]: an empty name, an
/// empty URL, or a URL whose scheme resolves to no registered driver.
fn validate_new_connection(name: &str, url: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("Cannot save: a connection name is required.".to_owned());
    }
    if url.trim().is_empty() {
        return Err("Cannot save: a connection URL is required.".to_owned());
    }
    if let Err(reason) = detect_driver_id(url) {
        return Err(format!("Cannot save: {reason}"));
    }
    Ok(())
}

/// Detect the driver id `url` would resolve to, using the same registered
/// drivers and selection function the real connect path uses.
fn detect_driver_id(url: &str) -> Result<&'static str, String> {
    let drivers = drivers::registered_drivers();
    zsql_core::select_driver(&drivers, url)
        .map(|driver| driver.id())
        .map_err(|err| err.to_string())
}

/// The branded label a driver id displays as in the UI (badge, divider,
/// list tag) -- distinct from the id itself, which stays lowercase for
/// scheme matching and query-param lookups.
fn driver_display_label(driver_id: &str) -> &'static str {
    match driver_id {
        "postgres" => "PostgreSQL",
        "mssql" => "MSSQL",
        "sqlite" => "SQLite",
        _ => "unrecognized",
    }
}

fn build_rows(connections: &[StoredConnection]) -> Vec<ConnectionRow> {
    connections
        .iter()
        .map(|connection| ConnectionRow {
            connection: connection.clone(),
            driver_id: detect_driver_id(&connection.url),
        })
        .collect()
}

/// The query-parameter key `zsql_core::ConnectionUrl` reads/writes for
/// `driver_id`'s TLS setting.
fn tls_param_key(driver_id: &str) -> &'static str {
    if driver_id == "mssql" {
        "trustServerCertificate"
    } else {
        "sslmode"
    }
}

/// The field label for `driver_id`'s TLS setting.
fn tls_param_label(driver_id: &str) -> &'static str {
    if driver_id == "mssql" {
        "Trust server certificate"
    } else {
        "SSL mode"
    }
}

/// Set `field`'s displayed value to `value` only if it currently differs,
/// quietly (see [`TextFieldState::set_value_quiet`]): this refills a
/// driver field from the URL (or the URL from a driver field) without
/// resetting the cursor of a field the user is not even looking at, and
/// without that refill itself being mistaken for a fresh edit that should
/// feed back into whichever side just supplied it.
fn set_field_value_if_changed(
    field: &Entity<TextFieldState>,
    value: &str,
    cx: &mut Context<ConnectionManagerView>,
) {
    if field.read(cx).value().as_ref() != value {
        field.update(cx, |field, _cx| field.set_value_quiet(value));
    }
}

// ---- URL <-> fields sync ---------------------------------------------

impl ConnectionManagerView {
    /// Recompute [`Self::driver_id`] (from the URL field's scheme alone) and
    /// [`Self::parsed_url`]/[`Self::dim_reason`] (from a full parse) from
    /// `url_field`'s current text, then refill every driver field the
    /// detected driver uses. The single entry point for the "URL edited ->
    /// reparse the fields" direction, called both by `url_field`'s own
    /// change observer and directly wherever this view sets `url_field`'s
    /// value itself (`show_edit_form`, `clear_inputs`) so a test asserting
    /// immediately after such a call sees fields already refilled rather
    /// than depending on `gpui`'s effect-flush timing.
    fn sync_fields_from_url(&mut self, cx: &mut Context<Self>) {
        let url_text = self.url_field.read(cx).value().to_string();
        self.driver_id = detect_driver_id(&url_text);

        match ConnectionUrl::parse(&url_text) {
            Ok(parsed) => {
                self.dim_reason = None;
                if let Ok(driver_id) = self.driver_id.clone() {
                    self.apply_parsed_url_to_fields(driver_id, &parsed, cx);
                }
                self.parsed_url = Some(parsed);
            }
            Err(err) => {
                self.parsed_url = None;
                self.dim_reason = Some(err.to_string());
            }
        }
        cx.notify();
    }

    /// Refill every field `driver_id`'s layout uses from `parsed`'s current
    /// values.
    fn apply_parsed_url_to_fields(
        &mut self,
        driver_id: &str,
        parsed: &ConnectionUrl,
        cx: &mut Context<Self>,
    ) {
        if driver_id == "sqlite" {
            let path = parsed.sqlite_path().unwrap_or_default();
            set_field_value_if_changed(&self.sqlite_path_field, path, cx);
        } else {
            let host = parsed.host().unwrap_or_default();
            set_field_value_if_changed(&self.host_field, &host, cx);
            let port = parsed
                .port()
                .map_or_else(String::new, |port| port.to_string());
            set_field_value_if_changed(&self.port_field, &port, cx);
            let user = parsed.user();
            set_field_value_if_changed(&self.user_field, &user, cx);
            let password = parsed.password().unwrap_or_default();
            set_field_value_if_changed(&self.password_field, &password, cx);
            let database = parsed.database();
            set_field_value_if_changed(&self.database_field, &database, cx);
            let tls = parsed
                .query_param(tls_param_key(driver_id))
                .unwrap_or_default();
            set_field_value_if_changed(&self.tls_field, &tls, cx);
        }
    }

    /// Reserialize [`Self::parsed_url`] into `url_field`'s displayed value.
    /// The single entry point for the "field edited -> rewrite the URL"
    /// direction, called by every driver field's change handler once it has
    /// mutated `parsed_url`.
    fn reserialize_url(&mut self, cx: &mut Context<Self>) {
        let Some(parsed) = &self.parsed_url else {
            return;
        };
        let new_url = parsed.to_url_string();
        set_field_value_if_changed(&self.url_field, &new_url, cx);
        cx.notify();
    }

    fn on_url_field_changed(&mut self, cx: &mut Context<Self>) {
        self.sync_fields_from_url(cx);
    }

    fn on_host_field_changed(&mut self, cx: &mut Context<Self>) {
        let value = self.host_field.read(cx).value().to_string();
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        if parsed.set_host(&value).is_ok() {
            self.reserialize_url(cx);
        }
    }

    fn on_port_field_changed(&mut self, cx: &mut Context<Self>) {
        let text = self.port_field.read(cx).value().to_string();
        let text = text.trim();
        // `None` means "leave the port alone" (an invalid partial number
        // mid-edit, e.g. out of `u16` range); `Some(None)` clears it;
        // `Some(Some(port))` sets it.
        let port = if text.is_empty() {
            Some(None)
        } else {
            text.parse::<u16>().ok().map(Some)
        };
        let Some(port) = port else {
            return;
        };
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        if parsed.set_port(port).is_ok() {
            self.reserialize_url(cx);
        }
    }

    fn on_user_field_changed(&mut self, cx: &mut Context<Self>) {
        let value = self.user_field.read(cx).value().to_string();
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        parsed.set_user(&value);
        self.reserialize_url(cx);
    }

    fn on_password_field_changed(&mut self, cx: &mut Context<Self>) {
        let value = self.password_field.read(cx).value().to_string();
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        parsed.set_password(&value);
        self.reserialize_url(cx);
    }

    fn on_database_field_changed(&mut self, cx: &mut Context<Self>) {
        let value = self.database_field.read(cx).value().to_string();
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        parsed.set_database(&value);
        self.reserialize_url(cx);
    }

    fn on_tls_field_changed(&mut self, cx: &mut Context<Self>) {
        let value = self.tls_field.read(cx).value().to_string();
        let Ok(driver_id) = self.driver_id.clone() else {
            return;
        };
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        let key = tls_param_key(driver_id);
        if value.trim().is_empty() {
            parsed.remove_query_param(key);
        } else {
            parsed.set_query_param(key, &value);
        }
        self.reserialize_url(cx);
    }

    fn on_sqlite_path_field_changed(&mut self, cx: &mut Context<Self>) {
        let value = self.sqlite_path_field.read(cx).value().to_string();
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        parsed.set_sqlite_path(&value);
        self.reserialize_url(cx);
    }

    /// Toggle whether the password field displays its content masked.
    pub fn toggle_password_visible(&mut self, cx: &mut Context<Self>) {
        let currently_masked = self.password_field.read(cx).is_masked();
        self.password_field
            .update(cx, |field, cx| field.set_masked(!currently_masked, cx));
        cx.notify();
    }
}

// ---- add / edit / delete / connect / test -----------------------------

impl ConnectionManagerView {
    /// Save a new connection from the current name/url inputs, persist it,
    /// refresh the row list, and return the modal to the list panel. Rejects
    /// an empty name, an empty URL, or a URL whose scheme resolves to no
    /// registered driver without persisting anything or leaving the form;
    /// leaves the inputs untouched in every failure case so the user can
    /// correct and retry.
    ///
    /// # Errors
    /// Returns [`ConnectionStoreError`] if the store could not be written.
    /// Input validation failures are reported through [`Self::status`]
    /// rather than this `Result`, since they never reach the store.
    #[tracing::instrument(name = "connection_manager_add", skip_all)]
    pub fn add_connection(&mut self, cx: &mut Context<Self>) -> Result<(), ConnectionStoreError> {
        let (name, url) = self.input_values(cx);
        if let Err(message) = validate_new_connection(&name, &url) {
            tracing::warn!(reason = %message, "rejected invalid connection input");
            self.status = Some(message);
            cx.notify();
            return Ok(());
        }
        let connection = StoredConnection {
            name: name.clone(),
            url,
        };
        match self.store.add(connection) {
            Ok(()) => {
                tracing::info!(name = %name, "connection saved");
                self.rebuild_rows(cx);
                self.view = ManagerView::List;
                self.clear_inputs(cx);
                self.status = Some("Connection saved.".to_owned());
                cx.notify();
                Ok(())
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to save connection");
                self.status = Some(format!("Failed to save: {err}"));
                cx.notify();
                Err(err)
            }
        }
    }

    /// Save the current name/url inputs over the stored connection at
    /// `index`, in place (same position, no duplicate row appended), and
    /// return the modal to the list panel. Same validation as
    /// [`Self::add_connection`].
    ///
    /// # Errors
    /// Returns [`ConnectionStoreError`] if the store could not be written.
    #[tracing::instrument(name = "connection_manager_save_edit", skip_all, fields(index))]
    pub fn save_edit(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), ConnectionStoreError> {
        let (name, url) = self.input_values(cx);
        if let Err(message) = validate_new_connection(&name, &url) {
            tracing::warn!(reason = %message, "rejected invalid connection edit");
            self.status = Some(message);
            cx.notify();
            return Ok(());
        }
        let connection = StoredConnection {
            name: name.clone(),
            url,
        };
        match self.store.update(index, connection) {
            Ok(()) => {
                tracing::info!(index, name = %name, "connection updated");
                self.rebuild_rows(cx);
                self.view = ManagerView::List;
                self.clear_inputs(cx);
                self.status = Some("Connection saved.".to_owned());
                cx.notify();
                Ok(())
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to save connection edit");
                self.status = Some(format!("Failed to save: {err}"));
                cx.notify();
                Err(err)
            }
        }
    }

    /// Delete the saved connection at `index` from the store, persist the
    /// removal, and refresh the row list. If the deleted connection was the
    /// tracked active one, clears [`Self::active`] so the footer/modal fall
    /// back to the disconnected prompt rather than continuing to show a
    /// name that no longer has a saved entry behind it -- deleting the
    /// active row does not touch the live [`Session`] connection itself,
    /// only this view's label for it.
    ///
    /// # Errors
    /// Returns [`ConnectionStoreError`] if the store could not be written.
    #[tracing::instrument(name = "connection_manager_delete", skip_all, fields(index))]
    pub fn delete_index(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), ConnectionStoreError> {
        let Some(row) = self.rows.get(index) else {
            tracing::warn!(index, "delete requested for an out-of-range row");
            return Ok(());
        };
        let deleted = row.connection.clone();

        match self.store.remove(index) {
            Ok(()) => {
                tracing::info!(name = %deleted.name, "connection deleted");
                self.rebuild_rows(cx);
                if self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.name == deleted.name && active.url == deleted.url)
                {
                    self.active = None;
                }
                self.status = Some(format!("Deleted {}.", deleted.name));
                cx.notify();
                Ok(())
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to delete connection");
                self.status = Some(format!("Failed to delete: {err}"));
                cx.notify();
                Err(err)
            }
        }
    }

    /// Rebuild [`Self::rows`] and [`Self::row_focus_handles`] from the
    /// store's current contents.
    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        self.rows = build_rows(self.store.connections());
        self.row_focus_handles = self.rows.iter().map(|_| cx.focus_handle()).collect();
    }

    /// Connect to the saved connection at `index` through
    /// [`Session::connect_to`], the same driver-selection path every
    /// connection in the app goes through, then -- mirroring the
    /// connect-then-introspect sequencing the app runs at startup -- follows
    /// a successful connect with [`Session::introspect`] so the schema
    /// sidebar reflects the newly chosen connection rather than staying
    /// empty or showing the previous connection's stale tree. On success,
    /// updates [`Self::active`] to this row's name/url so the footer and the
    /// modal's active-row highlight both reflect the switch. Updates
    /// [`Self::status`] with the final outcome once the whole sequence
    /// settles. Does not itself close the modal; see
    /// [`Self::connect_and_close`] for the row-click/Enter path that does.
    #[tracing::instrument(name = "connection_manager_connect", skip_all)]
    pub fn connect_index(&mut self, index: usize, cx: &mut Context<Self>) -> Task<()> {
        let Some(row) = self.rows.get(index) else {
            tracing::warn!(index, "connect requested for an out-of-range row");
            return Task::ready(());
        };
        let name = row.connection.name.clone();
        let url = row.connection.url.clone();
        tracing::info!(name = %name, driver = ?row.driver_id, "connecting to saved connection");
        self.status = Some(format!("Connecting to {name}..."));
        cx.notify();

        let session = self.session.clone();
        let active_on_success = ActiveConnection {
            name: name.clone(),
            url: url.clone(),
        };
        cx.spawn(async move |this, cx| {
            let Ok(connect_task) = session.update(cx, |session, cx| session.connect_to(url, cx))
            else {
                return;
            };
            connect_task.await;

            let outcome = session.read_with(cx, |session, _app| match session.state() {
                SessionState::Connected => Ok(()),
                SessionState::Error(message) => Err(message.clone()),
                other => Err(format!("unexpected state after connect: {other:?}")),
            });
            let Ok(outcome) = outcome else {
                return;
            };

            if outcome.is_ok()
                && let Ok(introspect_task) = session.update(cx, Session::introspect)
            {
                introspect_task.await;
            }

            let _ = this.update(cx, |view, cx| {
                if outcome.is_ok() {
                    view.active = Some(active_on_success);
                }
                view.status = Some(match outcome {
                    Ok(()) => format!("Connected to {name}."),
                    Err(reason) => format!("Failed to connect to {name}: {reason}"),
                });
                cx.notify();
            });
        })
    }

    /// Connect to the row at `index` (see [`Self::connect_index`]) and close
    /// the modal, the behavior a click on a list row's body -- or an
    /// `Enter` while it is focused -- triggers. Closing happens immediately
    /// once the connect attempt is dispatched, not once it resolves: a
    /// connect can take a while, and the modal closing right away (matching
    /// the click) is what makes this feel instantaneous.
    pub fn connect_and_close(&mut self, index: usize, cx: &mut Context<Self>) -> Task<()> {
        let task = self.connect_index(index, cx);
        self.close(cx);
        task
    }

    /// Connect to the form's current URL through the session, without
    /// persisting it to the store. Rejects an empty or unrecognized-scheme
    /// URL the same way [`validate_new_connection`] does, without touching
    /// the session. On a successful connect, closes the modal; a failed
    /// connect leaves the modal open with the error in the status line.
    #[tracing::instrument(name = "connection_manager_connect_unsaved", skip_all)]
    pub fn connect_unsaved(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let url = self.url_field.read(cx).value().to_string();
        let name = self.name_field.read(cx).value().to_string();
        if let Err(reason) = detect_driver_id(&url) {
            self.status = Some(format!("Cannot connect: {reason}"));
            cx.notify();
            return Task::ready(());
        }
        let display_name = if name.trim().is_empty() {
            host_label(&url)
        } else {
            name
        };
        tracing::info!(name = %display_name, "connecting to unsaved connection");
        self.status = Some(format!("Connecting to {display_name}..."));
        cx.notify();

        let session = self.session.clone();
        let active_on_success = ActiveConnection {
            name: display_name.clone(),
            url: url.clone(),
        };
        cx.spawn(async move |this, cx| {
            let Ok(connect_task) = session.update(cx, |session, cx| session.connect_to(url, cx))
            else {
                return;
            };
            connect_task.await;

            let outcome = session.read_with(cx, |session, _app| match session.state() {
                SessionState::Connected => Ok(()),
                SessionState::Error(message) => Err(message.clone()),
                other => Err(format!("unexpected state after connect: {other:?}")),
            });
            let Ok(outcome) = outcome else {
                return;
            };

            if outcome.is_ok()
                && let Ok(introspect_task) = session.update(cx, Session::introspect)
            {
                introspect_task.await;
            }

            let _ = this.update(cx, |view, cx| {
                if outcome.is_ok() {
                    view.active = Some(active_on_success);
                    view.close(cx);
                } else if let Err(reason) = &outcome {
                    view.status = Some(format!("Failed to connect to {display_name}: {reason}"));
                }
                cx.notify();
            });
        })
    }

    /// Open a real connection to the form's current URL and ping it, on
    /// [`Self::probe_timeout`], without saving anything or touching the
    /// session's active connection. Updates [`Self::test_outcome`] with
    /// `Pending` immediately, then the final result once the attempt
    /// settles.
    #[tracing::instrument(name = "connection_manager_test", skip_all)]
    pub fn run_test(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let url = self.url_field.read(cx).value().to_string();
        if let Err(reason) = detect_driver_id(&url) {
            self.test_outcome = Some(TestOutcome::Failed(reason));
            cx.notify();
            return Task::ready(());
        }
        tracing::info!("connection test starting");
        self.test_outcome = Some(TestOutcome::Pending);
        cx.notify();
        let timeout = self.probe_timeout;

        cx.spawn(async move |this, cx| {
            let started = Instant::now();
            let connect_result = cx.background_spawn(drivers::connect(url)).await;
            let outcome = match connect_result {
                Ok(conn) => {
                    let conn: Arc<dyn Connection> = Arc::from(conn);
                    let executor = cx.background_executor().clone();
                    let probe_result = cx
                        .background_spawn(probe_connection(conn, timeout, executor))
                        .await;
                    match probe_result {
                        Ok(()) => {
                            let elapsed_ms =
                                u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                            TestOutcome::Connected { elapsed_ms }
                        }
                        Err(message) => TestOutcome::Failed(message),
                    }
                }
                Err(err) => TestOutcome::Failed(err.to_string()),
            };
            tracing::info!(?outcome, "connection test finished");
            let _ = this.update(cx, |view, cx| {
                view.test_outcome = Some(outcome);
                cx.notify();
            });
        })
    }
}

impl Render for ConnectionManagerView {
    /// The modal overlay: a dimmed backdrop (clicking it closes the modal)
    /// centering a panel that shows either the list or the add/edit form.
    /// Only ever mounted while [`Self::is_open`] is true -- the caller
    /// (`ui::workspace::WorkspaceView`) is responsible for conditionally
    /// mounting this entity in the first place, so `render` does not
    /// re-check `open` itself.
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                view.close(cx);
            }))
            .child(
                div()
                    .id("connection-modal-panel")
                    .debug_selector(|| "connection-modal-panel".to_owned())
                    .w(theme::MODAL_WIDTH)
                    .bg(rgb(colors.bg_panel))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .rounded(px(theme::MODAL_RADIUS))
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
                        ManagerView::List => self.render_modal_list(cx).into_any_element(),
                        ManagerView::AddForm | ManagerView::EditForm { .. } => {
                            self.render_modal_form(cx).into_any_element()
                        }
                    }),
            )
    }
}

impl ConnectionManagerView {
    /// The modal's title bar: a back arrow on the form, the panel title
    /// (naming the connection being edited, for the edit form), a
    /// saved-count subtitle on the list, and a close (`x`) button.
    fn render_modal_head(&self, cx: &Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let mut head = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .h(theme::MODAL_HEAD_HEIGHT)
            .px_3()
            .border_b_1()
            .border_color(rgb(colors.border_soft));

        if !matches!(self.current_view(), ManagerView::List) {
            head = head.child(
                div()
                    .id("connection-form-back")
                    .cursor_pointer()
                    .pr_2()
                    .text_color(rgb(colors.text_tertiary))
                    .child("<")
                    .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.cancel_form(cx);
                    })),
            );
        }

        let title = match self.current_view() {
            ManagerView::List => "Connections".to_owned(),
            ManagerView::AddForm => "Add connection".to_owned(),
            ManagerView::EditForm { .. } => "Edit connection".to_owned(),
        };
        head = head.child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(colors.text_primary))
                .child(title),
        );

        if matches!(self.current_view(), ManagerView::List) {
            head = head.child(
                div()
                    .pl_2()
                    .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(format!("{} saved", self.rows.len())),
            );
        }
        if matches!(self.current_view(), ManagerView::EditForm { .. }) {
            let name = self.name_field.read(cx).value().to_string();
            head = head.child(
                div()
                    .pl_2()
                    .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(name),
            );
        }

        head.child(
            div()
                .id("connection-modal-close")
                .group(theme::MODAL_CLOSE_HOVER_GROUP)
                .ml_auto()
                .cursor_pointer()
                .child(
                    icon(
                        IconName::Close,
                        theme::MODAL_CLOSE_ICON_SIZE,
                        colors.text_tertiary,
                    )
                    .group_hover(theme::MODAL_CLOSE_HOVER_GROUP, |style| {
                        style.text_color(rgb(colors.text_primary))
                    }),
                )
                .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                    view.close(cx);
                })),
        )
    }

    /// The saved-connections list panel: every row plus the "Add
    /// connection" affordance and the inline status line.
    fn render_modal_list(&self, cx: &Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let mut list = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .max_h(theme::MODAL_LIST_MAX_HEIGHT)
            .overflow_hidden();
        for (index, row) in self.connections().iter().enumerate() {
            list = list.child(self.render_modal_row(index, row, cx));
        }

        div()
            .flex()
            .flex_col()
            .child(list)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(colors.border_soft))
                    .child(
                        div()
                            .id("add-connection-button")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .px_3()
                            .py_1()
                            .rounded(px(theme::MODAL_ROW_RADIUS))
                            .border_1()
                            .border_color(rgb(colors.accent))
                            .text_color(rgb(colors.text_primary))
                            .child(icon(
                                IconName::Add,
                                theme::MODAL_ADD_ICON_SIZE,
                                colors.text_primary,
                            ))
                            .child("Add connection")
                            .on_click(cx.listener(|view, _event: &ClickEvent, window, cx| {
                                view.show_add_form(cx);
                                let handle = view.name_field.read(cx).focus_handle(cx);
                                window.focus(&handle);
                            })),
                    ),
            )
            .child(self.render_status(cx))
    }

    /// One connection-list row: status dot, name (+ "connected" label and
    /// teal tint when this row is the active connection), url, driver tag,
    /// an edit affordance, and a delete affordance. Clicking the row's body
    /// connects to it and closes the modal; `Enter` while the row is
    /// focused does the same. The edit/delete controls stop propagation so
    /// neither triggers the row's own connect. Deliberately has no
    /// `border_l` rail on the active row -- the teal dot, label, and
    /// background tint are the only "this one is active" cues.
    fn render_modal_row(
        &self,
        index: usize,
        row: &ConnectionRow,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let driver_label = match &row.driver_id {
            Ok(id) => driver_display_label(id).to_owned(),
            Err(_) => "unrecognized".to_owned(),
        };
        let is_active = self.active.as_ref().is_some_and(|active| {
            active.name == row.connection.name && active.url == row.connection.url
        });
        let focus_handle = self
            .row_focus_handles
            .get(index)
            .cloned()
            .unwrap_or_else(|| cx.focus_handle());

        let mut item = div()
            .id(("connection-modal-row", index))
            .track_focus(&focus_handle)
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded(px(theme::MODAL_ROW_RADIUS))
            .cursor_pointer()
            .hover(|el| el.bg(rgb(colors.bg_raised)))
            .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                view.connect_and_close(index, cx).detach();
            }))
            .on_key_down(cx.listener(move |view, event: &KeyDownEvent, _window, cx| {
                if event.keystroke.key == "enter" {
                    view.connect_and_close(index, cx).detach();
                }
            }))
            .child(if is_active {
                grid::status_dot(colors.accent)
            } else {
                grid::status_dot_outline(colors.text_tertiary)
            })
            .child(Self::render_row_meta(row, is_active, colors))
            .child(grid::type_tag(&driver_label, &active_theme))
            .child(Self::row_icon_button(
                cx,
                RowIconButton {
                    id_name: "edit-connection-button",
                    index,
                    icon_name: IconName::Edit,
                    icon_size: theme::MODAL_EDIT_ICON_SIZE,
                    idle_color: colors.text_tertiary,
                    hover_color: colors.text_secondary,
                },
                move |view, cx| view.show_edit_form(index, cx),
            ))
            .child(Self::row_icon_button(
                cx,
                RowIconButton {
                    id_name: "delete-connection-button",
                    index,
                    icon_name: IconName::Delete,
                    icon_size: theme::MODAL_DELETE_ICON_SIZE,
                    idle_color: colors.text_tertiary,
                    hover_color: colors.status_error,
                },
                move |view, cx| {
                    let _ = view.delete_index(index, cx);
                },
            ));

        if is_active {
            item = item.bg(rgba(theme::modal_row_active_bg(active_theme)));
        }
        item
    }

    /// A row's name (+ "connected" label when active) and url, stacked.
    fn render_row_meta(
        row: &ConnectionRow,
        is_active: bool,
        colors: zsql_ui::theme::Colors,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(theme::MODAL_ROW_INNER_GAP)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(theme::MODAL_ROW_NAME_TEXT_SIZE))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(colors.text_primary))
                            .child(row.connection.name.clone()),
                    )
                    .when(is_active, |el| {
                        el.child(
                            div()
                                .text_size(px(theme::MODAL_ROW_CONNECTED_LABEL_TEXT_SIZE))
                                .text_color(rgb(colors.accent))
                                .child("connected"),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(px(theme::MODAL_ROW_URL_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .truncate()
                    .child(row.connection.url.clone()),
            )
    }

    /// One of a list row's trailing icon buttons (edit/delete): a
    /// hover-tinted icon whose click stops propagation -- so it never also
    /// dispatches the row's own connect-on-click -- before running
    /// `on_click`.
    fn row_icon_button(
        cx: &Context<Self>,
        spec: RowIconButton,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let hover_group = format!("{}-hover-{}", spec.id_name, spec.index);
        div()
            .id((spec.id_name, spec.index))
            .group(hover_group.clone())
            .cursor_pointer()
            .px_1()
            .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                cx.stop_propagation();
                on_click(view, cx);
            }))
            .child(
                icon(spec.icon_name, spec.icon_size, spec.idle_color)
                    .group_hover(hover_group, move |style| {
                        style.text_color(rgb(spec.hover_color))
                    }),
            )
    }

    /// A field's caption label, in the small uppercase-weight style every
    /// field in the form shares.
    fn field_label(text: impl Into<String>, colors: zsql_ui::theme::Colors) -> Div {
        div()
            .text_size(px(theme::CONNECTION_FORM_LABEL_TEXT_SIZE))
            .text_color(rgb(colors.text_tertiary))
            .child(text.into())
    }

    /// A labeled field: a caption above the given input entity.
    fn labeled_field(
        label: impl Into<String>,
        colors: zsql_ui::theme::Colors,
        field: impl IntoElement,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_LABEL_GAP)
            .child(Self::field_label(label, colors))
            .child(field)
    }

    /// The add/edit form panel: Name, URL (with its live detected-driver
    /// badge), a divider labeled with the detected driver, the
    /// driver-specific field section (dimmed with an inline reason while
    /// the URL does not parse), the Test result banner, and the footer
    /// buttons.
    fn render_modal_form(&self, cx: &Context<Self>) -> Div {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let url_is_empty = self.url_field.read(cx).value().is_empty();
        let driver_label = match self.pending_driver_id() {
            Ok(id) => driver_display_label(id).to_owned(),
            Err(_) if url_is_empty => String::new(),
            Err(_) => "unrecognized".to_owned(),
        };

        let mut body = div()
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_FIELD_GAP)
            .p_4()
            .child(Self::labeled_field("Name", colors, self.name_field.clone()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::CONNECTION_FORM_LABEL_GAP)
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .child(Self::field_label("URL", colors))
                            .when(!driver_label.is_empty(), |el| {
                                el.child(
                                    div()
                                        .ml_auto()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_1()
                                        .text_size(px(theme::CONNECTION_FORM_TOGGLE_TEXT_SIZE))
                                        .text_color(rgb(colors.text_secondary))
                                        .child(grid::status_dot(colors.accent))
                                        .child(driver_label.clone()),
                                )
                            }),
                    )
                    .child(self.url_field.clone()),
            );

        body = body.child(self.render_driver_field_section(&driver_label, cx));
        body = body.child(self.render_test_outcome(cx));
        body = body.child(self.render_status(cx));

        div()
            .flex()
            .flex_col()
            .child(body)
            .child(self.render_form_footer(cx))
    }

    /// The divider + driver-specific fields, or a plain hint when the URL's
    /// scheme is not (yet) recognized at all.
    fn render_driver_field_section(&self, driver_label: &str, cx: &Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let Ok(driver_id) = self.pending_driver_id() else {
            return div()
                .text_size(px(theme::CONNECTION_FORM_DIVIDER_TEXT_SIZE))
                .text_color(rgb(colors.text_tertiary))
                .child("Enter a URL above to see its fields.");
        };

        let divider = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_size(px(theme::CONNECTION_FORM_DIVIDER_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(driver_label.to_owned()),
            )
            .child(div().flex_1().h(px(1.0)).bg(rgb(colors.border_soft)));

        let mut section = div()
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_FIELD_GAP)
            .when(self.dim_reason().is_some(), |el| {
                el.opacity(theme::CONNECTION_FORM_DIM_OPACITY)
            });

        section = if driver_id == "sqlite" {
            section.child(Self::labeled_field(
                "Database file",
                colors,
                self.sqlite_path_field.clone(),
            ))
        } else {
            self.render_network_fields(section, driver_id, colors, cx)
        };

        if let Some(extras_line) = self.render_extra_query_params_line(driver_id, colors) {
            section = section.child(extras_line);
        }

        let mut wrapper = div()
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_FIELD_GAP);
        wrapper = wrapper.child(divider).child(section);
        if let Some(reason) = self.dim_reason() {
            wrapper = wrapper.child(
                div()
                    .text_size(px(theme::CONNECTION_FORM_DIVIDER_TEXT_SIZE))
                    .text_color(rgb(colors.text_tertiary))
                    .child(reason.to_owned()),
            );
        }
        wrapper
    }

    /// The Host/Port, User/Password, Database, and TLS-param fields shared
    /// by the network drivers (postgres, mssql), appended onto `section`.
    fn render_network_fields(
        &self,
        section: Div,
        driver_id: &str,
        colors: zsql_ui::theme::Colors,
        cx: &Context<Self>,
    ) -> Div {
        section
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(theme::CONNECTION_FORM_ROW_GAP)
                    .child(div().flex_1().child(Self::labeled_field(
                        "Host",
                        colors,
                        self.host_field.clone(),
                    )))
                    .child(
                        div()
                            .w(theme::CONNECTION_FORM_PORT_WIDTH)
                            .child(Self::labeled_field("Port", colors, self.port_field.clone())),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(theme::CONNECTION_FORM_ROW_GAP)
                    .child(div().flex_1().child(Self::labeled_field(
                        "User",
                        colors,
                        self.user_field.clone(),
                    )))
                    .child(div().flex_1().child(self.render_password_field(colors, cx))),
            )
            .child(Self::labeled_field(
                "Database",
                colors,
                self.database_field.clone(),
            ))
            .child(Self::labeled_field(
                tls_param_label(driver_id),
                colors,
                self.tls_field.clone(),
            ))
    }

    /// A read-only line listing any query parameters the URL carries beyond
    /// `driver_id`'s own TLS param -- parts a field edit still preserves
    /// (see [`zsql_core::ConnectionUrl::extra_query_params`]) but has no
    /// field of its own to show, so they are surfaced here instead of
    /// silently hidden. `None` for sqlite (no query params) or when there
    /// are none to show.
    fn render_extra_query_params_line(
        &self,
        driver_id: &str,
        colors: zsql_ui::theme::Colors,
    ) -> Option<Div> {
        if driver_id == "sqlite" {
            return None;
        }
        let parsed = self.parsed_url.as_ref()?;
        let extras = parsed.extra_query_params(&[tls_param_key(driver_id)]);
        if extras.is_empty() {
            return None;
        }
        let text = extras
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ");
        Some(
            div()
                .text_size(px(theme::CONNECTION_FORM_DIVIDER_TEXT_SIZE))
                .text_color(rgb(colors.text_tertiary))
                .child(format!("extra params (edit in the URL above): {text}")),
        )
    }

    /// The password field with its trailing show/hide toggle.
    fn render_password_field(&self, colors: zsql_ui::theme::Colors, cx: &Context<Self>) -> Div {
        let masked = self.password_field.read(cx).is_masked();
        div()
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_LABEL_GAP)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(Self::field_label("Password", colors))
                    .child(
                        div()
                            .id("connection-form-password-toggle")
                            .ml_auto()
                            .cursor_pointer()
                            .text_size(px(theme::CONNECTION_FORM_TOGGLE_TEXT_SIZE))
                            .text_color(rgb(colors.text_tertiary))
                            .child(if masked { "show" } else { "hide" })
                            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                                view.toggle_password_visible(cx);
                            })),
                    ),
            )
            .child(self.password_field.clone())
    }

    /// The inline Test-button result banner: nothing, a pending indicator,
    /// a connected-with-elapsed-ms line, or the driver's verbatim error.
    fn render_test_outcome(&self, cx: &Context<Self>) -> Div {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let Some(outcome) = self.test_outcome() else {
            return div();
        };
        let (bg, dot_color, text) = match outcome {
            TestOutcome::Pending => (
                theme::connection_test_pending_bg(&active_theme),
                colors.status_warn,
                "Testing...".to_owned(),
            ),
            TestOutcome::Connected { elapsed_ms } => (
                theme::connection_test_ok_bg(&active_theme),
                colors.accent,
                format!("Connected - {elapsed_ms} ms"),
            ),
            TestOutcome::Failed(message) => (
                theme::connection_test_error_bg(&active_theme),
                colors.status_error,
                message.clone(),
            ),
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .rounded(px(theme::MODAL_ROW_RADIUS))
            .bg(rgba(bg))
            .text_size(px(theme::CONNECTION_FORM_RESULT_TEXT_SIZE))
            .text_color(rgb(dot_color))
            .child(grid::status_dot(dot_color))
            .child(text)
    }

    /// The form's footer: Cancel, Test, and (add form) Connect + Save, or
    /// (edit form) Save changes only.
    fn render_form_footer(&self, cx: &Context<Self>) -> Div {
        let colors = cx.theme().colors;
        let ghost_button = |id: &'static str, focus: FocusHandle, label: &'static str| {
            div()
                .id(id)
                .track_focus(&focus)
                .cursor_pointer()
                .px_3()
                .py_1()
                .rounded(px(theme::MODAL_ROW_RADIUS))
                .border_1()
                .border_color(rgb(colors.border))
                .text_color(rgb(colors.text_secondary))
                .child(label)
        };
        let primary_button = |id: &'static str, focus: FocusHandle, label: &'static str| {
            div()
                .id(id)
                .track_focus(&focus)
                .cursor_pointer()
                .px_3()
                .py_1()
                .rounded(px(theme::MODAL_ROW_RADIUS))
                .bg(rgb(colors.bg_raised))
                .text_color(rgb(colors.text_primary))
                .child(label)
        };

        let mut footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(rgb(colors.border_soft))
            .child(
                ghost_button(
                    "connection-form-cancel",
                    self.cancel_focus.clone(),
                    "Cancel",
                )
                .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                    view.cancel_form(cx);
                })),
            )
            .child(
                ghost_button("connection-form-test", self.test_focus.clone(), "Test").on_click(
                    cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.run_test(cx).detach();
                    }),
                ),
            )
            .child(div().flex_1());

        match self.current_view() {
            ManagerView::AddForm => {
                footer = footer
                    .child(
                        ghost_button(
                            "connection-form-connect",
                            self.connect_focus.clone(),
                            "Connect",
                        )
                        .on_click(cx.listener(
                            |view, _event: &ClickEvent, _window, cx| {
                                view.connect_unsaved(cx).detach();
                            },
                        )),
                    )
                    .child(
                        primary_button("connection-form-save", self.save_focus.clone(), "Save")
                            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                                let _ = view.add_connection(cx);
                            })),
                    );
            }
            ManagerView::EditForm { index } => {
                footer = footer.child(
                    primary_button(
                        "connection-form-save",
                        self.save_focus.clone(),
                        "Save changes",
                    )
                    .on_click(cx.listener(
                        move |view, _event: &ClickEvent, _window, cx| {
                            let _ = view.save_edit(index, cx);
                        },
                    )),
                );
            }
            ManagerView::List => {}
        }

        footer
    }

    fn render_status(&self, cx: &Context<Self>) -> Div {
        div()
            .px_3()
            .pb_2()
            .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
            .text_color(rgb(cx.theme().colors.text_tertiary))
            .child(self.status().unwrap_or_default().to_owned())
    }
}

#[cfg(test)]
mod tests;
