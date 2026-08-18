use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, KeyDownEvent, Keystroke, Modifiers, TestAppContext};
use zsql_ui::modal::ModalSize;

use super::{ConnectionManagerView, ConnectionStore, ManagerView, TestOutcome, host_label};
use crate::{
    connections::{ConnectionArgs, SshAuthKind},
    session::{Session, SessionState},
    ui::connections::form::ConnectionFormEvent,
};

/// The liveness probe timeout every test builds its manager with, unless a
/// test specifically cares about the value itself.
fn test_probe_timeout() -> Duration {
    crate::config::Config::default().liveness.probe_timeout()
}

/// The batch size every test builds its manager with, unless a test
/// specifically cares about the value itself.
fn test_batch_size() -> usize {
    crate::config::Config::default().query.batch_size
}

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

fn session_with_no_url() -> Session {
    Session::new(&crate::config::Config::default())
}

fn new_manager(
    cx: &mut Context<ConnectionManagerView>,
    session: Entity<Session>,
    store: ConnectionStore,
) -> ConnectionManagerView {
    ConnectionManagerView::new(session, store, test_probe_timeout(), test_batch_size(), cx)
}

#[gpui::test]
fn a_freshly_loaded_store_lists_every_saved_connection(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("list");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "local pg".to_owned(),
            url: "postgres://localhost/app".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");
    store
        .add(ConnectionArgs {
            name: "local sqlite".to_owned(),
            url: "sqlite::memory:".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");

    let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, reloaded));

    manager.read_with(cx, |view, _app| {
        let names: Vec<&str> = view
            .connections()
            .iter()
            .map(|row| row.connection.name.as_str())
            .collect();
        assert_eq!(names, vec!["local pg", "local sqlite"]);

        assert_eq!(view.connections()[0].connection.display_kind, "PostgreSQL");
        assert_eq!(view.connections()[1].connection.display_kind, "SQLite");
    });
}

#[gpui::test]
fn adding_a_connection_appends_it_and_persists_to_disk(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("add");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.set_name_input("new db", cx);
        view.set_url_input("sqlite::memory:", cx);
        let (name, url) = view.form.read(cx).input_values(cx);
        view.add_connection(cx, &name, url)
            .expect("add must succeed");
    });

    manager.read_with(cx, |view, _app| {
        assert_eq!(view.connections().len(), 1);
        assert_eq!(view.connections()[0].connection.name, "new db");
        assert_eq!(view.connections()[0].connection.display_kind, "SQLite");
    });

    let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
    assert_eq!(reloaded.connections().len(), 1);
    assert_eq!(reloaded.connections()[0].name, "new db");
}

/// Drives `add_connection` with the given form inputs against a fresh store
/// and checks that nothing was persisted. `expected_status_substring`, when
/// present, is asserted against `view.status()`; omit it for cases that
/// don't surface a specific status message.
fn assert_rejected_without_persisting(
    temp_label: &str,
    name_input: &str,
    url_input: &str,
    expected_status_substring: Option<&str>,
    cx: &mut TestAppContext,
) {
    let temp = TempStorePath::new(temp_label);
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.set_name_input(name_input, cx);
        view.set_url_input(url_input, cx);
        let (name, url) = view.form.read(cx).input_values(cx);
        view.add_connection(cx, &name, url)
            .expect("validation rejection is Ok(())");

        assert!(view.connections().is_empty());
        if let Some(expected) = expected_status_substring {
            assert!(view.status().is_some_and(|s| s.contains(expected)));
        }
    });
}

#[gpui::test]
fn adding_a_connection_with_an_empty_name_is_rejected_without_persisting(cx: &mut TestAppContext) {
    assert_rejected_without_persisting(
        "reject-empty-name",
        "",
        "sqlite::memory:",
        Some("name"),
        cx,
    );
}

#[gpui::test]
fn adding_a_connection_with_an_empty_url_is_rejected_without_persisting(cx: &mut TestAppContext) {
    assert_rejected_without_persisting("reject-empty-url", "new db", "", Some("URL"), cx);
}

#[gpui::test]
fn adding_a_connection_with_an_unrecognized_scheme_is_rejected_without_persisting(
    cx: &mut TestAppContext,
) {
    assert_rejected_without_persisting(
        "reject-bad-scheme",
        "mystery",
        "cassandra://host/db",
        None,
        cx,
    );
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
    let label = host_label("sqlite:///~/dev/scratch.db");
    assert!(!label.is_empty());
}

// ---- modal open/close/view transitions ---------------------------------

#[gpui::test]
fn opening_the_modal_starts_on_the_list_panel(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("open");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        assert!(!view.is_open());
        view.open(cx);
        assert!(view.is_open());
        assert_eq!(view.current_view(), ManagerView::List);
    });
}

