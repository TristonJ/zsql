//! The connection manager: a centered modal (opened from
//! [`super::footer::ConnectionFooterView`]) that lists persisted
//! connections, supports adding one (name + URL, showing its auto-detected
//! driver tag), deleting one, and connecting a chosen entry through the
//! driver-selection connect path ([`crate::drivers::connect`] via
//! [`crate::session::Session::connect_to`]).
//!
//! Name/URL entry uses the reusable [`zsql_ui::text_field::TextFieldState`]
//! widget: a bordered field with a teal focus ring, blinking caret, muted
//! placeholder, selection, clipboard, and IME. Each field is its own entity;
//! this view reads their values when adding, and an `Enter` in either field
//! (a [`TextFieldEvent::Submit`]) submits the add form.

use gpui::{
    ClickEvent, Context, Div, Entity, FocusHandle, KeyDownEvent, Render, Stateful, Task, Window,
    div, prelude::*, px, rgb, rgba,
};
use zsql_ui::text_field::{TextFieldEvent, TextFieldState};
use zsql_ui::{colors, grid};

use super::theme;
use crate::connections::{ConnectionStore, ConnectionStoreError, StoredConnection};
use crate::drivers;
use crate::session::{Session, SessionState};

/// Which panel the connection-manager modal currently shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagerView {
    /// The saved-connections list, with add/delete/switch affordances.
    List,
    /// The "new connection" name/url form.
    AddForm,
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

/// The connection-manager modal's state: a saved-connections list, an add
/// form, and whether the modal is currently open at all.
pub struct ConnectionManagerView {
    session: Entity<Session>,
    store: ConnectionStore,
    rows: Vec<ConnectionRow>,
    /// The add form's name field: a reusable interactive text input.
    name_field: Entity<TextFieldState>,
    /// The add form's URL field. Observed so the detected-driver preview
    /// recomputes as the user types.
    url_field: Entity<TextFieldState>,
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
    /// The most recent add/connect/delete attempt's outcome, shown inline.
    status: Option<String>,
}

impl ConnectionManagerView {
    /// Build a manager over `session`, listing whatever `store` already
    /// holds. Starts closed, on the list panel, with no tracked active
    /// connection.
    #[must_use]
    pub fn new(session: Entity<Session>, store: ConnectionStore, cx: &mut Context<Self>) -> Self {
        let rows = build_rows(store.connections());
        let name_field = cx.new(|cx| TextFieldState::new("name", None, cx));
        let url_field =
            cx.new(|cx| TextFieldState::new("postgres://... or sqlite://...", None, cx));

        // Enter in either field submits the add form.
        cx.subscribe(&name_field, |view, _field, _event: &TextFieldEvent, cx| {
            let _ = view.add_connection(cx);
        })
        .detach();
        cx.subscribe(&url_field, |view, _field, _event: &TextFieldEvent, cx| {
            let _ = view.add_connection(cx);
        })
        .detach();
        // Recompute the detected-driver preview as the URL is edited.
        cx.observe(&url_field, |_view, _field, cx| cx.notify())
            .detach();

        Self {
            session,
            store,
            rows,
            name_field,
            url_field,
            modal_focus: cx.focus_handle(),
            open: false,
            view: ManagerView::List,
            active: None,
            status: None,
        }
    }

    /// Every persisted connection, with its auto-detected driver tag.
    #[must_use]
    pub fn connections(&self) -> &[ConnectionRow] {
        &self.rows
    }

    /// The most recent add/connect/delete attempt's status message, if any.
    #[must_use]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
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

