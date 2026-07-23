use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable as _, KeyDownEvent, Keystroke,
    Modifiers, TestAppContext, VisualTestContext,
};

use super::form::ConnectionFormEvent;
use super::{
    ActiveConnection, ConnectionManagerView, ConnectionStore, ManagerView, TestOutcome,
    footer_display, host_label,
};
use crate::{
    connections::ConnectionArgs,
    session::{LivenessState, Session, SessionState},
};
use zsql_ui::text_field::TextFieldEvent;

/// The liveness probe timeout every test builds its manager with, unless a
/// test specifically cares about the value itself.
fn test_probe_timeout() -> Duration {
    crate::config::Config::default().liveness.probe_timeout()
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
    ConnectionManagerView::new(session, store, test_probe_timeout(), cx)
}

#[gpui::test]
fn a_freshly_loaded_store_lists_every_saved_connection(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("list");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "local pg".to_owned(),
            url: "postgres://localhost/app".to_owned(),
        })
        .expect("add must succeed");
    store
        .add(ConnectionArgs {
            name: "local sqlite".to_owned(),
            url: "sqlite::memory:".to_owned(),
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
fn an_unrecognized_scheme_surfaces_as_an_error_tag_not_a_panic(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("bad-scheme");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "mystery".to_owned(),
            url: "cassandra://host/db".to_owned(),
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.read_with(cx, |view, _app| {
        assert_eq!(view.connections()[0].connection.display_kind, "Unknown");
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
        view.add_connection(cx).expect("add must succeed");
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

#[gpui::test]
fn adding_a_connection_with_an_empty_name_is_rejected_without_persisting(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("reject-empty-name");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.set_name_input("", cx);
        view.set_url_input("sqlite::memory:", cx);
        view.add_connection(cx)
            .expect("validation rejection is Ok(())");

        assert!(view.connections().is_empty());
        assert!(view.status().is_some_and(|s| s.contains("name")));
    });
}

#[gpui::test]
fn adding_a_connection_with_an_empty_url_is_rejected_without_persisting(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("reject-empty-url");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.set_name_input("new db", cx);
        view.set_url_input("", cx);
        view.add_connection(cx)
            .expect("validation rejection is Ok(())");

        assert!(view.connections().is_empty());
        assert!(view.status().is_some_and(|s| s.contains("URL")));
    });
}

#[gpui::test]
fn adding_a_connection_with_an_unrecognized_scheme_is_rejected_without_persisting(
    cx: &mut TestAppContext,
) {
    let temp = TempStorePath::new("reject-bad-scheme");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.set_name_input("mystery", cx);
        view.set_url_input("cassandra://host/db", cx);
        view.add_connection(cx)
            .expect("validation rejection is Ok(())");

        assert!(view.connections().is_empty());
    });
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

// ---- footer_display ---------------------------------------------------

fn sample_active_connection() -> ActiveConnection {
    ActiveConnection {
        id: None,
        name: "zsql local".to_owned(),
        url: "postgres://localhost:5432/zsql".to_owned(),
    }
}

#[test]
fn footer_display_shows_the_active_connections_name_and_host_when_connected() {
    let active = sample_active_connection();
    match footer_display(
        &SessionState::Connected,
        &LivenessState::Unknown,
        true,
        Some(&active),
    ) {
        super::FooterDisplay::Connected { name, host } => {
            assert_eq!(name, "zsql local");
            assert_eq!(host, "localhost:5432");
        }
        other => panic!("expected FooterDisplay::Connected, got {other:?}"),
    }
}

#[test]
fn footer_display_is_disconnected_when_the_session_holds_no_live_connection() {
    let active = sample_active_connection();
    assert_eq!(
        footer_display(
            &SessionState::Error("connection refused".to_owned()),
            &LivenessState::Unknown,
            false,
            Some(&active)
        ),
        super::FooterDisplay::Disconnected,
        "a failed connect must render the not-connected prompt, not an error affordance"
    );
}

#[test]
fn footer_display_is_disconnected_when_connected_but_no_active_connection_is_tracked() {
    assert_eq!(
        footer_display(
            &SessionState::Connected,
            &LivenessState::Unknown,
            true,
            None
        ),
        super::FooterDisplay::Disconnected
    );
}

#[test]
fn footer_display_shows_connecting_during_a_connect_attempt() {
    assert_eq!(
        footer_display(
            &SessionState::Connecting,
            &LivenessState::Unknown,
            false,
            None
        ),
        super::FooterDisplay::Connecting
    );
}

#[test]
fn footer_display_shows_connected_immediately_after_a_successful_connect_needs_no_probe() {
    // A fresh `Connected` session has not had time for the recurring
    // liveness probe to complete even once yet.
    let active = sample_active_connection();
    assert_eq!(
        footer_display(
            &SessionState::Connected,
            &LivenessState::Unknown,
            true,
            Some(&active)
        ),
        super::FooterDisplay::Connected {
            name: "zsql local".to_owned(),
            host: "localhost:5432".to_owned(),
        },
        "Connected must not wait on the first Healthy probe result"
    );
}

