use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, Task, Window};
use zsql_core::Connection;
use zsql_ui::text_field::{TextFieldEvent, TextFieldState};

use crate::connections::StoredConnection;
use crate::drivers;
use crate::session::probe_connection;

use super::TestOutcome;

/// Whether the form is adding a new connection or editing a saved one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormMode {
    Add,
    Edit { index: usize },
}

/// What the form asks its host to do. The form never touches the store or
/// the session itself.
pub enum FormEvent {
    /// Save was clicked, or Enter was pressed in the name/url field.
    SaveRequested { name: String, url: String },
    /// Connect was clicked (add form only): connect without saving.
    ConnectRequested { url: String },
    /// Cancel was clicked.
    Cancelled,
}

/// Reject a would-be [`StoredConnection`] before it ever reaches
/// [`ConnectionStore::add`]/[`ConnectionStore::update`]: an empty name, an
/// empty URL, or a URL whose scheme resolves to no registered driver.
pub(super) fn validate_new_connection(name: &str, url: &str) -> Result<(), String> {
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
pub(super) fn detect_driver_id(url: &str) -> Result<&'static str, String> {
    let drivers = drivers::registered_drivers();
    zsql_core::select_driver(&drivers, url)
        .map(|driver| driver.id())
        .map_err(|err| err.to_string())
}

/// The branded label a driver id displays as in the UI (badge, divider,
/// list tag) -- distinct from the id itself, which stays lowercase for
/// scheme matching and query-param lookups.
pub(super) fn driver_display_label(driver_id: &str) -> &'static str {
    match driver_id {
        "postgres" => "PostgreSQL",
        "mssql" => "MSSQL",
        "sqlite" => "SQLite",
        _ => "unrecognized",
    }
}

/// The query-parameter key `zsql_core::ConnectionUrl` reads/writes for
/// `driver_id`'s TLS setting.
pub(super) fn tls_param_key(driver_id: &str) -> &'static str {
    if driver_id == "mssql" {
        "trustServerCertificate"
    } else {
        "sslmode"
    }
}

/// The field label for `driver_id`'s TLS setting.
pub(super) fn tls_param_label(driver_id: &str) -> &'static str {
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
    cx: &mut Context<ConnectionFormView>,
) {
    if field.read(cx).value().as_ref() != value {
        field.update(cx, |field, _cx| field.set_value_quiet(value));
    }
}

pub struct ConnectionFormView {
    pub(super) mode: FormMode,
    pub(super) name_field: Entity<TextFieldState>,
    pub(super) url_field: Entity<TextFieldState>,
    pub(super) host_field: Entity<TextFieldState>,
    pub(super) port_field: Entity<TextFieldState>,
    pub(super) user_field: Entity<TextFieldState>,
    pub(super) password_field: Entity<TextFieldState>,
    pub(super) database_field: Entity<TextFieldState>,
    pub(super) tls_field: Entity<TextFieldState>,
    pub(super) sqlite_path_field: Entity<TextFieldState>,
    pub(super) parsed_url: Option<zsql_core::ConnectionUrl>,
    pub(super) driver_id: Result<&'static str, String>,
    pub(super) dim_reason: Option<String>,
    pub(super) test_outcome: Option<TestOutcome>,
    pub(super) probe_timeout: Duration,
    pub(super) cancel_focus: FocusHandle,
    pub(super) test_focus: FocusHandle,
    pub(super) connect_focus: FocusHandle,
    pub(super) save_focus: FocusHandle,
}

impl EventEmitter<FormEvent> for ConnectionFormView {}

impl std::fmt::Debug for ConnectionFormView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionFormView").finish()
    }
}

impl ConnectionFormView {
    /// Create a new form for adding or editing a connection. If `prefill` is
    /// `Some`, the fields are populated from the stored connection; otherwise
    /// they start empty.
    pub fn new(
        mode: FormMode,
        prefill: Option<&StoredConnection>,
        probe_timeout: Duration,
        cx: &mut Context<Self>,
    ) -> Self {
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

        // Enter in the name/url fields emits SaveRequested.
        cx.subscribe(&name_field, |form, _field, _event: &TextFieldEvent, cx| {
            let (name, url) = form.input_values(cx);
            cx.emit(FormEvent::SaveRequested { name, url });
        })
        .detach();
        cx.subscribe(&url_field, |form, _field, _event: &TextFieldEvent, cx| {
            let (name, url) = form.input_values(cx);
            cx.emit(FormEvent::SaveRequested { name, url });
        })
        .detach();

        cx.observe(&url_field, |form, _field, cx| form.on_url_field_changed(cx))
            .detach();
        cx.observe(&host_field, |form, _field, cx| {
            form.on_host_field_changed(cx);
        })
        .detach();
        cx.observe(&port_field, |form, _field, cx| {
            form.on_port_field_changed(cx);
        })
        .detach();
        cx.observe(&user_field, |form, _field, cx| {
            form.on_user_field_changed(cx);
        })
        .detach();
        cx.observe(&password_field, |form, _field, cx| {
            form.on_password_field_changed(cx);
        })
        .detach();
        cx.observe(&database_field, |form, _field, cx| {
            form.on_database_field_changed(cx);
        })
        .detach();
        cx.observe(&tls_field, |form, _field, cx| form.on_tls_field_changed(cx))
            .detach();
        cx.observe(&sqlite_path_field, |form, _field, cx| {
            form.on_sqlite_path_field_changed(cx);
        })
        .detach();

        let mut form = Self {
            mode,
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
            probe_timeout,
            cancel_focus: cx.focus_handle(),
            test_focus: cx.focus_handle(),
            connect_focus: cx.focus_handle(),
            save_focus: cx.focus_handle(),
        };

        if let Some(connection) = prefill {
            form.name_field
                .update(cx, |field, _cx| field.set_value_quiet(&connection.name));
            form.url_field
                .update(cx, |field, _cx| field.set_value_quiet(&connection.url));
            form.sync_fields_from_url(cx);
        }

        form
    }

