use std::time::Duration;

use gpui::{AppContext as _, Context, Entity, Modifiers, TestAppContext};

use super::super::{ConnectionManagerView, ConnectionStore, ManagerView, StoredConnection};
use crate::connections::{ConnectionArgs, HostKeyPolicy, SshAuthKind, StoredSsh};
use crate::session::Session;

/// Registers the shared `TextField`'s key bindings (Enter-to-submit among
/// them), which production startup does once in `main.rs` but a test's
/// isolated `App` never gets for free.
fn init_text_field_bindings(cx: &mut TestAppContext) {
    cx.update(|cx| {
        zsql_ui::text_field::init(cx, &zsql_ui::text_field::TextFieldBindings::default());
    });
}

fn test_probe_timeout() -> Duration {
    crate::config::Config::default().liveness.probe_timeout()
}

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
            "zsql-password-prompt-test-{label}-{}-{n}.toml",
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

/// A store with one connection saved with `url`, its keyring secret already
/// deleted so it is genuinely absent.
fn store_with_absent_secret(
    label: &str,
    name: &str,
    url: &str,
) -> (TempStorePath, ConnectionStore) {
    let temp = TempStorePath::new(label);
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: name.to_owned(),
            url: url.to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");
    store.connections()[0]
        .delete_url()
        .expect("delete_url must succeed");
    (temp, store)
}

// ---- connect() branching on the keyring error kind ---------------------

#[gpui::test]
fn connecting_with_an_absent_secret_and_a_sanitized_url_opens_the_prompt_not_a_status(
    cx: &mut TestAppContext,
) {
    let (_temp, store) = store_with_absent_secret(
        "absent-with-sanitized",
        "analytics - prod",
        "postgres://readonly:hunter2@db.internal:5432/analytics",
    );

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    let id = manager.read_with(cx, |view, _app| view.connections()[0].connection.id);
    manager.update(cx, |view, cx| view.connect(id, cx)).detach();

    manager.read_with(cx, |view, _app| {
        assert!(
            view.password_prompt_is_open(),
            "an absent secret with a sanitized_url must open the password prompt"
        );
        assert!(
            view.status().is_none(),
            "opening the prompt must not also set a 'Failed to connect' status, got {:?}",
            view.status()
        );
    });
}

#[gpui::test]
fn connecting_with_a_non_absent_keyring_error_still_reports_failed_to_connect(
    cx: &mut TestAppContext,
) {
    let temp = TempStorePath::new("not-absent");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "locked".to_owned(),
            url: "postgres://host/db".to_owned(),
            ssh: None,
            ssh_secret: None,
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    let id = manager.read_with(cx, |view, _app| view.connections()[0].connection.id);
    manager
        .update(cx, |view, cx| {
            view.connections()[0].connection.corrupt_url_for_test();
            view.connect(id, cx)
        })
        .detach();

    manager.read_with(cx, |view, _app| {
        assert!(
            !view.password_prompt_is_open(),
            "a non-absent keyring error must not open the password prompt"
        );
        assert!(
            view.status()
                .is_some_and(|status| status.contains("Failed to connect to locked")),
            "expected the usual failed-to-connect status, got {:?}",
            view.status()
        );
    });
}

#[gpui::test]
async fn connecting_to_a_sqlite_connection_with_an_absent_secret_skips_the_prompt_and_connects_directly(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let (_temp, store) =
        store_with_absent_secret("sqlite-skip-prompt", "local cache", "sqlite::memory:");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    let id = manager.read_with(cx, |view, _app| view.connections()[0].connection.id);
    manager.update(cx, |view, cx| view.connect(id, cx)).await;

    manager.read_with(cx, |view, _app| {
        assert!(
            !view.password_prompt_is_open(),
            "a sqlite connection has no password to lose, so it must never open the prompt"
        );
        assert_eq!(
            view.status(),
            Some("Connected to local cache."),
            "it must reconnect directly instead of prompting"
        );
    });

    let fresh = StoredConnection {
        id,
        name: String::new(),
        display_kind: String::new(),
        display_host: String::new(),
        ssh: None,
        sanitized_url: None,
    };
    assert_eq!(
        fresh.get_url().expect("the keyring entry must be restored"),
        "sqlite::memory:"
    );
}

