//! The connection form: name/url plus driver-specific fields kept in sync
//! with the URL, the inline test-outcome banner, and the footer buttons. A
//! self-contained input component with no session or store of its own --
//! every button emits a [`ConnectionFormEvent`] for [`super::ConnectionManagerView`]
//! (which owns the session and store) to act on.
//!
//! The SSH tunnel section ([`ssh`]), the per-driver TLS control ([`tls`]),
//! and the one-vs-two-column layout it opens ([`layout`]) are split into
//! their own submodules to keep this file under the project's line-count
//! convention; all three extend [`ConnectionForm`] with `impl` blocks
//! rather than owning a separate type, since their state and rendering are
//! as much a part of the form as the driver fields above them.

use gpui::{
    App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, Window, div,
    prelude::*, px, rgb, rgba,
};
use uuid::Uuid;
use zsql_core::ConnectionUrl;
use zsql_ui::{
    button::{primary_button, secondary_button},
    text_field::{TextFieldEvent, TextFieldState},
    theme::ActiveTheme,
};

use super::TestOutcome;
use crate::connections::{SshAuthKind, StoredSsh};
use crate::{
    drivers::{detect_driver_id, is_network},
    ui::{connections::driver_display_label, theme},
};

mod layout;
mod ssh;
mod tls;

pub(crate) use layout::FormColumns;
pub(crate) use ssh::HostKeyMode;

/// The connection form's input fields, test-outcome banner, and footer.
pub struct ConnectionForm {
    mode: ConnectionFormMode,
    /// The form's name field.
    pub(crate) name_field: Entity<TextFieldState>,
    /// The form's URL field: the single source of truth every driver field
    /// below is parsed from and reserialized into.
    pub(crate) url_field: Entity<TextFieldState>,
    pub(crate) host_field: Entity<TextFieldState>,
    pub(crate) port_field: Entity<TextFieldState>,
    pub(crate) user_field: Entity<TextFieldState>,
    /// Masked by default; see [`Self::toggle_password_visible`].
    pub(crate) password_field: Entity<TextFieldState>,
    pub(crate) database_field: Entity<TextFieldState>,
    /// Focus target for the TLS-verification mode control (see [`tls`]),
    /// which has no text field of its own to carry a focus handle.
    pub(crate) tls_focus: FocusHandle,
    /// `SQLite`'s single field: a file path (or `:memory:`).
    pub(crate) sqlite_path_field: Entity<TextFieldState>,
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
    /// The Test button's most recent (or in-flight) outcome, if any has run
    /// since the form was last opened.
    test_outcome: Option<TestOutcome>,
    pub(crate) cancel_focus: FocusHandle,
    pub(crate) test_focus: FocusHandle,
    pub(crate) connect_focus: FocusHandle,
    pub(crate) save_focus: FocusHandle,

    // -- SSH tunnel section (see `ssh`) --------------------------------
    //
    // Independent of `parsed_url`/`ConnectionUrl`: these settings are not
    // part of the connection URL and are never written into it.
    /// Whether the SSH tunnel is used when connecting.
    pub(crate) ssh_enabled: bool,
    pub(crate) ssh_enabled_focus: FocusHandle,
    pub(crate) ssh_host_field: Entity<TextFieldState>,
    pub(crate) ssh_port_field: Entity<TextFieldState>,
    pub(crate) ssh_user_field: Entity<TextFieldState>,
    pub(crate) ssh_auth_kind: SshAuthKind,
    pub(crate) ssh_auth_focus: FocusHandle,
    pub(crate) ssh_password_field: Entity<TextFieldState>,
    pub(crate) ssh_key_path_field: Entity<TextFieldState>,
    pub(crate) ssh_key_passphrase_field: Entity<TextFieldState>,
    pub(crate) ssh_host_key_mode: HostKeyMode,
    pub(crate) ssh_host_key_focus: FocusHandle,
    pub(crate) ssh_known_hosts_path_field: Entity<TextFieldState>,
}

