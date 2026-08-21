//! Live `Session::switch_database` integration tests against a real
//! Postgres server. Gated behind the `driver-integration-tests` feature and
//! skipped from the default `cargo test --all` run entirely -- see
//! `scripts/pg-dev.sh` for how to bring the fixture database up.
#![cfg(all(test, feature = "driver-integration-tests"))]

use gpui::{AppContext as _, TestAppContext};
use zsql_core::Connection;

use crate::session::{SchemaState, Session, SessionState};

/// The database this suite creates (and drops) alongside whatever
/// `ZSQL_TEST_POSTGRES_URL` already points at, to switch into.
const SECOND_DATABASE: &str = "zsql_test_switch_target";
/// A table seeded in [`SECOND_DATABASE`] so a switch into it has something
/// distinctive to assert on.
const SECOND_DATABASE_MARKER_TABLE: &str = "zsql_switch_marker";

fn live_database_url() -> String {
    std::env::var("ZSQL_TEST_POSTGRES_URL")
        .expect("ZSQL_TEST_POSTGRES_URL must be set to run database tests")
}

/// `url` with its database path segment replaced by `database`.
fn url_with_database(url: &str, database: &str) -> String {
    let mut parsed = zsql_core::ConnectionUrl::parse(url).expect("URL must parse");
    parsed.set_database(database);
    parsed.to_url_string()
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    futures::executor::block_on(fut)
}

/// Run `sql` to completion against `conn`, panicking on any error.
fn run_ddl(conn: &dyn Connection, sql: &str) {
    let (tx, rx) = flume::unbounded();
    let _handle = conn.stream_query(sql.to_owned(), tx);
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Ok(zsql_core::QueryEvent::Done { .. })) => break,
            Ok(Ok(_)) => {}
            Ok(Err(err)) => panic!("ddl setup failed: {err:?}"),
            Err(err) => panic!("ddl setup did not complete: {err:?}"),
        }
    }
}

/// Drop and recreate [`SECOND_DATABASE`] (via a connection to the original,
/// seeded database, since `CREATE`/`DROP DATABASE` cannot target the
/// database a connection is itself attached to), then seed
/// [`SECOND_DATABASE_MARKER_TABLE`] inside it via a fresh connection to
/// `SECOND_DATABASE` itself.
fn seed_second_database(original_url: &str) {
    let setup_conn = block_on(crate::drivers::connect(
        original_url.to_owned(),
        zsql_core::DEFAULT_QUERY_BATCH_SIZE,
    ))
    .expect("connecting to the original database for setup should succeed");
    terminate_other_sessions(&*setup_conn, SECOND_DATABASE);
    run_ddl(
        &*setup_conn,
        &format!("DROP DATABASE IF EXISTS {SECOND_DATABASE}"),
    );
    run_ddl(&*setup_conn, &format!("CREATE DATABASE {SECOND_DATABASE}"));
    block_on(setup_conn.close());

    let second_url = url_with_database(original_url, SECOND_DATABASE);
    let second_conn = block_on(crate::drivers::connect(
        second_url,
        zsql_core::DEFAULT_QUERY_BATCH_SIZE,
    ))
    .expect("connecting to the freshly created database should succeed");
    run_ddl(
        &*second_conn,
        &format!("CREATE TABLE {SECOND_DATABASE_MARKER_TABLE} (id int)"),
    );
    block_on(second_conn.close());
}

/// Drop [`SECOND_DATABASE`] via a connection to the original database.
fn drop_second_database(original_url: &str) {
    let setup_conn = block_on(crate::drivers::connect(
        original_url.to_owned(),
        zsql_core::DEFAULT_QUERY_BATCH_SIZE,
    ))
    .expect("connecting to the original database for teardown should succeed");
    terminate_other_sessions(&*setup_conn, SECOND_DATABASE);
    run_ddl(&*setup_conn, &format!("DROP DATABASE {SECOND_DATABASE}"));
    block_on(setup_conn.close());
}

/// Terminate every backend connected to `database` other than this one, so
/// a stray still-closing connection (e.g. from a pool this same test
/// dropped moments earlier) never blocks a `DROP DATABASE`.
fn terminate_other_sessions(conn: &dyn Connection, database: &str) {
    run_ddl(
        conn,
        &format!(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE datname = '{database}' AND pid <> pg_backend_pid()"
        ),
    );
}