#[test]
fn footer_display_shows_connecting_when_switching_even_though_the_prior_connection_is_still_held() {
    // Mid-switch: `connect_url` moves `state` to `Connecting` but keeps the
    // prior connection's `Arc` alive (and `is_connected()` therefore still
    // true) until the new attempt resolves.
    let active = sample_active_connection();
    assert_eq!(
        footer_display(
            &SessionState::Connecting,
            &LivenessState::Healthy,
            true,
            Some(&active)
        ),
        super::FooterDisplay::Connecting,
        "Connecting must win over a stale still-connected read from the connection being replaced"
    );
}

#[test]
fn footer_display_is_disconnected_when_liveness_is_unreachable_even_though_connected() {
    let active = sample_active_connection();
    let unreachable = LivenessState::Unreachable("connection reset".to_owned());
    assert_eq!(
        footer_display(&SessionState::Connected, &unreachable, true, Some(&active)),
        super::FooterDisplay::Disconnected
    );
}

#[test]
fn footer_display_stays_connected_through_a_query_error_that_leaves_the_connection_live() {
    let active = sample_active_connection();
    assert_eq!(
        footer_display(
            &SessionState::Error("syntax error at or near \"selct\"".to_owned()),
            &LivenessState::Healthy,
            true,
            Some(&active)
        ),
        super::FooterDisplay::Connected {
            name: "zsql local".to_owned(),
            host: "localhost:5432".to_owned(),
        },
        "a query error must not be mistaken for a connect failure while the connection is live"
    );
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
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.open(cx);
        view.show_add_form(cx);
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
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.open(cx);
        view.show_add_form(cx);
        view.set_name_input("staging", cx);
        view.set_url_input("postgres://host/db", cx);
        view.add_connection(cx).expect("add must succeed");

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
        })
        .expect("add first");
    store
        .add(ConnectionArgs {
            name: "second".to_owned(),
            url: "sqlite:///tmp/b.db".to_owned(),
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
fn show_edit_form_prefills_name_url_and_the_driver_fields(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("edit-prefill");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "staging".to_owned(),
            url: "postgres://app:s3cr3t@staging.internal:5432/app?sslmode=require".to_owned(),
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_edit_form(0, cx);

        let id = view.connections()[0].connection.id;
        assert_eq!(
            view.current_view(),
            ManagerView::Form,
            "edit form must be shown"
        );
        assert_eq!(
            view.form.read(cx).edit_id(),
            Some(id),
            "edit form must be shown for the right row"
        );
        assert_eq!(
            view.form.read(cx).name_field.read(cx).value().as_ref(),
            "staging"
        );
        assert_eq!(
            view.form.read(cx).url_field.read(cx).value().as_ref(),
            "postgres://app:s3cr3t@staging.internal:5432/app?sslmode=require"
        );
        assert_eq!(
            view.form.read(cx).host_field.read(cx).value().as_ref(),
            "staging.internal"
        );
        assert_eq!(
            view.form.read(cx).port_field.read(cx).value().as_ref(),
            "5432"
        );
        assert_eq!(
            view.form.read(cx).user_field.read(cx).value().as_ref(),
            "app"
        );
        assert_eq!(
            view.form.read(cx).password_field.read(cx).value().as_ref(),
            "s3cr3t"
        );
        assert_eq!(
            view.form.read(cx).database_field.read(cx).value().as_ref(),
            "app"
        );
        assert_eq!(
            view.form.read(cx).tls_field.read(cx).value().as_ref(),
            "require"
        );
    });
}

#[gpui::test]
fn show_edit_form_for_a_sqlite_url_prefills_only_the_path_field(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("edit-prefill-sqlite");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "reports".to_owned(),
            url: "sqlite:///tmp/reports.db".to_owned(),
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_edit_form(0, cx);
        assert_eq!(view.pending_driver_id(cx), Ok("sqlite"));
        assert_eq!(
            view.form
                .read(cx)
                .sqlite_path_field
                .read(cx)
                .value()
                .as_ref(),
            "/tmp/reports.db"
        );
        assert!(view.form.read(cx).host_field.read(cx).value().is_empty());
    });
}

#[gpui::test]
fn saving_an_edit_updates_the_row_in_place_without_appending_a_duplicate(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("save-edit");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "first".to_owned(),
            url: "postgres://host/a".to_owned(),
        })
        .expect("add first");
    store
        .add(ConnectionArgs {
            name: "second".to_owned(),
            url: "postgres://host/b".to_owned(),
        })
        .expect("add second");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_edit_form(0, cx);
        view.set_name_input("first renamed", cx);
        let host_field = view.form.read(cx).host_field.clone();
        host_field.update(cx, |field, cx| field.set_value("otherhost", cx));
    });

    manager.update(cx, |view, cx| {
        let id = view.connections()[0].connection.id;
        view.save_edit(id, cx).expect("save_edit must succeed");

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

// ---- URL -> fields sync ---------------------------------------------

#[gpui::test]
fn editing_the_url_field_reparses_every_driver_field(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("url-to-fields");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_url_input(
            "mssql://sa:pw@dbhost:1433/zsql?trustServerCertificate=true",
            cx,
        );
    });

    manager.read_with(cx, |view, cx| {
        assert_eq!(view.pending_driver_id(cx), Ok("mssql"));
        assert_eq!(
            view.form.read(cx).host_field.read(cx).value().as_ref(),
            "dbhost"
        );
        assert_eq!(
            view.form.read(cx).port_field.read(cx).value().as_ref(),
            "1433"
        );
        assert_eq!(
            view.form.read(cx).user_field.read(cx).value().as_ref(),
            "sa"
        );
        assert_eq!(
            view.form.read(cx).password_field.read(cx).value().as_ref(),
            "pw"
        );
        assert_eq!(
            view.form.read(cx).database_field.read(cx).value().as_ref(),
            "zsql"
        );
        assert_eq!(
            view.form.read(cx).tls_field.read(cx).value().as_ref(),
            "true"
        );
        assert!(view.form.read(cx).dim_reason().is_none());
    });
}