/// What [`ConnectionForm`] asks its parent to do, in response to a footer
/// button click or an Enter keystroke in the name/url fields
pub enum ConnectionFormEvent {
    /// The form has been discarded
    Cancel,
    /// The test action has been triggered for a URL
    Test { url: String },
    /// Connect to the URL without persisting it.
    Connect { name: String, url: String },
    /// Persist a new connection
    Add { name: String, url: String },
    /// Persist changes to a connection
    Edit {
        /// The connection being edited.
        id: Uuid,
        name: String,
        url: String,
    },
}

/// Whether the form is creating a new connection or editing an existing
/// one -- and, for the latter, which one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionFormMode {
    /// A brand-new connection, not yet persisted.
    Add,
    /// An existing connection, identified by its stored id.
    Edit {
        /// The connection being edited.
        id: Uuid,
    },
}

impl EventEmitter<ConnectionFormEvent> for ConnectionForm {}

impl ConnectionForm {
    #[must_use]
    pub fn new(cx: &mut Context<Self>) -> Self {
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
        let sqlite_path_field =
            cx.new(|cx| TextFieldState::new("/path/to.db or :memory:", None, cx));

        let ssh_host_field = cx.new(|cx| TextFieldState::new("ssh host", None, cx));
        let ssh_port_field = cx.new(|cx| {
            TextFieldState::new(crate::config::DEFAULT_SSH_TUNNEL_PORT.to_string(), None, cx)
        });
        let ssh_user_field = cx.new(|cx| TextFieldState::new("ssh user", None, cx));
        let ssh_password_field = cx.new(|cx| {
            let mut field = TextFieldState::new("ssh password", None, cx);
            field.set_masked(true, cx);
            field
        });
        let ssh_key_path_field = cx.new(|cx| TextFieldState::new("/path/to/key", None, cx));
        let ssh_key_passphrase_field = cx.new(|cx| {
            let mut field = TextFieldState::new("key passphrase", None, cx);
            field.set_masked(true, cx);
            field
        });
        let ssh_known_hosts_path_field =
            cx.new(|cx| TextFieldState::new("/path/to/known_hosts", None, cx));

        cx.observe(&url_field, |view, _field, cx| view.on_url_field_changed(cx))
            .detach();
        cx.observe(&host_field, Self::on_host_field_changed)
            .detach();
        cx.observe(&port_field, Self::on_port_field_changed)
            .detach();
        cx.observe(&user_field, Self::on_user_field_changed)
            .detach();
        cx.observe(&password_field, Self::on_password_field_changed)
            .detach();
        cx.observe(&database_field, Self::on_database_field_changed)
            .detach();
        cx.observe(&sqlite_path_field, Self::on_sqlite_path_field_changed)
            .detach();

        cx.subscribe(&name_field, |view, _field, event, cx| {
            view.on_submit_field_event(event, cx);
        })
        .detach();
        cx.subscribe(&url_field, |view, _field, event, cx| {
            view.on_submit_field_event(event, cx);
        })
        .detach();

        Self {
            mode: ConnectionFormMode::Add,
            name_field,
            url_field,
            host_field,
            port_field,
            user_field,
            password_field,
            database_field,
            tls_focus: cx.focus_handle(),
            sqlite_path_field,
            parsed_url: None,
            driver_id: Err("empty URL".to_owned()),
            dim_reason: Some("empty URL".to_owned()),
            test_outcome: None,
            cancel_focus: cx.focus_handle(),
            test_focus: cx.focus_handle(),
            connect_focus: cx.focus_handle(),
            save_focus: cx.focus_handle(),

            ssh_enabled: false,
            ssh_enabled_focus: cx.focus_handle(),
            ssh_host_field,
            ssh_port_field,
            ssh_user_field,
            ssh_auth_kind: SshAuthKind::Agent,
            ssh_auth_focus: cx.focus_handle(),
            ssh_password_field,
            ssh_key_path_field,
            ssh_key_passphrase_field,
            ssh_host_key_mode: HostKeyMode::AcceptNew,
            ssh_host_key_focus: cx.focus_handle(),
            ssh_known_hosts_path_field,
        }
    }