#[gpui::test]
async fn a_keyring_write_failure_restoring_a_sqlite_connections_entry_is_surfaced_in_the_status(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let (_temp, store) = store_with_absent_secret(
        "sqlite-restore-write-failure",
        "local cache",
        "sqlite::memory:",
    );

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    let id = manager.read_with(cx, |view, _app| view.connections()[0].connection.id);
    manager.update(cx, |view, _cx| {
        view.connections()[0].connection.block_url_writes_for_test();
    });
    manager.update(cx, |view, cx| view.connect(id, cx)).await;

    manager.read_with(cx, |view, _app| {
        let status = view.status().unwrap_or_default();
        assert!(
            status.starts_with("Connected to local cache."),
            "expected the successful-connect prefix, got {status:?}"
        );
        assert!(
            status.contains("keyring entry could not be restored"),
            "a keyring write failure restoring the sqlite entry must be surfaced in the status \
             text, got {status:?}"
        );
    });
}

// ---- legacy fallback: absent secret, no sanitized_url -------------------

#[gpui::test]
fn connecting_to_a_legacy_connection_with_no_sanitized_url_opens_the_edit_form_blank(
    cx: &mut TestAppContext,
) {
    let temp = TempStorePath::new("legacy-no-sanitized-url");
    let id = uuid::Uuid::new_v4();
    let pre_field_toml = format!(
        "[[connections]]\n\
         id = \"{id}\"\n\
         name = \"legacy\"\n\
         display_kind = \"postgres\"\n\
         display_host = \"localhost\"\n"
    );
    std::fs::write(&temp.0, pre_field_toml).expect("setup write failed");
    let store = ConnectionStore::load(&temp.0).expect("a pre-feature store file must still load");
    assert_eq!(
        store.connections()[0].sanitized_url,
        None,
        "the fixture must genuinely predate sanitized_url"
    );

    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update(vcx, |view, cx| {
        view.connect(id, cx).detach();
    });
    vcx.run_until_parked();

    manager.read_with(vcx, |view, cx| {
        assert!(
            !view.password_prompt_is_open(),
            "a legacy connection with no sanitized_url must never open the password prompt"
        );
        assert_eq!(
            view.current_view(),
            ManagerView::Form,
            "it must open the edit form instead of dead-ending on a status message"
        );
        let (name, url) = view.form.read(cx).input_values(cx);
        assert_eq!(name, "legacy", "the name must be pre-filled");
        assert_eq!(
            url, "",
            "the URL field must be left blank, not stuck or panicking"
        );
    });
}

// ---- connect_and_close keeps the modal open for the prompt/form fallback ----

#[gpui::test]
fn connect_and_close_keeps_the_modal_open_when_it_opens_the_password_prompt(
    cx: &mut TestAppContext,
) {
    let (_temp, store) = store_with_absent_secret(
        "connect-and-close-prompt",
        "prompt via connect_and_close",
        "postgres://readonly:hunter2@db.internal:5432/analytics",
    );

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, ConnectionManagerView::open);
    let id = manager.read_with(cx, |view, _app| view.connections()[0].connection.id);
    manager
        .update(cx, |view, cx| view.connect_and_close(id, cx))
        .detach();

    manager.read_with(cx, |view, _app| {
        assert!(
            view.password_prompt_is_open(),
            "connect_and_close must open the password prompt for an absent secret with a \
             sanitized_url"
        );
        assert!(
            view.is_open(),
            "the modal must stay open (behind the prompt) rather than being closed"
        );
    });
}

#[gpui::test]
fn connect_and_close_keeps_the_modal_open_on_the_legacy_edit_form_fallback(
    cx: &mut TestAppContext,
) {
    let temp = TempStorePath::new("connect-and-close-legacy");
    let id = uuid::Uuid::new_v4();
    let pre_field_toml = format!(
        "[[connections]]\n\
         id = \"{id}\"\n\
         name = \"legacy\"\n\
         display_kind = \"postgres\"\n\
         display_host = \"localhost\"\n"
    );
    std::fs::write(&temp.0, pre_field_toml).expect("setup write failed");
    let store = ConnectionStore::load(&temp.0).expect("a pre-feature store file must still load");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, ConnectionManagerView::open);
    manager
        .update(cx, |view, cx| view.connect_and_close(id, cx))
        .detach();

    manager.read_with(cx, |view, _app| {
        assert_eq!(
            view.current_view(),
            ManagerView::Form,
            "connect_and_close must fall back to the edit form for a legacy connection"
        );
        assert!(
            view.is_open(),
            "the modal must stay open on the edit form, not be closed"
        );
    });
}

// ---- the prompt's own behavior ------------------------------------------