#[gpui::test]
fn an_unparseable_url_dims_the_field_section_with_a_reason_and_re_enables_once_valid(
    cx: &mut TestAppContext,
) {
    let temp = TempStorePath::new("dim-reenable");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_url_input("postgres://app@", cx);
    });
    manager.read_with(cx, |view, cx| {
        assert!(
            view.form.read(cx).dim_reason().is_some(),
            "an incomplete URL must dim the field section"
        );
        // The scheme is still recognizable, so the layout stays Postgres-shaped.
        assert_eq!(view.pending_driver_id(cx), Ok("postgres"));
    });

    manager.update(cx, |view, cx| {
        view.set_url_input("postgres://app@host:5432/db", cx);
    });
    manager.read_with(cx, |view, cx| {
        assert!(
            view.form.read(cx).dim_reason().is_none(),
            "a now-valid URL must clear the dim reason"
        );
        assert_eq!(
            view.form.read(cx).host_field.read(cx).value().as_ref(),
            "host"
        );
    });
}

// ---- fields -> URL sync -----------------------------------------------

#[gpui::test]
fn editing_the_port_field_changes_only_the_urls_port(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("field-port");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_url_input("postgres://app:s3cr3t@host:5432/app?sslmode=require", cx);
    });
    manager.update(cx, |view, cx| {
        let port_field = view.form.read(cx).port_field.clone();
        port_field.update(cx, |field, cx| field.set_value("6543", cx));
    });
    manager.read_with(cx, |view, cx| {
        assert_eq!(
            view.form.read(cx).url_field.read(cx).value().as_ref(),
            "postgres://app:s3cr3t@host:6543/app?sslmode=require",
            "only the port must change in the rebuilt URL"
        );
    });
}

#[gpui::test]
fn editing_the_host_field_leaves_user_password_database_and_params_intact(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("field-host");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_url_input("postgres://app:s3cr3t@host:5432/app?sslmode=require", cx);
    });
    manager.update(cx, |view, cx| {
        let host_field = view.form.read(cx).host_field.clone();
        host_field.update(cx, |field, cx| field.set_value("otherhost", cx));
    });
    manager.read_with(cx, |view, cx| {
        assert_eq!(
            view.form.read(cx).url_field.read(cx).value().as_ref(),
            "postgres://app:s3cr3t@otherhost:5432/app?sslmode=require"
        );
    });
}

#[gpui::test]
fn clearing_the_tls_field_removes_the_param_instead_of_leaving_it_empty(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("field-tls-clear");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_url_input("postgres://app:s3cr3t@host:5432/app?sslmode=require", cx);
    });
    manager.update(cx, |view, cx| {
        let tls_field = view.form.read(cx).tls_field.clone();
        tls_field.update(cx, |field, cx| field.set_value("", cx));
    });
    manager.read_with(cx, |view, cx| {
        let url = view.form.read(cx).url_field.read(cx).value().to_string();
        assert_eq!(
            url, "postgres://app:s3cr3t@host:5432/app",
            "clearing the TLS field must drop sslmode entirely, not leave 'sslmode='"
        );
        assert!(!url.contains("sslmode"));
    });
}

#[gpui::test]
fn editing_a_driver_field_rewrites_the_url_field_live(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("field-to-url-live");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_url_input("postgres://app@host:5432/orders", cx);
    });
    manager.update(cx, |view, cx| {
        let database_field = view.form.read(cx).database_field.clone();
        database_field.update(cx, |field, cx| field.set_value("other_db", cx));
    });
    manager.read_with(cx, |view, cx| {
        assert_eq!(
            view.form.read(cx).url_field.read(cx).value().as_ref(),
            "postgres://app@host:5432/other_db",
            "URL rewritten"
        );
    });
}

#[gpui::test]
fn editing_the_sqlite_path_field_rewrites_the_url(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("field-sqlite-path");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_url_input("sqlite::memory:", cx);
    });
    manager.update(cx, |view, cx| {
        let sqlite_path_field = view.form.read(cx).sqlite_path_field.clone();
        sqlite_path_field.update(cx, |field, cx| field.set_value("/tmp/scratch.db", cx));
    });
    manager.read_with(cx, |view, cx| {
        assert_eq!(
            view.form.read(cx).url_field.read(cx).value().as_ref(),
            "sqlite:///tmp/scratch.db"
        );
    });
}

// ---- password masking --------------------------------------------------

