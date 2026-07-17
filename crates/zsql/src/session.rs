//! `Session` owns the app's single active database connection and drives the
//! query lifecycle the results grid renders

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{Context, Task, prelude::*};
use zsql_core::{ConnConfig, Connection, CoreError, Driver, QueryEvent, ResultSet, SchemaTree};
use zsql_postgres::PostgresDriver;

use crate::config::Config;

/// What the session (and the results grid it drives) currently displays.
#[derive(Debug, Clone)]
pub enum SessionState {
    /// No DSN is configured (`DATABASE_URL` unset and no
    /// `connection.default_url` in the loaded [`Config`])
    Empty,
    /// A connection attempt is in flight.
    Connecting,
    /// Connected, idle: the connection succeeded and no query has run yet
    Connected,
    /// Connected; a query is currently streaming results
    Running,
    /// The most recent query completed successfully
    Results(Duration),
    /// Connecting or running a query failed. The message is safe to show
    /// directly in the UI
    Error(String),
}

/// What the schema sidebar currently has to render, tracked independently of
/// [`SessionState`]
#[derive(Debug, Clone)]
pub enum SchemaState {
    /// No introspection has completed yet
    NotLoaded,
    /// Introspection is in flight.
    Loading,
    /// Introspection succeeded; the sidebar renders this tree.
    Ready(SchemaTree),
    /// Introspection failed. The message is safe to show directly in the
    /// UI
    Error(String),
}

/// Owns the active connection and the current query's lifecycle.
pub struct Session {
    /// Resolved DSN (`Config::resolve_url`), if any.
    dsn: Option<String>,
    /// The live connection, once `connect` succeeds
    connection: Option<Arc<dyn Connection>>,
    /// The current lifecycle state a view renders.
    state: SessionState,
    /// The schema sidebar's current state
    schema: SchemaState,
    /// Bumped every time `schema` is reassigned (see [`Session::set_schema`]).
    schema_generation: u64,
    /// `LIMIT` applied to [`Session::preview_relation`]'s generated query
    preview_limit: u64,
    /// Cancellation handle for whichever query is currently streaming.
    active_query: Option<zsql_core::QueryHandle>,
    /// Columns/rows accumulated so far for the query currently streaming
    accumulating: ResultSet,
    /// When the currently-streaming query started
    query_started_at: Option<Instant>,
    /// Incremented every `run_query` call. Each query's consumer loop
    /// captures the generation it was started with and compares it against
    /// this field before folding an event into `state`/`accumulating`.
    query_generation: u64,
}

impl Session {
    /// Build a session for `cfg`'s resolved connection URL
    #[must_use]
    pub fn new(cfg: &Config) -> Self {
        let dsn = cfg.resolve_url();
        let state = if dsn.is_some() {
            SessionState::Connecting
        } else {
            SessionState::Empty
        };
        Self {
            dsn,
            connection: None,
            state,
            schema: SchemaState::NotLoaded,
            schema_generation: 0,
            preview_limit: cfg.query.preview_limit,
            active_query: None,
            accumulating: ResultSet::default(),
            query_started_at: None,
            query_generation: 0,
        }
    }

    /// The session's current lifecycle state.
    #[must_use]
    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// The schema sidebar's current state.
    #[must_use]
    pub fn schema(&self) -> &SchemaState {
        &self.schema
    }

    /// Monotonically increases every time `schema` is reassigned
    #[must_use]
    pub fn schema_generation(&self) -> u64 {
        self.schema_generation
    }

    /// Replace `schema` and bump [`Session::schema_generation`]
    fn set_schema(&mut self, schema: SchemaState) {
        self.schema = schema;
        self.schema_generation = self.schema_generation.wrapping_add(1);
    }

    /// The result set accumulated by the most recently dispatched query.
    #[must_use]
    pub fn result(&self) -> &ResultSet {
        &self.accumulating
    }

