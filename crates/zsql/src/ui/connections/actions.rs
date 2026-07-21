use gpui::{Context, Task};

use crate::connections::{ConnectionStoreError, StoredConnection};
use crate::session::{Session, SessionState};

use super::active::host_label;
use super::form::{detect_driver_id, validate_new_connection};
use super::{ActiveConnection, ConnectionManagerView, ConnectionRow};

pub(super) fn build_rows(connections: &[StoredConnection]) -> Vec<ConnectionRow> {
    connections
        .iter()
        .map(|connection| ConnectionRow {
            connection: connection.clone(),
            driver_id: detect_driver_id(&connection.url),
        })
        .collect()
}

impl ConnectionManagerView {
    /// Save a new connection with the given name and url, persist it,
    /// refresh the row list, and close the form. Rejects an empty name, an
    /// empty URL, or a URL whose scheme resolves to no registered driver
    /// without persisting anything; leaves the form open with an error
    /// message so the user can correct and retry.
    ///
    /// # Errors
    /// Returns [`ConnectionStoreError`] if the store could not be written.
    /// Input validation failures are reported through [`Self::status`]
    /// rather than this `Result`, since they never reach the store.
    #[tracing::instrument(name = "connection_manager_add", skip_all)]
    pub fn add_connection(
        &mut self,
        name: &str,
        url: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), ConnectionStoreError> {
        if let Err(message) = validate_new_connection(name, url) {
            tracing::warn!(reason = %message, "rejected invalid connection input");
            self.status = Some(message);
            cx.notify();
            return Ok(());
        }
        let connection = StoredConnection {
            name: name.to_owned(),
            url: url.to_owned(),
        };
        match self.store.add(connection) {
            Ok(()) => {
                tracing::info!(name = %name, "connection saved");
                self.rebuild_rows(cx);
                self.close_form(cx);
                self.status = Some("connection saved".to_owned());
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

    /// Save the name and url over the stored connection at `index`, in place
    /// (same position, no duplicate row appended), and close the form. Same
    /// validation as [`Self::add_connection`].
    ///
    /// # Errors
    /// Returns [`ConnectionStoreError`] if the store could not be written.
    #[tracing::instrument(name = "connection_manager_save_edit", skip_all, fields(index))]
    pub fn save_edit(
        &mut self,
        index: usize,
        name: &str,
        url: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), ConnectionStoreError> {
        if let Err(message) = validate_new_connection(name, url) {
            tracing::warn!(reason = %message, "rejected invalid connection edit");
            self.status = Some(message);
            cx.notify();
            return Ok(());
        }
        let connection = StoredConnection {
            name: name.to_owned(),
            url: url.to_owned(),
        };
        match self.store.update(index, connection) {
            Ok(()) => {
                tracing::info!(index, name = %name, "connection updated");
                self.rebuild_rows(cx);
                self.close_form(cx);
                self.status = Some("connection saved".to_owned());
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
    pub(super) fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
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
        self.status = Some("connecting...".to_string());
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

    /// Connect to the given url through the session, without persisting it to
    /// the store. Rejects an empty or unrecognized-scheme URL the same way
    /// [`validate_new_connection`] does, without touching the session. On a
    /// successful connect, closes the modal; a failed connect leaves the
    /// modal open with the error in the status line.
    #[tracing::instrument(name = "connection_manager_connect_unsaved", skip_all)]
    pub fn connect_unsaved(&mut self, url: String, cx: &mut Context<Self>) -> Task<()> {
        if let Err(reason) = detect_driver_id(&url) {
            self.status = Some(format!("Cannot connect: {reason}"));
            cx.notify();
            return Task::ready(());
        }
        let display_name = host_label(&url);
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
}