#[gpui::test]
fn the_password_field_starts_masked_and_the_toggle_reveals_it(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("password-mask");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.read_with(cx, |view, cx| {
        assert!(view.form.read(cx).password_field.read(cx).is_masked());
    });

    manager.update(cx, |view, cx| {
        view.form
            .update(cx, super::form::ConnectionForm::toggle_password_visible);
    });
    manager.read_with(cx, |view, cx| {
        assert!(!view.form.read(cx).password_field.read(cx).is_masked());
    });
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
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let session_for_assert = session.clone();
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, ConnectionManagerView::open);
    let task = manager.update(cx, |view, cx| view.connect_and_close(0, cx));
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
        // `connect_index` sets this synchronously, before the connect
        // attempt itself completes -- proof the real keyboard-dispatch path
        // reached the row's connect handler, the same one the click path
        // uses, not just that the modal happened to close some other way.
        assert!(
            view.status()
                .is_some_and(|status| status.contains("connecting")),
            "expected a real connect to have been dispatched, got status {:?}",
            view.status()
        );
    });
}

#[gpui::test]
async fn connect_index_updates_active_synchronously_before_the_connect_resolves(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("connect-index-sync-active");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "mem".to_owned(),
            url: "sqlite::memory:".to_owned(),
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    let task = manager.update(cx, |view, cx| {
        assert!(view.active().is_none(), "no connection is active yet");
        let task = view.connect_index(0, cx);
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
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    let task = manager.update(cx, |view, cx| {
        let task = view.connect_and_close(0, cx);
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
async fn a_failed_connect_index_does_not_revert_active_to_the_previous_connection(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io();

    let temp = TempStorePath::new("connect-index-fail-keeps-active");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "mem".to_owned(),
            url: "sqlite::memory:".to_owned(),
        })
        .expect("add must succeed");
    store
        .add(ConnectionArgs {
            name: "unrecognized".to_owned(),
            url: "cassandra://host/db".to_owned(),
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager
        .update(cx, |view, cx| view.connect_index(0, cx))
        .await;
    manager.read_with(cx, |view, _app| {
        assert_eq!(
            view.active().map(|active| active.name.as_str()),
            Some("mem")
        );
    });

    manager
        .update(cx, |view, cx| view.connect_index(1, cx))
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
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_name_input("scratch", cx);
        view.set_url_input("sqlite::memory:", cx);
    });

    let task = manager.update(cx, ConnectionManagerView::connect_unsaved);
    task.await;

    manager.read_with(cx, |view, _app| {
        assert!(
            !view.is_open(),
            "connect_unsaved must close the modal once the connect succeeds"
        );
        assert!(
            view.connections().is_empty(),
            "connect_unsaved must never persist the connection to the store"
        );
    });
    session_for_assert.read_with(cx, |session, _app| {
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
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_name_input("scratch", cx);
        view.set_url_input("sqlite::memory:", cx);
    });

    let task = manager.update(cx, |view, cx| {
        let task = view.connect_unsaved(cx);
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
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.open(cx);
        view.show_add_form(cx);
        view.set_name_input("will-fail", cx);
        // SQLite path in nonexistent directory: deterministically fails to connect
        let unopenable = format!(
            "sqlite:{}/zsql-connections-test-nonexistent-dir/db.sqlite3",
            std::env::temp_dir().display()
        );
        view.set_url_input(&unopenable, cx);
    });

    let task = manager.update(cx, ConnectionManagerView::connect_unsaved);
    task.await;

    manager.read_with(cx, |view, _app| {
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
        })
        .expect("add must succeed");
    store
        .add(ConnectionArgs {
            name: "other".to_owned(),
            url: "sqlite::memory:".to_owned(),
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let session_for_assert = session.clone();
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_edit_form(0, cx);
        view.cancel_form(cx);
        let id = view.connections()[1].connection.id;
        let _ = view.delete_id(id, cx);
    });

    session_for_assert.read_with(cx, |session, _app| {
        assert!(
            matches!(session.state(), SessionState::Empty),
            "neither editing nor deleting a row may connect the session, got {:?}",
            session.state()
        );
    });
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
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_name_input("scratch", cx);
        view.set_url_input("sqlite::memory:", cx);
    });

    let task = manager.update(cx, ConnectionManagerView::run_test);
    task.await;

    manager.read_with(cx, |view, app| {
        match view.test_outcome(app) {
            Some(TestOutcome::Connected { .. }) => {}
            other => panic!("expected a successful Test outcome, got {other:?}"),
        }
        assert!(
            view.connections().is_empty(),
            "Test must never persist a connection to the store"
        );
    });
    session_for_assert.read_with(cx, |session, _app| {
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
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_url_input(
            "postgres://nobody:nobody@zsql-test-unreachable.invalid:5432/db",
            cx,
        );
    });

    let task = manager.update(cx, ConnectionManagerView::run_test);
    task.await;

    manager.read_with(cx, |view, app| match view.test_outcome(app) {
        Some(TestOutcome::Failed(message)) => assert!(!message.is_empty()),
        other => panic!("expected a failed Test outcome, got {other:?}"),
    });
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
        })
        .expect("add must succeed");
    store
        .add(ConnectionArgs {
            name: "local sqlite".to_owned(),
            url: "sqlite::memory:".to_owned(),
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update(vcx, ConnectionManagerView::open);
    manager.update(vcx, ConnectionManagerView::show_add_form);
    manager.update(vcx, ConnectionManagerView::cancel_form);
    vcx.run_until_parked();
}

#[gpui::test]
fn the_edit_form_renders_prefilled_without_panicking(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("render-edit");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "staging".to_owned(),
            url: "postgres://app@staging.internal:5432/app".to_owned(),
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update(vcx, |view, cx| {
        view.open(cx);
        view.show_edit_form(0, cx);
    });
    vcx.run_until_parked();
}

#[gpui::test]
fn the_sqlite_field_section_renders_the_path_field_and_not_host_or_port(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("render-sqlite");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update(vcx, |view, cx| {
        view.open(cx);
        view.show_add_form(cx);
        view.set_url_input("sqlite::memory:", cx);
    });
    vcx.run_until_parked();

    manager.read_with(vcx, |view, app| {
        assert_eq!(view.pending_driver_id(app), Ok("sqlite"));
    });
}

#[gpui::test]
fn the_field_section_renders_dimmed_while_unparseable_and_undimmed_once_valid(
    cx: &mut TestAppContext,
) {
    let temp = TempStorePath::new("render-dim");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update(vcx, |view, cx| {
        view.open(cx);
        view.show_add_form(cx);
        view.set_url_input("postgres://app@", cx);
    });
    vcx.run_until_parked();
    manager.read_with(vcx, |view, app| {
        assert!(view.form.read(app).dim_reason().is_some());
    });

    manager.update(vcx, |view, cx| {
        view.set_url_input("postgres://app@host/db", cx);
    });
    vcx.run_until_parked();
    manager.read_with(vcx, |view, app| {
        assert!(view.form.read(app).dim_reason().is_none());
    });
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

    manager.update(vcx, |view, cx| {
        view.open(cx);
        view.show_add_form(cx);
        view.set_url_input(&long_url, cx);
    });
    vcx.run_until_parked();

    let bounds = vcx
        .debug_bounds("connection-modal-panel")
        .expect("the modal panel must be tagged and painted");
    assert_eq!(
        bounds.size.width,
        crate::ui::theme::MODAL_WIDTH,
        "a long URL must never widen the modal panel"
    );
}

// ---- Tab / Shift-Tab focus order -----------------------------------------

#[gpui::test]
fn tab_and_shift_tab_move_focus_through_the_form_in_visual_order(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("tab-order");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update(vcx, |view, cx| {
        view.open(cx);
        view.show_add_form(cx);
        view.set_url_input("postgres://app@host:5432/db", cx);
    });
    vcx.run_until_parked();

    let expected_order = manager.update(vcx, |view, cx| view.focus_order(cx));
    assert!(
        expected_order.len() >= 3,
        "the add form over a parsed postgres URL must expose name, url, and driver fields"
    );

    vcx.update(|window, _cx| {
        window.focus(&expected_order[0]);
    });

    // Tab moves focus forward through each control in order.
    for expected in expected_order.iter().skip(1) {
        vcx.simulate_keystrokes("tab");
        vcx.update(|window, cx| {
            assert_eq!(window.focused(cx).as_ref(), Some(expected));
        });
    }

    // Tab from the last control wraps back to the first.
    vcx.simulate_keystrokes("tab");
    vcx.update(|window, cx| {
        assert_eq!(window.focused(cx).as_ref(), Some(&expected_order[0]));
    });

    // Shift-Tab from the first control wraps back to the last.
    vcx.simulate_keystrokes("shift-tab");
    vcx.update(|window, cx| {
        assert_eq!(
            window.focused(cx).as_ref(),
            Some(&expected_order[expected_order.len() - 1])
        );
    });
}

/// [`tab_and_shift_tab_move_focus_through_the_form_in_visual_order`]'s
/// scenario, parameterized over `url`: opens the add form on `url`, then
/// checks the form's own `focus_order()` round-trips through Tab and
/// Shift-Tab, including wrap-around at both ends.
fn assert_focus_order_round_trips_through_tab_and_shift_tab(cx: &mut TestAppContext, url: &str) {
    let temp = TempStorePath::new("tab-order-round-trip");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update(vcx, |view, cx| {
        view.open(cx);
        view.show_add_form(cx);
        view.set_url_input(url, cx);
    });
    vcx.run_until_parked();

    let expected_order = manager.update(vcx, |view, cx| view.focus_order(cx));
    assert!(
        expected_order.len() >= 3,
        "the add form over a parsed url must expose name, url, and driver fields"
    );

    assert_tab_cycles_through_in_order(vcx, &expected_order);
}

#[gpui::test]
fn tab_and_shift_tab_move_focus_through_the_form_in_visual_order_for_an_mssql_url(
    cx: &mut TestAppContext,
) {
    assert_focus_order_round_trips_through_tab_and_shift_tab(cx, "mssql://sa:pw@dbhost:1433/db");
}

#[gpui::test]
fn tab_and_shift_tab_move_focus_through_the_form_in_visual_order_for_a_sqlite_url(
    cx: &mut TestAppContext,
) {
    assert_focus_order_round_trips_through_tab_and_shift_tab(cx, "sqlite::memory:");
}

/// Tab forward through every handle in `order` starting from `order[0]`,
/// asserting each keystroke lands on the next concrete handle, then checks
/// wrap-around in both directions: Tab from the last control back to the
/// first, and Shift-Tab from the first back to the last.
fn assert_tab_cycles_through_in_order(vcx: &mut VisualTestContext, order: &[FocusHandle]) {
    assert!(
        order.len() >= 2,
        "need at least two controls to cycle through"
    );
    vcx.update(|window, _cx| window.focus(&order[0]));
    vcx.run_until_parked();

    for expected in order.iter().skip(1) {
        vcx.simulate_keystrokes("tab");
        vcx.update(|window, cx| {
            assert_eq!(window.focused(cx).as_ref(), Some(expected));
        });
    }

    vcx.simulate_keystrokes("tab");
    vcx.update(|window, cx| {
        assert_eq!(
            window.focused(cx).as_ref(),
            Some(&order[0]),
            "Tab from the last control must wrap to the first"
        );
    });

    vcx.simulate_keystrokes("shift-tab");
    vcx.update(|window, cx| {
        assert_eq!(
            window.focused(cx).as_ref(),
            Some(&order[order.len() - 1]),
            "Shift-Tab from the first control must wrap to the last"
        );
    });
}

/// The add form's full concrete focus chain over a parsed, non-sqlite URL:
/// name, url, every network field, then the footer buttons in visual order.
fn add_form_network_focus_chain(
    view: &ConnectionManagerView,
    cx: &Context<ConnectionManagerView>,
) -> Vec<FocusHandle> {
    let form = view.form.read(cx);
    vec![
        form.name_field.read(cx).focus_handle(cx),
        form.url_field.read(cx).focus_handle(cx),
        form.host_field.read(cx).focus_handle(cx),
        form.port_field.read(cx).focus_handle(cx),
        form.user_field.read(cx).focus_handle(cx),
        form.password_field.read(cx).focus_handle(cx),
        form.database_field.read(cx).focus_handle(cx),
        form.tls_field.read(cx).focus_handle(cx),
        form.cancel_focus.clone(),
        form.test_focus.clone(),
        form.connect_focus.clone(),
        form.save_focus.clone(),
    ]
}

/// The edit form's equivalent of [`add_form_network_focus_chain`] (no
/// Connect button).
fn edit_form_network_focus_chain(
    view: &ConnectionManagerView,
    cx: &Context<ConnectionManagerView>,
) -> Vec<FocusHandle> {
    let form = view.form.read(cx);
    vec![
        form.name_field.read(cx).focus_handle(cx),
        form.url_field.read(cx).focus_handle(cx),
        form.host_field.read(cx).focus_handle(cx),
        form.port_field.read(cx).focus_handle(cx),
        form.user_field.read(cx).focus_handle(cx),
        form.password_field.read(cx).focus_handle(cx),
        form.database_field.read(cx).focus_handle(cx),
        form.tls_field.read(cx).focus_handle(cx),
        form.cancel_focus.clone(),
        form.test_focus.clone(),
        form.save_focus.clone(),
    ]
}

/// For the add form over `url` (expected to resolve to the `mysql` driver,
/// whether via a `mysql://` or `mariadb://` scheme), Tab from the URL field
/// must advance through host, port, user, password, database, and tls
/// before reaching the footer buttons, and the whole chain must wrap in
/// both directions. Asserts against each field's own focus handle, not a
/// re-derived `focus_order()` list, so the assertion cannot pass merely
/// because `focus_order()` and the test agree on the same (possibly wrong)
/// derivation.
fn assert_add_form_tab_order_covers_network_fields(cx: &mut TestAppContext, url: &str) {
    let temp = TempStorePath::new("tab-order-network-add");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update(vcx, |view, cx| {
        view.open(cx);
        view.show_add_form(cx);
        view.set_url_input(url, cx);
    });
    vcx.run_until_parked();

    manager.read_with(vcx, |view, app| {
        assert_eq!(
            view.pending_driver_id(app),
            Ok("mysql"),
            "url {url} must resolve to the registered mysql driver"
        );
    });

    let order = manager.update(vcx, |view, cx| add_form_network_focus_chain(view, cx));
    assert_tab_cycles_through_in_order(vcx, &order);
}

/// [`assert_add_form_tab_order_covers_network_fields`]'s edit-form
/// equivalent: the row at index 0 is pre-loaded from `url` and the form is
/// opened via [`ConnectionManagerView::show_edit_form`].
fn assert_edit_form_tab_order_covers_network_fields(cx: &mut TestAppContext, url: &str) {
    let temp = TempStorePath::new("tab-order-network-edit");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "db".to_owned(),
            url: url.to_owned(),
        })
        .expect("add must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let (manager, vcx) = cx.add_window_view(|_window, cx| new_manager(cx, session, store));

    manager.update(vcx, |view, cx| {
        view.open(cx);
        view.show_edit_form(0, cx);
    });
    vcx.run_until_parked();

    manager.read_with(vcx, |view, app| {
        assert_eq!(
            view.pending_driver_id(app),
            Ok("mysql"),
            "url {url} must resolve to the registered mysql driver"
        );
    });

    let order = manager.update(vcx, |view, cx| edit_form_network_focus_chain(view, cx));
    assert_tab_cycles_through_in_order(vcx, &order);
}