    /// `Escape` closes the modal; every other key is ignored here (the
    /// name/url [`TextFieldState`] fields handle their own keys, and an
    /// `Enter` in either submits the add form via their `Submit` event).
    pub fn handle_modal_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            self.close(cx);
        }
    }

    /// Switch the open modal to the add-connection form, clearing any
    /// pending input/status left over from a previous visit to the form.
    pub fn show_add_form(&mut self, cx: &mut Context<Self>) {
        self.view = ManagerView::AddForm;
        self.clear_inputs(cx);
        self.status = None;
        cx.notify();
    }

    /// Cancel out of the add-connection form back to the list, discarding
    /// whatever was typed without saving anything.
    pub fn cancel_add(&mut self, cx: &mut Context<Self>) {
        self.view = ManagerView::List;
        self.clear_inputs(cx);
        self.status = None;
        cx.notify();
    }

    /// Empty both add-form fields.
    fn clear_inputs(&mut self, cx: &mut Context<Self>) {
        self.name_field
            .update(cx, |field, cx| field.set_value("", cx));
        self.url_field
            .update(cx, |field, cx| field.set_value("", cx));
    }

    /// The add form's current name and URL values, read from the fields.
    fn input_values(&self, cx: &Context<Self>) -> (String, String) {
        (
            self.name_field.read(cx).value().to_string(),
            self.url_field.read(cx).value().to_string(),
        )
    }

    /// Replace the tracked active connection, e.g. after a successful
    /// connect (see [`Self::connect_index`]) or at startup once
    /// [`Session::connect`]'s fallback DSN resolves (see
    /// [`active_connection_for_url`]).
    pub fn set_active(&mut self, active: Option<ActiveConnection>, cx: &mut Context<Self>) {
        self.active = active;
        cx.notify();
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

    /// The add form's current driver tag preview, computed from the URL
    /// field's content exactly as [`ConnectionRow::driver_id`] would be once
    /// saved.
    pub fn pending_driver_id(&self, cx: &Context<Self>) -> Result<&'static str, String> {
        detect_driver_id(&self.url_field.read(cx).value())
    }

    /// Save a new connection from the current name/url inputs, persist it,
    /// refresh the row list, clear the inputs, and return the modal to the
    /// list panel. Rejects an empty name, an empty URL, or a URL whose
    /// scheme resolves to no registered driver without persisting anything
    /// or leaving the form; leaves the inputs untouched in every failure
    /// case so the user can correct and retry.
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
                self.rows = build_rows(self.store.connections());
                self.clear_inputs(cx);
                self.status = Some("Connection saved.".to_owned());
                self.view = ManagerView::List;
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
                self.rows = build_rows(self.store.connections());
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
    /// settles.
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
}

/// Reject a would-be [`StoredConnection`] before it ever reaches
/// [`ConnectionStore::add`]: an empty name, an empty URL, or a URL whose
/// scheme resolves to no registered driver.
fn validate_new_connection(name: &str, url: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("Cannot add: a connection name is required.".to_owned());
    }
    if url.trim().is_empty() {
        return Err("Cannot add: a connection URL is required.".to_owned());
    }
    if let Err(reason) = detect_driver_id(url) {
        return Err(format!("Cannot add: {reason}"));
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

fn build_rows(connections: &[StoredConnection]) -> Vec<ConnectionRow> {
    connections
        .iter()
        .map(|connection| ConnectionRow {
            connection: connection.clone(),
            driver_id: detect_driver_id(&connection.url),
        })
        .collect()
}

impl Render for ConnectionManagerView {
    /// The modal overlay: a dimmed backdrop (clicking it closes the modal)
    /// centering a panel that shows either the list or the add form. Only
    /// ever mounted while [`Self::is_open`] is true -- the caller
    /// (`ui::workspace::WorkspaceView`) is responsible for conditionally
    /// mounting this entity in the first place, so `render` does not
    /// re-check `open` itself.
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("connection-modal-scrim")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(theme::MODAL_BACKDROP))
            .track_focus(&self.modal_focus)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, _window, cx| {
                view.handle_modal_key_down(event, cx);
            }))
            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                view.close(cx);
            }))
            .child(
                div()
                    .id("connection-modal-panel")
                    .w(theme::MODAL_WIDTH)
                    .bg(rgb(colors::PANEL))
                    .border_1()
                    .border_color(rgb(colors::LINE))
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
                        ManagerView::AddForm => self.render_modal_add_form(cx).into_any_element(),
                    }),
            )
    }
}