#[gpui::test]
fn escape_closes_an_open_modal(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("escape");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update(vcx, |view, cx| {
        view.open(cx);
    });
    vcx.update(|window, cx| {
        manager.update(cx, |view, cx| {
            let escape = KeyDownEvent {
                keystroke: Keystroke {
                    key: "escape".to_owned(),
                    key_char: None,
                    modifiers: Modifiers::default(),
                },
                is_held: false,
            };
            view.handle_modal_key_down(&escape, window, cx);
        });
    });
    manager.read_with(vcx, |view, _app| {
        assert!(!view.is_open(), "Escape must close an open modal");
    });
}

#[gpui::test]
fn show_add_form_then_cancel_returns_to_the_list_without_persisting(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("cancel-add");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.open(cx);
        view.show_add_form(window, cx);
        assert_eq!(view.current_view(), ManagerView::Form);

        view.set_name_input("staging", cx);
        view.set_url_input("postgres://host/db", cx);
        view.cancel_form(cx);

        assert_eq!(view.current_view(), ManagerView::List);
        assert!(view.connections().is_empty());
        assert!(view.form.read(cx).name_field.read(cx).value().is_empty());
        assert!(view.form.read(cx).url_field.read(cx).value().is_empty());
    });

    let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
    assert!(reloaded.connections().is_empty());
}

