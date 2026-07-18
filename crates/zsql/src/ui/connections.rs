//! The connection manager: lists persisted connections, supports adding one
//! (name + URL, showing its auto-detected driver tag), and connecting a
//! chosen entry through the driver-selection connect path
//! ([`crate::drivers::connect`] via [`crate::session::Session::connect_to`]).
//!
//! Name/URL entry here is plain append/backspace key capture
//! (`handle_key_down` folding into `set_name_input`/`set_url_input`) rather
//! than a full cursor/IME text-editing widget like `ui::editor::EditorView`:
//! no selection, no cursor movement, no clipboard. A richer input is a
//! UI-only concern layered on top of the state transitions this module
//! already makes independently testable.

use gpui::{
    ClickEvent, Context, Div, Entity, FocusHandle, KeyDownEvent, Keystroke, Render, Stateful, Task,
    Window, div, prelude::*, px, rgb,
};
use zsql_ui::colors;

use super::theme;
use crate::connections::{ConnectionStore, ConnectionStoreError, StoredConnection};
use crate::drivers;
use crate::session::{Session, SessionState};

/// Which of the add form's two fields a key event targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputField {
    Name,
    Url,
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

/// The connection manager view: a saved-connections list plus an add form.
pub struct ConnectionManagerView {
    session: Entity<Session>,
    store: ConnectionStore,
    rows: Vec<ConnectionRow>,
    name_input: String,
    url_input: String,
    name_focus: FocusHandle,
    url_focus: FocusHandle,
    /// The most recent add/connect attempt's outcome, shown inline.
    status: Option<String>,
}

impl ConnectionManagerView {
    /// Build a manager over `session`, listing whatever `store` already
    /// holds.
    #[must_use]
    pub fn new(session: Entity<Session>, store: ConnectionStore, cx: &mut Context<Self>) -> Self {
        let rows = build_rows(store.connections());
        Self {
            session,
            store,
            rows,
            name_input: String::new(),
            url_input: String::new(),
            name_focus: cx.focus_handle(),
            url_focus: cx.focus_handle(),
            status: None,
        }
    }

    /// Every persisted connection, with its auto-detected driver tag.
    #[must_use]
    pub fn connections(&self) -> &[ConnectionRow] {
        &self.rows
    }

    /// The most recent add/connect attempt's status message, if any.
    #[must_use]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Replace the pending "name" field of the add form.
    pub fn set_name_input(&mut self, name: impl Into<String>) {
        self.name_input = name.into();
    }

    /// Replace the pending "url" field of the add form.
    pub fn set_url_input(&mut self, url: impl Into<String>) {
        self.url_input = url.into();
    }

    /// The add form's current driver tag preview, computed from
    /// `url_input` exactly as [`ConnectionRow::driver_id`] would be once
    /// saved.
    pub fn pending_driver_id(&self) -> Result<&'static str, String> {
        detect_driver_id(&self.url_input)
    }

    /// Save a new connection from the current name/url inputs, persist it,
    /// refresh the row list, and clear the inputs. Rejects an empty name, an
    /// empty URL, or a URL whose scheme resolves to no registered driver
    /// without persisting anything; leaves the inputs untouched in every
    /// failure case so the user can correct and retry.
    ///
    /// # Errors
    /// Returns [`ConnectionStoreError`] if the store could not be written.
    /// Input validation failures are reported through [`Self::status`]
    /// rather than this `Result`, since they never reach the store.
    #[tracing::instrument(name = "connection_manager_add", skip_all)]
    pub fn add_connection(&mut self, cx: &mut Context<Self>) -> Result<(), ConnectionStoreError> {
        if let Err(message) = validate_new_connection(&self.name_input, &self.url_input) {
            tracing::warn!(reason = %message, "rejected invalid connection input");
            self.status = Some(message);
            cx.notify();
            return Ok(());
        }
        let connection = StoredConnection {
            name: self.name_input.clone(),
            url: self.url_input.clone(),
        };
        match self.store.add(connection) {
            Ok(()) => {
                tracing::info!(name = %self.name_input, "connection saved");
                self.rows = build_rows(self.store.connections());
                self.name_input.clear();
                self.url_input.clear();
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

    /// Connect to the saved connection at `index` through
    /// [`Session::connect_to`], the same driver-selection path every
    /// connection in the app goes through, then -- mirroring the
    /// connect-then-introspect sequencing the app runs at startup -- follows
    /// a successful connect with [`Session::introspect`] so the schema
    /// sidebar reflects the newly chosen connection rather than staying
    /// empty or showing the previous connection's stale tree. Updates
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
                view.status = Some(match outcome {
                    Ok(()) => format!("Connected to {name}."),
                    Err(reason) => format!("Failed to connect to {name}: {reason}"),
                });
                cx.notify();
            });
        })
    }

    /// Fold a raw keystroke into `field`'s pending input: backspace pops the
    /// last character, an unmodified printable key appends its `key_char`.
    /// Anything else (arrow keys, cmd/ctrl combinations, ...) is ignored --
    /// this is deliberately not a full text-editing widget, see the module
    /// doc comment.
    fn handle_key_down(&mut self, field: InputField, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let mut target = self.field_value(field).to_owned();
        if !apply_keystroke(&mut target, &event.keystroke) {
            return;
        }
        match field {
            InputField::Name => self.set_name_input(target),
            InputField::Url => self.set_url_input(target),
        }
        cx.notify();
    }
}

