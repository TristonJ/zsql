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

use gpui::{App, Context, Entity, FocusHandle, KeyDownEvent, Task, Window, prelude::*};
use uuid::Uuid;
use zsql_core::Connection;

use crate::connections::{
    ConnectionArgs, ConnectionStore, ConnectionStoreError, StoredConnection, ssh_config_from_stored,
};
use crate::drivers::detect_driver_id;
use crate::session::{
    LivenessState, Session, SessionState, open_tunnel_and_connect, probe_connection,
};
use crate::tab_session::ConnectionKey;
use crate::ui::connections::form::{ConnectionForm, ConnectionFormEvent};
use crate::ui::format::host_label;

mod form;
mod list;

/// Which panel the connection-manager modal currently shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerView {
    /// The saved-connections list, with add/edit/delete/switch affordances.
    List,
    /// The form view is being displayed
    Form,
}

/// The name + URL of whichever connection the session is currently pointed
/// at, tracked independently of [`Session`] (which only knows the connected
/// URL, not which saved [`StoredConnection`] -- if any -- it came from, nor
/// a display name for a `DATABASE_URL` fallback connection that was never
/// saved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveConnection {
    /// The id of the [`StoredConnection`] this name/url pair came from, if any. `None`
    /// for a `DATABASE_URL`/`Config` fallback connection with no saved entry behind it.
    pub id: Option<Uuid>,
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

/// One persisted connection as shown in the manager, with its auto-detected
/// driver id (or the detection failure's message, surfaced inline rather
/// than hidden) derived fresh from the URL every time the row list is built
/// -- never stored, so it can never go stale relative to the registered
/// drivers.
#[derive(Debug, Clone)]
pub struct ConnectionRow {
    /// The persisted connection this row renders.
    pub connection: StoredConnection,
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
    /// Focus target for the modal overlay itself, so an `Escape` keystroke
    /// reaches [`Self::handle_modal_key_down`] once the caller that opens
    /// the modal focuses it (see `ui::footer::ConnectionFooterView`).
    modal_focus: FocusHandle,
    /// Whether the modal overlay is currently mounted/visible.
    open: bool,
    /// Which panel the open modal shows.
    view: ManagerView,
    /// The connection the session is currently pointed at, if any.
    active: Option<ActiveConnection>,
    /// The most recent add/connect/delete/save attempt's outcome, shown
    /// inline.
    status: Option<String>,
    /// Timeout a Test attempt's ping races against, taken from
    /// [`crate::config::Config::liveness`] so Test and the footer's live
    /// indicator agree on what "unreachable" means.
    probe_timeout: Duration,
    form: Entity<ConnectionForm>,
    /// We need to refocus the modal's own focus handle
    refocus_modal: bool,
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

        let form = cx.new(ConnectionForm::new);
        cx.subscribe(&form, |manager, _form, event, cx| {
            manager.on_form_event(event, cx);
        })
        .detach();