    /// Connect using the resolved DSN. If none is configured, sets
    /// [`SessionState::Empty`] and returns a completed task
    pub fn connect(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let Some(dsn) = self.dsn.clone() else {
            self.state = SessionState::Empty;
            cx.notify();
            return Task::ready(());
        };

        self.state = SessionState::Connecting;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let cfg = match ConnConfig::from_dsn(&dsn) {
                Ok(cfg) => cfg,
                Err(err) => {
                    let _ = this.update(cx, |session, cx| {
                        tracing::warn!(error = %err, "session dsn rejected");
                        session.state = SessionState::Error(err.to_string());
                        cx.notify();
                    });
                    return;
                }
            };

            let connect_result = cx.background_spawn(connect_postgres(cfg)).await;

            let _ = this.update(cx, |session, cx| {
                match connect_result {
                    Ok(conn) => {
                        tracing::info!("session connected");
                        session.connection = Some(Arc::from(conn));
                        session.state = SessionState::Connected;
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "session connect failed");
                        session.state = SessionState::Error(err.to_string());
                    }
                }
                cx.notify();
            });
        })
    }

    /// Run `sql` on the active connection, streaming its `QueryEvent`s into
    /// [`SessionState`]
    pub fn run_query(&mut self, sql: impl Into<String>, cx: &mut Context<Self>) -> Task<()> {
        let sql = sql.into();
        let Some(connection) = self.connection.as_ref() else {
            self.state = SessionState::Error("cannot run a query: not connected".to_owned());
            cx.notify();
            return Task::ready(());
        };

        tracing::debug!(sql = %sql, "session running query");
        self.accumulating = ResultSet::default();
        self.state = SessionState::Running;
        self.query_started_at = Some(Instant::now());
        self.query_generation += 1;
        let generation = self.query_generation;

        let (tx, rx) = flume::unbounded();
        // Starting a new query supersedes whatever the previous
        // `active_query` handle pointed at: replacing the field drops that
        // old handle, which (per `QueryHandle`'s own contract) cooperatively
        // cancels it
        self.active_query = Some(connection.stream_query(sql, tx));
        cx.notify();

        cx.spawn(async move |this, cx| {
            while let Ok(evt) = rx.recv_async().await {
                let reached_terminal_state = this.update(cx, |session, cx| {
                    if session.query_generation != generation {
                        // A newer `run_query` call has superseded this one;
                        // stop folding this stream's events into state that
                        // no longer belongs to it.
                        return true;
                    }
                    session.apply_query_event(evt);
                    cx.notify();
                    matches!(
                        session.state,
                        SessionState::Results(..) | SessionState::Error(_)
                    )
                });
                match reached_terminal_state {
                    Ok(true) | Err(_) => break,
                    Ok(false) => {}
                }
            }
        })
    }

    /// Snapshot the reachable schema via the active connection's
    /// `Connection::introspect`
    ///
    /// A failure here only updates [`Session::schema`]. If there
    /// is no active connection, this sets [`SchemaState::Error`]
    /// immediately and returns a completed task.
    pub fn introspect(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let Some(connection) = self.connection.clone() else {
            self.set_schema(SchemaState::Error(
                "cannot introspect: not connected".to_owned(),
            ));
            cx.notify();
            return Task::ready(());
        };

        self.set_schema(SchemaState::Loading);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(introspect_connection(connection)).await;

            let _ = this.update(cx, |session, cx| {
                match result {
                    Ok(tree) => {
                        tracing::info!(
                            catalogs = tree.catalogs.len(),
                            "session introspected schema"
                        );
                        session.set_schema(SchemaState::Ready(tree));
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "session introspection failed");
                        session.set_schema(SchemaState::Error(err.to_string()));
                    }
                }
                cx.notify();
            });
        })
    }

    /// Preview a relation's rows: `SELECT * FROM "<schema>"."<relation>"
    /// LIMIT <configured preview limit>`
    pub fn preview_relation(
        &mut self,
        schema: &str,
        relation: &str,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let sql = crate::sql::preview_sql(schema, relation, self.preview_limit);
        self.run_query(sql, cx)
    }

    /// Fold one `QueryEvent` (or a terminal error) into `state`/`accumulating`.
    fn apply_query_event(&mut self, event: Result<QueryEvent, CoreError>) {
        match event {
            Ok(QueryEvent::Columns(columns)) => {
                self.accumulating.columns = columns;
                self.state = SessionState::Running;
            }
            Ok(QueryEvent::Batch(batch)) => {
                self.accumulating.rows.extend(batch.rows);
                self.state = SessionState::Running;
            }
            Ok(QueryEvent::Done { affected }) => {
                self.accumulating.affected = affected;
                let elapsed = self
                    .query_started_at
                    .take()
                    .map_or(Duration::ZERO, |started| started.elapsed());
                tracing::info!(
                    rows = self.accumulating.rows.len(),
                    elapsed_ms = elapsed.as_millis(),
                    "session query completed"
                );
                self.state = SessionState::Results(elapsed);
                self.active_query = None;
            }
            Err(err) => {
                tracing::warn!(error = %err, "session query failed");
                self.query_started_at = None;
                self.state = SessionState::Error(err.to_string());
                self.active_query = None;
            }
        }
    }
}