/// A successful `switch_database` must land the session on the new
/// database's own schema (the seeded marker table present) with the
/// original database's own relations (e.g. the shared `users` seed table)
/// no longer part of the tree.
#[gpui::test]
async fn switch_database_re_introspects_into_the_new_databases_own_schema(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io_with_kicker(cx.executor());

    let original_url = live_database_url();
    seed_second_database(&original_url);

    let session = cx.new(|_cx| Session::new(&crate::config::Config::default()));
    session
        .update(cx, |session, cx| {
            session.connect_to(original_url.clone(), cx)
        })
        .await;
    session.read_with(cx, |session, _app| {
        assert!(matches!(session.state(), SessionState::Connected));
    });

    session
        .update(cx, |session, cx| {
            session.switch_database(SECOND_DATABASE, cx)
        })
        .await;

    session.read_with(cx, |session, _app| {
        assert!(
            matches!(session.state(), SessionState::Connected),
            "expected Connected after switching database, got {:?}",
            session.state()
        );
        assert_eq!(session.current_database(), Some(SECOND_DATABASE));

        match session.schema() {
            SchemaState::Ready(tree) => {
                assert_eq!(tree.catalogs.len(), 1);
                let catalog = &tree.catalogs[0];
                assert_eq!(catalog.name, SECOND_DATABASE);

                let public = catalog
                    .schemas
                    .iter()
                    .find(|s| s.name == "public")
                    .expect("the new database has a public schema");
                assert!(
                    public
                        .tables
                        .iter()
                        .any(|r| r.name == SECOND_DATABASE_MARKER_TABLE),
                    "expected the new database's own seeded table, got {:?}",
                    public.tables.iter().map(|r| &r.name).collect::<Vec<_>>()
                );
                assert!(
                    !public.tables.iter().any(|r| r.name == "users"),
                    "the original database's users table must not appear \
                     after switching away from it"
                );
            }
            other => panic!("expected a Ready schema after switching, got {other:?}"),
        }
    });

    // Switch back to the original database (and drop the session's own
    // handle entirely) before dropping SECOND_DATABASE: a live pool
    // connection into it would otherwise block the DROP the same way a
    // leftover setup connection does.
    session
        .update(cx, |session, cx| {
            session.switch_database(
                zsql_core::ConnectionUrl::parse(&original_url)
                    .expect("original URL must parse")
                    .database(),
                cx,
            )
        })
        .await;
    // The switch's own closing of its outgoing (SECOND_DATABASE) connection
    // is dispatched as a detached background task, not awaited by the
    // switch itself; park until it actually completes before dropping
    // SECOND_DATABASE below.
    cx.run_until_parked();
    drop(session);

    drop_second_database(&original_url);
}

/// Switching to a database that does not exist must leave the session
/// exactly on the original connection and database, with the failure
/// surfaced as an error.
#[gpui::test]
async fn switch_database_to_a_nonexistent_database_leaves_the_session_on_the_original(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let _guard = crate::test_support::serialize_real_io_with_kicker(cx.executor());

    let original_url = live_database_url();
    let original_database = zsql_core::ConnectionUrl::parse(&original_url)
        .expect("original URL must parse")
        .database();

    let session = cx.new(|_cx| Session::new(&crate::config::Config::default()));
    session
        .update(cx, |session, cx| session.connect_to(original_url, cx))
        .await;
    session.read_with(cx, |session, _app| {
        assert!(matches!(session.state(), SessionState::Connected));
    });
    let original_connection = session.read_with(cx, |session, _app| {
        session
            .connection_for_test()
            .expect("expected an active connection after connecting")
    });

    session
        .update(cx, |session, cx| {
            session.switch_database("zsql_test_database_that_does_not_exist", cx)
        })
        .await;

    session.read_with(cx, |session, _app| {
        assert!(
            matches!(session.state(), SessionState::Error(_)),
            "expected Error after switching to a nonexistent database, got {:?}",
            session.state()
        );
        assert_eq!(
            session.current_database(),
            Some(original_database.as_str()),
            "current_database must remain the original database"
        );
        assert!(
            session.holds_connection_for_test(&original_connection),
            "the session must still hold the original connection instance"
        );
    });
}