/// A store with one connection whose keyring secret has been deleted but
/// whose `sanitized_url` survives, with the manager's password prompt opened
/// directly for it (not through [`ConnectionManagerView::connect`], so a
/// sqlite `url` can still be used to exercise a genuine connect attempt
/// offline even though `connect` itself never routes a sqlite connection
/// through the prompt). `url` is the URL the connection is saved with before
/// its keyring entry is deleted.
fn open_prompt_over_absent_secret(
    cx: &mut TestAppContext,
    label: &str,
    url: &str,
) -> (Entity<ConnectionManagerView>, uuid::Uuid) {
    let (_temp, store) = store_with_absent_secret(label, "prompt target", url);

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));
    let id = manager.read_with(cx, |view, _app| view.connections()[0].connection.id);
    manager.update(cx, |view, cx| {
        let connection = view.connections()[0].connection.clone();
        view.open_password_prompt(&connection, cx);
    });
    assert!(manager.read_with(cx, |view, _app| view.password_prompt_is_open()));
    (manager, id)
}

#[gpui::test]
fn the_password_field_is_masked_when_the_prompt_opens(cx: &mut TestAppContext) {
    let (manager, _id) = open_prompt_over_absent_secret(cx, "masked-field", "postgres://host/db");
    manager.read_with(cx, |view, cx| {
        assert!(
            view.password_prompt_field_is_masked(cx),
            "the password field must render in masked (dots) mode"
        );
    });
}

#[gpui::test]
fn the_save_to_keyring_checkbox_defaults_to_checked(cx: &mut TestAppContext) {
    let (manager, _id) =
        open_prompt_over_absent_secret(cx, "default-checked", "postgres://host/db");
    manager.read_with(cx, |view, cx| {
        assert!(view.password_prompt_save_checked(cx));
    });
}

#[gpui::test]
fn toggling_the_checkbox_flips_the_save_to_keyring_state(cx: &mut TestAppContext) {
    let (manager, _id) = open_prompt_over_absent_secret(cx, "toggle", "postgres://host/db");
    manager.update(cx, |view, cx| {
        view.toggle_password_prompt_save(cx);
    });
    manager.read_with(cx, |view, cx| {
        assert!(!view.password_prompt_save_checked(cx));
    });
}

#[gpui::test]
fn submitting_an_empty_password_sets_an_inline_error_without_connecting(cx: &mut TestAppContext) {
    let (manager, _id) = open_prompt_over_absent_secret(cx, "empty-password", "postgres://host/db");
    manager
        .update(cx, ConnectionManagerView::submit_password_prompt)
        .detach();

    manager.read_with(cx, |view, cx| {
        assert!(
            view.password_prompt_is_open(),
            "an empty password must not close the prompt"
        );
        assert!(view.password_prompt_error(cx).is_some());
    });
}

#[gpui::test]
fn submitting_over_an_unparseable_sanitized_url_sets_an_inline_error_without_connecting(
    cx: &mut TestAppContext,
) {
    let temp = TempStorePath::new("invalid-sanitized-url");
    let id = uuid::Uuid::new_v4();
    let toml_text = format!(
        "[[connections]]\n\
         id = \"{id}\"\n\
         name = \"broken\"\n\
         display_kind = \"postgres\"\n\
         display_host = \"localhost\"\n\
         sanitized_url = \"not-a-url\"\n"
    );
    std::fs::write(&temp.0, toml_text).expect("setup write failed");
    let store = ConnectionStore::load(&temp.0).expect("must still parse");
    assert_eq!(
        store.connections()[0].sanitized_url.as_deref(),
        Some("not-a-url"),
        "the fixture must carry an unparseable sanitized_url"
    );

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));
    manager.update(cx, |view, cx| view.connect(id, cx)).detach();
    assert!(manager.read_with(cx, |view, _app| view.password_prompt_is_open()));

    manager.update(cx, |view, cx| {
        view.set_password_prompt_input("whatever", cx);
    });
    manager
        .update(cx, ConnectionManagerView::submit_password_prompt)
        .detach();
    cx.run_until_parked();

    manager.read_with(cx, |view, cx| {
        assert!(
            view.password_prompt_is_open(),
            "an unparseable sanitized_url must not close the prompt"
        );
        assert_eq!(
            view.password_prompt_error(cx).as_deref(),
            Some("Saved connection URL is invalid."),
        );
    });
}