#[gpui::test]
fn tab_order_for_the_add_form_covers_network_fields_for_a_mysql_url(cx: &mut TestAppContext) {
    assert_add_form_tab_order_covers_network_fields(cx, "mysql://app:pw@dbhost:3306/orders");
}

#[gpui::test]
fn tab_order_for_the_add_form_covers_network_fields_for_a_mariadb_url(cx: &mut TestAppContext) {
    assert_add_form_tab_order_covers_network_fields(cx, "mariadb://app:pw@dbhost:3306/orders");
}

#[gpui::test]
fn tab_order_for_the_edit_form_covers_network_fields_for_a_mysql_url(cx: &mut TestAppContext) {
    assert_edit_form_tab_order_covers_network_fields(cx, "mysql://app:pw@dbhost:3306/orders");
}

#[gpui::test]
fn tab_order_for_the_edit_form_covers_network_fields_for_a_mariadb_url(cx: &mut TestAppContext) {
    assert_edit_form_tab_order_covers_network_fields(cx, "mariadb://app:pw@dbhost:3306/orders");
}

#[gpui::test]
fn focus_order_for_an_empty_url_contains_only_name_url_and_footer_buttons(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("tab-order-empty-url");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
    });

    manager.update(cx, |view, cx| {
        assert!(view.pending_driver_id(cx).is_err());
        let order = view.focus_order(cx);
        let form = view.form.read(cx);
        let expected = vec![
            form.name_field.read(cx).focus_handle(cx),
            form.url_field.read(cx).focus_handle(cx),
            form.cancel_focus.clone(),
            form.test_focus.clone(),
            form.connect_focus.clone(),
            form.save_focus.clone(),
        ];
        assert_eq!(
            order, expected,
            "an empty URL must expose no driver fields at all"
        );
    });
}

