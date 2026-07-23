//! The connection form: name/url plus driver-specific fields kept in sync
//! with the URL, the inline test-outcome banner, and the footer buttons. A
//! self-contained input component with no session or store of its own --
//! every button emits a [`ConnectionFormEvent`] for [`super::ConnectionManagerView`]
//! (which owns the session and store) to act on.

use gpui::{
    App, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable, Window, div,
    prelude::*, px, rgb, rgba,
};
use uuid::Uuid;
use zsql_core::ConnectionUrl;
use zsql_ui::{
    button::{primary_button, secondary_button},
    grid,
    text_field::{TextFieldEvent, TextFieldState},
    theme::ActiveTheme,
};

use super::{TestOutcome, driver_display_label};
use crate::{drivers::detect_driver_id, ui::theme};

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
    /// The driver's TLS query-parameter value (`sslmode` for postgres,
    /// `trustServerCertificate` for mssql).
    pub(crate) tls_field: Entity<TextFieldState>,
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
}

/// What [`ConnectionForm`] asks its parent to do, in response to a footer
/// button click or an Enter keystroke in the name/url fields. Carries no
/// name/url payload: the parent reads the live values back out through
/// [`ConnectionForm::input_values`], so they never have a chance to drift
/// out of sync with what is actually shown.
pub enum ConnectionFormEvent {
    /// The Cancel button, or Escape: discard the form and return to the
    /// list.
    Cancel,
    /// The Test button: probe the current URL without saving or connecting.
    Test,
    /// The add form's Connect button: connect to the current URL without
    /// persisting it.
    Connect,
    /// The add form's Save button, or Enter in add mode: persist a new
    /// connection.
    Add,
    /// The edit form's Save changes button, or Enter in edit mode: persist
    /// the edit to the connection with `id`.
    Edit {
        /// The connection being edited.
        id: Uuid,
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
        let tls_field = cx.new(|cx| TextFieldState::new("", None, cx));
        let sqlite_path_field =
            cx.new(|cx| TextFieldState::new("/path/to.db or :memory:", None, cx));

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
            tls_field,
            sqlite_path_field,
            parsed_url: None,
            driver_id: Err("empty URL".to_owned()),
            dim_reason: Some("empty URL".to_owned()),
            test_outcome: None,
            cancel_focus: cx.focus_handle(),
            test_focus: cx.focus_handle(),
            connect_focus: cx.focus_handle(),
            save_focus: cx.focus_handle(),
        }
    }

    /// `Enter` in the name/url fields: emits the same event the footer's
    /// Save/Save-changes button would, according to which mode the form is
    /// in.
    fn on_submit_field_event(&mut self, event: &TextFieldEvent, cx: &mut Context<Self>) {
        if !matches!(event, TextFieldEvent::Submit) {
            return;
        }
        match self.mode {
            ConnectionFormMode::Add => cx.emit(ConnectionFormEvent::Add),
            ConnectionFormMode::Edit { id } => cx.emit(ConnectionFormEvent::Edit { id }),
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
    /// `name`/`url` and every driver field parsed out of `url`.
    pub fn begin_edit(&mut self, id: Uuid, name: String, url: String, cx: &mut Context<Self>) {
        self.mode = ConnectionFormMode::Edit { id };
        self.name_field
            .update(cx, |field, _cx| field.set_value_quiet(name));
        self.url_field
            .update(cx, |field, _cx| field.set_value_quiet(url));
        self.sync_fields_from_url(cx);
        self.test_outcome = None;
        cx.notify();
    }

    /// The form's current name and URL field values.
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
    /// by the non-sqlite network drivers, appended onto `section`.
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
                    .child("Cancel")
                    .on_click(cx.listener(|_view, _event: &ClickEvent, _window, cx| {
                        cx.emit(ConnectionFormEvent::Cancel);
                    })),
            )
            .child(
                secondary_button("connection-form-test", window, cx)
                    .track_focus(&self.test_focus)
                    .child("Test")
                    .on_click(cx.listener(|_view, _event: &ClickEvent, _window, cx| {
                        cx.emit(ConnectionFormEvent::Test);
                    })),
            )
            .child(div().flex_1());

        match self.mode {
            ConnectionFormMode::Add => {
                footer = footer
                    .child(
                        secondary_button("connection-form-connect", window, cx)
                            .track_focus(&self.connect_focus)
                            .child("Connect")
                            .on_click(cx.listener(|_view, _event: &ClickEvent, _window, cx| {
                                cx.emit(ConnectionFormEvent::Connect);
                            })),
                    )
                    .child(
                        primary_button("connection-form-save", window, cx)
                            .track_focus(&self.save_focus)
                            .child("Save")
                            .on_click(cx.listener(|_view, _event: &ClickEvent, _window, cx| {
                                cx.emit(ConnectionFormEvent::Add);
                            })),
                    );
            }
            ConnectionFormMode::Edit { id } => {
                footer = footer.child(
                    primary_button("connection-form-save", window, cx)
                        .track_focus(&self.save_focus)
                        .child("Save changes")
                        .on_click(cx.listener(move |_view, _event: &ClickEvent, _window, cx| {
                            cx.emit(ConnectionFormEvent::Edit { id });
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

    /// The focusable controls in the currently-shown form, in visual
    /// top-to-bottom order
    #[must_use]
    pub fn focus_order(&self, cx: &App) -> Vec<FocusHandle> {
        let mut order = vec![
            self.name_field.read(cx).focus_handle(cx),
            self.url_field.read(cx).focus_handle(cx),
        ];
        if let Ok(driver_id) = self.driver_id.as_deref() {
            if driver_id == "sqlite" {
                order.push(self.sqlite_path_field.read(cx).focus_handle(cx));
            } else {
                order.push(self.host_field.read(cx).focus_handle(cx));
                order.push(self.port_field.read(cx).focus_handle(cx));
                order.push(self.user_field.read(cx).focus_handle(cx));
                order.push(self.password_field.read(cx).focus_handle(cx));
                order.push(self.database_field.read(cx).focus_handle(cx));
                order.push(self.tls_field.read(cx).focus_handle(cx));
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

    /// Empty every form field and reset the parsed-URL/driver-detection
    /// state to the empty-URL baseline
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

        div()
            .flex()
            .flex_col()
            .child(body)
            .child(self.render_footer(window, cx))
    }
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
