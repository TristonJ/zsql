//! Runs a batch of statements as one explicit database transaction against
//! the session's active connection.

use gpui::{AppContext as _, Context, Task};
use zsql_core::Transaction;

use super::Session;

/// A transactional batch that failed partway through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionFailure {
    /// Index into the submitted statement list of the statement that
    /// failed. `None` when the failure happened opening or closing the
    /// transaction itself, rather than executing one of its statements.
    pub statement_index: Option<usize>,
    /// The database's own error text.
    pub message: String,
}

impl Session {
    /// Run every statement in `statements`, in order, as one explicit
    /// transaction against the active connection. On the first statement to
    /// fail, rolls back and resolves with that statement's index and the
    /// database's own error text. Nothing already executed in the batch is
    /// left committed. Resolves with an index-less failure if there is no
    /// active connection, or if opening or committing the transaction
    /// itself fails.
    pub fn run_in_transaction(
        &self,
        statements: Vec<String>,
        cx: &Context<Self>,
    ) -> Task<Result<(), TransactionFailure>> {
        let Some(connection) = self.connection.clone() else {
            return Task::ready(Err(TransactionFailure {
                statement_index: None,
                message: "cannot run the batch: not connected".to_owned(),
            }));
        };
        cx.background_spawn(run_transaction_batch(connection, statements))
    }
}