#[gpui::test]
fn focus_order_for_an_unrecognized_scheme_contains_only_name_url_and_footer_buttons(
    cx: &mut TestAppContext,
) {
    let temp = TempStorePath::new("tab-order-unrecognized-scheme");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_url_input("cassandra://host/db", cx);
    });

    manager.update(cx, |view, cx| {
        assert!(view.pending_driver_id(cx).is_err());
        let order = view.focus_order(cx);
        let form = view.form.read(cx);
        let expected = vec![
            form.name_field.read(cx).focus_handle(cx),
            form.url_field.read(cx).focus_handle(cx),
            form.cancel_focus.clone(),
            form.test_focus.clone(),
            form.connect_focus.clone(),
            form.save_focus.clone(),
        ];
        assert_eq!(
            order, expected,
            "an unrecognized scheme must expose no driver fields, not even a stale sqlite path field"
        );
    });
}

// ---- on_form_event router (footer button wiring) -------------------------

#[gpui::test]
fn a_cancel_event_from_the_form_returns_the_modal_to_the_list(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("form-event-cancel");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.open(cx);
        view.show_add_form(cx);
        view.set_name_input("staging", cx);
        view.form
            .update(cx, |_form, cx| cx.emit(ConnectionFormEvent::Cancel));
    });

    manager.read_with(cx, |view, cx| {
        assert_eq!(
            view.current_view(),
            ManagerView::List,
            "a Cancel event emitted by the form must route through on_form_event to cancel_form"
        );
        assert!(view.form.read(cx).name_field.read(cx).value().is_empty());
    });
}