#[gpui::test]
async fn a_successful_connect_with_the_checkbox_checked_saves_the_password_and_closes(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let (manager, id) = open_prompt_over_absent_secret(cx, "success-save", "sqlite::memory:");
    manager.update(cx, |view, cx| {
        view.set_password_prompt_input("does-not-matter-for-sqlite", cx);
    });

    manager
        .update(cx, ConnectionManagerView::submit_password_prompt)
        .await;

    manager.read_with(cx, |view, _app| {
        assert!(
            !view.password_prompt_is_open(),
            "a successful connect must close the prompt"
        );
        assert!(
            !view.is_open(),
            "a successful connect must close the whole modal"
        );
    });

    let fresh = StoredConnection {
        id,
        name: String::new(),
        display_kind: String::new(),
        display_host: String::new(),
        ssh: None,
        sanitized_url: None,
    };
    assert_eq!(
        fresh.get_url().expect("the keyring entry must be restored"),
        "sqlite::memory:"
    );
}

#[gpui::test]
async fn a_successful_connect_with_the_checkbox_unchecked_never_touches_the_keyring(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let (manager, id) = open_prompt_over_absent_secret(cx, "success-no-save", "sqlite::memory:");
    manager.update(cx, |view, cx| {
        view.toggle_password_prompt_save(cx);
        view.set_password_prompt_input("session-only-password", cx);
    });
    manager.read_with(cx, |view, cx| {
        assert!(!view.password_prompt_save_checked(cx));
    });

    manager
        .update(cx, ConnectionManagerView::submit_password_prompt)
        .await;

    manager.read_with(cx, |view, _app| {
        assert!(
            !view.password_prompt_is_open(),
            "the connect still succeeds"
        );
    });

    let fresh = StoredConnection {
        id,
        name: String::new(),
        display_kind: String::new(),
        display_host: String::new(),
        ssh: None,
        sanitized_url: None,
    };
    let result = fresh.get_url();
    assert!(
        result.is_err(),
        "an unchecked checkbox must leave the keyring untouched, got {result:?}"
    );
}

#[gpui::test]
async fn a_keyring_write_failure_after_a_successful_connect_is_surfaced_in_the_status(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let (manager, id) = open_prompt_over_absent_secret(cx, "write-failure", "sqlite::memory:");
    manager.update(cx, |view, cx| {
        let connection = view.connections()[0].connection.clone();
        connection.block_url_writes_for_test();
        view.set_password_prompt_input("does-not-matter-for-sqlite", cx);
    });

    manager
        .update(cx, ConnectionManagerView::submit_password_prompt)
        .await;

    manager.read_with(cx, |view, _app| {
        assert!(
            !view.password_prompt_is_open(),
            "the connect itself still succeeds, so the prompt must close"
        );
        let status = view.status().unwrap_or_default();
        assert!(
            status.starts_with("Connected to prompt target."),
            "expected the successful-connect prefix, got {status:?}"
        );
        assert!(
            status.contains("could not be saved to the keyring"),
            "a keyring write failure must be surfaced in the status text, got {status:?}"
        );
    });

    let fresh = StoredConnection {
        id,
        name: String::new(),
        display_kind: String::new(),
        display_host: String::new(),
        ssh: None,
        sanitized_url: None,
    };
    let result = fresh.get_url();
    assert!(
        result.is_err(),
        "the write was blocked, so nothing must have been saved, got {result:?}"
    );
}

#[gpui::test]
async fn a_failed_connect_never_writes_the_keyring_and_keeps_the_prompt_open_with_an_error(
    cx: &mut TestAppContext,
) {
    let (manager, id) = open_prompt_over_absent_secret(cx, "failed-connect", "cassandra://host/db");
    manager.update(cx, |view, cx| {
        view.set_password_prompt_input("some-password", cx);
    });

    manager
        .update(cx, ConnectionManagerView::submit_password_prompt)
        .await;

    manager.read_with(cx, |view, cx| {
        assert!(
            view.password_prompt_is_open(),
            "a failed connect must keep the prompt open, not revert to a generic status"
        );
        assert!(
            view.password_prompt_error(cx).is_some(),
            "a failed connect must surface a readable inline error"
        );
        assert!(
            !view
                .status()
                .is_some_and(|status| status.contains("Failed to connect")),
            "a failed connect from the prompt must not fall back to the generic status text"
        );
    });

    let fresh = StoredConnection {
        id,
        name: String::new(),
        display_kind: String::new(),
        display_host: String::new(),
        ssh: None,
        sanitized_url: None,
    };
    let result = fresh.get_url();
    assert!(
        result.is_err(),
        "a failed connect must never write anything to the keyring, got {result:?}"
    );
}