#[tracing::instrument(
    name = "session_run_in_transaction",
    skip(connection, statements),
    fields(statement_count = statements.len())
)]
async fn run_transaction_batch(
    connection: std::sync::Arc<dyn zsql_core::Connection>,
    statements: Vec<String>,
) -> Result<(), TransactionFailure> {
    let transaction = Transaction::begin(connection)
        .await
        .map_err(|err| TransactionFailure {
            statement_index: None,
            message: err.to_string(),
        })?;

    for (index, sql) in statements.iter().enumerate() {
        if let Err(err) = transaction.execute(sql).await {
            tracing::warn!(index, error = %err, "batch statement failed; rolling back");
            let _ = transaction.rollback().await;
            return Err(TransactionFailure {
                statement_index: Some(index),
                message: err.to_string(),
            });
        }
    }

    transaction
        .commit()
        .await
        .map_err(|err| TransactionFailure {
            statement_index: None,
            message: err.to_string(),
        })?;

    tracing::info!(statement_count = statements.len(), "transaction committed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};

    use crate::config::Config;
    use crate::session::{Session, SessionState};

    /// Runs `sql` and waits for it to finish, panicking on failure. Test
    /// setup only: does not go through [`Session::run_in_transaction`].
    async fn run_setup_sql(session: &gpui::Entity<Session>, sql: &str, cx: &mut TestAppContext) {
        session
            .update(cx, |session, cx| session.run_query(sql.to_owned(), cx))
            .await;
        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Results(_)),
                "setup SQL must succeed, got {:?}",
                session.state()
            );
        });
    }

    async fn sqlite_session_with_two_rows(cx: &mut TestAppContext) -> gpui::Entity<Session> {
        cx.executor().allow_parking();
        let session = cx.new(|_cx| Session::new(&Config::default()));
        session
            .update(cx, |session, cx| session.connect_to("sqlite::memory:", cx))
            .await;
        run_setup_sql(
            &session,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)",
            cx,
        )
        .await;
        run_setup_sql(
            &session,
            "INSERT INTO t (id, name) VALUES (1, 'a'), (2, 'b')",
            cx,
        )
        .await;
        session
    }

    async fn row_ids(session: &gpui::Entity<Session>, cx: &mut TestAppContext) -> Vec<i64> {
        session
            .update(cx, |session, cx| {
                session.run_query("SELECT id FROM t ORDER BY id".to_owned(), cx)
            })
            .await;
        session.read_with(cx, |session, _app| {
            session
                .result()
                .rows
                .iter()
                .map(|row| match &row.0[0] {
                    zsql_core::Value::Int(id) => *id,
                    other => panic!("expected an integer id, got {other:?}"),
                })
                .collect()
        })
    }

    /// `(id, name)` pairs for every row still present, in id order.
    async fn row_id_names(
        session: &gpui::Entity<Session>,
        cx: &mut TestAppContext,
    ) -> Vec<(i64, String)> {
        session
            .update(cx, |session, cx| {
                session.run_query("SELECT id, name FROM t ORDER BY id".to_owned(), cx)
            })
            .await;
        session.read_with(cx, |session, _app| {
            session
                .result()
                .rows
                .iter()
                .map(|row| {
                    let id = match &row.0[0] {
                        zsql_core::Value::Int(id) => *id,
                        other => panic!("expected an integer id, got {other:?}"),
                    };
                    let name = match &row.0[1] {
                        zsql_core::Value::Text(name) => name.clone(),
                        other => panic!("expected a text name, got {other:?}"),
                    };
                    (id, name)
                })
                .collect()
        })
    }

    #[gpui::test]
    async fn a_successful_batch_commits_every_statement(cx: &mut TestAppContext) {
        let _guard = crate::test_support::serialize_real_io();
        let session = sqlite_session_with_two_rows(cx).await;

        let outcome = session
            .update(cx, |session, cx| {
                session.run_in_transaction(
                    vec![
                        "DELETE FROM \"main\".\"t\" WHERE \"id\" = 1;".to_owned(),
                        "DELETE FROM \"main\".\"t\" WHERE \"id\" = 2;".to_owned(),
                    ],
                    cx,
                )
            })
            .await;

        assert_eq!(outcome, Ok(()));
        assert_eq!(row_ids(&session, cx).await, Vec::<i64>::new());
    }

    #[gpui::test]
    async fn a_failing_statement_rolls_back_every_statement_in_the_batch(cx: &mut TestAppContext) {
        let _guard = crate::test_support::serialize_real_io();
        let session = sqlite_session_with_two_rows(cx).await;

        let outcome = session
            .update(cx, |session, cx| {
                session.run_in_transaction(
                    vec![
                        "DELETE FROM \"main\".\"t\" WHERE \"id\" = 1;".to_owned(),
                        "DELETE FROM \"main\".\"missing\" WHERE \"id\" = 1;".to_owned(),
                    ],
                    cx,
                )
            })
            .await;

        let failure = outcome.expect_err("the batch's second statement must fail");
        assert_eq!(failure.statement_index, Some(1));
        assert!(!failure.message.is_empty());

        assert_eq!(
            row_ids(&session, cx).await,
            vec![1, 2],
            "a failed batch must leave the database completely unchanged, including the \
             statement that would have succeeded on its own"
        );
    }

    #[gpui::test]
    async fn a_mixed_batch_of_one_update_and_one_delete_commits_both_in_one_transaction(
        cx: &mut TestAppContext,
    ) {
        let _guard = crate::test_support::serialize_real_io();
        let session = sqlite_session_with_two_rows(cx).await;

        let outcome = session
            .update(cx, |session, cx| {
                session.run_in_transaction(
                    vec![
                        "UPDATE \"main\".\"t\" SET \"name\" = 'shipped' WHERE \"id\" = 1;"
                            .to_owned(),
                        "DELETE FROM \"main\".\"t\" WHERE \"id\" = 2;".to_owned(),
                    ],
                    cx,
                )
            })
            .await;

        assert_eq!(outcome, Ok(()));
        assert_eq!(
            row_id_names(&session, cx).await,
            vec![(1, "shipped".to_owned())],
            "the update must take effect on row 1 and the delete must remove row 2, both \
             committed by the same batch"
        );
    }

    #[gpui::test]
    async fn a_failing_statement_in_a_mixed_batch_rolls_back_both_the_update_and_the_delete(
        cx: &mut TestAppContext,
    ) {
        let _guard = crate::test_support::serialize_real_io();
        let session = sqlite_session_with_two_rows(cx).await;

        let outcome = session
            .update(cx, |session, cx| {
                session.run_in_transaction(
                    vec![
                        "UPDATE \"main\".\"t\" SET \"name\" = 'shipped' WHERE \"id\" = 1;"
                            .to_owned(),
                        "DELETE FROM \"main\".\"t\" WHERE \"id\" = 2;".to_owned(),
                        "UPDATE \"main\".\"missing\" SET \"x\" = 1 WHERE \"id\" = 1;".to_owned(),
                    ],
                    cx,
                )
            })
            .await;

        let failure = outcome.expect_err("the batch's third statement must fail");
        assert_eq!(failure.statement_index, Some(2));
        assert!(!failure.message.is_empty());

        assert_eq!(
            row_id_names(&session, cx).await,
            vec![(1, "a".to_owned()), (2, "b".to_owned())],
            "a failed mixed batch must leave the database completely unchanged: neither the \
             update nor the delete that would have succeeded on their own"
        );
    }

    #[gpui::test]
    async fn run_in_transaction_without_a_connection_fails_with_no_statement_index(
        cx: &mut TestAppContext,
    ) {
        let session = cx.new(|_cx| Session::new(&Config::default()));
        let outcome = session
            .update(cx, |session, cx| {
                session.run_in_transaction(vec!["DELETE FROM t WHERE id = 1".to_owned()], cx)
            })
            .await;

        let failure = outcome.expect_err("no connection means the batch cannot run");
        assert_eq!(failure.statement_index, None);
    }

    /// A connection whose very first `stream_query` call (the transaction's
    /// own `BEGIN`) fails, so [`Session::run_in_transaction`] can be
    /// exercised against a live connection that never reaches its own
    /// statements.
    struct BeginFailsConnection;

    #[async_trait::async_trait]
    impl zsql_core::Connection for BeginFailsConnection {
        fn stream_query(&self, _sql: String, sink: zsql_core::BatchSink) -> zsql_core::QueryHandle {
            let (cancel_tx, _cancel_rx) = flume::unbounded();
            let _ = sink.send(Err(zsql_core::CoreError::query("begin failed")));
            zsql_core::QueryHandle::new(cancel_tx)
        }

        async fn introspect(&self) -> Result<zsql_core::SchemaTree, zsql_core::CoreError> {
            unimplemented!("not exercised by this test")
        }

        async fn ping(&self) -> Result<(), zsql_core::CoreError> {
            unimplemented!("not exercised by this test")
        }

        async fn count_rows(
            &self,
            _schema: &str,
            _relation: &str,
            _filters: &zsql_core::FilterState,
        ) -> Result<zsql_core::RowCount, zsql_core::CoreError> {
            unimplemented!("not exercised by this test")
        }

        async fn describe_relation(
            &self,
            _schema: &str,
            _relation: &str,
        ) -> Result<zsql_core::RelationSchema, zsql_core::CoreError> {
            unimplemented!("not exercised by this test")
        }
    }

    #[gpui::test]
    async fn a_begin_failure_fails_with_no_statement_index_and_sends_no_statement(
        cx: &mut TestAppContext,
    ) {
        let _guard = crate::test_support::serialize_real_io();
        let connection: std::sync::Arc<dyn zsql_core::Connection> =
            std::sync::Arc::new(BeginFailsConnection);
        let session = cx.new(|_cx| Session::new_for_query_test(connection));

        let outcome = session
            .update(cx, |session, cx| {
                session.run_in_transaction(
                    vec!["DELETE FROM \"main\".\"t\" WHERE \"id\" = 1;".to_owned()],
                    cx,
                )
            })
            .await;

        let failure = outcome.expect_err("a failing BEGIN must fail the whole batch");
        assert_eq!(
            failure.statement_index, None,
            "a BEGIN failure happens before any statement runs, so no statement index applies"
        );
        assert!(!failure.message.is_empty());
    }
}