#[gpui::test]
fn adding_a_connection_returns_the_modal_to_the_list_panel(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("add-returns-to-list");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.open(cx);
        view.show_add_form(window, cx);
        view.set_name_input("staging", cx);
        view.set_url_input("postgres://host/db", cx);
        let (name, url) = view.form.read(cx).input_values(cx);
        view.add_connection(cx, &name, url)
            .expect("add must succeed");

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
        .add(ConnectionArgs {
            name: "first".to_owned(),
            url: "postgres://host/a".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add first");
    store
        .add(ConnectionArgs {
            name: "second".to_owned(),
            url: "sqlite:///tmp/b.db".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add second");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        let id = view.connections()[0].connection.id;
        view.delete_id(id, cx).expect("delete must succeed");
        assert_eq!(view.connections().len(), 1);
        assert_eq!(view.connections()[0].connection.name, "second");
    });

    let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
    assert_eq!(reloaded.connections().len(), 1);
    assert_eq!(reloaded.connections()[0].name, "second");
}

// ---- edit / save_edit ---------------------------------------------------

#[gpui::test]
fn saving_an_edit_updates_the_row_in_place_without_appending_a_duplicate(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("save-edit");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "first".to_owned(),
            url: "postgres://host/a".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add first");
    store
        .add(ConnectionArgs {
            name: "second".to_owned(),
            url: "postgres://host/b".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add second");

    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    let id = manager.read_with(vcx, |view, _app| view.connections()[0].connection.id);

    manager.update_in(vcx, |view, window, cx| {
        view.show_edit_form(id, window, cx);
        view.set_name_input("first renamed", cx);
        let host_field = view.form.read(cx).host_field.clone();
        host_field.update(cx, |field, cx| field.set_value("otherhost", cx));
    });

    manager.update(vcx, |view, cx| {
        let (name, url) = view.form.read(cx).input_values(cx);
        view.save_edit(cx, id, &name, url)
            .expect("save_edit must succeed");

        assert_eq!(
            view.connections().len(),
            2,
            "editing must not change the list length"
        );
        assert_eq!(view.connections()[0].connection.name, "first renamed");
        assert_eq!(
            view.connections()[0].connection.get_url().unwrap(),
            "postgres://otherhost/a"
        );
        assert_eq!(
            view.connections()[1].connection.get_url().unwrap(),
            "postgres://host/b",
            "the untouched row must be unaffected"
        );
        assert_eq!(view.current_view(), ManagerView::List);
    });

    let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
    assert_eq!(reloaded.connections().len(), 2);
    assert_eq!(reloaded.connections()[0].name, "first renamed");
}

// ---- connect_and_close (row click / Enter) ------------------------------

#[gpui::test]
async fn connect_and_close_connects_and_clears_is_open(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("connect-close");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "mem".to_owned(),
            url: "sqlite::memory:".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let session_for_assert = session.clone();
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, ConnectionManagerView::open);
    let id = manager.read_with(cx, |view, _app| view.connections()[0].connection.id);
    let task = manager.update(cx, |view, cx| view.connect_and_close(id, cx));
    task.await;

    manager.read_with(cx, |view, _app| {
        assert!(
            !view.is_open(),
            "connect_and_close must close the modal immediately"
        );
    });
    session_for_assert.read_with(cx, |session, _app| {
        assert!(matches!(session.state(), SessionState::Connected));
    });
}

#[gpui::test]
fn enter_on_a_focused_row_connects_and_closes_the_modal_the_same_as_a_click(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("row-enter");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "mem".to_owned(),
            url: "sqlite::memory:".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update(vcx, ConnectionManagerView::open);
    vcx.run_until_parked();

    let row_handle = manager.read_with(vcx, |view, _app| view.row_focus_handles[0].clone());
    vcx.update(|window, _cx| window.focus(&row_handle));
    vcx.run_until_parked();

    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();

    manager.read_with(vcx, |view, _app| {
        assert!(
            !view.is_open(),
            "Enter on a focused row must close the modal, the same as clicking it"
        );
        // `connect` sets this synchronously, before the connect attempt
        // itself completes -- proof the real keyboard-dispatch path reached
        // the row's connect handler, the same one the click path uses, not
        // just that the modal happened to close some other way.
        assert!(
            view.status()
                .is_some_and(|status| status.contains("connecting")),
            "expected a real connect to have been dispatched, got status {:?}",
            view.status()
        );
    });
}

#[gpui::test]
async fn connect_updates_active_synchronously_before_the_connect_resolves(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("connect-sync-active");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "mem".to_owned(),
            url: "sqlite::memory:".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    let id = manager.read_with(cx, |view, _app| view.connections()[0].connection.id);
    let task = manager.update(cx, |view, cx| {
        assert!(view.active().is_none(), "no connection is active yet");
        let task = view.connect(id, cx);
        assert_eq!(
            view.active().map(|active| active.name.as_str()),
            Some("mem"),
            "active must reflect the target connection synchronously at dispatch time, \
             before the connect attempt itself has resolved"
        );
        task
    });
    task.await;
}

#[gpui::test]
async fn connect_and_close_updates_active_synchronously_before_the_connect_resolves(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("connect-and-close-sync-active");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "mem".to_owned(),
            url: "sqlite::memory:".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    let id = manager.read_with(cx, |view, _app| view.connections()[0].connection.id);
    let task = manager.update(cx, |view, cx| {
        let task = view.connect_and_close(id, cx);
        assert_eq!(
            view.active().map(|active| active.name.as_str()),
            Some("mem"),
            "connect_and_close must update active synchronously too"
        );
        assert!(
            !view.is_open(),
            "connect_and_close must close the modal immediately"
        );
        task
    });
    task.await;
}

#[gpui::test]
async fn a_failed_connect_does_not_revert_active_to_the_previous_connection(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("connect-fail-keeps-active");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "mem".to_owned(),
            url: "sqlite::memory:".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");
    store
        .add(ConnectionArgs {
            name: "unrecognized".to_owned(),
            url: "cassandra://host/db".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    let (id_mem, id_unrecognized) = manager.read_with(cx, |view, _app| {
        (
            view.connections()[0].connection.id,
            view.connections()[1].connection.id,
        )
    });

    manager
        .update(cx, |view, cx| view.connect(id_mem, cx))
        .await;
    manager.read_with(cx, |view, _app| {
        assert_eq!(
            view.active().map(|active| active.name.as_str()),
            Some("mem")
        );
    });

    manager
        .update(cx, |view, cx| view.connect(id_unrecognized, cx))
        .await;

    manager.read_with(cx, |view, _app| {
        assert_eq!(
            view.active().map(|active| active.name.as_str()),
            Some("unrecognized"),
            "a failed switch must stay pointed at its own target, not revert to \
             the connection that preceded it"
        );
        assert!(
            view.status()
                .is_some_and(|status| status.contains("Failed")),
            "expected a failure status, got {:?}",
            view.status()
        );
    });
}

// ---- connect_unsaved (add form's "Connect" button) -----------------------

#[gpui::test]
async fn connect_unsaved_connects_without_persisting_and_closes_the_modal(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("connect-unsaved");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let session_for_assert = session.clone();
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.show_add_form(window, cx);
        view.set_name_input("scratch", cx);
        view.set_url_input("sqlite::memory:", cx);
    });

    let task = manager.update(vcx, |view, cx| {
        let (name, url) = view.form.read(cx).input_values(cx);
        view.connect_unsaved(cx, name, url)
    });
    task.await;

    manager.read_with(vcx, |view, _app| {
        assert!(
            !view.is_open(),
            "connect_unsaved must close the modal once the connect succeeds"
        );
        assert!(
            view.connections().is_empty(),
            "connect_unsaved must never persist the connection to the store"
        );
    });
    session_for_assert.read_with(vcx, |session, _app| {
        assert!(
            matches!(session.state(), SessionState::Connected),
            "connect_unsaved must actually connect the session, got {:?}",
            session.state()
        );
    });
}

#[gpui::test]
async fn connect_unsaved_updates_active_synchronously_before_the_connect_resolves(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("connect-unsaved-sync-active");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.show_add_form(window, cx);
        view.set_name_input("scratch", cx);
        view.set_url_input("sqlite::memory:", cx);
    });

    let task = manager.update(vcx, |view, cx| {
        let (name, url) = view.form.read(cx).input_values(cx);
        let task = view.connect_unsaved(cx, name, url);
        assert_eq!(
            view.active().map(|active| active.name.as_str()),
            Some("scratch"),
            "active must reflect the target connection synchronously at dispatch time"
        );
        task
    });
    task.await;
}

#[gpui::test]
async fn a_failed_connect_unsaved_does_not_revert_active_and_leaves_the_modal_open(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("connect-unsaved-fail-keeps-active");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.open(cx);
        view.show_add_form(window, cx);
        view.set_name_input("will-fail", cx);
        // SQLite path in nonexistent directory: deterministically fails to connect
        let unopenable = format!(
            "sqlite:{}/zsql-connections-test-nonexistent-dir/db.sqlite3",
            std::env::temp_dir().display()
        );
        view.set_url_input(&unopenable, cx);
    });

    let task = manager.update(vcx, |view, cx| {
        let (name, url) = view.form.read(cx).input_values(cx);
        view.connect_unsaved(cx, name, url)
    });
    task.await;

    manager.read_with(vcx, |view, _app| {
        assert_eq!(
            view.active().map(|active| active.name.as_str()),
            Some("will-fail"),
            "a failed connect_unsaved must keep active pointing at the failed target, not revert"
        );
        assert!(
            view.is_open(),
            "a failed connect_unsaved must leave the modal open"
        );
        assert!(
            view.status()
                .is_some_and(|status| status.contains("Failed to connect")),
            "expected a failure status, got {:?}",
            view.status()
        );
    });
}

