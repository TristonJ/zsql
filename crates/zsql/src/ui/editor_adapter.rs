//! The seam between this binary's app state and the framework-independent
//! `zsql_editor::EditorView`. This is the only place in the binary that
//! constructs an `EditorView`; `TabModel::build_editor` is its only
//! production caller, passing a tab-scoped `QueryRunner` that dispatches
//! back through itself (`TabModel::run_for_tab`) so each tab's run is
//! tracked independently.

use gpui::Context;
use zsql_editor::{EditorView, QueryRunner};

/// Build an [`EditorView`] whose `RunQuery`/Run-button seam is `run_query`
/// verbatim.
#[must_use]
pub fn new_tab_editor_view(run_query: QueryRunner, cx: &mut Context<EditorView>) -> EditorView {
    EditorView::new(run_query, cx)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use gpui::{Focusable as _, TestAppContext, VisualTestContext};
    use zsql_editor::{QueryRunner, RunQuery};

    use super::new_tab_editor_view;

    /// A `QueryRunner` double that records every SQL string it was asked to
    /// run, standing in for a tab's real `TabModel::run_for_tab` dispatch.
    fn recording_query_runner() -> (QueryRunner, Arc<Mutex<Vec<String>>>) {
        let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let queries = recorded.clone();
        let runner: QueryRunner = Box::new(move |sql, _cx| {
            queries.lock().expect("queries lock poisoned").push(sql);
        });
        (runner, recorded)
    }

    #[gpui::test]
    fn running_a_query_invokes_the_editors_query_runner(cx: &mut TestAppContext) {
        let (runner, queries) = recording_query_runner();

        let (editor, vcx): (_, &mut VisualTestContext) = cx.add_window_view(|window, cx| {
            let view = new_tab_editor_view(runner, cx);
            window.focus(&view.focus_handle(cx));
            view
        });
        editor.update(vcx, |view, _cx| {
            view.set_text_for_test("select * from orders");
        });

        vcx.dispatch_action(RunQuery);

        assert_eq!(
            queries.lock().expect("queries lock poisoned").as_slice(),
            ["select * from orders"]
        );
    }
}

/// Live-database end-to-end tests, gated on `ZSQL_TEST_DATABASE_URL` so
/// `cargo test` passes with no database present
#[cfg(test)]
mod live_tests {
    use std::time::Duration;

    use gpui::{AppContext as _, Focusable as _, TestAppContext, Timer};
    use zsql_editor::{QueryRunner, RunQuery};

    use super::new_tab_editor_view;
    use crate::config::Config;
    use crate::session::{Session, SessionState};

    fn live_database_url() -> Option<String> {
        let Ok(url) = std::env::var("ZSQL_TEST_DATABASE_URL") else {
            eprintln!("skipping live test: ZSQL_TEST_DATABASE_URL not set");
            return None;
        };
        Some(url)
    }

    /// Types a query into a focused, adapter-wired `EditorView`, dispatches
    /// `RunQuery`, and drives the session all the way to a live `Results`
    /// state -- the type-and-run loop end to end.
    #[gpui::test]
    async fn dispatching_run_query_reaches_live_results_when_configured(cx: &mut TestAppContext) {
        let Some(url) = live_database_url() else {
            return;
        };
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let mut cfg = Config::default();
        cfg.connection.default_url = Some(url);

        let session = cx.new(|_cx| Session::new(&cfg));
        session.update(cx, Session::connect).await;
        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "connect should succeed against a reachable database, got {:?}",
                session.state()
            );
        });

        let session_for_runner = session.clone();
        let run_query: QueryRunner = Box::new(move |sql, cx| {
            session_for_runner.update(cx, |session, cx| {
                session.run_query(sql, cx).detach();
            });
        });
        let (_editor, vcx) = cx.add_window_view(|window, cx| {
            let mut view = new_tab_editor_view(run_query, cx);
            view.set_text_for_test("SELECT * FROM orders ORDER BY placed_at DESC");
            window.focus(&view.focus_handle(cx));
            view
        });

        // `dispatch_action` already runs gpui's deterministic dispatcher
        // until nothing is immediately ready, but the query itself streams
        // over a real socket on a background OS thread (see
        // `zsql-postgres`'s driver), so a single pass is not enough: the
        // loop below alternates a real (not virtual-clock) sleep -- giving
        // that thread genuine wall-clock time to make progress and wake the
        // consumer task gpui's dispatcher scheduled -- with another
        // deterministic drain, until the session reaches a terminal state.
        vcx.dispatch_action(RunQuery);

        let mut reached_terminal_state = session.read_with(vcx, |session, _app| {
            matches!(
                session.state(),
                SessionState::Results(_) | SessionState::Error(_)
            )
        });
        for _ in 0..200 {
            if reached_terminal_state {
                break;
            }
            Timer::after(Duration::from_millis(10)).await;
            vcx.run_until_parked();
            reached_terminal_state = session.read_with(vcx, |session, _app| {
                matches!(
                    session.state(),
                    SessionState::Results(_) | SessionState::Error(_)
                )
            });
        }
        assert!(
            reached_terminal_state,
            "session did not reach a terminal state after dispatching RunQuery"
        );

        session.read_with(vcx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Results(_)),
                "expected SessionState::Results, got {:?}",
                session.state()
            );
            let result = session.result();
            let column_names: Vec<&str> = result.columns.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(
                column_names,
                vec![
                    "id",
                    "user_id",
                    "total_cents",
                    "status",
                    "metadata",
                    "placed_at"
                ]
            );
            assert_eq!(result.rows.len(), 3, "the seeded orders table has 3 rows");
        });
    }
}