/// Test-only constructors used by `ui::results`'s render tests
#[cfg(test)]
impl Session {
    /// Build a session already in `state`, with `result` as its accumulated
    /// result set
    pub(crate) fn new_for_render_test(state: SessionState, result: ResultSet) -> Self {
        Self {
            dsn: None,
            connection: None,
            state,
            schema: SchemaState::NotLoaded,
            schema_generation: 0,
            preview_limit: Config::default().query.preview_limit,
            active_query: None,
            accumulating: result,
            query_started_at: None,
            query_generation: 0,
        }
    }

    /// Replace the accumulated result set in place, simulating another
    /// batch (or a fresh result)
    pub(crate) fn set_result_for_test(&mut self, result: ResultSet) {
        self.accumulating = result;
    }

    /// Build a session already holding `schema` as its introspected schema
    /// state, connected but idle, with no result set
    pub(crate) fn new_for_schema_test(schema: SchemaState) -> Self {
        let mut session = Self::new_for_render_test(SessionState::Connected, ResultSet::default());
        session.set_schema(schema);
        session
    }
}

/// Connect to Postgres via [`PostgresDriver`]
///
/// # Errors
/// Returns whatever [`PostgresDriver::connect`] returns on failure.
#[tracing::instrument(name = "session_connect_postgres", skip_all)]
async fn connect_postgres(cfg: ConnConfig) -> Result<Box<dyn Connection>, CoreError> {
    PostgresDriver.connect(&cfg).await
}

/// Introspect `connection`'s reachable schema.
#[tracing::instrument(name = "session_introspect", skip_all)]
async fn introspect_connection(connection: Arc<dyn Connection>) -> Result<SchemaTree, CoreError> {
    connection.introspect().await
}

#[cfg(test)]
mod tests {
    use zsql_core::{ColumnMeta, CoreError, QueryEvent, Row, RowBatch, Value};

    use super::{Config, Session, SessionState};

    fn session_with_no_dsn() -> Session {
        Session::new(&Config::default())
    }

    #[test]
    fn new_session_with_no_dsn_starts_empty() {
        let session = session_with_no_dsn();
        assert!(matches!(session.state(), SessionState::Empty));
    }

    #[test]
    fn new_session_with_a_configured_dsn_starts_connecting() {
        let mut cfg = Config::default();
        cfg.connection.default_url = Some("postgres://localhost/db".to_owned());
        let session = Session::new(&cfg);
        assert!(matches!(session.state(), SessionState::Connecting));
    }