/// Apply one keystroke to `target`, returning whether it changed anything.
fn apply_keystroke(target: &mut String, keystroke: &Keystroke) -> bool {
    if keystroke.key == "backspace" {
        return target.pop().is_some();
    }
    let no_editing_modifier =
        !keystroke.modifiers.control && !keystroke.modifiers.alt && !keystroke.modifiers.platform;
    match (no_editing_modifier, &keystroke.key_char) {
        (true, Some(key_char)) => {
            target.push_str(key_char);
            true
        }
        _ => false,
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .w_full()
            .flex_shrink_0()
            .bg(rgb(colors::PANEL))
            .border_b_1()
            .border_color(rgb(colors::LINE))
            .child(self.render_rows(cx))
            .child(self.render_add_form(cx))
            .child(self.render_status())
    }
}

impl ConnectionManagerView {
    /// A single click-to-focus, type-to-append field bound to `field`.
    fn render_input(
        &self,
        field: InputField,
        placeholder: &'static str,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let (value, focus_handle) = match field {
            InputField::Name => (self.name_input.clone(), self.name_focus.clone()),
            InputField::Url => (self.url_input.clone(), self.url_focus.clone()),
        };
        let label = if value.is_empty() {
            placeholder.to_owned()
        } else {
            value
        };
        let text_color = if self.field_value(field).is_empty() {
            colors::FAINT
        } else {
            colors::INK
        };