// ---- edit / delete never connect ----------------------------------------

#[gpui::test]
fn showing_the_edit_form_or_deleting_a_row_never_touches_the_session(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("edit-delete-no-connect");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "mem".to_owned(),
            url: "sqlite::memory:".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");
    store
        .add(ConnectionArgs {
            name: "other".to_owned(),
            url: "sqlite::memory:".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let session_for_assert = session.clone();
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    let (id_mem, id_other) = manager.read_with(vcx, |view, _app| {
        (
            view.connections()[0].connection.id,
            view.connections()[1].connection.id,
        )
    });

    manager.update_in(vcx, |view, window, cx| {
        view.show_edit_form(id_mem, window, cx);
        view.cancel_form(cx);
        let _ = view.delete_id(id_other, cx);
    });

    session_for_assert.read_with(vcx, |session, _app| {
        assert!(
            matches!(session.state(), SessionState::Empty),
            "neither editing nor deleting a row may connect the session, got {:?}",
            session.state()
        );
    });
}

// ---- database switching -------------------------------------

/// Switching databases in-session (`Session::switch_database`) is a live,
/// transient action: it must never rewrite the saved [`StoredConnection`]
/// (or its keyring-held URL) a connection was chosen from, on disk or in
/// the manager's own in-memory row list.
#[gpui::test]
async fn switching_database_in_session_does_not_mutate_the_stored_connection(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("switch-no-mutate");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "local sqlite".to_owned(),
            url: "sqlite::memory:".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");
    let id = store.connections()[0].id;
    let stored_before = store.connections()[0].clone();

    let session = cx.new(|_cx| session_with_no_url());
    let session_for_switch = session.clone();
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| view.connect(id, cx)).await;
    session_for_switch.read_with(cx, |session, _app| {
        assert!(matches!(session.state(), SessionState::Connected));
    });

    session_for_switch
        .update(cx, |session, cx| session.switch_database("other", cx))
        .await;

    manager.read_with(cx, |view, _app| {
        let stored_after = &view
            .connections()
            .iter()
            .find(|row| row.connection.id == id)
            .expect("connection must still be present")
            .connection;
        assert_eq!(
            *stored_after, stored_before,
            "the in-memory stored connection must be unchanged by an in-session switch"
        );
    });

    let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
    assert_eq!(
        reloaded.connections()[0],
        stored_before,
        "the on-disk stored connection must be unchanged by an in-session switch"
    );
    let url_after = reloaded.connections()[0]
        .get_url()
        .expect("get_url must succeed");
    assert_eq!(
        url_after, "sqlite::memory:",
        "the keyring-held URL must be unchanged by an in-session switch"
    );
}

// ---- Test button ----------------------------------------------------

#[gpui::test]
async fn running_test_never_mutates_the_sessions_active_connection_or_persists(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("test-button");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let session_for_assert = session.clone();
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.show_add_form(window, cx);
        view.set_name_input("scratch", cx);
        view.set_url_input("sqlite::memory:", cx);
    });

    let task = manager.update(vcx, |view, cx| {
        let url = view.form.read(cx).input_values(cx).1;
        view.run_test(cx, url)
    });
    task.await;

    manager.read_with(vcx, |view, app| {
        match view.test_outcome(app) {
            Some(TestOutcome::Connected { .. }) => {}
            other => panic!("expected a successful Test outcome, got {other:?}"),
        }
        assert!(
            view.connections().is_empty(),
            "Test must never persist a connection to the store"
        );
    });
    session_for_assert.read_with(vcx, |session, _app| {
        assert!(
            matches!(session.state(), SessionState::Empty),
            "Test must never touch the app's live session, got {:?}",
            session.state()
        );
    });
}