    /// The form's current name and URL values, read from the fields.
    pub(super) fn input_values(&self, cx: &App) -> (String, String) {
        (
            self.name_field.read(cx).value().to_string(),
            self.url_field.read(cx).value().to_string(),
        )
    }

    /// Set the name field's content. Test helper: users type into the field
    /// directly, so this is only needed to drive the field from tests.
    #[cfg(test)]
    pub(super) fn set_name_input(&mut self, name: impl AsRef<str>, cx: &mut Context<Self>) {
        self.name_field
            .update(cx, |field, cx| field.set_value(name, cx));
    }

    /// Set the URL field's content. Test helper (see [`Self::set_name_input`]).
    #[cfg(test)]
    pub(super) fn set_url_input(&mut self, url: impl AsRef<str>, cx: &mut Context<Self>) {
        self.url_field
            .update(cx, |field, cx| field.set_value(url, cx));
    }

    /// Recompute [`Self::driver_id`] (from the URL field's scheme alone) and
    /// [`Self::parsed_url`]/[`Self::dim_reason`] (from a full parse) from
    /// `url_field`'s current text, then refill every driver field the
    /// detected driver uses. The single entry point for the "URL edited ->
    /// reparse the fields" direction.
    pub(super) fn sync_fields_from_url(&mut self, cx: &mut Context<Self>) {
        let url_text = self.url_field.read(cx).value().to_string();
        self.driver_id = detect_driver_id(&url_text);

        match zsql_core::ConnectionUrl::parse(&url_text) {
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
        parsed: &zsql_core::ConnectionUrl,
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

    pub(super) fn on_url_field_changed(&mut self, cx: &mut Context<Self>) {
        self.sync_fields_from_url(cx);
    }

    pub(super) fn on_host_field_changed(&mut self, cx: &mut Context<Self>) {
        let value = self.host_field.read(cx).value().to_string();
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        if parsed.set_host(&value).is_ok() {
            self.reserialize_url(cx);
        }
    }

    pub(super) fn on_port_field_changed(&mut self, cx: &mut Context<Self>) {
        let text = self.port_field.read(cx).value().to_string();
        let text = text.trim();
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

    pub(super) fn on_user_field_changed(&mut self, cx: &mut Context<Self>) {
        let value = self.user_field.read(cx).value().to_string();
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        parsed.set_user(&value);
        self.reserialize_url(cx);
    }

    pub(super) fn on_password_field_changed(&mut self, cx: &mut Context<Self>) {
        let value = self.password_field.read(cx).value().to_string();
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        parsed.set_password(&value);
        self.reserialize_url(cx);
    }

    pub(super) fn on_database_field_changed(&mut self, cx: &mut Context<Self>) {
        let value = self.database_field.read(cx).value().to_string();
        let Some(parsed) = self.parsed_url.as_mut() else {
            return;
        };
        parsed.set_database(&value);
        self.reserialize_url(cx);
    }

    pub(super) fn on_tls_field_changed(&mut self, cx: &mut Context<Self>) {
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

    pub(super) fn on_sqlite_path_field_changed(&mut self, cx: &mut Context<Self>) {
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

    /// The Test button's most recent (or in-flight) outcome, if any.
    #[must_use]
    pub fn test_outcome(&self) -> Option<&TestOutcome> {
        self.test_outcome.as_ref()
    }

    /// Open a real connection to the form's current URL and ping it, on
    /// [`Self::probe_timeout`], without saving anything or touching the
    /// session's active connection. Updates [`Self::test_outcome`] with
    /// `Pending` immediately, then the final result once the attempt
    /// settles.
    #[tracing::instrument(name = "form_test", skip_all)]
    pub fn run_test(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let url = self.url_field.read(cx).value().to_string();
        if let Err(reason) = detect_driver_id(&url) {
            self.test_outcome = Some(TestOutcome::Failed(reason));
            cx.notify();
            return Task::ready(());
        }
        tracing::info!("form test starting");
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
            tracing::info!(?outcome, "form test finished");
            let _ = this.update(cx, |form, cx| {
                form.test_outcome = Some(outcome);
                cx.notify();
            });
        })
    }

    /// The focusable controls in the form, in visual top-to-bottom order:
    /// Name, URL, then the driver-specific fields (whichever set the detected
    /// driver picks), then the footer buttons left-to-right.
    pub(super) fn focus_order(&self, cx: &App) -> Vec<FocusHandle> {
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
        match self.mode {
            FormMode::Add => {
                order.push(self.connect_focus.clone());
                order.push(self.save_focus.clone());
            }
            FormMode::Edit { .. } => order.push(self.save_focus.clone()),
        }
        order
    }

    /// Move focus to the next (or, if `backward`, previous) control in
    /// [`Self::focus_order`], wrapping past either end. A no-op if nothing
    /// in the form currently holds focus and the list to search is empty.
    pub(super) fn move_focus(&self, backward: bool, window: &mut Window, cx: &Context<Self>) {
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
}