#[gpui::test]
fn an_add_event_from_the_form_persists_a_new_connection(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("form-event-add");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_name_input("from-footer", cx);
        view.set_url_input("sqlite::memory:", cx);
        view.form
            .update(cx, |_form, cx| cx.emit(ConnectionFormEvent::Add));
    });

    manager.read_with(cx, |view, _app| {
        assert_eq!(
            view.connections().len(),
            1,
            "an Add event emitted by the form must route through on_form_event to add_connection"
        );
        assert_eq!(view.connections()[0].connection.name, "from-footer");
        assert_eq!(view.current_view(), ManagerView::List);
    });
}

#[gpui::test]
fn an_edit_event_from_the_form_updates_the_row_in_place(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("form-event-edit");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "first".to_owned(),
            url: "postgres://host/a".to_owned(),
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_edit_form(0, cx);
        view.set_name_input("renamed via footer", cx);
        let id = view.connections()[0].connection.id;
        view.form
            .update(cx, |_form, cx| cx.emit(ConnectionFormEvent::Edit { id }));
    });

    manager.read_with(cx, |view, _app| {
        assert_eq!(
            view.connections().len(),
            1,
            "an Edit event must update the row in place, not append a duplicate"
        );
        assert_eq!(view.connections()[0].connection.name, "renamed via footer");
        assert_eq!(
            view.current_view(),
            ManagerView::List,
            "an Edit event emitted by the form must route through on_form_event to save_edit"
        );
    });
}

/// [`ConnectionManagerView::run_test`]'s spawned probe is real I/O the
/// manager never awaits synchronously; the routing this test pins is
/// [`super::ConnectionManagerView::on_form_event`] reaching `run_test` at
/// all, which is already visible in the `Pending` outcome `run_test` sets
/// before it ever spawns the probe -- the probe's own eventual result is
/// covered by [`running_test_never_mutates_the_sessions_active_connection_or_persists`].
#[gpui::test]
fn a_test_event_from_the_form_starts_a_probe_via_run_test(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("form-event-test");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_url_input("sqlite::memory:", cx);
        view.form
            .update(cx, |_form, cx| cx.emit(ConnectionFormEvent::Test));
    });

    manager.read_with(cx, |view, app| {
        assert_eq!(
            view.test_outcome(app),
            Some(&TestOutcome::Pending),
            "a Test event emitted by the form must route through on_form_event to run_test"
        );
    });
}