#[gpui::test]
async fn a_failed_test_reports_the_drivers_error_inline(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("test-button-fail");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.show_add_form(window, cx);
        view.set_url_input(
            "postgres://nobody:nobody@zsql-test-unreachable.invalid:5432/db",
            cx,
        );
    });

    let task = manager.update(vcx, |view, cx| {
        let url = view.form.read(cx).input_values(cx).1;
        view.run_test(cx, url)
    });
    task.await;

    manager.read_with(vcx, |view, app| match view.test_outcome(app) {
        Some(TestOutcome::Failed(message)) => assert!(!message.is_empty()),
        other => panic!("expected a failed Test outcome, got {other:?}"),
    });
}

// ---- form event routing (on_form_event) ---------------------------------
//
// Emits each ConnectionFormEvent variant on the manager's real form entity
// (the same [`Entity<ConnectionForm>`] the manager subscribes to in
// `ConnectionManagerView::new`), so a miswired or dropped match arm in
// `on_form_event` would fail these even though nothing here calls
// cancel_form/run_test/connect_unsaved/add_connection/save_edit directly.

#[gpui::test]
fn a_cancel_event_from_the_real_form_returns_the_modal_to_the_list(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("route-cancel");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.open(cx);
        view.show_add_form(window, cx);
        assert_eq!(view.current_view(), ManagerView::Form);
    });

    let form = manager.read_with(vcx, |view, _app| view.form.clone());
    form.update(vcx, |_form, cx| cx.emit(ConnectionFormEvent::Cancel));

    manager.read_with(vcx, |view, _app| {
        assert_eq!(
            view.current_view(),
            ManagerView::List,
            "a Cancel event emitted by the real form must reach cancel_form"
        );
    });
}

// The Test/Connect routing tests below emit their event with an
// unregistered-scheme URL so `run_test`/`connect_unsaved` take their
// synchronous validation-failure path (see [`super::validate_new_connection`]'s
// sibling check in each) rather than spawning a real connect -- enough to
// prove the event reached the right method, without the real-IO machinery
// (`allow_parking`/`serialize_real_io`) the connect/test *behavior* tests
// elsewhere in this module already cover.

#[gpui::test]
fn a_test_event_from_the_real_form_reaches_run_test(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("route-test");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.show_add_form(window, cx);
    });

    let form = manager.read_with(vcx, |view, _app| view.form.clone());
    form.update(vcx, |_form, cx| {
        cx.emit(ConnectionFormEvent::Test {
            url: "cassandra://host/db".to_owned(),
        });
    });

    manager.read_with(vcx, |view, app| {
        assert!(
            matches!(view.test_outcome(app), Some(TestOutcome::Failed(_))),
            "a Test event emitted by the real form must reach run_test, got {:?}",
            view.test_outcome(app)
        );
    });
}

#[gpui::test]
fn a_connect_event_from_the_real_form_reaches_connect_unsaved(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("route-connect");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.open(cx);
        view.show_add_form(window, cx);
    });

    let form = manager.read_with(vcx, |view, _app| view.form.clone());
    form.update(vcx, |_form, cx| {
        cx.emit(ConnectionFormEvent::Connect {
            name: "scratch".to_owned(),
            url: "cassandra://host/db".to_owned(),
        });
    });

    manager.read_with(vcx, |view, _app| {
        assert!(
            view.status()
                .is_some_and(|status| status.contains("Cannot connect")),
            "a Connect event emitted by the real form must reach connect_unsaved, got {:?}",
            view.status()
        );
        assert!(
            view.connections().is_empty(),
            "connect_unsaved must never persist the connection to the store"
        );
    });
}

#[gpui::test]
fn an_add_event_from_the_real_form_persists_a_new_connection(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("route-add");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.open(cx);
        view.show_add_form(window, cx);
    });

    let form = manager.read_with(vcx, |view, _app| view.form.clone());
    form.update(vcx, |_form, cx| {
        cx.emit(ConnectionFormEvent::Add {
            name: "staging".to_owned(),
            url: "postgres://host/db".to_owned(),
        });
    });

    manager.read_with(vcx, |view, _app| {
        assert_eq!(
            view.current_view(),
            ManagerView::List,
            "an Add event emitted by the real form must reach add_connection"
        );
        assert_eq!(view.connections().len(), 1);
        assert_eq!(view.connections()[0].connection.name, "staging");
    });

    let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
    assert_eq!(reloaded.connections().len(), 1);
    assert_eq!(reloaded.connections()[0].name, "staging");
}