#[gpui::test]
fn a_password_auth_ssh_tunnel_with_an_absent_secret_keeps_the_prompt_open_with_an_error(
    cx: &mut TestAppContext,
) {
    let temp = TempStorePath::new("ssh-secret-absent");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "tunnelled".to_owned(),
            url: "postgres://host/db".to_owned(),
            ssh: Some(StoredSsh {
                enabled: true,
                host: "bastion.example.com".to_owned(),
                port: 22,
                user: "deploy".to_owned(),
                auth_kind: SshAuthKind::Password,
                key_path: None,
                host_key_policy: HostKeyPolicy::AcceptNew,
            }),
            ssh_secret: None,
        })
        .expect("add must succeed");
    store.connections()[0]
        .delete_url()
        .expect("delete_url must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));
    manager.update(cx, |view, cx| {
        let connection = view.connections()[0].connection.clone();
        view.open_password_prompt(&connection, cx);
    });
    assert!(manager.read_with(cx, |view, _app| view.password_prompt_is_open()));

    manager.update(cx, |view, cx| {
        view.set_password_prompt_input("some-password", cx);
    });
    manager
        .update(cx, ConnectionManagerView::submit_password_prompt)
        .detach();
    cx.run_until_parked();

    manager.read_with(cx, |view, cx| {
        assert!(
            view.password_prompt_is_open(),
            "a missing SSH tunnel secret must keep the prompt open"
        );
        assert_eq!(
            view.password_prompt_error(cx)
                .as_deref()
                .map(|e| e.starts_with("Failed to read tunnel secret:")),
            Some(true),
            "expected the tunnel-secret error, got {:?}",
            view.password_prompt_error(cx)
        );
    });

    let fresh = StoredConnection {
        id: manager.read_with(cx, |view, _app| view.connections()[0].connection.id),
        name: String::new(),
        display_kind: String::new(),
        display_host: String::new(),
        ssh: None,
        sanitized_url: None,
    };
    assert!(
        fresh.get_url().is_err(),
        "a tunnel-secret failure must never write the connection's own password to the keyring"
    );
    assert!(
        fresh.get_ssh_secret().is_err(),
        "a tunnel-secret failure must never write an SSH-secret keyring entry either"
    );
}

#[gpui::test]
fn cancelling_the_prompt_discards_the_password_and_leaves_the_session_untouched(
    cx: &mut TestAppContext,
) {
    let (manager, id) = open_prompt_over_absent_secret(cx, "cancel", "postgres://host/db");
    assert!(
        manager.read_with(cx, |view, _app| view.active().is_none()),
        "opening the prompt must not touch the session's active connection"
    );

    manager.update(cx, |view, cx| {
        view.set_password_prompt_input("typed-but-discarded", cx);
        view.cancel_password_prompt(cx);
    });

    manager.read_with(cx, |view, _app| {
        assert!(!view.password_prompt_is_open());
        assert!(!view.is_open());
        assert!(
            view.active().is_none(),
            "cancel must leave the session's connection state exactly as it was before the \
             prompt opened"
        );
    });

    let fresh = StoredConnection {
        id,
        name: String::new(),
        display_kind: String::new(),
        display_host: String::new(),
        ssh: None,
        sanitized_url: None,
    };
    let result = fresh.get_url();
    assert!(
        result.is_err(),
        "cancel must never write the typed password to the keyring, got {result:?}"
    );
}

#[gpui::test]
fn cancelling_then_reopening_the_prompt_for_the_same_connection_creates_a_new_prompt_entity(
    cx: &mut TestAppContext,
) {
    let (manager, id) =
        open_prompt_over_absent_secret(cx, "reopen-same-connection", "postgres://host/db");
    let first = manager
        .read_with(cx, |view, _app| view.password_prompt_entity())
        .expect("the prompt is open");

    manager.update(cx, |view, cx| {
        view.cancel_password_prompt(cx);
        view.connect(id, cx).detach();
    });

    let second = manager
        .read_with(cx, |view, _app| view.password_prompt_entity())
        .expect("reopening for the same connection must open the prompt again");
    assert_ne!(
        first, second,
        "reopening the prompt for the same connection must create a new prompt entity, so an \
         in-flight attempt started by the cancelled prompt cannot be mistaken for the new \
         prompt's own on completion"
    );
}