        div()
            .id(match field {
                InputField::Name => "connection-name-input",
                InputField::Url => "connection-url-input",
            })
            .track_focus(&focus_handle)
            .on_click(cx.listener(move |_view, _event: &ClickEvent, window, _cx| {
                window.focus(&focus_handle);
            }))
            .on_key_down(cx.listener(move |view, event: &KeyDownEvent, _window, cx| {
                view.handle_key_down(field, event, cx);
            }))
            .min_w(px(180.0))
            .px_2()
            .py_1()
            .bg(rgb(colors::RAISE))
            .text_color(rgb(text_color))
            .child(label)
    }

    fn field_value(&self, field: InputField) -> &str {
        match field {
            InputField::Name => &self.name_input,
            InputField::Url => &self.url_input,
        }
    }

    fn render_add_form(&self, cx: &Context<Self>) -> Div {
        let driver_preview = match self.pending_driver_id() {
            Ok(id) => id.to_owned(),
            Err(_) if self.url_input.is_empty() => String::new(),
            Err(_) => "unrecognized".to_owned(),
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .child(self.render_input(InputField::Name, "name", cx))
            .child(self.render_input(InputField::Url, "postgres://... or sqlite://...", cx))
            .child(
                div()
                    .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
                    .text_color(rgb(colors::FAINT))
                    .min_w(px(70.0))
                    .child(driver_preview),
            )
            .child(
                div()
                    .id("add-connection-button")
                    .cursor_pointer()
                    .px_2()
                    .bg(rgb(colors::RAISE))
                    .child("Add")
                    .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        let _ = view.add_connection(cx);
                    })),
            )
    }

    fn render_rows(&self, cx: &Context<Self>) -> Div {
        let mut list = div().flex().flex_col().px_2().py_1();
        for (index, row) in self.connections().iter().enumerate() {
            let driver_label = match &row.driver_id {
                Ok(id) => (*id).to_owned(),
                Err(_) => "unrecognized".to_owned(),
            };
            list = list.child(
                div()
                    .id(("connection-row", index))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .py_1()
                    .child(div().min_w(px(120.0)).child(row.connection.name.clone()))
                    .child(
                        div()
                            .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
                            .text_color(rgb(colors::FAINT))
                            .child(driver_label),
                    )
                    .child(
                        div()
                            .id(("connect-button", index))
                            .cursor_pointer()
                            .px_2()
                            .bg(rgb(colors::RAISE))
                            .child("Connect")
                            .on_click(cx.listener(
                                move |view, _event: &ClickEvent, _window, cx| {
                                    view.connect_index(index, cx).detach();
                                },
                            )),
                    ),
            );
        }
        list
    }

    fn render_status(&self) -> Div {
        div()
            .px_2()
            .pb_1()
            .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
            .text_color(rgb(colors::FAINT))
            .child(self.status().unwrap_or_default().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, Keystroke, Modifiers, TestAppContext};

    use super::{ConnectionManagerView, ConnectionStore, StoredConnection, apply_keystroke};
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
            view.set_name_input("local sqlite");
            view.set_url_input("sqlite::memory:");
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
            view.set_name_input("new db");
            view.set_url_input("sqlite::memory:");
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
            view.set_name_input("x");
            view.set_url_input("sqlite::memory:");
            view.add_connection(cx).expect("add must succeed");
            assert_eq!(view.name_input, "");
            assert_eq!(view.url_input, "");
        });
    }

    #[gpui::test]
    fn pending_driver_id_previews_the_add_forms_current_url(cx: &mut TestAppContext) {
        let temp = TempStorePath::new("preview");
        let store = ConnectionStore::load(&temp.0).expect("load must succeed");
        let session = cx.new(|_cx| session_with_no_dsn());
        let manager = cx.new(|cx| ConnectionManagerView::new(session, store, cx));

        manager.update(cx, |view, _cx| {
            view.set_url_input("postgresql://host/db");
            assert_eq!(view.pending_driver_id(), Ok("postgres"));

            view.set_url_input("nope://host");
            assert!(view.pending_driver_id().is_err());
        });
    }

    #[gpui::test]
    async fn connecting_a_chosen_row_dispatches_through_the_selected_driver(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();

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
            view.set_name_input("");
            view.set_url_input("sqlite::memory:");
            view.add_connection(cx)
                .expect("validation rejection is Ok(())");

            assert!(view.connections().is_empty());
            assert_eq!(
                view.url_input, "sqlite::memory:",
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
            view.set_name_input("new db");
            view.set_url_input("");
            view.add_connection(cx)
                .expect("validation rejection is Ok(())");

            assert!(view.connections().is_empty());
            assert_eq!(view.name_input, "new db", "inputs must be preserved");
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
            view.set_name_input("mystery");
            view.set_url_input("cassandra://host/db");
            view.add_connection(cx)
                .expect("validation rejection is Ok(())");

            assert!(view.connections().is_empty());
            assert_eq!(view.name_input, "mystery", "inputs must be preserved");
        });
    }

    #[test]
    fn apply_keystroke_backspace_pops_the_last_character() {
        let mut target = "hello".to_owned();
        let keystroke = Keystroke {
            key: "backspace".to_owned(),
            key_char: None,
            modifiers: Modifiers::default(),
        };
        assert!(apply_keystroke(&mut target, &keystroke));
        assert_eq!(target, "hell");
    }

    #[test]
    fn apply_keystroke_backspace_on_an_empty_string_returns_false() {
        let mut target = String::new();
        let keystroke = Keystroke {
            key: "backspace".to_owned(),
            key_char: None,
            modifiers: Modifiers::default(),
        };
        assert!(!apply_keystroke(&mut target, &keystroke));
        assert_eq!(target, "");
    }

    #[test]
    fn apply_keystroke_appends_an_unmodified_printable_keys_char() {
        let mut target = "ab".to_owned();
        let keystroke = Keystroke {
            key: "c".to_owned(),
            key_char: Some("c".to_owned()),
            modifiers: Modifiers::default(),
        };
        assert!(apply_keystroke(&mut target, &keystroke));
        assert_eq!(target, "abc");
    }

    #[test]
    fn apply_keystroke_rejects_a_control_chord_without_mutating_the_target() {
        let mut target = "ab".to_owned();
        let keystroke = Keystroke {
            key: "c".to_owned(),
            key_char: Some("c".to_owned()),
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
        };
        assert!(!apply_keystroke(&mut target, &keystroke));
        assert_eq!(target, "ab");
    }

    #[test]
    fn apply_keystroke_rejects_a_platform_chord_without_mutating_the_target() {
        let mut target = "ab".to_owned();
        let keystroke = Keystroke {
            key: "v".to_owned(),
            key_char: Some("v".to_owned()),
            modifiers: Modifiers {
                platform: true,
                ..Modifiers::default()
            },
        };
        assert!(!apply_keystroke(&mut target, &keystroke));
        assert_eq!(target, "ab");
    }

    #[test]
    fn apply_keystroke_rejects_an_alt_chord_without_mutating_the_target() {
        let mut target = "ab".to_owned();
        let keystroke = Keystroke {
            key: "c".to_owned(),
            key_char: Some("c".to_owned()),
            modifiers: Modifiers {
                alt: true,
                ..Modifiers::default()
            },
        };
        assert!(!apply_keystroke(&mut target, &keystroke));
        assert_eq!(target, "ab");
    }

    #[test]
    fn apply_keystroke_ignores_a_key_with_no_key_char_and_no_modifiers() {
        // e.g. an arrow key: not backspace, and there is no printable
        // character to append.
        let mut target = "ab".to_owned();
        let keystroke = Keystroke {
            key: "left".to_owned(),
            key_char: None,
            modifiers: Modifiers::default(),
        };
        assert!(!apply_keystroke(&mut target, &keystroke));
        assert_eq!(target, "ab");
    }
}