#[gpui::test]
fn an_edit_event_from_the_real_form_updates_the_row_in_place(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("route-edit");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "first".to_owned(),
            url: "postgres://host/a".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add first");

    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    let id = manager.read_with(vcx, |view, _app| view.connections()[0].connection.id);

    manager.update_in(vcx, |view, window, cx| {
        view.show_edit_form(id, window, cx);
    });

    let form = manager.read_with(vcx, |view, _app| view.form.clone());
    form.update(vcx, |_form, cx| {
        cx.emit(ConnectionFormEvent::Edit {
            id,
            name: "first renamed".to_owned(),
            url: "postgres://otherhost/a".to_owned(),
        });
    });

    manager.read_with(vcx, |view, _app| {
        assert_eq!(
            view.current_view(),
            ManagerView::List,
            "an Edit event emitted by the real form must reach save_edit"
        );
        assert_eq!(view.connections().len(), 1, "editing must not append a row");
        assert_eq!(view.connections()[0].connection.name, "first renamed");
        assert_eq!(
            view.connections()[0].connection.get_url().unwrap(),
            "postgres://otherhost/a"
        );
    });

    let reloaded = ConnectionStore::load(&temp.0).expect("reload must succeed");
    assert_eq!(reloaded.connections()[0].name, "first renamed");
}

// ---- render smoke tests --------------------------------------------------

#[gpui::test]
fn the_closed_modal_renders_nothing_and_the_open_modal_renders_without_panicking(
    cx: &mut TestAppContext,
) {
    let temp = TempStorePath::new("render");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "local pg".to_owned(),
            url: "postgres://localhost/app".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");
    store
        .add(ConnectionArgs {
            name: "local sqlite".to_owned(),
            url: "sqlite::memory:".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update(vcx, ConnectionManagerView::open);
    manager.update_in(vcx, ConnectionManagerView::show_add_form);
    manager.update(vcx, ConnectionManagerView::cancel_form);
    vcx.run_until_parked();
}

#[gpui::test]
fn a_long_url_never_widens_the_modal_panel(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("render-long-url");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    let long_url = format!(
        "postgres://app:s3cr3t@{}.example.com:5432/app?sslmode=require",
        "very-long-hostname-segment".repeat(10)
    );
    assert!(long_url.len() > 200, "the URL must actually be long");

    manager.update_in(vcx, |view, window, cx| {
        view.open(cx);
        view.show_add_form(window, cx);
        view.set_url_input(&long_url, cx);
    });
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("connection-modal-panel")
        .expect("the modal panel must be tagged and painted");
    assert_eq!(
        bounds.size.width,
        ModalSize::Small.width(),
        "a long URL must never widen the modal panel"
    );
}

#[gpui::test]
fn enabling_the_ssh_tunnel_widens_the_modal_panel_and_disabling_it_narrows_it_back(
    cx: &mut TestAppContext,
) {
    let temp = TempStorePath::new("render-ssh-width");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.open(cx);
        view.show_add_form(window, cx);
        view.set_url_input("postgres://app@host:5432/app", cx);
    });
    vcx.run_until_parked();

    let narrow = vcx
        .debug_bounds("connection-modal-panel")
        .expect("the modal panel must be tagged and painted");
    assert_eq!(
        narrow.size.width,
        ModalSize::Small.width(),
        "the SSH tunnel starts off, so the panel starts narrow"
    );

    let form = manager.read_with(vcx, |view, _app| view.form.clone());
    form.update(vcx, |form, cx| form.set_ssh_enabled(true, cx));
    vcx.run_until_parked();

    let wide = vcx
        .debug_bounds("connection-modal-panel")
        .expect("the modal panel must be tagged and painted");
    assert_eq!(
        wide.size.width,
        ModalSize::Wide.width(),
        "enabling the tunnel for a network driver must widen the panel to the wide constant"
    );

    form.update(vcx, |form, cx| form.set_ssh_enabled(false, cx));
    vcx.run_until_parked();

    let narrow_again = vcx
        .debug_bounds("connection-modal-panel")
        .expect("the modal panel must be tagged and painted");
    assert_eq!(
        narrow_again.size.width,
        ModalSize::Small.width(),
        "disabling the tunnel must narrow the panel back to the small constant"
    );
}

// ---- SSH round-trip through add_connection -------------------------------

#[gpui::test]
fn adding_a_connection_with_ssh_agent_auth_persists_it(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("add-ssh-agent");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.show_add_form(window, cx);
        view.set_name_input("tunneled agent", cx);
        view.set_url_input("postgres://app@dbhost:5432/app", cx);
        let form = view.form.clone();
        form.update(cx, |form, cx| form.set_ssh_enabled(true, cx));
        let host_field = form.read(cx).ssh_host_field.clone();
        host_field.update(cx, |field, cx| field.set_value("bastion.example.com", cx));
        let port_field = form.read(cx).ssh_port_field.clone();
        port_field.update(cx, |field, cx| field.set_value("2222", cx));
        let user_field = form.read(cx).ssh_user_field.clone();
        user_field.update(cx, |field, cx| field.set_value("deploy", cx));
    });

    manager.update(vcx, |view, cx| {
        let (name, url) = view.form.read(cx).input_values(cx);
        view.add_connection(cx, &name, url)
            .expect("add must succeed");
    });

    manager.read_with(vcx, |view, _app| {
        let stored = &view.connections()[0].connection;
        let ssh = stored.ssh.clone().expect("ssh must be persisted");
        assert!(ssh.enabled);
        assert_eq!(ssh.host, "bastion.example.com");
        assert_eq!(ssh.port, 2222);
        assert_eq!(ssh.user, "deploy");
        assert!(matches!(ssh.auth_kind, SshAuthKind::Agent));
        assert!(
            stored.get_ssh_secret().is_err(),
            "agent auth must not write a keyring secret"
        );
    });
}