        Self {
            session,
            store,
            row_focus_handles: rows.iter().map(|_| cx.focus_handle()).collect(),
            rows,
            modal_focus: cx.focus_handle(),
            open: false,
            view: ManagerView::List,
            active: None,
            status: None,
            probe_timeout,
            form,
            refocus_modal: false,
        }
    }

    /// Every persisted connection, with its auto-detected driver tag.
    #[must_use]
    pub fn connections(&self) -> &[ConnectionRow] {
        &self.rows
    }

    /// The Test button's most recent (or in-flight) outcome, if any. Test
    /// helper: production rendering reads this straight off [`Self::form`]
    /// itself (see [`ConnectionForm::test_outcome`]) since the form draws
    /// its own banner.
    #[cfg(test)]
    pub fn test_outcome<'a>(&self, cx: &'a App) -> Option<&'a TestOutcome> {
        self.form.read(cx).test_outcome()
    }

    /// The focusable controls in the currently-shown form, in visual
    /// top-to-bottom order -- delegates to [`ConnectionForm::focus_order`].
    #[must_use]
    pub fn focus_order(&self, cx: &App) -> Vec<FocusHandle> {
        self.form.read(cx).focus_order(cx)
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

    /// The connection currently tracked as active (the session's connected
    /// URL, named), if any.
    #[must_use]
    pub fn active(&self) -> Option<&ActiveConnection> {
        self.active.as_ref()
    }

    /// The most recent add/connect/delete/save attempt's outcome, if any.
    /// Test helper: production rendering hands this straight to the list
    /// panel's status line rather than reading it back off itself.
    #[cfg(test)]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
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
            .any(|connection| Some(connection.id) == active.id);
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
    pub fn show_add_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view = ManagerView::Form;
        self.form.update(cx, ConnectionForm::begin_add);
        self.status = None;
        self.form.read(cx).name_focus_handle(cx).focus(window);
        cx.notify();
    }

    /// Switch the open modal to the edit form for the connection at `index`,
    /// pre-filled from its stored name/url.
    pub fn show_edit_form(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.iter().find(|r| r.connection.id == id) else {
            tracing::warn!(id = %id, "edit requested for a non-existing row");
            return;
        };
        let url = match row.connection.get_url() {
            Ok(url) => url,
            Err(e) => {
                tracing::warn!(id = %id, "edit requested for a row with an invalid URL: {e}");
                self.status = Some(format!("Cannot edit: {e}"));
                cx.notify();
                return;
            }
        };
        let name = row.connection.name.clone();
        let ssh = row.connection.ssh.clone();
        let ssh_secret = if ssh.is_some() {
            row.connection.get_ssh_secret().ok()
        } else {
            None
        };
        let id = row.connection.id;
        self.view = ManagerView::Form;
        self.status = None;
        self.form.update(cx, |form, cx| {
            form.begin_edit(id, name, url, ssh, ssh_secret, cx);
        });
        self.form.read(cx).name_focus_handle(cx).focus(window);
        cx.notify();
    }

    /// Cancel out of the form back to the list, discarding whatever was
    /// typed without saving anything.
    pub fn cancel_form(&mut self, cx: &mut Context<Self>) {
        self.view = ManagerView::List;
        self.form.update(cx, ConnectionForm::begin_add);
        self.status = None;
        self.refocus_modal = true;
        cx.notify();
    }

    /// Replace the tracked active connection
    #[cfg(test)]
    pub fn set_active(&mut self, active: Option<ActiveConnection>, cx: &mut Context<Self>) {
        self.active = active;
        cx.notify();
    }

    /// Set the form's name input. Test helper: users type into the field
    /// directly, so this is only needed to drive the form from tests.
    #[cfg(test)]
    pub fn set_name_input(&mut self, name: impl AsRef<str>, cx: &mut Context<Self>) {
        self.form
            .update(cx, |form, cx| form.set_name_input(name, cx));
    }

    /// Set the form's URL input. Test helper (see [`Self::set_name_input`]).
    #[cfg(test)]
    pub fn set_url_input(&mut self, url: impl AsRef<str>, cx: &mut Context<Self>) {
        self.form.update(cx, |form, cx| form.set_url_input(url, cx));
    }

    /// React to a [`ConnectionFormEvent`] emitted by [`Self::form`]: routes
    /// each footer button (and Enter-to-submit) to the session/store action
    /// it represents.
    fn on_form_event(&mut self, event: &ConnectionFormEvent, cx: &mut Context<Self>) {
        match event {
            ConnectionFormEvent::Cancel => self.cancel_form(cx),
            ConnectionFormEvent::Test { url } => {
                self.run_test(cx, url.clone()).detach();
            }
            ConnectionFormEvent::Connect { name, url } => {
                self.connect_unsaved(cx, name.clone(), url.clone()).detach();
            }
            ConnectionFormEvent::Add { name, url } => {
                let _ = self.add_connection(cx, name, url.clone());
            }
            ConnectionFormEvent::Edit { id, name, url } => {
                let _ = self.save_edit(cx, *id, name, url.clone());
            }
        }
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

/// The branded label a driver id displays as in the UI (badge, divider,
/// list tag) -- distinct from the id itself, which stays lowercase for
/// scheme matching and query-param lookups.
pub(super) fn driver_display_label(driver_id: &str) -> &'static str {
    match driver_id {
        "postgres" => "PostgreSQL",
        "mssql" => "MSSQL",
        "sqlite" => "SQLite",
        "mysql" => "MySQL",
        "mariadb" => "MariaDB",
        _ => "unrecognized",
    }
}