    /// `Enter` in the name/url fields: emits the same event the footer's
    /// Save/Save-changes button would, according to which mode the form is
    /// in.
    fn on_submit_field_event(&mut self, event: &TextFieldEvent, cx: &mut Context<Self>) {
        if !matches!(event, TextFieldEvent::Submit) {
            return;
        }

        let (name, url) = self.input_values(cx);
        match self.mode {
            ConnectionFormMode::Add => cx.emit(ConnectionFormEvent::Add { name, url }),
            ConnectionFormMode::Edit { id } => cx.emit(ConnectionFormEvent::Edit { id, name, url }),
        }
    }

    /// Reset the form to an empty add-connection form: clears every input
    /// and the test-outcome banner.
    pub fn begin_add(&mut self, cx: &mut Context<Self>) {
        self.mode = ConnectionFormMode::Add;
        self.clear_inputs(cx);
        self.test_outcome = None;
        cx.notify();
    }

    /// Reset the form to an edit form for connection `id`, prefilled with
    /// `name`/`url` and every driver field parsed out of `url`, plus `ssh`/
    /// `ssh_secret` in the SSH section (both `None` for a connection with no
    /// tunnel configured, or one saved before SSH tunnel support existed).
    pub fn begin_edit(
        &mut self,
        id: Uuid,
        name: String,
        url: String,
        ssh: Option<StoredSsh>,
        ssh_secret: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.mode = ConnectionFormMode::Edit { id };
        self.name_field
            .update(cx, |field, _cx| field.set_value_quiet(name));
        self.url_field
            .update(cx, |field, _cx| field.set_value_quiet(url));
        self.sync_fields_from_url(cx);
        self.apply_ssh_state(ssh, ssh_secret, cx);
        self.test_outcome = None;
        cx.notify();
    }

    /// The form's current name and URL field values.
    ///
    /// Returns (name, url)
    #[must_use]
    pub fn input_values(&self, cx: &App) -> (String, String) {
        let name = self.name_field.read(cx).value().to_string();
        let url = self.url_field.read(cx).value().to_string();
        (name, url)
    }

    /// Whether the form is editing an existing connection, as opposed to
    /// creating a new one.
    #[must_use]
    pub fn is_edit(&self) -> bool {
        matches!(self.mode, ConnectionFormMode::Edit { .. })
    }

    /// The id of the connection being edited, if the form is in edit mode.
    /// Test helper: production code branches on [`Self::is_edit`] alone,
    /// since it never needs the id without also needing the manager's own
    /// row/store access to act on it.
    #[cfg(test)]
    pub(crate) fn edit_id(&self) -> Option<Uuid> {
        match self.mode {
            ConnectionFormMode::Edit { id } => Some(id),
            ConnectionFormMode::Add => None,
        }
    }

    /// The name field's focus handle, so a caller (e.g. the list panel's
    /// "Add connection" button) can focus it once the form is shown.
    #[must_use]
    pub fn name_focus_handle(&self, cx: &App) -> FocusHandle {
        self.name_field.read(cx).focus_handle(cx)
    }

    /// The Test button's most recent (or in-flight) outcome, if any.
    #[must_use]
    pub fn test_outcome(&self) -> Option<&TestOutcome> {
        self.test_outcome.as_ref()
    }

    /// Set the Test button's outcome, replacing whatever it previously
    /// showed. The caller (which owns the session and runs the actual
    /// probe) calls this with `Some(TestOutcome::Pending)` immediately, then
    /// with the final outcome once the attempt settles.
    pub fn set_test_outcome(&mut self, outcome: Option<TestOutcome>, cx: &mut Context<Self>) {
        self.test_outcome = outcome;
        cx.notify();
    }

    /// The divider + driver-specific fields, or a plain hint when the URL's
    /// scheme is not (yet) recognized at all.
    fn render_driver_field_section(
        &self,
        driver_label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
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
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(driver_label.to_uppercase()),
            )
            .child(div().flex_1().h(px(1.0)).bg(rgb(colors.border_soft)));

        let mut section = div()
            .flex()
            .flex_col()
            .gap(theme::CONNECTION_FORM_FIELD_GAP)
            .when(self.dim_reason().is_some(), |el| {
                el.opacity(theme::CONNECTION_FORM_DIM_OPACITY)
            });