/// [`ConnectionManagerView::connect_unsaved`]'s equivalent of
/// [`a_test_event_from_the_form_starts_a_probe_via_run_test`]'s doc comment:
/// pins the routing through its synchronous, pre-await side effects (`active`
/// and `status` updated before the connect itself resolves), the same
/// contract [`connect_unsaved_updates_active_synchronously_before_the_connect_resolves`]
/// asserts when calling `connect_unsaved` directly.
#[gpui::test]
fn a_connect_event_from_the_form_dispatches_a_connect_via_connect_unsaved(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("form-event-connect");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_name_input("via-footer", cx);
        view.set_url_input("sqlite::memory:", cx);
        view.form
            .update(cx, |_form, cx| cx.emit(ConnectionFormEvent::Connect));
    });

    manager.read_with(cx, |view, _app| {
        assert_eq!(
            view.active().map(|active| active.name.as_str()),
            Some("via-footer"),
            "a Connect event emitted by the form must route through on_form_event to \
             connect_unsaved"
        );
        assert!(
            view.status()
                .is_some_and(|status| status.contains("Connecting")),
            "expected the connecting status connect_unsaved sets synchronously, got {:?}",
            view.status()
        );
    });
}

// ---- Enter-to-submit (name/url TextFieldEvent::Submit) --------------------

#[gpui::test]
fn submitting_the_name_field_in_add_mode_persists_a_new_connection(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("submit-name-add");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_name_input("enter-submitted", cx);
        view.set_url_input("sqlite::memory:", cx);
        let name_field = view.form.read(cx).name_field.clone();
        name_field.update(cx, |_field, cx| cx.emit(TextFieldEvent::Submit));
    });

    manager.read_with(cx, |view, _app| {
        assert_eq!(
            view.connections().len(),
            1,
            "Enter in the name field must submit the add form, the same as clicking Save"
        );
        assert_eq!(view.connections()[0].connection.name, "enter-submitted");
        assert_eq!(view.current_view(), ManagerView::List);
    });
}

#[gpui::test]
fn submitting_the_url_field_in_add_mode_persists_a_new_connection(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("submit-url-add");
    let store = ConnectionStore::load(&temp.0).expect("load must succeed");
    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_add_form(cx);
        view.set_name_input("enter-submitted-url", cx);
        view.set_url_input("sqlite::memory:", cx);
        let url_field = view.form.read(cx).url_field.clone();
        url_field.update(cx, |_field, cx| cx.emit(TextFieldEvent::Submit));
    });

    manager.read_with(cx, |view, _app| {
        assert_eq!(
            view.connections().len(),
            1,
            "Enter in the url field must submit the add form, the same as clicking Save"
        );
        assert_eq!(view.connections()[0].connection.name, "enter-submitted-url");
        assert_eq!(view.current_view(), ManagerView::List);
    });
}

#[gpui::test]
fn submitting_the_name_field_in_edit_mode_updates_the_row_in_place(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("submit-name-edit");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "first".to_owned(),
            url: "postgres://host/a".to_owned(),
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_edit_form(0, cx);
        view.set_name_input("edited via enter", cx);
        let name_field = view.form.read(cx).name_field.clone();
        name_field.update(cx, |_field, cx| cx.emit(TextFieldEvent::Submit));
    });

    manager.read_with(cx, |view, _app| {
        assert_eq!(
            view.connections().len(),
            1,
            "Enter in edit mode must update the row in place, not append a duplicate"
        );
        assert_eq!(view.connections()[0].connection.name, "edited via enter");
        assert_eq!(view.current_view(), ManagerView::List);
    });
}

#[gpui::test]
fn submitting_the_url_field_in_edit_mode_updates_the_row_in_place(cx: &mut TestAppContext) {
    let temp = TempStorePath::new("submit-url-edit");
    let mut store = ConnectionStore::load(&temp.0).expect("load must succeed");
    store
        .add(ConnectionArgs {
            name: "first".to_owned(),
            url: "postgres://host/a".to_owned(),
        })
        .expect("add must succeed");

    let session = cx.new(|_cx| session_with_no_url());
    let manager = cx.new(|cx| new_manager(cx, session, store));

    manager.update(cx, |view, cx| {
        view.show_edit_form(0, cx);
        view.set_name_input("edited via enter url", cx);
        let url_field = view.form.read(cx).url_field.clone();
        url_field.update(cx, |_field, cx| cx.emit(TextFieldEvent::Submit));
    });

    manager.read_with(cx, |view, _app| {
        assert_eq!(
            view.connections().len(),
            1,
            "Enter in edit mode must update the row in place, not append a duplicate"
        );
        assert_eq!(
            view.connections()[0].connection.name,
            "edited via enter url"
        );
        assert_eq!(view.current_view(), ManagerView::List);
    });
}