fn build_rows(connections: &[StoredConnection]) -> Vec<ConnectionRow> {
    connections
        .iter()
        .map(|connection| ConnectionRow {
            connection: connection.clone(),
        })
        .collect()
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
    pub fn add_connection(
        &mut self,
        cx: &mut Context<Self>,
        name: &str,
        url: String,
    ) -> Result<(), ConnectionStoreError> {
        if let Err(message) = validate_new_connection(name, &url) {
            tracing::warn!(reason = %message, "rejected invalid connection input");
            self.status = Some(message);
            cx.notify();
            return Ok(());
        }
        let (ssh, ssh_secret) = self.form.read(cx).ssh_state(cx);
        let connection = ConnectionArgs {
            name: name.to_string(),
            url,
            ssh,
            ssh_secret,
        };
        match self.store.add(connection) {
            Ok(()) => {
                tracing::info!(name = %name, "connection saved");
                self.rebuild_rows(cx);
                self.view = ManagerView::List;
                self.form.update(cx, ConnectionForm::begin_add);
                self.status = Some("connection saved".to_owned());
                self.refocus_modal = true;
                cx.notify();
                Ok(())
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to save connection");
                self.status = Some(format!("{err}"));
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
        cx: &mut Context<Self>,
        id: Uuid,
        name: &str,
        url: String,
    ) -> Result<(), ConnectionStoreError> {
        if let Err(message) = validate_new_connection(name, &url) {
            tracing::warn!(reason = %message, "rejected invalid connection edit");
            self.status = Some(message);
            cx.notify();
            return Ok(());
        }
        let (ssh, ssh_secret) = self.form.read(cx).ssh_state(cx);
        let args = ConnectionArgs {
            name: name.to_string(),
            url,
            ssh,
            ssh_secret,
        };
        match self.store.update(id, args) {
            Ok(()) => {
                tracing::info!(id = %id, name = %name, "connection updated");
                self.rebuild_rows(cx);
                self.view = ManagerView::List;
                self.form.update(cx, ConnectionForm::begin_add);
                self.status = Some("sonnection saved".to_owned());
                self.refocus_modal = true;
                cx.notify();
                Ok(())
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to save connection edit");
                self.status = Some(format!("{err}"));
                cx.notify();
                Err(err)
            }
        }
    }

    /// Delete the saved connection at `id` from the store, persist the
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
    pub fn delete_id(
        &mut self,
        id: Uuid,
        cx: &mut Context<Self>,
    ) -> Result<(), ConnectionStoreError> {
        let row = self
            .rows
            .iter()
            .find(|row| row.connection.id == id)
            .cloned();
        let Some(row) = row else {
            tracing::warn!(id = %id, "delete requested for an non-existing row");
            return Ok(());
        };
        let deleted = row.connection;

        match self.store.remove(id) {
            Ok(()) => {
                tracing::info!(name = %deleted.name, "connection deleted");
                self.rebuild_rows(cx);
                if self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.id == Some(deleted.id))
                {
                    self.active = None;
                }
                self.status = Some("connection deleted".to_string());
                cx.notify();
                Ok(())
            }
            Err(err) => {
                tracing::warn!(error = %err, "failed to delete connection");
                self.status = Some(format!("{err}"));
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

    /// Connect to the saved connection with `id` through
    /// [`Session::connect_to_with_ssh`], the same driver-selection path
    /// every connection in the app goes through (opening the connection's
    /// SSH tunnel first when one is configured and enabled), then --
    /// mirroring the connect-then-introspect sequencing the app runs at
    /// startup -- follows a successful connect with [`Session::introspect`]
    /// so the schema sidebar reflects the newly chosen connection rather
    /// than staying empty or showing the previous connection's stale tree.
    ///
    /// [`Self::active`] is updated to this row's name/url synchronously,
    /// before the connect attempt itself runs, so anything observing this
    /// view's active connection (the footer, the modal's active-row
    /// highlight, and -- through it -- `ui::workspace::WorkspaceView`'s own
    /// tab/schema reset) reacts immediately rather than waiting for the
    /// attempt to succeed or fail; a failed attempt never reverts `active`
    /// back to whatever connection preceded it. Updates [`Self::status`]
    /// with the final outcome once the whole sequence settles. Does not
    /// itself close the modal; see [`Self::connect_and_close`] for the
    /// row-click/Enter path that does.
    #[tracing::instrument(name = "connection_manager_connect", skip_all)]
    pub fn connect(&mut self, id: Uuid, cx: &mut Context<Self>) -> Task<()> {
        let Some(row) = self.rows.iter().find(|r| r.connection.id == id) else {
            tracing::warn!(id = %id, "connect requested for an out-of-range row");
            return Task::ready(());
        };
        let name = row.connection.name.clone();
        let url = match row.connection.get_url() {
            Ok(url) => url,
            Err(err) => {
                tracing::error!(error = %err, "unable to read connection URL");
                self.status = Some(format!("Failed to connect to {name}: {err}"));
                cx.notify();
                return Task::ready(());
            }
        };
        let ssh = match row.connection.ssh_config() {
            Ok(ssh) => ssh,
            Err(err) => {
                tracing::error!(error = %err, "unable to read SSH tunnel secret");
                self.status = Some(format!("Failed to connect to {name}: {err}"));
                cx.notify();
                return Task::ready(());
            }
        };
        tracing::info!(name = %name, "connecting to saved connection");
        self.status = Some("connecting...".to_string());
        self.active = Some(ActiveConnection {
            id: Some(row.connection.id),
            name: name.clone(),
            url: url.clone(),
        });
        cx.notify();

        let connect_task = self
            .session
            .update(cx, |session, cx| session.connect_to_with_ssh(url, ssh, cx));
        let session = self.session.clone();
        cx.spawn(async move |this, cx| {
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
                view.status = Some(match outcome {
                    Ok(()) => format!("Connected to {name}."),
                    Err(reason) => format!("Failed to connect to {name}: {reason}"),
                });
                cx.notify();
            });
        })
    }

    /// Connect to the row with `id` (see [`Self::connect`]) and close the modal
    pub fn connect_and_close(&mut self, id: Uuid, cx: &mut Context<Self>) -> Task<()> {
        let task = self.connect(id, cx);
        self.close(cx);
        task
    }

    /// Connect to the form's current URL through the session, without
    /// persisting it to the store. Rejects an empty or unrecognized-scheme
    /// URL the same way [`validate_new_connection`] does, without touching
    /// the session. When the form's SSH section is enabled, opens its
    /// tunnel first, through [`Session::connect_to_with_ssh`] -- the same
    /// tunnel-before-connect path a saved connection's own [`Self::connect`]
    /// uses.
    ///
    /// [`Self::active`] is updated to this URL synchronously, before the
    /// connect attempt itself runs (see [`Self::connect_index`]'s doc
    /// comment for why). On a successful connect, closes the modal; a
    /// failed connect leaves the modal open with the error in the status
    /// line, without reverting `active`.
    #[tracing::instrument(name = "connection_manager_connect_unsaved", skip_all)]
    pub fn connect_unsaved(
        &mut self,
        cx: &mut Context<Self>,
        name: String,
        url: String,
    ) -> Task<()> {
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
        self.active = Some(ActiveConnection {
            id: None,
            name: display_name.clone(),
            url: url.clone(),
        });
        cx.notify();

        let (ssh, ssh_secret) = self.form.read(cx).ssh_state(cx);
        let ssh_cfg = ssh
            .as_ref()
            .map(|ssh| ssh_config_from_stored(ssh, ssh_secret));

        let connect_task = self.session.update(cx, |session, cx| {
            session.connect_to_with_ssh(url, ssh_cfg, cx)
        });
        let session = self.session.clone();
        cx.spawn(async move |this, cx| {
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
    /// session's active connection. When the form's SSH section is enabled,
    /// its tunnel is opened first (the same tunnel-before-connect path
    /// [`Session::connect_to_with_ssh`] uses); a tunnel failure surfaces as
    /// `Failed` without the driver connect ever being attempted. Pushes
    /// `Pending` into the form (see [`ConnectionForm::set_test_outcome`])
    /// immediately, then the final result once the attempt settles.
    #[tracing::instrument(name = "connection_manager_test", skip_all)]
    pub fn run_test(&mut self, cx: &mut Context<Self>, url: String) -> Task<()> {
        if let Err(reason) = detect_driver_id(&url) {
            self.form.update(cx, |form, cx| {
                form.set_test_outcome(Some(TestOutcome::Failed(reason.to_string())), cx);
            });
            return Task::ready(());
        }
        tracing::info!("connection test starting");
        self.form.update(cx, |form, cx| {
            form.set_test_outcome(Some(TestOutcome::Pending), cx);
        });
        let timeout = self.probe_timeout;
        let form = self.form.clone();
        let (ssh, ssh_secret) = self.form.read(cx).ssh_state(cx);
        let ssh_cfg = ssh
            .as_ref()
            .map(|ssh| ssh_config_from_stored(ssh, ssh_secret));

        cx.spawn(async move |_this, cx| {
            let started = Instant::now();
            let connect_result = cx
                .background_spawn(open_tunnel_and_connect(url, ssh_cfg))
                .await;
            let outcome = match connect_result {
                Ok((conn, _tunnel)) => {
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
            let _ = form.update(cx, |form, cx| {
                form.set_test_outcome(Some(outcome), cx);
            });
        })
    }
}

mod render;

#[cfg(test)]
mod tests;