    #[test]
    fn columns_then_batches_then_done_builds_the_expected_result_set() {
        let mut session = session_with_no_dsn();
        session.state = SessionState::Running;

        session.apply_query_event(Ok(QueryEvent::Columns(vec![
            ColumnMeta {
                name: "id".to_owned(),
                type_name: "int8".to_owned(),
                nullable: false,
            },
            ColumnMeta {
                name: "label".to_owned(),
                type_name: "text".to_owned(),
                nullable: true,
            },
        ])));
        session.apply_query_event(Ok(QueryEvent::Batch(RowBatch {
            rows: vec![Row(vec![Value::Int(1), Value::Text("a".to_owned())])],
        })));
        session.apply_query_event(Ok(QueryEvent::Batch(RowBatch {
            rows: vec![Row(vec![Value::Int(2), Value::Null])],
        })));
        session.apply_query_event(Ok(QueryEvent::Done { affected: None }));

        assert!(
            matches!(session.state(), SessionState::Results(_)),
            "expected SessionState::Results, got {:?}",
            session.state()
        );
        let result = session.result();
        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.columns[0].name, "id");
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0].0[0], Value::Int(1));
        assert_eq!(result.rows[1].0[1], Value::Null);
        assert_eq!(result.affected, None);
    }

    #[test]
    fn multiple_batches_accumulate_into_the_final_result_set_correctly() {
        let mut session = session_with_no_dsn();
        session.state = SessionState::Running;

        session.apply_query_event(Ok(QueryEvent::Columns(vec![ColumnMeta {
            name: "n".to_owned(),
            type_name: "int8".to_owned(),
            nullable: false,
        }])));
        for batch_start in [0, 2, 4] {
            session.apply_query_event(Ok(QueryEvent::Batch(RowBatch {
                rows: vec![
                    Row(vec![Value::Int(batch_start)]),
                    Row(vec![Value::Int(batch_start + 1)]),
                ],
            })));
        }
        session.apply_query_event(Ok(QueryEvent::Done { affected: None }));

        assert!(matches!(session.state(), SessionState::Results(_)));
        let result = session.result();
        assert_eq!(result.rows.len(), 6);
        let values: Vec<i64> = result
            .rows
            .iter()
            .map(|row| match row.0[0] {
                Value::Int(v) => v,
                ref other => panic!("expected Value::Int, got {other:?}"),
            })
            .collect();
        assert_eq!(values, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn done_with_no_columns_reports_affected_rows_for_dml_style_results() {
        let mut session = session_with_no_dsn();
        session.state = SessionState::Running;

        session.apply_query_event(Ok(QueryEvent::Columns(Vec::new())));
        session.apply_query_event(Ok(QueryEvent::Done { affected: Some(3) }));

        assert!(matches!(session.state(), SessionState::Results(_)));
        let result = session.result();
        assert!(result.columns.is_empty());
        assert!(result.rows.is_empty());
        assert_eq!(result.affected, Some(3));
    }

    #[test]
    fn columns_and_batches_paint_incrementally_before_done_arrives() {
        let mut session = session_with_no_dsn();
        session.state = SessionState::Running;

        session.apply_query_event(Ok(QueryEvent::Columns(vec![ColumnMeta {
            name: "id".to_owned(),
            type_name: "int8".to_owned(),
            nullable: false,
        }])));
        assert!(matches!(session.state(), SessionState::Running));
        assert_eq!(session.result().columns.len(), 1);
        assert!(
            session.result().rows.is_empty(),
            "no batch has arrived yet, so rows should still be empty"
        );

        session.apply_query_event(Ok(QueryEvent::Batch(RowBatch {
            rows: vec![Row(vec![Value::Int(1)])],
        })));
        assert!(matches!(session.state(), SessionState::Running));
        assert_eq!(
            session.result().columns.len(),
            1,
            "columns must still be present"
        );
        assert_eq!(session.result().rows.len(), 1);
        assert_eq!(session.result().rows[0].0[0], Value::Int(1));
    }

    #[test]
    fn an_error_event_produces_a_readable_error_state() {
        let mut session = session_with_no_dsn();
        session.state = SessionState::Running;

        session.apply_query_event(Err(CoreError::Query(
            "syntax error at or near \"selct\"".to_owned(),
        )));

        match session.state() {
            SessionState::Error(message) => {
                assert!(
                    message.contains("syntax error"),
                    "error message should be readable, got: {message}"
                );
            }
            other => panic!("expected SessionState::Error, got {other:?}"),
        }
    }
}

/// `TestAppContext`-driven `Session` tests that need no live database
#[cfg(test)]
mod gpui_tests {
    use std::sync::{Arc, Mutex};

    use gpui::{AppContext as _, TestAppContext};
    use zsql_core::{
        BatchSink, Catalog, ColumnMeta, Connection, CoreError, QueryEvent, QueryHandle, Relation,
        RelationKind, SchemaNs, SchemaTree,
    };

    use super::{Config, SchemaState, Session, SessionState};

    fn session_with_no_dsn() -> Session {
        Session::new(&Config::default())
    }

    #[gpui::test]
    async fn run_query_without_a_connection_sets_a_not_connected_error(cx: &mut TestAppContext) {
        let session = cx.new(|_cx| session_with_no_dsn());

        session
            .update(cx, |session, cx| session.run_query("SELECT 1", cx))
            .await;

        session.read_with(cx, |session, _app| match session.state() {
            SessionState::Error(message) => {
                assert!(
                    message.contains("not connected"),
                    "expected a 'not connected' error, got: {message}"
                );
            }
            other => panic!("expected SessionState::Error, got {other:?}"),
        });
    }

    #[gpui::test]
    async fn introspect_without_a_connection_sets_a_schema_error_and_leaves_state_untouched(
        cx: &mut TestAppContext,
    ) {
        let session = cx.new(|_cx| session_with_no_dsn());
        let state_before = session.read_with(cx, |session, _app| format!("{:?}", session.state()));

        session.update(cx, Session::introspect).await;

        session.read_with(cx, |session, _app| {
            match session.schema() {
                SchemaState::Error(message) => {
                    assert!(
                        message.contains("not connected"),
                        "expected a 'not connected' schema error, got: {message}"
                    );
                }
                other => panic!("expected SchemaState::Error, got {other:?}"),
            }
            assert_eq!(
                format!("{:?}", session.state()),
                state_before,
                "an introspection failure must not touch session state"
            );
        });
    }

    #[gpui::test]
    async fn connect_with_no_dsn_configured_stays_empty(cx: &mut TestAppContext) {
        let session = cx.new(|_cx| session_with_no_dsn());

        session.update(cx, Session::connect).await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Empty),
                "expected SessionState::Empty, got {:?}",
                session.state()
            );
        });
    }

    #[gpui::test]
    async fn connect_with_an_empty_resolved_dsn_reports_a_readable_error(cx: &mut TestAppContext) {
        let mut cfg = Config::default();
        cfg.connection.default_url = Some(String::new());
        let session = cx.new(|_cx| Session::new(&cfg));

        session.read_with(cx, |session, _app| {
            assert!(matches!(session.state(), SessionState::Connecting));
        });

        session.update(cx, Session::connect).await;

        session.read_with(cx, |session, _app| match session.state() {
            SessionState::Error(message) => {
                assert!(
                    message.to_lowercase().contains("dsn"),
                    "expected an invalid-DSN error, got: {message}"
                );
            }
            other => panic!("expected SessionState::Error, got {other:?}"),
        });
    }

    #[gpui::test]
    async fn connect_to_an_unreachable_host_reports_a_readable_error(cx: &mut TestAppContext) {
        cx.executor().allow_parking();

        let mut cfg = Config::default();
        cfg.connection.default_url =
            Some("postgres://nobody:nobody@zsql-test-unreachable.invalid:5432/db".to_owned());
        let session = cx.new(|_cx| Session::new(&cfg));

        session.update(cx, Session::connect).await;

        session.read_with(cx, |session, _app| match session.state() {
            SessionState::Error(message) => {
                assert!(
                    !message.is_empty(),
                    "expected a non-empty, readable connect error"
                );
            }
            other => panic!("expected SessionState::Error, got {other:?}"),
        });
    }

    /// What [`FakeConnection::introspect`] returns
    enum FakeIntrospectOutcome {
        Ready(SchemaTree),
        Failed(String),
    }

    /// A `Connection` double that records every `stream_query` call's SQL
    /// text and sink instead of running anything
    struct FakeConnection {
        sinks: Arc<Mutex<Vec<BatchSink>>>,
        queries: Arc<Mutex<Vec<String>>>,
        introspect_outcome: FakeIntrospectOutcome,
    }

    impl FakeConnection {
        fn new(sinks: Arc<Mutex<Vec<BatchSink>>>, queries: Arc<Mutex<Vec<String>>>) -> Self {
            Self {
                sinks,
                queries,
                introspect_outcome: FakeIntrospectOutcome::Ready(SchemaTree::default()),
            }
        }
    }

    #[async_trait::async_trait]
    impl Connection for FakeConnection {
        fn stream_query(&self, sql: String, sink: BatchSink) -> QueryHandle {
            self.queries
                .lock()
                .expect("queries lock poisoned")
                .push(sql);
            self.sinks.lock().expect("sinks lock poisoned").push(sink);
            let (cancel_tx, _cancel_rx) = flume::unbounded();
            QueryHandle::new(cancel_tx)
        }

        async fn introspect(&self) -> Result<SchemaTree, CoreError> {
            match &self.introspect_outcome {
                FakeIntrospectOutcome::Ready(tree) => Ok(tree.clone()),
                FakeIntrospectOutcome::Failed(message) => {
                    Err(CoreError::Introspection(message.clone()))
                }
            }
        }
    }

    #[gpui::test]
    fn superseding_a_query_ignores_late_events_from_the_previous_query(cx: &mut TestAppContext) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection = FakeConnection::new(sinks.clone(), queries);

        let session = cx.new(|_cx| {
            let mut session = session_with_no_dsn();
            session.connection = Some(Arc::new(connection));
            session
        });

        session
            .update(cx, |session, cx| session.run_query("SELECT 1", cx))
            .detach();
        cx.run_until_parked();

        // Start a second query before the first ever reaches a terminal
        // state, superseding it.
        session
            .update(cx, |session, cx| session.run_query("SELECT 2", cx))
            .detach();
        cx.run_until_parked();

        let (first_sink, second_sink) = {
            let sinks = sinks.lock().expect("sinks lock poisoned");
            assert_eq!(sinks.len(), 2, "expected exactly two stream_query calls");
            (sinks[0].clone(), sinks[1].clone())
        };

        first_sink
            .send(Ok(QueryEvent::Columns(vec![ColumnMeta {
                name: "stale".to_owned(),
                type_name: "text".to_owned(),
                nullable: true,
            }])))
            .expect("first sink send failed");
        cx.run_until_parked();

        session.read_with(cx, |session, _app| {
            assert!(
                !matches!(session.state(), SessionState::Results(..)),
                "a stale event from the superseded query must not produce a result"
            );
            assert!(
                session.accumulating.columns.is_empty(),
                "a stale event must not be folded into the current query's accumulating state, got {:?}",
                session.accumulating.columns
            );
        });

        second_sink
            .send(Ok(QueryEvent::Columns(vec![ColumnMeta {
                name: "fresh".to_owned(),
                type_name: "text".to_owned(),
                nullable: true,
            }])))
            .expect("second sink send failed");
        second_sink
            .send(Ok(QueryEvent::Done { affected: None }))
            .expect("second sink send failed");
        cx.run_until_parked();

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Results(_)),
                "expected SessionState::Results from the current query, got {:?}",
                session.state()
            );
            let result = session.result();
            assert_eq!(result.columns.len(), 1);
            assert_eq!(result.columns[0].name, "fresh");
        });
    }

    /// A sample tree with one catalog, one schema, and one table, used by
    /// the introspection tests below.
    fn sample_schema_tree() -> SchemaTree {
        SchemaTree {
            catalogs: vec![Catalog {
                name: "zsql".to_owned(),
                schemas: vec![SchemaNs {
                    name: "public".to_owned(),
                    tables: vec![Relation {
                        name: "orders".to_owned(),
                        kind: RelationKind::Table,
                        columns: vec![ColumnMeta {
                            name: "id".to_owned(),
                            type_name: "int8".to_owned(),
                            nullable: false,
                        }],
                    }],
                }],
            }],
        }
    }

    #[gpui::test]
    async fn introspect_populates_schema_state_from_a_successful_connection(
        cx: &mut TestAppContext,
    ) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut connection = FakeConnection::new(sinks, queries);
        connection.introspect_outcome = FakeIntrospectOutcome::Ready(sample_schema_tree());

        let session = cx.new(|_cx| {
            let mut session = session_with_no_dsn();
            session.connection = Some(Arc::new(connection));
            session
        });

        session.update(cx, Session::introspect).await;

        session.read_with(cx, |session, _app| match session.schema() {
            SchemaState::Ready(tree) => {
                assert_eq!(tree.catalogs.len(), 1);
                assert_eq!(tree.catalogs[0].schemas[0].tables[0].name, "orders");
            }
            other => panic!("expected SchemaState::Ready, got {other:?}"),
        });
    }

    #[gpui::test]
    async fn schema_generation_advances_on_introspection_but_not_on_query_events(
        cx: &mut TestAppContext,
    ) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut connection = FakeConnection::new(sinks.clone(), queries);
        connection.introspect_outcome = FakeIntrospectOutcome::Ready(sample_schema_tree());

        let session = cx.new(|_cx| {
            let mut session = session_with_no_dsn();
            session.connection = Some(Arc::new(connection));
            session
        });

        let generation_before_introspect =
            session.read_with(cx, |session, _app| session.schema_generation());

        session.update(cx, Session::introspect).await;

        let generation_after_introspect =
            session.read_with(cx, |session, _app| session.schema_generation());
        assert!(
            generation_after_introspect > generation_before_introspect,
            "introspecting must advance schema_generation"
        );

        session
            .update(cx, |session, cx| session.run_query("SELECT 1", cx))
            .detach();
        cx.run_until_parked();

        let sink = {
            let sinks = sinks.lock().expect("sinks lock poisoned");
            assert_eq!(sinks.len(), 1, "expected exactly one stream_query call");
            sinks[0].clone()
        };
        sink.send(Ok(QueryEvent::Columns(vec![ColumnMeta {
            name: "n".to_owned(),
            type_name: "int8".to_owned(),
            nullable: false,
        }])))
        .expect("sink send failed");
        sink.send(Ok(QueryEvent::Done { affected: None }))
            .expect("sink send failed");
        cx.run_until_parked();

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Results(_)),
                "expected the query to complete, got {:?}",
                session.state()
            );
            assert_eq!(
                session.schema_generation(),
                generation_after_introspect,
                "a query's QueryEvents must not touch schema_generation"
            );
        });
    }

    #[gpui::test]
    async fn a_failed_introspection_does_not_block_running_a_query_afterwards(
        cx: &mut TestAppContext,
    ) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mut connection = FakeConnection::new(sinks.clone(), queries);
        connection.introspect_outcome =
            FakeIntrospectOutcome::Failed("permission denied for schema pg_catalog".to_owned());

        let session = cx.new(|_cx| {
            let mut session = session_with_no_dsn();
            session.state = SessionState::Connected;
            session.connection = Some(Arc::new(connection));
            session
        });

        session.update(cx, Session::introspect).await;

        session.read_with(cx, |session, _app| {
            match session.schema() {
                SchemaState::Error(message) => assert!(!message.is_empty()),
                other => panic!("expected SchemaState::Error, got {other:?}"),
            }
            assert!(
                matches!(session.state(), SessionState::Connected),
                "a failed introspection must not touch SessionState, got {:?}",
                session.state()
            );
        });

        // The connection must still be usable for an ordinary query.
        session
            .update(cx, |session, cx| session.run_query("SELECT 1", cx))
            .detach();
        cx.run_until_parked();

        let sink = {
            let sinks = sinks.lock().expect("sinks lock poisoned");
            assert_eq!(sinks.len(), 1, "expected exactly one stream_query call");
            sinks[0].clone()
        };
        sink.send(Ok(QueryEvent::Done { affected: Some(0) }))
            .expect("sink send failed");
        cx.run_until_parked();

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Results(_)),
                "a query after a failed introspection must still complete normally, got {:?}",
                session.state()
            );
        });
    }

    /// `preview_relation` must build a quoted, `LIMIT`-bounded query from
    /// `Config`'s `preview_limit` and dispatch it exactly like any other
    /// query, through `run_query`.
    #[gpui::test]
    fn preview_relation_dispatches_a_quoted_limited_select(cx: &mut TestAppContext) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection = FakeConnection::new(sinks, queries.clone());

        let mut cfg = Config::default();
        cfg.query.preview_limit = 50;
        let session = cx.new(|_cx| {
            let mut session = Session::new(&cfg);
            session.connection = Some(Arc::new(connection));
            session
        });

        session
            .update(cx, |session, cx| {
                session.preview_relation("public", "orders", cx)
            })
            .detach();
        cx.run_until_parked();

        let recorded = queries.lock().expect("queries lock poisoned");
        assert_eq!(
            recorded.as_slice(),
            ["SELECT * FROM \"public\".\"orders\" LIMIT 50"],
        );
    }
}

