use std::sync::Arc;
use std::time::Instant;

use gpui::{AppContext, Context, Task};
use zsql_core::Connection;

use crate::connections::{ConnectionStoreError, StoredConnection};
use crate::drivers;
use crate::session::{Session, SessionState, probe_connection};

use super::active::host_label;
use super::form::{detect_driver_id, validate_new_connection};
use super::{ActiveConnection, ConnectionManagerView, ConnectionRow, ManagerView, TestOutcome};

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
                self.status = Some("sonnection saved".to_owned());
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