impl ConnectionManagerView {
    /// The modal's title bar: a back arrow on the add form, the panel
    /// title, a saved-count subtitle on the list, and a close (`x`) button.
    fn render_modal_head(&self, cx: &Context<Self>) -> Div {
        let mut head = div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .h(theme::MODAL_HEAD_HEIGHT)
            .px_3()
            .border_b_1()
            .border_color(rgb(colors::LINE_SOFT));

        if matches!(self.current_view(), ManagerView::AddForm) {
            head = head.child(
                div()
                    .id("connection-form-back")
                    .cursor_pointer()
                    .pr_2()
                    .text_color(rgb(colors::FAINT))
                    .child("<")
                    .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.cancel_add(cx);
                    })),
            );
        }

        let title = match self.current_view() {
            ManagerView::List => "Connections",
            ManagerView::AddForm => "New connection",
        };
        head = head.child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(colors::TEXT))
                .child(title),
        );

        if matches!(self.current_view(), ManagerView::List) {
            head = head.child(
                div()
                    .pl_2()
                    .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
                    .text_color(rgb(colors::FAINT))
                    .child(format!("{} saved", self.rows.len())),
            );
        }

        head.child(
            div()
                .id("connection-modal-close")
                .ml_auto()
                .cursor_pointer()
                .text_color(rgb(colors::FAINT))
                .hover(|el| el.text_color(rgb(colors::TEXT)))
                .child("x")
                .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                    view.close(cx);
                })),
        )
    }

    /// The saved-connections list panel: every row plus the "Add
    /// connection" affordance and the inline status line.
    fn render_modal_list(&self, cx: &Context<Self>) -> Div {
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
                    .border_color(rgb(colors::LINE_SOFT))
                    .child(
                        div()
                            .id("add-connection-button")
                            .cursor_pointer()
                            .px_3()
                            .py_1()
                            .rounded(px(theme::MODAL_ROW_RADIUS))
                            .border_1()
                            .border_color(rgb(colors::TEAL))
                            .text_color(rgb(colors::TEXT))
                            .child("+ Add connection")
                            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                                view.show_add_form(cx);
                            })),
                    ),
            )
            .child(self.render_status())
    }

    /// One connection-list row: status dot, name (+ "connected" label and
    /// teal tint when this row is the active connection), url, driver tag,
    /// and a delete affordance. Deliberately has no `border_l` rail on the
    /// active row -- the teal dot, label, and background tint are the only
    /// "this one is active" cues.
    fn render_modal_row(
        &self,
        index: usize,
        row: &ConnectionRow,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let driver_label = match &row.driver_id {
            Ok(id) => (*id).to_owned(),
            Err(_) => "unrecognized".to_owned(),
        };
        let is_active = self.active.as_ref().is_some_and(|active| {
            active.name == row.connection.name && active.url == row.connection.url
        });

        let mut item = div()
            .id(("connection-modal-row", index))
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded(px(theme::MODAL_ROW_RADIUS))
            .cursor_pointer()
            .hover(|el| el.bg(rgb(colors::RAISE)))
            .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                view.connect_index(index, cx).detach();
            }))
            .child(if is_active {
                grid::status_dot(colors::TEAL)
            } else {
                grid::status_dot_outline(colors::FAINT)
            })
            .child(
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
                                    .text_color(rgb(colors::TEXT))
                                    .child(row.connection.name.clone()),
                            )
                            .when(is_active, |el| {
                                el.child(
                                    div()
                                        .text_size(px(theme::MODAL_ROW_CONNECTED_LABEL_TEXT_SIZE))
                                        .text_color(rgb(colors::TEAL))
                                        .child("connected"),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(theme::MODAL_ROW_URL_TEXT_SIZE))
                            .text_color(rgb(colors::FAINT))
                            .truncate()
                            .child(row.connection.url.clone()),
                    ),
            )
            .child(grid::type_tag(&driver_label))
            .child(
                div()
                    .id(("delete-connection-button", index))
                    .cursor_pointer()
                    .px_1()
                    .text_color(rgb(colors::FAINT))
                    .hover(|el| el.text_color(rgb(theme::STATUS_ERROR)))
                    .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                        // Swallowed here so deleting a row never also
                        // dispatches that row's own connect-on-click above.
                        cx.stop_propagation();
                        let _ = view.delete_index(index, cx);
                    }))
                    .child("del"),
            );

        if is_active {
            item = item.bg(rgba(theme::MODAL_ROW_ACTIVE_BG));
        }
        item
    }

    /// The "new connection" form panel: name/url fields, a detected-driver
    /// preview, and Cancel/Add actions.
    fn render_modal_add_form(&self, cx: &Context<Self>) -> Div {
        let url_is_empty = self.url_field.read(cx).value().is_empty();
        let driver_preview = match self.pending_driver_id(cx) {
            Ok(id) => id.to_owned(),
            Err(_) if url_is_empty => String::new(),
            Err(_) => "unrecognized".to_owned(),
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(self.name_field.clone())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(self.url_field.clone())
                    .when(!driver_preview.is_empty(), |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
                                .text_color(rgb(colors::FAINT))
                                .child("detected driver")
                                .child(grid::type_tag(&driver_preview)),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .child(
                        div()
                            .id("connection-form-cancel")
                            .cursor_pointer()
                            .px_3()
                            .py_1()
                            .rounded(px(theme::MODAL_ROW_RADIUS))
                            .border_1()
                            .border_color(rgb(colors::LINE))
                            .text_color(rgb(colors::MUTED))
                            .child("Cancel")
                            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                                view.cancel_add(cx);
                            })),
                    )
                    .child(
                        div()
                            .id("connection-form-add")
                            .cursor_pointer()
                            .px_3()
                            .py_1()
                            .rounded(px(theme::MODAL_ROW_RADIUS))
                            .bg(rgb(colors::RAISE))
                            .text_color(rgb(colors::TEXT))
                            .child("Add")
                            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                                let _ = view.add_connection(cx);
                            })),
                    ),
            )
            .child(self.render_status())
    }

    fn render_status(&self) -> Div {
        div()
            .px_3()
            .pb_2()
            .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
            .text_color(rgb(colors::FAINT))
            .child(self.status().unwrap_or_default().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, KeyDownEvent, Keystroke, Modifiers, TestAppContext};

    use super::{
        ActiveConnection, ConnectionManagerView, ConnectionStore, ManagerView, StoredConnection,
        active_connection_for_url, footer_display, host_label,
    };
    use crate::session::{Session, SessionState};

    /// A temp store path this test owns exclusively, removed on drop.
    struct TempStorePath(std::path::PathBuf);

    impl TempStorePath {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "zsql-connection-manager-test-{label}-{}-{n}.toml",
                std::process::id()
            ));
            Self(path)
        }
    }

    impl Drop for TempStorePath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn session_with_no_dsn() -> Session {
        Session::new(&crate::config::Config::default())
    }

    #[gpui::test]
    fn a_freshly_loaded_store_lists_every_saved_connection(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("list");
        let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
        store
            .add(StoredConnection {
                name: "local pg".to_owned(),
                url: "postgres://localhost/app".to_owned(),
            })
            .expect("add must succeed");
        store
            .add(StoredConnection {
                name: "local sqlite".to_owned(),
                url: "sqlite::memory:".to_owned(),
            })
            .expect("add must succeed");

        // Reload to prove the view lists what's actually on disk, not just
        // whatever `store` happens to hold in memory.
        let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");

        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, reloaded, cx));

        manager.read_with(cx, |view, _app| {
            let names: Vec<&str> = view
                .connections()
                .iter()
                .map(|row| row.connection.name.as_str())
                .collect();
            assert_eq!(names, vec!["local pg", "local sqlite"]);

            assert_eq!(view.connections()[0].driver_id, Ok("postgres"));
            assert_eq!(view.connections()[1].driver_id, Ok("sqlite"));
        });
    }

    #[gpui::test]
    fn an_unrecognized_scheme_surfaces_as_an_error_tag_not_a_panic(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("bad-scheme");
        let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
        store
            .add(StoredConnection {
                name: "mystery".to_owned(),
                url: "cassandra://host/db".to_owned(),
            })
            .expect("add must succeed");

        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.read_with(cx, |view, _app| {
            assert!(view.connections()[0].driver_id.is_err());
        });
    }

    #[cfg(unix)]
    #[gpui::test]
    fn adding_a_connection_reports_a_save_failure(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt as _;

        let base = std::env::temp_dir().join(format!(
            "zsql-connection-manager-test-unwritable-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("setup: create base dir");
        // Owner-read-execute only: the store file's parent exists but a new
        // file cannot be written into it, so `store.add`'s save fails.
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o500))
            .expect("setup: restrict base dir permissions");

        let path = base.join("connections.toml");
        let store = ConnectionStore::load(&path).expect("initial load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.set_name_input("local sqlite", cx);
            view.set_url_input("sqlite::memory:", cx);
            let result = view.add_connection(cx);
            assert!(
                result.is_err(),
                "add_connection must surface a save failure as Err"
            );
            assert!(
                view.status()
                    .is_some_and(|status| status.contains("Failed to save")),
                "status must report the save failure, got {:?}",
                view.status()
            );
        });

        // Restore write permission so the temp dir can be removed.
        let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[gpui::test]
    fn adding_a_connection_appends_it_and_persists_to_disk(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("add");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.set_name_input("new db", cx);
            view.set_url_input("sqlite::memory:", cx);
            view.add_connection(cx).expect("add must succeed");
        });

        manager.read_with(cx, |view, _app| {
            assert_eq!(view.connections().len(), 1);
            assert_eq!(view.connections()[0].connection.name, "new db");
            assert_eq!(view.connections()[0].driver_id, Ok("sqlite"));
        });

        // Persistence: a fresh load from the same path sees it too.
        let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
        assert_eq!(reloaded.connections().len(), 1);
        assert_eq!(reloaded.connections()[0].name, "new db");
    }

    #[gpui::test]
    fn adding_a_connection_clears_the_pending_inputs(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("clears");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.set_name_input("x", cx);
            view.set_url_input("sqlite::memory:", cx);
            view.add_connection(cx).expect("add must succeed");
            assert!(view.name_field.read(cx).value().is_empty());
            assert!(view.url_field.read(cx).value().is_empty());
        });
    }

    #[gpui::test]
    fn pending_driver_id_previews_the_add_forms_current_url(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("preview");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.set_url_input("postgresql://host/db", cx);
            assert_eq!(view.pending_driver_id(cx), Ok("postgres"));

            view.set_url_input("nope://host", cx);
            assert!(view.pending_driver_id(cx).is_err());
        });
    }

    #[gpui::test]
    async fn connecting_a_chosen_row_dispatches_through_the_selected_driver(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let temp = TempStorePath::new("connect");
        let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
        store
            .add(StoredConnection {
                name: "mem".to_owned(),
                url: "sqlite::memory:".to_owned(),
            })
            .expect("add must succeed");

        let session = cx.new(|_cx| session_with_no_dsn());
        let session_for_assert = session.clone();
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        let task = manager.update(cx, |view, cx| view.connect_index(0, cx));
        task.await;

        session_for_assert.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "expected connecting the saved sqlite row to succeed, got {:?}",
                session.state()
            );
        });
    }

    #[gpui::test]
    fn connecting_an_out_of_range_index_does_not_panic(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("out-of-range");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.connect_index(0, cx).detach();
        });
    }

    #[gpui::test]
    async fn connecting_a_chosen_row_also_introspects_and_updates_the_status(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let temp = TempStorePath::new("connect-introspect");
        let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
        store
            .add(StoredConnection {
                name: "mem".to_owned(),
                url: "sqlite::memory:".to_owned(),
            })
            .expect("add must succeed");

        let session = cx.new(|_cx| session_with_no_dsn());
        let session_for_assert = session.clone();
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        let task = manager.update(cx, |view, cx| view.connect_index(0, cx));
        task.await;

        session_for_assert.read_with(cx, |session, _app| {
            assert!(
                matches!(session.schema(), crate::session::SchemaState::Ready(_)),
                "connecting a row must also introspect the schema, got {:?}",
                session.schema()
            );
        });
        manager.read_with(cx, |view, _app| {
            assert_eq!(view.status(), Some("Connected to mem."));
        });
    }

    #[gpui::test]
    fn adding_a_connection_with_an_empty_name_is_rejected_without_persisting(
        cx: &mut TestAppContext,
    ) {
        let temp = TempStorePath::new("reject-empty-name");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.set_name_input("", cx);
            view.set_url_input("sqlite::memory:", cx);
            view.add_connection(cx)
                .expect("validation rejection is Ok(())");

            assert!(view.connections().is_empty());
            assert_eq!(
                view.url_field.read(cx).value().to_string(),
                "sqlite::memory:",
                "inputs must be preserved"
            );
            assert!(view.status().is_some_and(|s| s.contains("name")));
        });
    }

    #[gpui::test]
    fn adding_a_connection_with_an_empty_url_is_rejected_without_persisting(
        cx: &mut TestAppContext,
    ) {
        let temp = TempStorePath::new("reject-empty-url");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.set_name_input("new db", cx);
            view.set_url_input("", cx);
            view.add_connection(cx)
                .expect("validation rejection is Ok(())");

            assert!(view.connections().is_empty());
            assert_eq!(
                view.name_field.read(cx).value().to_string(),
                "new db",
                "inputs must be preserved"
            );
            assert!(view.status().is_some_and(|s| s.contains("URL")));
        });
    }

    #[gpui::test]
    fn adding_a_connection_with_an_unrecognized_scheme_is_rejected_without_persisting(
        cx: &mut TestAppContext,
    ) {
        let temp = TempStorePath::new("reject-bad-scheme");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.set_name_input("mystery", cx);
            view.set_url_input("cassandra://host/db", cx);
            view.add_connection(cx)
                .expect("validation rejection is Ok(())");

            assert!(view.connections().is_empty());
            assert_eq!(
                view.name_field.read(cx).value().to_string(),
                "mystery",
                "inputs must be preserved"
            );
        });
    }

    // ---- host_label / active_connection_for_url -------------------------

    #[test]
    fn host_label_extracts_host_and_port_from_a_postgres_url() {
        assert_eq!(
            host_label("postgres://localhost:5432/zsql"),
            "localhost:5432"
        );
    }

    #[test]
    fn host_label_strips_userinfo_before_the_host() {
        assert_eq!(
            host_label("postgres://reader@10.0.1.4:5432/analytics"),
            "10.0.1.4:5432"
        );
    }

    #[test]
    fn host_label_falls_back_to_the_scheme_stripped_remainder_when_no_host_segment_is_found() {
        // `sqlite:///path/to.db` has an empty host segment before the
        // leading slash of the path; the fallback must still produce
        // something non-empty rather than an empty string.
        let label = host_label("sqlite:///~/dev/scratch.db");
        assert!(!label.is_empty());
    }

    #[test]
    fn active_connection_for_url_uses_the_matching_saved_connections_name() {
        let saved = vec![StoredConnection {
            name: "zsql local".to_owned(),
            url: "postgres://localhost:5432/zsql".to_owned(),
        }];
        let active = active_connection_for_url("postgres://localhost:5432/zsql", &saved);
        assert_eq!(active.name, "zsql local");
        assert_eq!(active.url, "postgres://localhost:5432/zsql");
    }

    #[test]
    fn active_connection_for_url_falls_back_to_a_host_derived_name_when_unsaved() {
        // The `DATABASE_URL`/`Config` fallback path: no `StoredConnection`
        // matches, so the footer must still get a sensible label instead of
        // panicking or showing blank.
        let active = active_connection_for_url("postgres://localhost:5432/zsql", &[]);
        assert_eq!(active.name, "localhost:5432");
    }

    // ---- footer_display ---------------------------------------------------

    #[test]
    fn footer_display_shows_the_active_connections_name_and_host_when_connected() {
        let active = ActiveConnection {
            name: "zsql local".to_owned(),
            url: "postgres://localhost:5432/zsql".to_owned(),
        };
        match footer_display(true, Some(&active)) {
            super::FooterDisplay::Connected { name, host } => {
                assert_eq!(name, "zsql local");
                assert_eq!(host, "localhost:5432");
            }
            other @ super::FooterDisplay::Disconnected => {
                panic!("expected FooterDisplay::Connected, got {other:?}")
            }
        }
    }

    #[test]
    fn footer_display_is_disconnected_when_the_session_holds_no_live_connection() {
        let active = ActiveConnection {
            name: "zsql local".to_owned(),
            url: "postgres://localhost:5432/zsql".to_owned(),
        };
        assert_eq!(
            footer_display(false, Some(&active)),
            super::FooterDisplay::Disconnected
        );
    }

    #[test]
    fn footer_display_is_disconnected_when_connected_but_no_active_connection_is_tracked() {
        assert_eq!(
            footer_display(true, None),
            super::FooterDisplay::Disconnected
        );
    }

    #[test]
    fn footer_display_stays_connected_through_a_query_error_on_a_still_live_connection() {
        // `SessionState::Error` after a query failure does not drop the
        // connection (see `Session::run_query`), so a caller that passes
        // `Session::is_connected()` -- true here -- must still see the
        // active database, not the disconnected prompt.
        let active = ActiveConnection {
            name: "zsql local".to_owned(),
            url: "postgres://localhost:5432/zsql".to_owned(),
        };
        match footer_display(true, Some(&active)) {
            super::FooterDisplay::Connected { name, .. } => assert_eq!(name, "zsql local"),
            other @ super::FooterDisplay::Disconnected => {
                panic!("expected FooterDisplay::Connected, got {other:?}")
            }
        }
    }

    // ---- modal open/close/view transitions ---------------------------------

    #[gpui::test]
    fn opening_the_modal_starts_on_the_list_panel(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("open");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            assert!(!view.is_open());
            view.open(cx);
            assert!(view.is_open());
            assert_eq!(view.current_view(), ManagerView::List);
        });
    }

    #[gpui::test]
    fn closing_the_modal_clears_is_open(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("close");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.open(cx);
            view.close(cx);
            assert!(!view.is_open());
        });
    }

    #[gpui::test]
    fn escape_closes_an_open_modal(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("escape");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.open(cx);
            let escape = KeyDownEvent {
                keystroke: Keystroke {
                    key: "escape".to_owned(),
                    key_char: None,
                    modifiers: Modifiers::default(),
                },
                is_held: false,
            };
            view.handle_modal_key_down(&escape, cx);
            assert!(!view.is_open(), "Escape must close an open modal");
        });
    }

    #[gpui::test]
    fn a_non_escape_key_does_not_close_the_modal(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("non-escape");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.open(cx);
            let enter = KeyDownEvent {
                keystroke: Keystroke {
                    key: "enter".to_owned(),
                    key_char: None,
                    modifiers: Modifiers::default(),
                },
                is_held: false,
            };
            view.handle_modal_key_down(&enter, cx);
            assert!(view.is_open(), "a non-Escape key must not close the modal");
        });
    }

    #[gpui::test]
    fn show_add_form_then_cancel_returns_to_the_list_without_persisting(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("cancel-add");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.open(cx);
            view.show_add_form(cx);
            assert_eq!(view.current_view(), ManagerView::AddForm);

            view.set_name_input("staging", cx);
            view.set_url_input("postgres://host/db", cx);
            view.cancel_add(cx);

            assert_eq!(view.current_view(), ManagerView::List);
            assert!(view.connections().is_empty());
            assert!(
                view.name_field.read(cx).value().is_empty(),
                "cancel must clear the pending name"
            );
            assert!(
                view.url_field.read(cx).value().is_empty(),
                "cancel must clear the pending url"
            );
        });

        // Nothing was ever persisted to disk.
        let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
        assert!(reloaded.connections().is_empty());
    }

    #[gpui::test]
    fn adding_a_connection_returns_the_modal_to_the_list_panel(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("add-returns-to-list");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.open(cx);
            view.show_add_form(cx);
            view.set_name_input("staging", cx);
            view.set_url_input("postgres://host/db", cx);
            view.add_connection(cx).expect("add must succeed");

            assert_eq!(view.current_view(), ManagerView::List);
            assert_eq!(view.connections().len(), 1);
            assert_eq!(view.connections()[0].connection.name, "staging");
        });
    }

    // ---- delete -------------------------------------------------------------

    #[gpui::test]
    fn deleting_a_connection_removes_it_from_the_list_and_persists(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("delete");
        let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
        store
            .add(StoredConnection {
                name: "first".to_owned(),
                url: "postgres://host/a".to_owned(),
            })
            .expect("add first");
        store
            .add(StoredConnection {
                name: "second".to_owned(),
                url: "sqlite:///tmp/b.db".to_owned(),
            })
            .expect("add second");

        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.delete_index(0, cx).expect("delete must succeed");
            assert_eq!(view.connections().len(), 1);
            assert_eq!(view.connections()[0].connection.name, "second");
        });

        let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
        assert_eq!(reloaded.connections().len(), 1);
        assert_eq!(reloaded.connections()[0].name, "second");
    }

    #[gpui::test]
    fn deleting_an_out_of_range_row_does_not_panic(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("delete-out-of-range");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, cx| {
            view.delete_index(0, cx)
                .expect("out-of-range delete is a no-op, not an error");
        });
    }

    #[gpui::test]
    async fn deleting_the_currently_active_connections_row_clears_the_active_label(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let temp = TempStorePath::new("delete-active");
        let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
        store
            .add(StoredConnection {
                name: "mem".to_owned(),
                url: "sqlite::memory:".to_owned(),
            })
            .expect("add must succeed");

        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        let task = manager.update(cx, |view, cx| view.connect_index(0, cx));
        task.await;
        manager.read_with(cx, |view, _app| {
            assert!(
                view.active().is_some(),
                "expected connect to set an active connection"
            );
        });

        manager.update(cx, |view, cx| {
            view.delete_index(0, cx).expect("delete must succeed");
            assert!(
                view.active().is_none(),
                "deleting the active row's connection must clear the active label, got {:?}",
                view.active()
            );
            assert!(view.connections().is_empty());
        });
    }

    // ---- connect_index tracks the active connection -------------------------

    #[gpui::test]
    async fn connecting_a_chosen_row_updates_the_active_connection(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let temp = TempStorePath::new("connect-active");
        let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
        store
            .add(StoredConnection {
                name: "mem".to_owned(),
                url: "sqlite::memory:".to_owned(),
            })
            .expect("add must succeed");

        let session = cx.new(|_cx| session_with_no_dsn());
        let session_for_assert = session.clone();
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        let task = manager.update(cx, |view, cx| view.connect_index(0, cx));
        task.await;

        session_for_assert.read_with(cx, |session, _app| {
            assert!(matches!(session.state(), SessionState::Connected));
        });
        manager.read_with(cx, |view, _app| {
            assert_eq!(
                view.active(),
                Some(&ActiveConnection {
                    name: "mem".to_owned(),
                    url: "sqlite::memory:".to_owned(),
                })
            );
        });
    }

    #[gpui::test]
    async fn a_failed_connect_switch_does_not_update_the_active_connection(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let temp = TempStorePath::new("connect-active-failure");
        let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
        store
            .add(StoredConnection {
                name: "bad".to_owned(),
                url: "cassandra://host/db".to_owned(),
            })
            .expect("add must succeed");

        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        let task = manager.update(cx, |view, cx| view.connect_index(0, cx));
        task.await;

        manager.read_with(cx, |view, _app| {
            assert!(
                view.active().is_none(),
                "a failed connect must not set an active connection, got {:?}",
                view.active()
            );
        });
    }

    // ---- render smoke tests (open/closed, connected/disconnected footer) ---

    #[gpui::test]
    fn the_closed_modal_renders_nothing_and_the_open_modal_renders_without_panicking(
        cx: &mut TestAppContext,
    ) {
        let temp = TempStorePath::new("render");
        let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
        store
            .add(StoredConnection {
                name: "local pg".to_owned(),
                url: "postgres://localhost/app".to_owned(),
            })
            .expect("add must succeed");
        store
            .add(StoredConnection {
                name: "local sqlite".to_owned(),
                url: "sqlite::memory:".to_owned(),
            })
            .expect("add must succeed");

        let session = cx.new(|_cx| session_with_no_dsn());
        let (manager, vcx) =
            cx.add_window_view(|_window, cx| ConnectionManagerView::new(session, store, cx));

        manager.update(vcx, ConnectionManagerView::open);
        manager.update(vcx, ConnectionManagerView::show_add_form);
        manager.update(vcx, ConnectionManagerView::cancel_add);
    }

    // ---- live-database gated switch test -------------------------------------

    /// Live-database test, gated on `ZSQL_TEST_DATABASE_URL` so `cargo test`
    /// passes with no database present. Proves the modal's row-click switch
    /// path (`connect_index`) actually drives `Session` to `Connected`
    /// against a real Postgres and updates the active-connection label to
    /// that row's saved name -- not just the sqlite-backed happy path above.
    #[gpui::test]
    async fn connecting_a_chosen_row_to_a_live_postgres_connection_updates_the_active_label_when_configured(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let Ok(url) = std::env::var("ZSQL_TEST_DATABASE_URL") else {
            eprintln!("skipping live test: ZSQL_TEST_DATABASE_URL not set");
            return;
        };

        let temp = TempStorePath::new("connect-active-live");
        let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
        store
            .add(StoredConnection {
                name: "live postgres".to_owned(),
                url: url.clone(),
            })
            .expect("add must succeed");

        let session = cx.new(|_cx| session_with_no_dsn());
        let session_for_assert = session.clone();
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        let task = manager.update(cx, |view, cx| view.connect_index(0, cx));
        task.await;

        session_for_assert.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "expected connecting the live postgres row to succeed, got {:?}",
                session.state()
            );
        });
        manager.read_with(cx, |view, _app| {
            assert_eq!(
                view.active(),
                Some(&ActiveConnection {
                    name: "live postgres".to_owned(),
                    url,
                }),
                "the active-connection label must switch to the row that was clicked"
            );
        });
    }
}