/// Live-database end-to-end tests, gated on `ZSQL_TEST_DATABASE_URL` so
/// `cargo test` passes with no database present
#[cfg(test)]
mod live_tests {
    use gpui::{AppContext as _, TestAppContext};
    use zsql_core::Value;

    use super::{Config, SchemaState, Session, SessionState};

    fn live_database_url() -> Option<String> {
        let Ok(url) = std::env::var("ZSQL_TEST_DATABASE_URL") else {
            eprintln!("skipping live test: ZSQL_TEST_DATABASE_URL not set");
            return None;
        };
        Some(url)
    }

    #[gpui::test]
    async fn session_connects_and_streams_a_live_query_when_configured(cx: &mut TestAppContext) {
        let Some(url) = live_database_url() else {
            return;
        };
        cx.executor().allow_parking();

        let mut cfg = Config::default();
        cfg.connection.default_url = Some(url);

        let session = cx.new(|_cx| Session::new(&cfg));

        let connect_task = session.update(cx, Session::connect);
        connect_task.await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "connect should succeed and land in SessionState::Connected, got {:?}",
                session.state()
            );
        });

        let run_task = session.update(cx, |session, cx| {
            session.run_query("SELECT * FROM orders ORDER BY placed_at DESC", cx)
        });
        run_task.await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Results(_)),
                "expected a terminal SessionState::Results, got {:?}",
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

            let mut total_cents: Vec<i64> = result
                .rows
                .iter()
                .map(|row| match &row.0[2] {
                    Value::Int(v) => *v,
                    other => panic!("expected total_cents to decode as Value::Int, got {other:?}"),
                })
                .collect();
            total_cents.sort_unstable();
            assert_eq!(total_cents, vec![250, 1299, 4900]);
        });
    }

    #[gpui::test]
    async fn connect_without_running_a_query_leaves_the_session_connected_when_configured(
        cx: &mut TestAppContext,
    ) {
        let Some(url) = live_database_url() else {
            return;
        };
        cx.executor().allow_parking();

        let mut cfg = Config::default();
        cfg.connection.default_url = Some(url);

        let session = cx.new(|_cx| Session::new(&cfg));
        session.update(cx, Session::connect).await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "expected SessionState::Connected after a successful connect with no query run, got {:?}",
                session.state()
            );
        });
    }

    #[gpui::test]
    async fn session_surfaces_a_readable_error_for_an_invalid_query_when_configured(
        cx: &mut TestAppContext,
    ) {
        let Some(url) = live_database_url() else {
            return;
        };
        cx.executor().allow_parking();

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

        session
            .update(cx, |session, cx| {
                session.run_query("SELECT * FROM this_table_does_not_exist", cx)
            })
            .await;

        session.read_with(cx, |session, _app| match session.state() {
            SessionState::Error(message) => {
                assert!(
                    message.contains("this_table_does_not_exist")
                        || message.to_lowercase().contains("does not exist")
                        || message.to_lowercase().contains("relation"),
                    "error message should be readable and query-specific, got: {message}"
                );
            }
            other => panic!("expected SessionState::Error, got {other:?}"),
        });
    }

    #[gpui::test]
    async fn session_introspects_and_previews_a_relation_when_configured(cx: &mut TestAppContext) {
        let Some(url) = live_database_url() else {
            return;
        };
        cx.executor().allow_parking();

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

        session.update(cx, Session::introspect).await;

        session.read_with(cx, |session, _app| {
            let tree = match session.schema() {
                SchemaState::Ready(tree) => tree,
                other => panic!("expected SchemaState::Ready, got {other:?}"),
            };
            let public = tree
                .catalogs
                .iter()
                .flat_map(|catalog| &catalog.schemas)
                .find(|schema| schema.name == "public")
                .expect("the seeded database has a public schema");

            assert!(
                public.tables.iter().any(|r| r.name == "orders"),
                "expected the seeded orders table in the introspected schema"
            );
            assert!(
                public.tables.iter().any(|r| r.name == "users"),
                "expected the seeded users table in the introspected schema"
            );
            let recent_orders = public
                .tables
                .iter()
                .find(|r| r.name == "recent_orders")
                .expect("expected the seeded recent_orders view in the introspected schema");
            assert_eq!(recent_orders.kind, zsql_core::RelationKind::View);
        });

        session
            .update(cx, |session, cx| {
                session.preview_relation("public", "orders", cx)
            })
            .await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Results(_)),
                "expected a terminal SessionState::Results from previewing orders, got {:?}",
                session.state()
            );
            assert_eq!(
                session.result().rows.len(),
                3,
                "the seeded orders table has 3 rows"
            );
        });
    }
}