/// Cancelling a prompt while its own connect attempt is still in flight
/// (checkbox checked) must not let that attempt's later completion write
/// the password to the keyring: the security-relevant guard this exercises
/// is [`ConnectionManagerView::on_password_prompt_event`]'s stale-entity
/// check.
#[gpui::test]
async fn cancelling_before_an_in_flight_attempt_resolves_never_writes_the_keyring(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let (manager, id) = open_prompt_over_absent_secret(cx, "cancel-in-flight", "sqlite::memory:");
    manager.update(cx, |view, cx| {
        view.set_password_prompt_input("does-not-matter-for-sqlite", cx);
    });

    let connect_attempt = manager.update(cx, ConnectionManagerView::submit_password_prompt);
    manager.update(cx, ConnectionManagerView::cancel_password_prompt);
    connect_attempt.await;

    manager.read_with(cx, |view, _app| {
        assert!(
            !view.password_prompt_is_open(),
            "the prompt must stay cancelled/closed, not reopen on its own"
        );
    });

    let fresh = StoredConnection {
        id,
        name: String::new(),
        display_kind: String::new(),
        display_host: String::new(),
        ssh: None,
        sanitized_url: None,
    };
    let result = fresh.get_url();
    assert!(
        result.is_err(),
        "a cancelled-before-resolution attempt must never write the keyring, got {result:?}"
    );
}

#[gpui::test]
fn escape_closes_the_open_prompt(cx: &mut TestAppContext) {
    let (_temp, store) = store_with_absent_secret("escape", "escape target", "postgres://host/db");

    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    let id = manager.read_with(vcx, |view, _app| view.connections()[0].connection.id);
    manager.update(vcx, |view, cx| {
        view.connect(id, cx).detach();
    });
    vcx.run_until_parked();
    assert!(manager.read_with(vcx, |view, _app| view.password_prompt_is_open()));
    let field_focus = manager
        .read_with(
            vcx,
            ConnectionManagerView::password_prompt_field_focus_handle,
        )
        .expect("the prompt is open");
    assert_eq!(
        vcx.update(|window, cx| window.focused(cx)),
        Some(field_focus),
        "the password field must be focused when the prompt opens"
    );

    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    manager.read_with(vcx, |view, _app| {
        assert!(
            !view.password_prompt_is_open(),
            "Escape must close the prompt"
        );
        assert!(!view.is_open());
    });
}

// The two tests below assert only that clicking Connect / pressing Enter
// dispatch the same synchronous connect attempt (the prompt enters its
// "connecting" state): they stop short of letting a real background
// connect actually resolve. `submitting_an_empty_password_...` and the
// `a_successful_connect_.../a_failed_connect_...` tests above already cover
// the full attempt end to end via a directly-driven `submit_password_prompt`
// call, without needing a UI-driven trigger.

#[gpui::test]
fn clicking_the_connect_button_dispatches_a_connect_attempt(cx: &mut TestAppContext) {
    let (_temp, store) =
        store_with_absent_secret("click-connect", "click target", "postgres://host/db");

    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    let id = manager.read_with(vcx, |view, _app| view.connections()[0].connection.id);
    manager.update(vcx, |view, cx| {
        view.connect(id, cx).detach();
    });
    vcx.run_until_parked();
    manager.update(vcx, |view, cx| {
        view.set_password_prompt_input("clicked-password", cx);
    });

    let bounds = vcx
        .debug_bounds("password-prompt-connect")
        .expect("the Connect button must be tagged and painted");
    vcx.simulate_click(bounds.center(), Modifiers::default());

    manager.read_with(vcx, |view, cx| {
        assert!(
            view.password_prompt_connecting(cx),
            "clicking Connect must dispatch the same connect attempt submit_password_prompt runs"
        );
    });
}

#[gpui::test]
fn pressing_enter_in_the_password_field_triggers_the_same_connect_as_clicking(
    cx: &mut TestAppContext,
) {
    init_text_field_bindings(cx);
    let (_temp, store) =
        store_with_absent_secret("enter-connect", "enter target", "postgres://host/db");

    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    let id = manager.read_with(vcx, |view, _app| view.connections()[0].connection.id);
    manager.update(vcx, |view, cx| {
        view.connect(id, cx).detach();
    });
    vcx.run_until_parked();
    manager.update(vcx, |view, cx| {
        view.set_password_prompt_input("entered-password", cx);
    });

    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();

    manager.read_with(vcx, |view, cx| {
        assert!(
            view.password_prompt_connecting(cx),
            "Enter in the password field must submit the same as clicking Connect"
        );
    });
}
