use gpui::{Context, Entity};
use zsql_core::ConnectionUrl;
use zsql_ui::text_field::TextFieldState;

use crate::drivers;

use super::{ConnectionManagerView, ManagerView};

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
    cx: &mut Context<ConnectionManagerView>,
) {
    if field.read(cx).value().as_ref() != value {
        field.update(cx, |field, _cx| field.set_value_quiet(value));
    }
}

// ---- Form show/hide/input helpers -----------------------------------

impl ConnectionManagerView {
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
    pub(super) fn clear_inputs(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn input_values(&self, cx: &Context<Self>) -> (String, String) {
        (
            self.name_field.read(cx).value().to_string(),
            self.url_field.read(cx).value().to_string(),
        )
    }

    /// `Enter` in the name/url fields: submits the add form, or saves an
    /// edit, according to which panel is open.
    pub(super) fn submit_form(&mut self, cx: &mut Context<Self>) {
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
}

// ---- URL <-> fields sync -----------------------------------------------

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
    pub(super) fn sync_fields_from_url(&mut self, cx: &mut Context<Self>) {
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
}