        section = if Self::is_network(driver_id) {
            self.render_network_fields(section, driver_id, colors, window, cx)
        } else {
            section.child(Self::labeled_field(
                "Database file",
                colors,
                self.sqlite_path_field.clone(),
            ))
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

    /// The Host/Port, User/Password, Database, and TLS fields shared by the
    /// non-sqlite network drivers, appended onto `section`. The SSH tunnel
    /// section is appended inline here only while it renders as a trailing
    /// row rather than its own column (see [`Self::form_columns`]) -- the
    /// two-column layout renders it separately once it does.
    fn render_network_fields(
        &self,
        section: Div,
        driver_id: &str,
        colors: zsql_ui::theme::Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let section = section
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
            .child(self.render_tls_control(driver_id, colors, window, cx));

        if self.ssh_enabled {
            section
        } else {
            section.child(self.render_ssh_section(colors, window, cx))
        }
    }

    /// A read-only line listing any query parameters the URL carries beyond
    /// `driver_id`'s own TLS param(s) -- parts a field edit still preserves
    /// (see [`zsql_core::ConnectionUrl::extra_query_params`]) but has no
    /// field of its own to show, so they are surfaced here instead of
    /// silently hidden. `None` for sqlite (no query params) or when there
    /// are none to show.
    fn render_extra_query_params_line(
        &self,
        driver_id: &str,
        colors: zsql_ui::theme::Colors,
    ) -> Option<Div> {
        if !Self::is_network(driver_id) {
            return None;
        }
        let parsed = self.parsed_url.as_ref()?;
        let extras = parsed.extra_query_params(&tls::known_query_keys(driver_id));
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
                theme::connection_test_pending_bg(active_theme),
                colors.status_warn,
                "Testing...".to_owned(),
            ),
            TestOutcome::Connected { elapsed_ms } => (
                theme::connection_test_ok_bg(active_theme),
                colors.accent,
                format!("Connected - {elapsed_ms} ms"),
            ),
            TestOutcome::Failed(message) => (
                theme::connection_test_error_bg(active_theme),
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
            .overflow_hidden()
            .child(div().min_w_0().child(text))
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
                            .on_click(cx.listener(|view, _event, _window, cx| {
                                view.toggle_password_visible(cx);
                            })),
                    ),
            )
            .child(self.password_field.clone())
    }

    fn render_footer(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let colors = cx.theme().colors;

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
                secondary_button("connection-form-cancel", window, cx)
                    .track_focus(&self.cancel_focus)
                    // Lets render tests find this button's painted bounds via
                    // `VisualTestContext::debug_bounds` -- a no-op outside
                    // test/test-support builds.
                    .debug_selector(|| "connection-form-cancel".to_owned())
                    .child("Cancel")
                    .on_click(cx.listener(|_view, _event: &ClickEvent, _window, cx| {
                        cx.emit(ConnectionFormEvent::Cancel);
                    })),
            )
            .child(
                secondary_button("connection-form-test", window, cx)
                    .track_focus(&self.test_focus)
                    .debug_selector(|| "connection-form-test".to_owned())
                    .child("Test")
                    .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        cx.emit(ConnectionFormEvent::Test {
                            url: view.input_values(cx).1,
                        });
                    })),
            )
            .child(div().flex_1());

        match self.mode {
            ConnectionFormMode::Add => {
                footer = footer
                    .child(
                        secondary_button("connection-form-connect", window, cx)
                            .track_focus(&self.connect_focus)
                            .debug_selector(|| "connection-form-connect".to_owned())
                            .child("Connect")
                            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                                let (name, url) = view.input_values(cx);
                                cx.emit(ConnectionFormEvent::Connect { name, url });
                            })),
                    )
                    .child(
                        primary_button("connection-form-save", window, cx)
                            .track_focus(&self.save_focus)
                            .debug_selector(|| "connection-form-save".to_owned())
                            .child("Save")
                            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                                let (name, url) = view.input_values(cx);
                                cx.emit(ConnectionFormEvent::Add { name, url });
                            })),
                    );
            }
            ConnectionFormMode::Edit { id } => {
                footer = footer.child(
                    primary_button("connection-form-save", window, cx)
                        .track_focus(&self.save_focus)
                        .debug_selector(|| "connection-form-save".to_owned())
                        .child("Save changes")
                        .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                            let (name, url) = view.input_values(cx);
                            cx.emit(ConnectionFormEvent::Edit { id, name, url });
                        })),
                );
            }
        }

        footer
    }

    /// A field's caption label, in the small uppercase style every field in
    /// the form shares: tertiary color, semibold, letters upper-cased (gpui
    /// has no letter-spacing, so the tracking in the design is dropped).
    fn field_label(text: impl Into<String>, colors: zsql_ui::theme::Colors) -> Div {
        div()
            .text_size(px(theme::CONNECTION_FORM_LABEL_TEXT_SIZE))
            .text_color(rgb(colors.text_tertiary))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(text.into().to_uppercase())
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

    /// Recompute [`Self::driver_id`] (from the URL field's scheme alone) and
    /// [`Self::parsed_url`]/[`Self::dim_reason`] (from a full parse) from
    /// `url_field`'s current text, then refill every driver field the
    /// detected driver uses
    fn sync_fields_from_url(&mut self, cx: &mut Context<Self>) {
        let url_text = self.url_field.read(cx).value().to_string();
        self.driver_id = detect_driver_id(&url_text).map_err(|err| err.to_string());

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
        if Self::is_network(driver_id) {
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
            // The TLS mode has no field of its own to sync: it is read
            // straight off `parsed_url` wherever it renders.
        } else {
            let path = parsed.sqlite_path().unwrap_or_default();
            set_field_value_if_changed(&self.sqlite_path_field, path, cx);
        }
    }

    /// The driver id the form's current URL field detects, purely from its
    /// scheme (see [`detect_driver_id`]) -- this is what picks the visible
    /// field layout, independent of whether the full URL currently parses.
    pub fn pending_driver_id(&self) -> Result<&'static str, String> {
        self.driver_id.clone()
    }

    /// Whether `driver_id` is a network driver. If not detected, we default
    /// to `true`.
    pub fn is_network(driver_id: &str) -> bool {
        is_network(driver_id).unwrap_or(true)
    }

    /// Why the driver-field section is currently dimmed, if it is.
    #[must_use]
    pub fn dim_reason(&self) -> Option<&str> {
        self.dim_reason.as_deref()
    }

    /// The focusable controls in the currently-shown form, in visual
    /// top-to-bottom order
    #[must_use]
    pub fn focus_order(&self, cx: &App) -> Vec<FocusHandle> {
        let mut order = vec![
            self.name_field.read(cx).focus_handle(cx),
            self.url_field.read(cx).focus_handle(cx),
        ];
        if let Ok(driver_id) = self.driver_id.as_deref() {
            if Self::is_network(driver_id) {
                order.push(self.host_field.read(cx).focus_handle(cx));
                order.push(self.port_field.read(cx).focus_handle(cx));
                order.push(self.user_field.read(cx).focus_handle(cx));
                order.push(self.password_field.read(cx).focus_handle(cx));
                order.push(self.database_field.read(cx).focus_handle(cx));
                order.push(self.tls_focus.clone());
                self.push_ssh_focus_order(&mut order, cx);
            } else {
                order.push(self.sqlite_path_field.read(cx).focus_handle(cx));
            }
        }
        order.push(self.cancel_focus.clone());
        order.push(self.test_focus.clone());
        match self.mode {
            ConnectionFormMode::Add => {
                order.push(self.connect_focus.clone());
                order.push(self.save_focus.clone());
            }
            ConnectionFormMode::Edit { .. } => order.push(self.save_focus.clone()),
        }
        order
    }

    /// Empty every form field and reset the parsed-URL/driver-detection and
    /// SSH-section state to the empty-URL baseline
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
        self.sqlite_path_field
            .update(cx, |field, _cx| field.set_value_quiet(""));
        self.parsed_url = None;
        self.driver_id = Err("empty URL".to_owned());
        self.dim_reason = Some("empty URL".to_owned());
        self.reset_ssh_state(cx);
        cx.notify();
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

    // `Context::observe`'s callback signature hands the observed entity by
    // value, not by reference.
    #[allow(clippy::needless_pass_by_value)]
    fn on_host_field_changed(&mut self, field: Entity<TextFieldState>, cx: &mut Context<Self>) {
        let value = field.read(cx).value().to_string();
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        if parsed.set_host(&value).is_ok() {
            self.reserialize_url(cx);
        }
    }

    // `Context::observe`'s callback signature hands the observed entity by
    // value, not by reference.
    #[allow(clippy::needless_pass_by_value)]
    fn on_port_field_changed(&mut self, field: Entity<TextFieldState>, cx: &mut Context<Self>) {
        let text = field.read(cx).value().to_string();
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

    // `Context::observe`'s callback signature hands the observed entity by
    // value, not by reference.
    #[allow(clippy::needless_pass_by_value)]
    fn on_user_field_changed(&mut self, field: Entity<TextFieldState>, cx: &mut Context<Self>) {
        let value = field.read(cx).value().to_string();
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        parsed.set_user(&value);
        self.reserialize_url(cx);
    }

    // `Context::observe`'s callback signature hands the observed entity by
    // value, not by reference.
    #[allow(clippy::needless_pass_by_value)]
    fn on_password_field_changed(&mut self, field: Entity<TextFieldState>, cx: &mut Context<Self>) {
        let value = field.read(cx).value().to_string();
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        parsed.set_password(&value);
        self.reserialize_url(cx);
    }

    // `Context::observe`'s callback signature hands the observed entity by
    // value, not by reference.
    #[allow(clippy::needless_pass_by_value)]
    fn on_database_field_changed(&mut self, field: Entity<TextFieldState>, cx: &mut Context<Self>) {
        let value = field.read(cx).value().to_string();
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        parsed.set_database(&value);
        self.reserialize_url(cx);
    }

    // `Context::observe`'s callback signature hands the observed entity by
    // value, not by reference.
    #[allow(clippy::needless_pass_by_value)]
    fn on_sqlite_path_field_changed(
        &mut self,
        field: Entity<TextFieldState>,
        cx: &mut Context<Self>,
    ) {
        let value = field.read(cx).value().to_string();
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

    /// Set the name field's content. Test helper: users type into the field
    /// directly, so this is only needed to drive the field from tests.
    #[cfg(test)]
    pub(crate) fn set_name_input(&mut self, name: impl AsRef<str>, cx: &mut Context<Self>) {
        self.name_field
            .update(cx, |field, cx| field.set_value(name, cx));
    }

    /// Set the URL field's content. Test helper (see [`Self::set_name_input`]).
    #[cfg(test)]
    pub(crate) fn set_url_input(&mut self, url: impl AsRef<str>, cx: &mut Context<Self>) {
        self.url_field
            .update(cx, |field, cx| field.set_value(url, cx));
    }
}

impl Render for ConnectionForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let url_is_empty = self.url_field.read(cx).value().is_empty();
        let driver_label = match self.pending_driver_id() {
            Ok(id) => driver_display_label(id).to_owned(),
            Err(_) if url_is_empty => String::new(),
            Err(_) => "unrecognized".to_owned(),
        };

        let body = match self.form_columns() {
            FormColumns::Single => {
                self.render_single_column_body(&driver_label, colors, window, cx)
            }
            FormColumns::Two => self.render_two_column_body(&driver_label, window, cx),
        };

        div()
            .flex()
            .flex_col()
            .child(body)
            .child(self.render_footer(window, cx))
    }
}

/// Set `field`'s displayed value to `value` only if it currently differs,
/// quietly (see [`TextFieldState::set_value_quiet`])
fn set_field_value_if_changed(
    field: &Entity<TextFieldState>,
    value: &str,
    cx: &mut Context<ConnectionForm>,
) {
    if field.read(cx).value().as_ref() != value {
        field.update(cx, |field, _cx| field.set_value_quiet(value));
    }
}

#[cfg(test)]
mod tests;