#[gpui::test]
fn adding_a_connection_with_ssh_password_auth_persists_it_and_its_secret(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("add-ssh-password");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.show_add_form(window, cx);
        view.set_name_input("tunneled password", cx);
        view.set_url_input("postgres://app@dbhost:5432/app", cx);
        let form = view.form.clone();
        form.update(cx, |form, cx| {
            form.set_ssh_enabled(true, cx);
            form.set_ssh_auth_kind(SshAuthKind::Password, cx);
        });
        let host_field = form.read(cx).ssh_host_field.clone();
        host_field.update(cx, |field, cx| field.set_value("bastion.example.com", cx));
        let user_field = form.read(cx).ssh_user_field.clone();
        user_field.update(cx, |field, cx| field.set_value("deploy", cx));
        let password_field = form.read(cx).ssh_password_field.clone();
        password_field.update(cx, |field, cx| field.set_value("hunter2", cx));
    });

    manager.update(vcx, |view, cx| {
        let (name, url) = view.form.read(cx).input_values(cx);
        view.add_connection(cx, &name, url)
            .expect("add must succeed");
    });

    manager.read_with(vcx, |view, _app| {
        let stored = &view.connections()[0].connection;
        let ssh = stored.ssh.clone().expect("ssh must be persisted");
        assert!(matches!(ssh.auth_kind, SshAuthKind::Password));
        assert_eq!(
            stored
                .get_ssh_secret()
                .expect("secret must be in the keyring"),
            "hunter2"
        );
    });
}

#[gpui::test]
fn adding_a_connection_with_ssh_key_auth_persists_it_and_the_passphrase(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("add-ssh-key");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.show_add_form(window, cx);
        view.set_name_input("tunneled key", cx);
        view.set_url_input("postgres://app@dbhost:5432/app", cx);
        let form = view.form.clone();
        form.update(cx, |form, cx| {
            form.set_ssh_enabled(true, cx);
            form.set_ssh_auth_kind(SshAuthKind::Key, cx);
        });
        let host_field = form.read(cx).ssh_host_field.clone();
        host_field.update(cx, |field, cx| field.set_value("bastion.example.com", cx));
        let key_path_field = form.read(cx).ssh_key_path_field.clone();
        key_path_field.update(cx, |field, cx| {
            field.set_value("/home/user/.ssh/id_ed25519", cx);
        });
        let passphrase_field = form.read(cx).ssh_key_passphrase_field.clone();
        passphrase_field.update(cx, |field, cx| field.set_value("key-secret", cx));
    });

    manager.update(vcx, |view, cx| {
        let (name, url) = view.form.read(cx).input_values(cx);
        view.add_connection(cx, &name, url)
            .expect("add must succeed");
    });

    manager.read_with(vcx, |view, _app| {
        let stored = &view.connections()[0].connection;
        let ssh = stored.ssh.clone().expect("ssh must be persisted");
        assert!(matches!(ssh.auth_kind, SshAuthKind::Key));
        assert_eq!(
            ssh.key_path.as_deref(),
            Some(std::path::Path::new("/home/user/.ssh/id_ed25519"))
        );
        assert_eq!(
            stored
                .get_ssh_secret()
                .expect("secret must be in the keyring"),
            "key-secret"
        );
    });
}

#[gpui::test]
fn adding_a_connection_with_ssh_left_disabled_persists_no_ssh_and_no_secret(
    cx: &mut TestAppContext,
) {
    let temp = TempStorePath::new("add-ssh-disabled");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.show_add_form(window, cx);
        view.set_name_input("no tunnel", cx);
        view.set_url_input("postgres://app@dbhost:5432/app", cx);
    });

    manager.update(vcx, |view, cx| {
        let (name, url) = view.form.read(cx).input_values(cx);
        view.add_connection(cx, &name, url)
            .expect("add must succeed");
    });

    manager.read_with(vcx, |view, _app| {
        let stored = &view.connections()[0].connection;
        assert_eq!(stored.ssh, None);
        assert!(
            stored.get_ssh_secret().is_err(),
            "no ssh secret may be written to the keyring when SSH is left disabled"
        );
    });
}

