//! The seam between this binary's `Session`/`ResultsView` and the
//! framework-independent `zsql_editor::EditorView`: builds the
//! `zsql_editor::QueryRunner` closure the editor calls to run the current
//! query. This is the only place in the binary that constructs an
//! `EditorView`.

use gpui::{Context, Entity};
use zsql_editor::{EditorView, QueryRunner};

use super::results::ResultsView;
use crate::session::Session;

/// Build an [`EditorView`] over `session`, running queries through it and
/// updating `results`'s source label to reflect each run.
#[must_use]
pub fn new_editor_view(
    session: Entity<Session>,
    results: Entity<ResultsView>,
    cx: &mut Context<EditorView>,
) -> EditorView {
    EditorView::new(query_runner(session, results), cx)
}

/// The `QueryRunner` seam passed to `EditorView::new`: runs `sql` through
/// `session` and relabels `results` to `"query"`, in that order, on every
/// invocation.
fn query_runner(session: Entity<Session>, results: Entity<ResultsView>) -> QueryRunner {
    Box::new(move |sql, cx| {
        results.update(cx, |results, cx| results.set_source_label("query", cx));
        session.update(cx, |session, cx| {
            session.run_query(sql, cx).detach();
        });
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use gpui::{AppContext as _, Focusable as _, TestAppContext, VisualTestContext};
    use zsql_core::{BatchSink, Connection, CoreError, QueryHandle, SchemaTree};
    use zsql_editor::RunQuery;

    use super::new_editor_view;
    use crate::session::Session;
    use crate::ui::results::ResultsView;

    /// A `Connection` double that records every `stream_query` call's SQL
    /// text instead of running anything.
    struct FakeConnection {
        queries: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Connection for FakeConnection {
        fn stream_query(&self, sql: String, _sink: BatchSink) -> QueryHandle {
            self.queries
                .lock()
                .expect("queries lock poisoned")
                .push(sql);
            let (cancel_tx, _cancel_rx) = flume::unbounded();
            QueryHandle::new(cancel_tx)
        }

        async fn introspect(&self) -> Result<SchemaTree, CoreError> {
            Ok(SchemaTree::default())
        }

        async fn ping(&self) -> Result<(), CoreError> {
            Ok(())
        }
    }

    /// Dispatching `RunQuery` through an adapter-wired `EditorView` must
    /// still produce both of the adapter's side effects: the SQL reaches
    /// `Session::run_query`, and `ResultsView`'s source label is set to
    /// "query". The Run button's `on_click` handler calls the same shared
    /// `run_current_query` method the `RunQuery` action does, so exercising
    /// the action covers both entry points.
    #[gpui::test]
    fn running_a_query_through_the_adapter_runs_it_and_labels_the_results(cx: &mut TestAppContext) {
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection: Arc<dyn Connection> = Arc::new(FakeConnection {
            queries: queries.clone(),
        });
        let session = cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)));
        let results = cx.update(|cx| cx.new(|cx| ResultsView::new(session.clone(), "", cx)));

        let session_for_editor = session.clone();
        let results_for_editor = results.clone();
        let (editor, vcx): (_, &mut VisualTestContext) = cx.add_window_view(|window, cx| {
            let view = new_editor_view(session_for_editor, results_for_editor, cx);
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
        results.update(vcx, |results, _cx| {
            assert_eq!(results.source_label_for_test(), "query");
        });
    }
}

/// Live-database end-to-end tests, gated on `ZSQL_TEST_DATABASE_URL` so
/// `cargo test` passes with no database present
#[cfg(test)]
mod live_tests {
    use std::time::Duration;

    use gpui::{AppContext as _, Focusable as _, TestAppContext, Timer};
    use zsql_editor::RunQuery;

    use super::new_editor_view;
    use crate::config::Config;
    use crate::session::{Session, SessionState};
    use crate::ui::results::ResultsView;

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

        let results = cx.new(|cx| ResultsView::new(session.clone(), "", cx));
        let session_for_editor = session.clone();
        let results_for_editor = results.clone();
        let (_editor, vcx) = cx.add_window_view(|window, cx| {
            let mut view = new_editor_view(session_for_editor, results_for_editor, cx);
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
