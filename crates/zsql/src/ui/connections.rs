//! The connection manager: a centered modal (opened from
//! [`super::footer::ConnectionFooterView`]) that lists persisted
//! connections and offers a sectioned add/edit form. The form is managed
//! as a separate [`ConnectionFormView`] entity; the manager coordinates
//! store operations, session interactions, and panel switching.

use std::time::Duration;

use gpui::{
    ClickEvent, Context, Div, Entity, FocusHandle, KeyDownEvent, Render, Window, div, prelude::*,
    px, rgb, rgba,
};
use zsql_ui::theme::ActiveTheme;

use crate::connections::{ConnectionStore, StoredConnection};
use crate::session::Session;
use crate::tab_session::ConnectionKey;

mod active;
pub use active::{ActiveConnection, FooterDisplay, active_connection_for_url, footer_display};

mod form;
pub use form::{ConnectionFormView, FormEvent, FormMode};

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

/// The connection-manager modal's state: a saved-connections list, a form
/// entity (when open), and whether the modal is currently open at all.
pub struct ConnectionManagerView {
    session: Entity<Session>,
    store: ConnectionStore,
    rows: Vec<ConnectionRow>,
    /// Per-row focus handles, rebuilt alongside `rows` so `Enter` on a
    /// focused row can connect-and-close the same as clicking it.
    row_focus_handles: Vec<FocusHandle>,
    /// The form entity, if a form panel is currently open.
    form: Option<Entity<ConnectionFormView>>,
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
        Self {
            session,
            store,
            row_focus_handles: rows.iter().map(|_| cx.focus_handle()).collect(),
            rows,
            form: None,
            modal_focus: cx.focus_handle(),
            open: false,
            view: ManagerView::List,
            active: None,
            status: None,
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

    /// The form entity, if one is open, for tests that need to inspect or
    /// drive the form directly.
    #[cfg(test)]
    pub(super) fn form(&self) -> Option<&Entity<ConnectionFormView>> {
        self.form.as_ref()
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
    /// form and footer buttons in visual order. Every other key is ignored
    /// here (the text fields handle their own keys, and an `Enter` in the
    /// name/url fields submits via their `Submit` event).
    pub fn handle_modal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "escape" => self.close(cx),
            "tab" => {
                if let Some(form) = &self.form {
                    form.update(cx, |form, cx| {
                        form.move_focus(event.keystroke.modifiers.shift, window, cx);
                    });
                }
            }
            _ => {}
        }
    }

    /// Replace the tracked active connection, e.g. after a successful
    /// connect (see [`Self::connect_index`]) or at startup once
    /// [`Session::connect`]'s fallback URL resolves (see
    /// [`active_connection_for_url`]).
    pub fn set_active(&mut self, active: Option<ActiveConnection>, cx: &mut Context<Self>) {
        self.active = active;
        cx.notify();
    }

    /// Create and open the add-connection form.
    pub fn show_add_form(&mut self, cx: &mut Context<Self>) {
        self.view = ManagerView::AddForm;
        let form =
            cx.new(|cx| ConnectionFormView::new(FormMode::Add, None, self.probe_timeout, cx));
        self.open_form(form, cx);
    }

    /// Create and open the edit-connection form for the connection at `index`,
    /// pre-filled from its stored name/url.
    pub fn show_edit_form(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            tracing::warn!(index, "edit requested for an out-of-range row");
            return;
        };
        let prefill = row.connection.clone();
        self.view = ManagerView::EditForm { index };
        let form = cx.new(|cx| {
            ConnectionFormView::new(
                FormMode::Edit { index },
                Some(&prefill),
                self.probe_timeout,
                cx,
            )
        });
        self.open_form(form, cx);
    }

    /// Wire a freshly created form's events into the store/session handlers
    /// and show it, clearing any stale status from the previous panel.
    fn open_form(&mut self, form: Entity<ConnectionFormView>, cx: &mut Context<Self>) {
        self.status = None;
        cx.subscribe(&form, |manager, _form, event: &FormEvent, cx| match event {
            FormEvent::Cancelled => manager.close_form(cx),
            FormEvent::SaveRequested { name, url } => match manager.view {
                ManagerView::EditForm { index } => {
                    let _ = manager.save_edit(index, name, url, cx);
                }
                _ => {
                    let _ = manager.add_connection(name, url, cx);
                }
            },
            FormEvent::ConnectRequested { url } => {
                manager.connect_unsaved(url.clone(), cx).detach();
            }
        })
        .detach();
        self.form = Some(form);
        cx.notify();
    }

    /// Close the open form, return to the list panel.
    pub(super) fn close_form(&mut self, cx: &mut Context<Self>) {
        self.view = ManagerView::List;
        self.form = None;
        cx.notify();
    }

    /// Render the status line at the bottom of the list panel.
    pub(super) fn render_status(&self, cx: &Context<Self>) -> Div {
        let text = self
            .status()
            .unwrap_or("click a row to connect\t•\tesc to close");
        div()
            .text_size(px(style::MODAL_ROW_URL_TEXT_SIZE))
            .text_color(rgb(cx.theme().colors.text_tertiary))
            .child(text.to_owned())
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
            .occlude()
            .track_focus(&self.modal_focus)
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.handle_modal_key_down(event, window, cx);
            }))
            .on_click(cx.listener(|_view, _event: &ClickEvent, _window, cx| {
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
                    .on_click(cx.listener(|_view, _event: &ClickEvent, _window, cx| {
                        cx.stop_propagation();
                    }))
                    .child(self.render_modal_head(cx))
                    .child(match self.current_view() {
                        ManagerView::List => self.render_modal_list(window, cx).into_any_element(),
                        ManagerView::AddForm | ManagerView::EditForm { .. } => {
                            if let Some(form) = &self.form {
                                form.clone().into_any_element()
                            } else {
                                div().into_any_element()
                            }
                        }
                    }),
            )
    }
}

#[cfg(test)]
mod tests;