#[gpui::test]
fn editing_a_connection_to_enable_ssh_persists_the_change(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("edit-enable-ssh");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "first".to_owned(),
            url: "postgres://host/a".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));
    let id = manager.read_with(vcx, |view, _app| view.connections()[0].connection.id);

    manager.update_in(vcx, |view, window, cx| {
        view.show_edit_form(id, window, cx);
        let form = view.form.clone();
        form.update(cx, |form, cx| {
            form.set_ssh_enabled(true, cx);
            form.set_ssh_auth_kind(SshAuthKind::Password, cx);
        });
        let host_field = form.read(cx).ssh_host_field.clone();
        host_field.update(cx, |field, cx| field.set_value("bastion.example.com", cx));
        let password_field = form.read(cx).ssh_password_field.clone();
        password_field.update(cx, |field, cx| field.set_value("edited-secret", cx));
    });

    manager.update(vcx, |view, cx| {
        let (name, url) = view.form.read(cx).input_values(cx);
        view.save_edit(cx, id, &name, url)
            .expect("save_edit must succeed");
    });

    manager.read_with(vcx, |view, _app| {
        let stored = &view.connections()[0].connection;
        let ssh = stored.ssh.clone().expect("ssh must be persisted");
        assert!(ssh.enabled);
        assert_eq!(ssh.host, "bastion.example.com");
        assert_eq!(
            stored
                .get_ssh_secret()
                .expect("secret must be in the keyring"),
            "edited-secret"
        );
    });
}

// ---- Test/Connect open the tunnel before the driver connect ---------------

#[gpui::test]
async fn run_test_with_an_unreachable_ssh_host_reports_an_ssh_layer_failure(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("test-ssh-unreachable");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.show_add_form(window, cx);
        view.set_url_input(
            "postgres://nobody@zsql-test-should-never-be-dialed.invalid:5432/db",
            cx,
        );
        let form = view.form.clone();
        form.update(cx, |form, cx| form.set_ssh_enabled(true, cx));
        let host_field = form.read(cx).ssh_host_field.clone();
        host_field.update(cx, |field, cx| {
            field.set_value("zsql-test-unreachable-ssh.invalid", cx);
        });
        let user_field = form.read(cx).ssh_user_field.clone();
        user_field.update(cx, |field, cx| field.set_value("nobody", cx));
        // Password auth keeps the test independent of an ambient ssh-agent;
        // with agent auth, an agentless environment fails before dialing.
        form.update(cx, |form, cx| {
            form.set_ssh_auth_kind(SshAuthKind::Password, cx);
        });
        let password_field = form.read(cx).ssh_password_field.clone();
        password_field.update(cx, |field, cx| field.set_value("test-password", cx));
    });

    let task = manager.update(vcx, |view, cx| {
        let url = view.form.read(cx).input_values(cx).1;
        view.run_test(cx, url)
    });
    task.await;

    manager.read_with(vcx, |view, app| match view.test_outcome(app) {
        Some(TestOutcome::Failed(message)) => {
            assert!(
                message.contains("ssh host"),
                "expected an ssh-layer failure (proving the driver connect was never \
                 attempted against the unreachable postgres host), got: {message}"
            );
        }
        other => panic!("expected a Failed outcome from the unreachable ssh host, got {other:?}"),
    });
}

#[gpui::test]
async fn connect_unsaved_does_not_silently_ignore_the_forms_ssh_section(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("connect-unsaved-ssh-ordering");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update_in(vcx, |view, window, cx| {
        view.open(cx);
        view.show_add_form(window, cx);
        view.set_name_input("scratch", cx);
        view.set_url_input(
            "postgres://nobody@zsql-test-should-never-be-dialed.invalid:5432/db",
            cx,
        );
        let form = view.form.clone();
        form.update(cx, |form, cx| form.set_ssh_enabled(true, cx));
        let host_field = form.read(cx).ssh_host_field.clone();
        host_field.update(cx, |field, cx| {
            field.set_value("zsql-test-unreachable-ssh.invalid", cx);
        });
        let user_field = form.read(cx).ssh_user_field.clone();
        user_field.update(cx, |field, cx| field.set_value("nobody", cx));
        // Password auth keeps the test independent of an ambient ssh-agent;
        // with agent auth, an agentless environment fails before dialing.
        form.update(cx, |form, cx| {
            form.set_ssh_auth_kind(SshAuthKind::Password, cx);
        });
        let password_field = form.read(cx).ssh_password_field.clone();
        password_field.update(cx, |field, cx| field.set_value("test-password", cx));
    });

    let task = manager.update(vcx, |view, cx| {
        let (name, url) = view.form.read(cx).input_values(cx);
        view.connect_unsaved(cx, name, url)
    });
    task.await;

    manager.read_with(vcx, |view, _app| {
        let status = view.status();
        assert!(
            status.is_some_and(|status| status.contains("ssh host")),
            "expected an ssh-layer failure (proving the driver connect was never \
             attempted against the unreachable postgres host) and that the form's \
             enabled SSH section was not silently ignored, got {status:?}"
        );
        assert!(
            view.is_open(),
            "a failed connect_unsaved must leave the modal open"
        );
    });
}
