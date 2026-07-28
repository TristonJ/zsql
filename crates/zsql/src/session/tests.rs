use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use gpui::{Context, Task};
use zsql_core::{
    BatchSink, ColumnMeta, Connection, CoreError, QueryEvent, QueryHandle, RelationSchema,
    ResultSet, Row, RowBatch, RowCount, SchemaTree, Value,
};

use super::{
    Config, LivenessState, SchemaState, Session, SessionState, TunnelHandle,
    connect_through_open_tunnel, remote_target,
};

/// Test-only constructors used by the UI views' render and action tests.
impl Session {
    /// Connect to an explicitly chosen URL without a tunnel: a test
    /// convenience for the common no-SSH case. Production connects go through
    /// [`Session::connect_to_with_ssh`] (with `ssh` left `None` when there is
    /// no tunnel), so this exists only in test builds.
    pub fn connect_to(&mut self, url: impl Into<String>, cx: &mut Context<Self>) -> Task<()> {
        self.connect_url(url.into(), None, cx)
    }

    /// Build a session already in `state`, with `result` as its accumulated
    /// result set
    pub(crate) fn new_for_render_test(state: SessionState, result: ResultSet) -> Self {
        Self {
            url: None,
            connection: None,
            tunnel: None,
            state,
            schema: SchemaState::NotLoaded,
            schema_generation: 0,
            preview_limit: Config::default().query.preview_limit,
            batch_size: Config::default().query.batch_size,
            max_result_rows: Config::default().query.max_result_rows,
            active_query: None,
            accumulating: result,
            row_count: None,
            query_started_at: None,
            query_generation: 0,
            liveness: LivenessState::Unknown,
            connection_generation: 0,
            probe_in_flight: false,
            probe_interval: Config::default().liveness.probe_interval(),
            probe_timeout: Config::default().liveness.probe_timeout(),
        }
    }

    /// Replace the accumulated result set in place, simulating another
    /// batch (or a fresh result)
    pub(crate) fn set_result_for_test(&mut self, result: ResultSet) {
        self.accumulating = result;
    }

    /// Set the exposed row count directly, simulating a completed (or
    /// absent) [`Session::preview_relation`] count fetch without going
    /// through a real `Connection`.
    pub(crate) fn set_row_count_for_test(&mut self, row_count: Option<RowCount>) {
        self.row_count = row_count;
    }

    /// Set the exposed liveness state directly, simulating a completed
    /// probe result without waiting for the recurring probe to tick.
    pub(crate) fn set_liveness_for_test(&mut self, liveness: LivenessState) {
        self.liveness = liveness;
    }

    /// Set the exposed schema state directly, letting a test stand up a
    /// session that already holds a given schema without going through a
    /// real introspection.
    pub(crate) fn set_schema_for_test(&mut self, schema: SchemaState) {
        self.set_schema(schema);
    }

    /// Build a session already holding `schema` as its introspected schema
    /// state, connected but idle, with no result set
    pub(crate) fn new_for_schema_test(schema: SchemaState) -> Self {
        let mut session = Self::new_for_render_test(SessionState::Connected, ResultSet::default());
        session.set_schema(schema);
        session
    }

    /// Build a session already connected to `connection`, idle, with no
    /// result set. Used by `ui::editor_adapter`'s tests to assert `RunQuery`
    /// dispatches the expected SQL through `Session::run_query`.
    pub(crate) fn new_for_query_test(connection: Arc<dyn Connection>) -> Self {
        let mut session = Self::new_for_render_test(SessionState::Connected, ResultSet::default());
        session.connection = Some(connection);
        session
    }

    /// Start the liveliness probe loop against whatever `connection` a test
    /// has already set, as if a real `connect()` had just succeeded. Bumps
    /// `connection_generation` first, exactly as `connect()` does, so a
    /// second call (simulating a reconnect) supersedes the first and any
    /// probe still in flight for it.
    pub(crate) fn start_liveness_probe_for_test(&mut self, cx: &mut Context<Self>) {
        self.connection_generation = self.connection_generation.wrapping_add(1);
        self.probe_in_flight = false;
        let generation = self.connection_generation;
        self.spawn_liveness_probe_loop(generation, cx);
    }

    /// The probe interval a test-constructed session was given, so tests
    /// can advance a `TestAppContext`'s clock by exactly that much.
    pub(crate) fn probe_interval_for_test(&self) -> Duration {
        self.probe_interval
    }

    /// The probe timeout a test-constructed session was given, so tests can
    /// advance a `TestAppContext`'s clock past it to force a probe timeout.
    pub(crate) fn probe_timeout_for_test(&self) -> Duration {
        self.probe_timeout
    }

    /// Install `tunnel` as the session's active tunnel directly, as if a
    /// prior `connect_to_with_ssh` call had already opened it, without
    /// actually connecting anything.
    pub(crate) fn set_tunnel_for_test(&mut self, tunnel: Box<dyn TunnelHandle>) {
        self.tunnel = Some(tunnel);
    }

    /// Whether the session currently holds an active tunnel.
    pub(crate) fn has_tunnel_for_test(&self) -> bool {
        self.tunnel.is_some()
    }

    /// Drop only the session's tunnel, leaving `connection` (and everything
    /// else) untouched -- simulating the tunnel dying out from under an
    /// otherwise-healthy connection, without tearing down the connection
    /// itself the way a real reconnect would. Only exercised by the gated
    /// `ssh_live_tests` module, which is the only place a real tunnel (as
    /// opposed to a fake one) is under test.
    #[cfg(feature = "ssh-integration-tests")]
    pub(crate) fn kill_tunnel_for_test(&mut self) {
        self.tunnel = None;
    }

    /// The active tunnel's local loopback address, if any. Only exercised by
    /// the gated `ssh_live_tests` module.
    #[cfg(feature = "ssh-integration-tests")]
    pub(crate) fn tunnel_local_addr_for_test(&self) -> Option<SocketAddr> {
        self.tunnel.as_ref().map(|tunnel| tunnel.local_addr())
    }
}

/// A [`TunnelHandle`] double whose drop is observable via an atomic counter,
/// letting a test prove a tunnel was torn down without opening a real SSH
/// session. Shared between this module's plain (`block_on`) tests and its
/// `gpui`-driven ones.
pub(crate) struct FakeTunnel {
    open_count: Arc<std::sync::atomic::AtomicUsize>,
}

impl FakeTunnel {
    /// Build a handle that increments `open_count` now and decrements it
    /// again on drop, so a caller can assert `open_count` reaching `0`
    /// proves this handle (and only this handle) was torn down.
    pub(crate) fn new(open_count: Arc<std::sync::atomic::AtomicUsize>) -> Self {
        open_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self { open_count }
    }
}

impl TunnelHandle for FakeTunnel {
    fn local_addr(&self) -> SocketAddr {
        "127.0.0.1:1".parse().expect("valid loopback address")
    }
}

impl Drop for FakeTunnel {
    fn drop(&mut self) {
        self.open_count
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A [`Connection`] double whose `close()` is observable via an atomic
/// counter, mirroring [`FakeTunnel`]'s pattern for proving teardown actually
/// happened rather than merely inferring it. Every other method panics: this
/// only ever stands in for a connection a switch is about to supersede, and
/// no test exercises it beyond that.
pub(crate) struct CloseCountingConnection {
    close_calls: Arc<AtomicUsize>,
}

impl CloseCountingConnection {
    pub(crate) fn new(close_calls: Arc<AtomicUsize>) -> Self {
        Self { close_calls }
    }
}

#[async_trait::async_trait]
impl Connection for CloseCountingConnection {
    fn stream_query(&self, _sql: String, _sink: BatchSink) -> QueryHandle {
        unimplemented!("not exercised by this test")
    }

    async fn introspect(&self) -> Result<SchemaTree, CoreError> {
        unimplemented!("not exercised by this test")
    }

    async fn ping(&self) -> Result<(), CoreError> {
        unimplemented!("not exercised by this test")
    }

    async fn count_rows(&self, _schema: &str, _relation: &str) -> Result<RowCount, CoreError> {
        unimplemented!("not exercised by this test")
    }

    async fn describe_relation(
        &self,
        _schema: &str,
        _relation: &str,
    ) -> Result<RelationSchema, CoreError> {
        unimplemented!("not exercised by this test")
    }

    async fn close(&self) {
        self.close_calls.fetch_add(1, Ordering::SeqCst);
    }
}

fn session_with_no_url() -> Session {
    Session::new(&Config::default())
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    futures::executor::block_on(fut)
}

#[test]
fn new_session_with_no_url_starts_empty() {
    let session = session_with_no_url();
    assert!(matches!(session.state(), SessionState::Empty));
}

#[test]
fn preview_sql_with_no_connection_falls_back_to_the_shared_default_form() {
    let session = session_with_no_url();
    assert_eq!(
        session.preview_sql("public", "orders"),
        "SELECT * FROM \"public\".\"orders\" LIMIT 200"
    );
}

#[test]
fn columns_then_batches_then_done_builds_the_expected_result_set() {
    let mut session = session_with_no_url();
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
    let mut session = session_with_no_url();
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
fn a_second_result_set_replaces_the_first_keeping_only_the_last() {
    let mut session = session_with_no_url();
    session.state = SessionState::Running;

    // First statement's result set.
    session.apply_query_event(Ok(QueryEvent::Columns(vec![ColumnMeta {
        name: "a".to_owned(),
        type_name: "int8".to_owned(),
        nullable: false,
    }])));
    session.apply_query_event(Ok(QueryEvent::Batch(RowBatch {
        rows: vec![Row(vec![Value::Int(1)]), Row(vec![Value::Int(2)])],
    })));

    // Second statement's result set: a fresh Columns event must drop the
    // first set entirely rather than accumulate on top of it.
    session.apply_query_event(Ok(QueryEvent::Columns(vec![
        ColumnMeta {
            name: "x".to_owned(),
            type_name: "text".to_owned(),
            nullable: true,
        },
        ColumnMeta {
            name: "y".to_owned(),
            type_name: "text".to_owned(),
            nullable: true,
        },
    ])));
    session.apply_query_event(Ok(QueryEvent::Batch(RowBatch {
        rows: vec![Row(vec![Value::Text("last".to_owned()), Value::Null])],
    })));
    session.apply_query_event(Ok(QueryEvent::Done { affected: None }));

    assert!(matches!(session.state(), SessionState::Results(_)));
    let result = session.result();
    let column_names: Vec<&str> = result.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        column_names,
        vec!["x", "y"],
        "only the last statement's columns should remain"
    );
    assert_eq!(
        result.rows.len(),
        1,
        "the first set's rows must not survive into the last set"
    );
    assert_eq!(result.rows[0].0[0], Value::Text("last".to_owned()));
}

#[test]
fn done_with_no_columns_reports_affected_rows_for_dml_style_results() {
    let mut session = session_with_no_url();
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
    let mut session = session_with_no_url();
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
    let mut session = session_with_no_url();
    session.state = SessionState::Running;

    session.apply_query_event(Err(CoreError::query(
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

#[test]
fn row_limit_reached_compares_accumulated_against_the_limit() {
    assert!(
        !super::row_limit_reached(4, 5),
        "an accumulated count below the limit must not be reached"
    );
    assert!(
        super::row_limit_reached(5, 5),
        "an accumulated count exactly at the limit must count as reached"
    );
    assert!(
        super::row_limit_reached(6, 5),
        "an accumulated count above the limit must count as reached"
    );
}

#[test]
fn a_batch_crossing_the_limit_is_capped_exactly_and_does_not_cancel() {
    let mut cfg = Config::default();
    cfg.query.max_result_rows = 5;
    let mut session = Session::new(&cfg);
    session.state = SessionState::Running;

    let (cancel_tx, _cancel_rx) = flume::unbounded();
    session.active_query = Some(zsql_core::QueryHandle::new(cancel_tx));

    session.apply_query_event(Ok(QueryEvent::Columns(vec![ColumnMeta {
        name: "n".to_owned(),
        type_name: "int8".to_owned(),
        nullable: false,
    }])));

    session.apply_query_event(Ok(QueryEvent::Batch(RowBatch {
        rows: (0..3).map(|n| Row(vec![Value::Int(n)])).collect(),
    })));
    assert!(
        matches!(session.state(), SessionState::Running),
        "still under the limit, so the query must keep running"
    );
    assert_eq!(session.result().rows.len(), 3);

    // This batch alone (5 rows) would push the total to 8, well past the
    // limit of 5: only 2 of its rows may be appended.
    session.apply_query_event(Ok(QueryEvent::Batch(RowBatch {
        rows: (3..8).map(|n| Row(vec![Value::Int(n)])).collect(),
    })));

    assert_eq!(
        session.result().rows.len(),
        5,
        "rows must be capped at exactly the configured limit, never overshot"
    );
    match session.state() {
        SessionState::Truncating { rows, .. } => assert_eq!(*rows, 8),
        other => panic!("expected SessionState::Truncating, got {other:?}"),
    }

    session.apply_query_event(Ok(QueryEvent::Batch(RowBatch {
        rows: vec![Row(vec![Value::Int(99)])],
    })));
    assert_eq!(
        session.result().rows.len(),
        5,
        "a late batch after truncation must not grow the capped result"
    );

    session.apply_query_event(Ok(QueryEvent::Done { affected: None }));
    match session.state() {
        SessionState::Truncated { rows, .. } => assert_eq!(*rows, 9),
        other => panic!("expected SessionState::Truncated, got {other:?}"),
    }
}

#[test]
fn a_batch_landing_exactly_on_the_limit_truncates_without_overshoot() {
    let mut cfg = Config::default();
    cfg.query.max_result_rows = 4;
    let mut session = Session::new(&cfg);
    session.state = SessionState::Running;

    session.apply_query_event(Ok(QueryEvent::Columns(vec![ColumnMeta {
        name: "n".to_owned(),
        type_name: "int8".to_owned(),
        nullable: false,
    }])));
    session.apply_query_event(Ok(QueryEvent::Batch(RowBatch {
        rows: (0..4).map(|n| Row(vec![Value::Int(n)])).collect(),
    })));

    assert_eq!(session.result().rows.len(), 4);
    assert!(matches!(session.state(), SessionState::Truncating { .. }));
}

// -- tunnel lifecycle ---------------------------------------------------

#[test]
fn remote_target_uses_the_urls_own_explicit_port() {
    let (host, port) = remote_target("postgres://user:pw@db.internal:6543/app").unwrap();
    assert_eq!(host, "db.internal");
    assert_eq!(port, 6543);
}

#[test]
fn remote_target_falls_back_to_the_drivers_default_port() {
    let (host, port) = remote_target("mysql://user@db.internal/app").unwrap();
    assert_eq!(host, "db.internal");
    assert_eq!(port, 3306);
}

#[test]
fn remote_target_rejects_a_sqlite_url() {
    assert!(remote_target("sqlite::memory:").is_err());
}

#[test]
fn remote_target_rejects_a_hostful_url_with_no_port_and_no_driver_default() {
    // A host is present but the scheme has no registered driver (so no
    // default port) and the URL carries no explicit port, leaving nothing
    // for the tunnel to forward to.
    assert!(remote_target("redis://host/db").is_err());
}

/// A tunnel that opened successfully but whose driver connect then fails
/// must be torn down as part of that same failed attempt: the returned
/// error must not carry the tunnel forward, and the fake's drop must
/// already have run by the time this function returns.
#[test]
fn a_failed_connect_through_an_open_tunnel_tears_the_tunnel_down() {
    let open_count = Arc::new(AtomicUsize::new(0));
    let tunnel: Box<dyn TunnelHandle> = Box::new(FakeTunnel::new(open_count.clone()));
    assert_eq!(open_count.load(Ordering::SeqCst), 1);

    let result = block_on(connect_through_open_tunnel(
        "cassandra://host/db".to_owned(),
        tunnel,
        zsql_core::DEFAULT_QUERY_BATCH_SIZE,
    ));

    assert!(
        result.is_err(),
        "an unrecognized-scheme URL must fail the driver connect"
    );
    assert_eq!(
        open_count.load(Ordering::SeqCst),
        0,
        "the tunnel opened for a failed connect attempt must be torn down \
         as part of that same attempt"
    );
}

/// Dropping the owning `Session` must drop any tunnel it still holds:
/// `Session` carries no manual `Drop` impl of its own, so this is really
/// pinning that the `tunnel` field is a plain, ordinarily-dropped value
/// rather than something leaked out from under it (e.g. into a detached
/// task).
#[test]
fn dropping_the_session_drops_its_active_tunnel() {
    let open_count = Arc::new(AtomicUsize::new(0));
    let mut session = session_with_no_url();
    session.set_tunnel_for_test(Box::new(FakeTunnel::new(open_count.clone())));
    assert_eq!(open_count.load(Ordering::SeqCst), 1);

    drop(session);

    assert_eq!(
        open_count.load(Ordering::SeqCst),
        0,
        "dropping the session must drop its active tunnel"
    );
}

/// `TestAppContext`-driven `Session` tests that need no live database
mod gpui_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use gpui::{AppContext as _, Entity, TestAppContext};
    use zsql_core::{
        BatchSink, Catalog, ColumnMeta, Connection, CoreError, QueryEvent, QueryHandle, Relation,
        RelationKind, ResultSet, Row, RowBatch, RowCount, SchemaNs, SchemaTree, Value,
    };

    use super::{CloseCountingConnection, FakeTunnel};
    use crate::session::{
        Config, LivenessState, SchemaState, Session, SessionState, apply_connect_outcome,
    };

    fn session_with_no_url() -> Session {
        Session::new(&Config::default())
    }

    #[gpui::test]
    async fn run_query_without_a_connection_sets_a_not_connected_error(cx: &mut TestAppContext) {
        let session = cx.new(|_cx| session_with_no_url());

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
        let session = cx.new(|_cx| session_with_no_url());
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
    async fn connect_with_no_url_configured_stays_empty(cx: &mut TestAppContext) {
        let session = cx.new(|_cx| session_with_no_url());

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
    async fn connect_with_an_empty_resolved_url_reports_a_readable_error(cx: &mut TestAppContext) {
        let cfg = Config::default();
        let session = cx.new(|_cx| Session::new(&cfg));
        session
            .update(cx, |session, cx| session.connect_to("", cx))
            .await;

        session.read_with(cx, |session, _app| match session.state() {
            SessionState::Error(message) => {
                assert!(
                    message.to_lowercase().contains("url"),
                    "expected an invalid-URL error, got: {message}"
                );
            }
            other => panic!("expected SessionState::Error, got {other:?}"),
        });
    }

    #[gpui::test]
    async fn connect_to_an_unreachable_host_reports_a_readable_error(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let cfg = Config::default();
        let session = cx.new(|_cx| Session::new(&cfg));
        session
            .update(cx, |session, cx| {
                session.connect_to(
                    "postgres://nobody:nobody@zsql-test-unreachable.invalid:5432/db",
                    cx,
                )
            })
            .await;

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

    /// A tunnel that cannot be opened must fail the connect before any driver
    /// connect is attempted, surfacing as `Error` with no connection installed
    /// -- the ordering contract of a tunneled connect.
    #[gpui::test]
    async fn a_tunnel_that_cannot_open_errors_without_connecting(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        // Bind then immediately release a loopback port so a connect to it is
        // deterministically refused, rather than depending on a port happening
        // to be unused.
        let refused_port = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind a loopback port")
            .local_addr()
            .expect("read the bound port")
            .port();

        let mut ssh = zsql_ssh::SshConfig::new(
            "127.0.0.1",
            "zsql",
            zsql_ssh::SshAuth::Password("unused".to_owned()),
        );
        ssh.port = refused_port;

        let cfg = Config::default();
        let session = cx.new(|_cx| Session::new(&cfg));
        session
            .update(cx, |session, cx| {
                session.connect_to_with_ssh(
                    "postgres://user:pw@db.internal:5432/app",
                    Some(ssh),
                    cx,
                )
            })
            .await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Error(_)),
                "a tunnel that cannot open must surface as Error, got {:?}",
                session.state()
            );
            assert!(
                session.connection.is_none(),
                "no driver connection may be installed when the tunnel fails to open"
            );
        });
    }

    /// Proves a `SQLite` connection now works end-to-end through the same
    /// selection-based connect path the app uses, where before this feature
    /// `SQLite` could not be connected at all. Unconditional: an in-memory
    /// database needs no external service.
    #[gpui::test]
    async fn connect_resolves_a_sqlite_url_and_actually_opens_it(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let cfg = Config::default();
        let session = cx.new(|_cx| Session::new(&cfg));
        session
            .update(cx, |session, cx| {
                session.connect_to("sqlite::memory:".to_owned(), cx)
            })
            .await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "expected a sqlite connection to succeed, got {:?}",
                session.state()
            );
        });
    }

    /// `Session::connect_to` (used by the connection manager to connect an
    /// explicitly chosen saved connection) must dispatch through the exact
    /// same selection-based path as `connect`, independent of whatever URL
    /// (if any) `Config` resolved at startup.
    #[gpui::test]
    async fn connect_to_opens_a_sqlite_url_regardless_of_the_configured_url(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        // No configured URL at all: `connect_to` must still work on its own.
        let session = cx.new(|_cx| Session::new(&Config::default()));

        session
            .update(cx, |session, cx| session.connect_to("sqlite::memory:", cx))
            .await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "expected connect_to to open a sqlite connection, got {:?}",
                session.state()
            );
        });
    }

    /// A failed connection switch must not leave the previous connection
    /// queryable: `connect_to` clears the active connection on failure, so
    /// `run_query` afterwards reports "not connected" instead of silently
    /// executing against the database connected before the failed switch.
    #[gpui::test]
    async fn a_failed_connect_switch_clears_the_previous_connection(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let session = cx.new(|_cx| Session::new(&Config::default()));

        session
            .update(cx, |session, cx| session.connect_to("sqlite::memory:", cx))
            .await;
        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "expected the first connection to succeed, got {:?}",
                session.state()
            );
        });

        session
            .update(cx, |session, cx| {
                session.connect_to("cassandra://host/db", cx)
            })
            .await;
        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Error(_)),
                "expected Error after a failed switch, got {:?}",
                session.state()
            );
            assert!(
                session.connection.is_none(),
                "a failed connect switch must clear the previous live connection"
            );
        });

        session
            .update(cx, |session, cx| session.run_query("SELECT 1", cx))
            .await;
        session.read_with(cx, |session, _app| match session.state() {
            SessionState::Error(message) => assert!(
                message.contains("not connected"),
                "expected a not-connected error, got {message:?}"
            ),
            other => panic!("expected a not-connected error, got {other:?}"),
        });
    }

    /// A connection switch that supersedes an already-active connection must
    /// close the outgoing connection exactly once, dispatched as its own
    /// background task rather than delaying the switch's own state update.
    #[gpui::test]
    async fn connect_to_closes_the_previously_active_connection_exactly_once(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let close_calls = Arc::new(AtomicUsize::new(0));
        let outgoing: Arc<dyn Connection> =
            Arc::new(CloseCountingConnection::new(close_calls.clone()));
        let session = cx.new(|_cx| Session::new_for_query_test(outgoing));

        session
            .update(cx, |session, cx| session.connect_to("sqlite::memory:", cx))
            .await;
        cx.run_until_parked();

        assert_eq!(
            close_calls.load(Ordering::SeqCst),
            1,
            "the superseded connection must be closed exactly once"
        );
        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "expected the new connection to succeed, got {:?}",
                session.state()
            );
        });
    }

    /// A connection switch that fails must still close the connection it was
    /// about to replace: the generation bump already invalidated that
    /// connection's probe loop, so leaving it open would leak its pool
    /// workers even though the switch itself never became queryable.
    #[gpui::test]
    async fn a_failed_connect_switch_still_closes_the_previously_active_connection(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let close_calls = Arc::new(AtomicUsize::new(0));
        let outgoing: Arc<dyn Connection> =
            Arc::new(CloseCountingConnection::new(close_calls.clone()));
        let session = cx.new(|_cx| Session::new_for_query_test(outgoing));

        session
            .update(cx, |session, cx| {
                session.connect_to("cassandra://host/db", cx)
            })
            .await;
        cx.run_until_parked();

        assert_eq!(
            close_calls.load(Ordering::SeqCst),
            1,
            "the previously active connection must be closed even though the switch failed"
        );
        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Error(_)),
                "expected Error after a failed switch, got {:?}",
                session.state()
            );
        });
    }

    /// A connect attempt whose outcome arrives after a newer attempt has
    /// already superseded it (`connection_generation` moved on while it was
    /// still running in the background) must still close the connection it
    /// produced, exactly as a non-stale replace does -- it must not rely on
    /// the discarded `Box<dyn Connection>`'s own `Drop` to release pools or
    /// background workers.
    #[gpui::test]
    fn a_stale_connect_outcomes_own_connection_is_closed_exactly_once(cx: &mut TestAppContext) {
        let close_calls = Arc::new(AtomicUsize::new(0));
        let discarded: Box<dyn Connection> =
            Box::new(CloseCountingConnection::new(close_calls.clone()));

        let session = cx.new(|_cx| session_with_no_url());
        let connected = session.update(cx, |session, cx| {
            // A newer attempt (generation 2) has already superseded the one
            // whose outcome (generation 1) is being applied here.
            session.connection_generation = 2;
            apply_connect_outcome(session, 1, Ok((discarded, None)), cx)
        });
        cx.run_until_parked();

        assert!(
            !connected,
            "a superseded attempt must never report itself connected"
        );
        assert_eq!(
            close_calls.load(Ordering::SeqCst),
            1,
            "a superseded attempt's own connection must be closed exactly once"
        );
        session.read_with(cx, |session, _app| {
            assert!(
                session.connection.is_none(),
                "a superseded attempt must never install its connection onto the session"
            );
        });
    }

    /// `connect_to` must reset the schema tree to `NotLoaded` synchronously,
    /// in the same call that dispatches the connect attempt, regardless of
    /// what the schema was showing before -- so a caller never sees a stale
    /// tree while a switch is in flight, whether or not it ever succeeds.
    #[gpui::test]
    async fn connect_to_resets_schema_to_not_loaded_synchronously_regardless_of_prior_state(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        for prior in [
            SchemaState::NotLoaded,
            SchemaState::Loading,
            SchemaState::Ready(SchemaTree::default()),
            SchemaState::Error("boom".to_owned()),
        ] {
            let prior_label = format!("{prior:?}");
            let session = cx.new(|_cx| Session::new_for_schema_test(prior));
            let generation_before =
                session.read_with(cx, |session, _app| session.schema_generation());

            let task = session.update(cx, |session, cx| {
                let task = session.connect_to("sqlite::memory:", cx);
                assert!(
                    matches!(session.schema(), SchemaState::NotLoaded),
                    "expected NotLoaded synchronously right after connect_to \
                     (prior state was {prior_label}), got {:?}",
                    session.schema()
                );
                assert!(
                    session.schema_generation() > generation_before,
                    "expected schema_generation to bump in the same call \
                     (prior state was {prior_label})"
                );
                task
            });

            task.await;
        }
    }

    /// A failed connection switch must not resurrect the connection it was
    /// replacing: the schema stays `NotLoaded` rather than reverting to
    /// whatever tree the previous, superseded connection had introspected.
    #[gpui::test]
    async fn a_failed_connect_switch_leaves_schema_not_loaded_rather_than_reverting(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(SchemaTree::default())));

        session
            .update(cx, |session, cx| {
                session.connect_to("cassandra://host/db", cx)
            })
            .await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Error(_)),
                "expected Error after a failed switch, got {:?}",
                session.state()
            );
            assert!(
                matches!(session.schema(), SchemaState::NotLoaded),
                "a failed switch must not resurrect the previous connection's \
                 schema, got {:?}",
                session.schema()
            );
        });
    }

    /// A switch initiated while a query is actively streaming for the
    /// previous connection must still reset the session immediately: there
    /// is no special-casing that delays or blocks the reset for an
    /// in-flight query.
    #[gpui::test]
    async fn connect_to_resets_synchronously_even_while_a_query_is_running(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let session =
            cx.new(|_cx| Session::new_for_render_test(SessionState::Running, ResultSet::default()));

        let task = session.update(cx, |session, cx| {
            let task = session.connect_to("sqlite::memory:", cx);
            assert!(
                matches!(session.state(), SessionState::Connecting),
                "expected the switch to move state to Connecting even with a \
                 query running, got {:?}",
                session.state()
            );
            assert!(
                matches!(session.schema(), SchemaState::NotLoaded),
                "expected the schema to reset even with a query running, got {:?}",
                session.schema()
            );
            task
        });

        task.await;
    }

    /// What [`FakeConnection::introspect`] returns
    enum FakeIntrospectOutcome {
        Ready(SchemaTree),
        Failed(String),
    }

    /// A `Connection` double that records every `stream_query` call's SQL
    /// text and sink instead of running anything. `ping` answers with
    /// whatever a test pushes through [`FakeConnection::ping_sender`], one
    /// scripted outcome per call; a call with nothing pushed yet stays
    /// pending, which is what lets tests observe a probe "in flight".
    /// `ping_calls` counts every `ping()` invocation, letting a test prove
    /// an overlapping tick was actually skipped (call count stays 1) rather
    /// than merely inferring it from an unresolved outcome.
    struct FakeConnection {
        sinks: Arc<Mutex<Vec<BatchSink>>>,
        queries: Arc<Mutex<Vec<String>>>,
        introspect_outcome: FakeIntrospectOutcome,
        ping_tx: flume::Sender<Result<(), CoreError>>,
        ping_rx: flume::Receiver<Result<(), CoreError>>,
        ping_calls: Arc<AtomicUsize>,
        count_calls: Arc<AtomicUsize>,
        count_outcome: Arc<Mutex<Result<RowCount, String>>>,
        count_gate: Arc<Mutex<Option<flume::Receiver<()>>>>,
    }

    impl FakeConnection {
        fn new(sinks: Arc<Mutex<Vec<BatchSink>>>, queries: Arc<Mutex<Vec<String>>>) -> Self {
            let (ping_tx, ping_rx) = flume::unbounded();
            Self {
                sinks,
                queries,
                introspect_outcome: FakeIntrospectOutcome::Ready(SchemaTree::default()),
                ping_tx,
                ping_rx,
                ping_calls: Arc::new(AtomicUsize::new(0)),
                count_calls: Arc::new(AtomicUsize::new(0)),
                count_outcome: Arc::new(Mutex::new(Ok(RowCount::Exact(0)))),
                count_gate: Arc::new(Mutex::new(None)),
            }
        }

        /// A sender that scripts this connection's next `ping()` calls, one
        /// outcome per call, in order.
        fn ping_sender(&self) -> flume::Sender<Result<(), CoreError>> {
            self.ping_tx.clone()
        }

        /// A shared counter of every `ping()` call made on this connection,
        /// so a test can assert exactly how many probes were actually
        /// dispatched rather than just observing their (non-)outcome.
        fn ping_call_counter(&self) -> Arc<AtomicUsize> {
            Arc::clone(&self.ping_calls)
        }

        /// A shared counter of every `count_rows()` call made on this
        /// connection, so a test can assert whether (and how often) a count
        /// was actually fetched.
        fn count_call_counter(&self) -> Arc<AtomicUsize> {
            Arc::clone(&self.count_calls)
        }

        /// Script the outcome of every future `count_rows()` call on this
        /// connection.
        fn set_count_outcome(&self, outcome: Result<RowCount, String>) {
            *self
                .count_outcome
                .lock()
                .expect("count_outcome lock poisoned") = outcome;
        }

        /// Install a gate that holds every future `count_rows()` call pending
        /// until the returned sender fires, so a test can keep a count in
        /// flight while a newer query supersedes the preview that started it.
        fn gate_count(&self) -> flume::Sender<()> {
            let (tx, rx) = flume::unbounded();
            *self.count_gate.lock().expect("count_gate lock poisoned") = Some(rx);
            tx
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
                    Err(CoreError::introspection(message.clone()))
                }
            }
        }

        async fn ping(&self) -> Result<(), CoreError> {
            self.ping_calls.fetch_add(1, Ordering::SeqCst);
            self.ping_rx.recv_async().await.unwrap_or_else(|_| {
                Err(CoreError::connection(
                    "fake connection closed".to_owned(),
                    false,
                ))
            })
        }

        async fn count_rows(&self, _schema: &str, _relation: &str) -> Result<RowCount, CoreError> {
            self.count_calls.fetch_add(1, Ordering::SeqCst);
            let gate = self
                .count_gate
                .lock()
                .expect("count_gate lock poisoned")
                .clone();
            if let Some(gate) = gate {
                let _ = gate.recv_async().await;
            }
            self.count_outcome
                .lock()
                .expect("count_outcome lock poisoned")
                .clone()
                .map_err(CoreError::query)
        }

        async fn describe_relation(
            &self,
            _schema: &str,
            _relation: &str,
        ) -> Result<zsql_core::RelationSchema, CoreError> {
            Ok(zsql_core::RelationSchema::default())
        }
    }

    /// A `Connection` double whose `preview_query` returns a form no dialect
    /// this codebase ships actually emits, so a test asserting on it can only
    /// pass if `Session` really dispatches through `Connection::preview_query`
    /// rather than a hardcoded string of its own.
    struct DialectStubConnection {
        queries: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl Connection for DialectStubConnection {
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

        async fn count_rows(&self, _schema: &str, _relation: &str) -> Result<RowCount, CoreError> {
            Ok(RowCount::Exact(0))
        }

        async fn describe_relation(
            &self,
            _schema: &str,
            _relation: &str,
        ) -> Result<zsql_core::RelationSchema, CoreError> {
            Ok(zsql_core::RelationSchema::default())
        }

        fn preview_query(&self, schema: &str, relation: &str, limit: u64) -> String {
            format!("SELECT TOP ({limit}) * FROM [{schema}].[{relation}]")
        }
    }

    #[gpui::test]
    fn preview_relation_executes_the_active_connections_own_dialect(cx: &mut TestAppContext) {
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection = DialectStubConnection {
            queries: queries.clone(),
        };

        let session = cx.new(|_cx| {
            let mut session = Session::new(&Config::default());
            session.connection = Some(Arc::new(connection));
            session
        });

        session
            .update(cx, |session, cx| {
                session.preview_relation("dbo", "orders", cx)
            })
            .detach();
        cx.run_until_parked();

        let recorded = queries.lock().expect("queries lock poisoned");
        assert_eq!(
            recorded.as_slice(),
            ["SELECT TOP (200) * FROM [dbo].[orders]"],
            "preview_relation must execute whatever the active connection's own \
             preview_query builds, not a hardcoded LIMIT form"
        );
    }

    #[gpui::test]
    fn liveness_probe_does_not_run_before_a_connection_exists(cx: &mut TestAppContext) {
        let session = cx.new(|_cx| session_with_no_url());

        // No `connect()` (let alone a successful one) has ever happened, so
        // no probe loop was ever started: advancing well past several probe
        // intervals must not change `liveness`, and must not leave any task
        // for `run_until_parked` to (unexpectedly) still be running.
        let interval = session.read_with(cx, |session, _app| session.probe_interval_for_test());
        cx.executor().advance_clock(interval * 5);
        cx.run_until_parked();

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Empty),
                "expected SessionState::Empty, got {:?}",
                session.state()
            );
            assert_eq!(
                *session.liveness(),
                LivenessState::Unknown,
                "liveness must stay Unknown when no connection ever existed"
            );
        });
    }

    #[gpui::test]
    fn a_failed_probe_updates_liveness_without_touching_a_running_query(cx: &mut TestAppContext) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection = FakeConnection::new(sinks, queries);
        let ping_sender = connection.ping_sender();

        let session = cx.new(|_cx| {
            let mut session = session_with_no_url();
            session.connection = Some(Arc::new(connection));
            // Simulate a query mid-flight: a probe tick landing now must
            // not disturb this.
            session.state = SessionState::Running;
            session
        });

        let interval = session.update(cx, |session, cx| {
            session.start_liveness_probe_for_test(cx);
            session.probe_interval_for_test()
        });

        ping_sender
            .send(Err(CoreError::connection(
                "connection reset".to_owned(),
                false,
            )))
            .expect("send failed");
        cx.executor().advance_clock(interval);
        cx.run_until_parked();

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Running),
                "a probe result must never touch SessionState::Running, got {:?}",
                session.state()
            );
            match session.liveness() {
                LivenessState::Unreachable(message) => assert!(!message.is_empty()),
                other => panic!("expected LivenessState::Unreachable, got {other:?}"),
            }
        });
    }

    #[gpui::test]
    fn a_subsequent_successful_probe_reverts_liveness_to_healthy(cx: &mut TestAppContext) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection = FakeConnection::new(sinks, queries);
        let ping_sender = connection.ping_sender();

        let session = cx.new(|_cx| {
            let mut session = session_with_no_url();
            session.connection = Some(Arc::new(connection));
            session.state = SessionState::Connected;
            session
        });

        let interval = session.update(cx, |session, cx| {
            session.start_liveness_probe_for_test(cx);
            session.probe_interval_for_test()
        });

        ping_sender
            .send(Err(CoreError::connection(
                "connection reset".to_owned(),
                true,
            )))
            .expect("send failed");
        cx.executor().advance_clock(interval);
        cx.run_until_parked();
        session.read_with(cx, |session, _app| {
            assert!(matches!(session.liveness(), LivenessState::Unreachable(_)));
        });

        ping_sender.send(Ok(())).expect("send failed");
        cx.executor().advance_clock(interval);
        cx.run_until_parked();

        session.read_with(cx, |session, _app| {
            assert_eq!(
                *session.liveness(),
                LivenessState::Healthy,
                "a subsequent successful probe must revert liveness to Healthy automatically"
            );
        });
    }

    #[gpui::test]
    fn an_overlapping_tick_is_skipped_while_a_probe_is_already_in_flight(cx: &mut TestAppContext) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        // Deliberately never send a ping response: the probe stays
        // outstanding for the whole test.
        let connection = FakeConnection::new(sinks, queries);
        let ping_calls = connection.ping_call_counter();

        // A timeout much longer than the interval so the probe's own
        // timeout can't race ahead and resolve it before the second tick
        // fires; the test needs the first probe to still be genuinely
        // outstanding when the second interval elapses.
        let mut cfg = Config::default();
        cfg.liveness.probe_interval_ms = 10;
        cfg.liveness.probe_timeout_ms = 10_000;

        let session = cx.new(|_cx| {
            let mut session = Session::new(&cfg);
            session.connection = Some(Arc::new(connection));
            session.state = SessionState::Connected;
            session
        });

        let interval = session.update(cx, |session, cx| {
            session.start_liveness_probe_for_test(cx);
            session.probe_interval_for_test()
        });

        // First tick: starts the (never-answered) probe.
        cx.executor().advance_clock(interval);
        cx.run_until_parked();
        assert_eq!(
            ping_calls.load(Ordering::SeqCst),
            1,
            "the first tick must issue exactly one ping"
        );

        // A second interval elapses while that probe is still outstanding;
        // this tick must be skipped, not start an overlapping probe. If the
        // in-flight guard were broken, this would issue a second `ping()`
        // even though `liveness` (which the never-answered fake can't move)
        // would look identical either way.
        cx.executor().advance_clock(interval);
        cx.run_until_parked();

        assert_eq!(
            ping_calls.load(Ordering::SeqCst),
            1,
            "an overlapping tick must be skipped, not start a second ping while one is in flight"
        );
        session.read_with(cx, |session, _app| {
            assert_eq!(
                *session.liveness(),
                LivenessState::Unknown,
                "no probe has ever completed, so liveness must still be Unknown"
            );
        });
    }

    #[gpui::test]
    fn a_probe_that_never_answers_before_its_timeout_is_treated_as_a_failure(
        cx: &mut TestAppContext,
    ) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        // Deliberately never send a ping response: the only way this probe
        // can resolve is via the timeout path, not the ping-error path.
        let connection = FakeConnection::new(sinks, queries);

        // A generous interval and a short timeout so the probe's timer,
        // not the loop's own next tick, is what resolves the probe.
        let mut cfg = Config::default();
        cfg.liveness.probe_interval_ms = 10_000;
        cfg.liveness.probe_timeout_ms = 10;

        let session = cx.new(|_cx| {
            let mut session = Session::new(&cfg);
            session.connection = Some(Arc::new(connection));
            session.state = SessionState::Connected;
            session
        });

        let (interval, timeout) = session.update(cx, |session, cx| {
            session.start_liveness_probe_for_test(cx);
            (
                session.probe_interval_for_test(),
                session.probe_timeout_for_test(),
            )
        });

        // First tick starts the probe; it then must time out on its own
        // before the (much longer) next interval would ever fire again.
        cx.executor().advance_clock(interval);
        cx.run_until_parked();
        cx.executor().advance_clock(timeout);
        cx.run_until_parked();

        session.read_with(cx, |session, _app| match session.liveness() {
            LivenessState::Unreachable(message) => {
                assert!(
                    message.contains("timed out"),
                    "expected a timeout-specific message, got: {message}"
                );
            }
            other => panic!("expected LivenessState::Unreachable, got {other:?}"),
        });
    }

    #[gpui::test]
    fn a_stale_probe_from_a_superseded_connection_is_ignored(cx: &mut TestAppContext) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let stale_connection = FakeConnection::new(sinks.clone(), queries.clone());
        let stale_ping = stale_connection.ping_sender();

        let session = cx.new(|_cx| {
            let mut session = session_with_no_url();
            session.connection = Some(Arc::new(stale_connection));
            session.state = SessionState::Connected;
            session
        });

        let interval = session.update(cx, |session, cx| {
            session.start_liveness_probe_for_test(cx);
            session.probe_interval_for_test()
        });

        // Fire the stale connection's first tick; its probe starts but
        // never resolves yet (no response sent), simulating a probe still
        // in flight at the moment a reconnect supersedes it below.
        cx.executor().advance_clock(interval);
        cx.run_until_parked();

        // Simulate a reconnect: a fresh connection supersedes the stale
        // one's generation, exactly as a real `connect()` would.
        let fresh_sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let fresh_queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let fresh_connection = FakeConnection::new(fresh_sinks, fresh_queries);
        let fresh_ping = fresh_connection.ping_sender();
        session.update(cx, |session, cx| {
            session.connection = Some(Arc::new(fresh_connection));
            session.start_liveness_probe_for_test(cx);
        });

        // Now let the stale probe resolve successfully. Its generation no
        // longer matches, so this must not overwrite `liveness`.
        stale_ping.send(Ok(())).expect("send failed");
        cx.run_until_parked();

        session.read_with(cx, |session, _app| {
            assert_eq!(
                *session.liveness(),
                LivenessState::Unknown,
                "a stale probe's success must not be folded into liveness"
            );
        });

        // The fresh connection's own first tick, however, must be honored.
        fresh_ping
            .send(Err(CoreError::connection(
                "fresh probe failed".to_owned(),
                false,
            )))
            .expect("send failed");
        cx.executor().advance_clock(interval);
        cx.run_until_parked();

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.liveness(), LivenessState::Unreachable(_)),
                "the fresh connection's own probe result must still be honored, got {:?}",
                session.liveness()
            );
        });
    }

    #[gpui::test]
    fn superseding_a_query_ignores_late_events_from_the_previous_query(cx: &mut TestAppContext) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection = FakeConnection::new(sinks.clone(), queries);

        let session = cx.new(|_cx| {
            let mut session = session_with_no_url();
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

    #[gpui::test]
    fn a_superseded_querys_over_limit_batch_does_not_truncate_the_current_query(
        cx: &mut TestAppContext,
    ) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection = FakeConnection::new(sinks.clone(), queries);

        let mut cfg = Config::default();
        cfg.query.max_result_rows = 3;
        let session = cx.new(|_cx| {
            let mut session = Session::new(&cfg);
            session.connection = Some(Arc::new(connection));
            session
        });

        session
            .update(cx, |session, cx| session.run_query("SELECT 1", cx))
            .detach();
        cx.run_until_parked();

        // Supersede the first query before it ever reaches a terminal state.
        session
            .update(cx, |session, cx| session.run_query("SELECT 2", cx))
            .detach();
        cx.run_until_parked();

        let (first_sink, second_sink) = {
            let sinks = sinks.lock().expect("sinks lock poisoned");
            assert_eq!(sinks.len(), 2, "expected exactly two stream_query calls");
            (sinks[0].clone(), sinks[1].clone())
        };

        // The superseded first query's batch alone would blow past the
        // limit of 3, but it must never be folded into the current
        // (second) query's state: the generation guard in `run_query`'s
        // consumer loop must reject it before `apply_query_event` even
        // sees it.
        first_sink
            .send(Ok(QueryEvent::Columns(vec![ColumnMeta {
                name: "stale".to_owned(),
                type_name: "int8".to_owned(),
                nullable: false,
            }])))
            .expect("first sink send failed");
        first_sink
            .send(Ok(QueryEvent::Batch(RowBatch {
                rows: (0..10).map(|n| Row(vec![Value::Int(n)])).collect(),
            })))
            .expect("first sink send failed");
        cx.run_until_parked();

        session.read_with(cx, |session, _app| {
            assert!(
                !matches!(session.state(), SessionState::Truncated { .. }),
                "a stale over-limit batch from a superseded query must not truncate \
                 the current query, got {:?}",
                session.state()
            );
        });

        second_sink
            .send(Ok(QueryEvent::Columns(vec![ColumnMeta {
                name: "fresh".to_owned(),
                type_name: "int8".to_owned(),
                nullable: false,
            }])))
            .expect("second sink send failed");
        second_sink
            .send(Ok(QueryEvent::Batch(RowBatch {
                rows: vec![Row(vec![Value::Int(1)])],
            })))
            .expect("second sink send failed");
        second_sink
            .send(Ok(QueryEvent::Done { affected: None }))
            .expect("second sink send failed");
        cx.run_until_parked();

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Results(_)),
                "the current query must complete normally, got {:?}",
                session.state()
            );
            assert_eq!(session.result().rows.len(), 1);
        });
    }

    #[gpui::test]
    fn a_query_error_leaves_the_underlying_connection_in_place(cx: &mut TestAppContext) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection = FakeConnection::new(sinks.clone(), queries);

        let session = cx.new(|_cx| {
            let mut session = session_with_no_url();
            session.connection = Some(Arc::new(connection));
            session
        });

        session
            .update(cx, |session, cx| session.run_query("SELECT bad", cx))
            .detach();
        cx.run_until_parked();

        let sink = {
            let sinks = sinks.lock().expect("sinks lock poisoned");
            sinks[0].clone()
        };
        sink.send(Err(CoreError::query("syntax error".to_owned())))
            .expect("sink send failed");
        cx.run_until_parked();

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Error(_)),
                "expected SessionState::Error after a query failure, got {:?}",
                session.state()
            );
            assert!(
                session.is_connected(),
                "a query error must not drop the underlying connection -- \
                 Session::is_connected() must stay true so the connection \
                 footer keeps showing the still-live database"
            );
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
            let mut session = session_with_no_url();
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
            let mut session = session_with_no_url();
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
            let mut session = session_with_no_url();
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

    #[gpui::test]
    fn run_query_does_not_fetch_a_row_count(cx: &mut TestAppContext) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection = FakeConnection::new(sinks.clone(), queries);
        let count_calls = connection.count_call_counter();

        let session = cx.new(|_cx| {
            let mut session = session_with_no_url();
            session.connection = Some(Arc::new(connection));
            session
        });

        session
            .update(cx, |session, cx| session.run_query("SELECT 1", cx))
            .detach();
        cx.run_until_parked();

        let sink = {
            let sinks = sinks.lock().expect("sinks lock poisoned");
            sinks[0].clone()
        };
        sink.send(Ok(QueryEvent::Done { affected: None }))
            .expect("sink send failed");
        cx.run_until_parked();

        assert_eq!(
            count_calls.load(Ordering::SeqCst),
            0,
            "run_query must never call count_rows -- only preview_relation does"
        );
        session.read_with(cx, |session, _app| {
            assert_eq!(session.row_count(), None);
        });
    }

    #[gpui::test]
    fn preview_relation_fetches_and_exposes_the_row_count(cx: &mut TestAppContext) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection = FakeConnection::new(sinks, queries);
        connection.set_count_outcome(Ok(RowCount::Exact(42)));
        let count_calls = connection.count_call_counter();

        let session = cx.new(|_cx| {
            let mut session = session_with_no_url();
            session.connection = Some(Arc::new(connection));
            session
        });

        session
            .update(cx, |session, cx| {
                session.preview_relation("public", "orders", cx)
            })
            .detach();
        cx.run_until_parked();

        assert_eq!(
            count_calls.load(Ordering::SeqCst),
            1,
            "preview_relation must fetch the row count exactly once"
        );
        session.read_with(cx, |session, _app| {
            assert_eq!(session.row_count(), Some(RowCount::Exact(42)));
        });
    }

    #[gpui::test]
    fn a_failing_row_count_fetch_leaves_row_count_none_without_touching_session_state(
        cx: &mut TestAppContext,
    ) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection = FakeConnection::new(sinks.clone(), queries);
        connection.set_count_outcome(Err("boom".to_owned()));

        let session = cx.new(|_cx| {
            let mut session = session_with_no_url();
            session.connection = Some(Arc::new(connection));
            session
        });

        session
            .update(cx, |session, cx| {
                session.preview_relation("public", "orders", cx)
            })
            .detach();
        cx.run_until_parked();

        let sink = {
            let sinks = sinks.lock().expect("sinks lock poisoned");
            sinks[0].clone()
        };
        sink.send(Ok(QueryEvent::Columns(vec![ColumnMeta {
            name: "id".to_owned(),
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
                "a failing row count fetch must not disturb the streaming preview's state, got {:?}",
                session.state()
            );
            assert_eq!(
                session.row_count(),
                None,
                "a failing row count fetch must leave row_count at None"
            );
        });
    }

    #[gpui::test]
    fn a_superseded_previews_late_count_does_not_overwrite_the_current_row_count(
        cx: &mut TestAppContext,
    ) {
        let sinks: Arc<Mutex<Vec<BatchSink>>> = Arc::new(Mutex::new(Vec::new()));
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection = FakeConnection::new(sinks, queries);
        connection.set_count_outcome(Ok(RowCount::Exact(42)));
        // Hold the count in flight so it can only land after a newer query
        // has bumped the generation out from under it.
        let release_count = connection.gate_count();

        let session = cx.new(|_cx| {
            let mut session = session_with_no_url();
            session.connection = Some(Arc::new(connection));
            session
        });

        session
            .update(cx, |session, cx| {
                session.preview_relation("public", "orders", cx)
            })
            .detach();
        cx.run_until_parked();

        // A newer query supersedes the preview while its count is still gated.
        // `run_query` never fetches a count, so no second count is in flight.
        session
            .update(cx, |session, cx| {
                session.run_query("SELECT 1".to_owned(), cx)
            })
            .detach();
        cx.run_until_parked();

        release_count.send(()).expect("count gate send failed");
        cx.run_until_parked();

        session.read_with(cx, |session, _app| {
            assert_eq!(
                session.row_count(),
                None,
                "a late count from a superseded preview must not overwrite the current result"
            );
        });
    }

    #[gpui::test]
    async fn preview_relation_against_a_real_connection_exposes_the_seeded_row_count(
        cx: &mut TestAppContext,
    ) {
        use zsql_core::Driver as _;

        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let cfg = zsql_core::ConnConfig::from_url("sqlite::memory:").unwrap();
        let conn = zsql_sqlite::SqliteDriver
            .connect(&cfg)
            .await
            .expect("sqlite connect should succeed");
        let conn: Arc<dyn Connection> = Arc::from(conn);

        let (setup_tx, setup_rx) = flume::unbounded();
        let _setup = conn.stream_query(
            "CREATE TABLE items(id INTEGER PRIMARY KEY); \
             INSERT INTO items DEFAULT VALUES; \
             INSERT INTO items DEFAULT VALUES; \
             INSERT INTO items DEFAULT VALUES"
                .to_owned(),
            setup_tx,
        );
        while setup_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok()
        {}

        let session = cx.new(|_cx| {
            let mut session = session_with_no_url();
            session.connection = Some(conn);
            session
        });

        session
            .update(cx, |session, cx| {
                session.preview_relation("main", "items", cx)
            })
            .await;

        // The count fetch runs as its own detached task, on a real OS
        // thread outside the `TestAppContext`'s deterministic dispatcher
        // (this crate has no tokio runtime to hand it a virtual clock), so
        // `run_until_parked` alone can return before it actually finishes -
        // the same real-IO timing this crate's liveness-probe tests already
        // document and poll around. Poll with a bounded number of short
        // real sleeps instead of asserting immediately.
        let mut row_count = None;
        for _ in 0..50 {
            cx.run_until_parked();
            row_count = session.read_with(cx, |session, _app| session.row_count());
            if row_count.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        assert_eq!(
            row_count,
            Some(RowCount::Exact(3)),
            "expected the row count to match the 3 seeded rows, got {row_count:?}"
        );
    }

    // -- tunnel lifecycle ---------------------------------------------------

    /// A successful connect switch must drop whatever tunnel the previous
    /// connection was using: `connect_to`'s synchronous reset clears it
    /// before the new attempt (which here opens no tunnel of its own) even
    /// starts.
    #[gpui::test]
    async fn a_successful_switch_drops_the_previous_tunnel(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let open_count = Arc::new(AtomicUsize::new(0));
        let session = cx.new(|_cx| session_with_no_url());
        session.update(cx, |session, _cx| {
            session.set_tunnel_for_test(Box::new(FakeTunnel::new(open_count.clone())));
        });
        assert_eq!(open_count.load(Ordering::SeqCst), 1);

        session
            .update(cx, |session, cx| session.connect_to("sqlite::memory:", cx))
            .await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "expected the switch to succeed, got {:?}",
                session.state()
            );
        });
        assert_eq!(
            open_count.load(Ordering::SeqCst),
            0,
            "a successful switch must drop the tunnel it superseded"
        );
    }

    /// A failed connect switch must still drop whatever tunnel the previous
    /// connection was using, exactly as a successful one does.
    #[gpui::test]
    async fn a_failed_switch_drops_the_previous_tunnel(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let open_count = Arc::new(AtomicUsize::new(0));
        let session = cx.new(|_cx| session_with_no_url());
        session.update(cx, |session, _cx| {
            session.set_tunnel_for_test(Box::new(FakeTunnel::new(open_count.clone())));
        });
        assert_eq!(open_count.load(Ordering::SeqCst), 1);

        session
            .update(cx, |session, cx| {
                session.connect_to("cassandra://host/db", cx)
            })
            .await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Error(_)),
                "expected the switch to fail, got {:?}",
                session.state()
            );
        });
        assert_eq!(
            open_count.load(Ordering::SeqCst),
            0,
            "a failed switch must still drop the tunnel it superseded"
        );
    }

    /// The old tunnel must be gone at the exact same synchronous point the
    /// schema resets to `NotLoaded` -- not deferred until the new attempt
    /// resolves -- mirroring `connect_to_resets_schema_to_not_loaded_synchronously_regardless_of_prior_state`.
    #[gpui::test]
    async fn a_switch_drops_the_previous_tunnel_synchronously_alongside_the_schema_reset(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let open_count = Arc::new(AtomicUsize::new(0));
        let session = cx.new(|_cx| session_with_no_url());
        session.update(cx, |session, _cx| {
            session.set_tunnel_for_test(Box::new(FakeTunnel::new(open_count.clone())));
        });

        let task = session.update(cx, |session, cx| {
            let task = session.connect_to("sqlite::memory:", cx);
            assert_eq!(
                open_count.load(Ordering::SeqCst),
                0,
                "the previous tunnel must already be gone synchronously, \
                 in the same call that dispatches the switch"
            );
            assert!(
                matches!(session.schema(), SchemaState::NotLoaded),
                "expected the schema to reset in that same synchronous call"
            );
            assert!(
                !session.has_tunnel_for_test(),
                "the session must not report holding a tunnel synchronously either"
            );
            task
        });

        task.await;
    }

    /// In-memory driver for the live end-to-end tests
    fn in_memory_database_url() -> String {
        "sqlite::memory:".to_string()
    }

    async fn seed_test_data(cx: &mut TestAppContext, session: &Entity<Session>) {
        let run_task = session.update(cx, |session, cx| {
            session.run_query(
                "CREATE TABLE orders(\
               id INTEGER PRIMARY KEY, \
               user_id INTEGER NOT NULL, \
               total_cents INTEGER NOT NULL, \
               status TEXT NOT NULL, \
               metadata TEXT, \
               placed_at TEXT NOT NULL); \
               INSERT INTO orders(user_id, total_cents, status, metadata, placed_at) VALUES \
               (1, 1299, 'shipped', '{\"gift\": true}', '2024-01-01T12:00:00Z'), \
               (2, 4900, 'pending', '{\"gift\": false}', '2024-01-02T15:30:00Z'), \
               (3, 250, 'delivered', '{\"gift\": true}', '2024-01-03T09:45:00Z')",
                cx,
            )
        });
        run_task.await;
    }

    /// Poll `session`'s liveness, advancing the deterministic test clock by
    /// one probe `interval` between polls, until `matches_target` returns
    /// true or `max_polls` is exhausted. Returns whether it matched.
    ///
    /// The probe's socket IO runs on a real OS thread outside the
    /// `TestAppContext`'s deterministic dispatcher (this crate has no tokio
    /// runtime to hand it a virtual clock), so each poll also sleeps a
    /// short, real amount of wall-clock time before checking again -
    /// negligible against `max_polls * interval`'s overall budget, but
    /// enough for a same-host round trip to actually land.
    fn wait_for_liveness(
        cx: &mut TestAppContext,
        session: &Entity<Session>,
        interval: Duration,
        max_polls: u32,
        matches_target: impl Fn(&LivenessState) -> bool,
    ) -> bool {
        for _ in 0..max_polls {
            let matched = session.read_with(cx, |session, _app| matches_target(session.liveness()));
            if matched {
                return true;
            }
            cx.executor().advance_clock(interval);
            std::thread::sleep(Duration::from_millis(20));
            cx.run_until_parked();
        }
        session.read_with(cx, |session, _app| matches_target(session.liveness()))
    }

    #[gpui::test]
    async fn session_connects_and_streams_a_live_query_when_configured(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let cfg = Config::default();
        let session = cx.new(|_cx| Session::new(&cfg));
        session
            .update(cx, |session, cx| {
                session.connect_to(in_memory_database_url(), cx)
            })
            .await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "connect should succeed and land in SessionState::Connected, got {:?}",
                session.state()
            );
        });

        seed_test_data(cx, &session).await;

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
    async fn a_runaway_result_is_capped_at_the_configured_limit_when_configured(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let mut cfg = Config::default();
        cfg.query.max_result_rows = 100;

        let session = cx.new(|_cx| Session::new(&cfg));
        session
            .update(cx, |session, cx| {
                session.connect_to(in_memory_database_url(), cx)
            })
            .await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "connect should succeed against a reachable database, got {:?}",
                session.state()
            );
        });

        let run_task = session.update(cx, |session, cx| {
            session.run_query(
                "
              WITH RECURSIVE generate_series(value) AS (
                SELECT 1
                UNION ALL
                SELECT value + 1
                FROM generate_series
                WHERE value + 1 <= 100000
              )
              SELECT value FROM generate_series",
                cx,
            )
        });
        run_task.await;

        session.read_with(cx, |session, _app| {
            match session.state() {
                SessionState::Truncated { rows, .. } => {
                    assert_eq!(
                        *rows, 100_000,
                        "the truncated state must report exactly the configured limit"
                    );
                }
                other => panic!(
                    "expected SessionState::Truncated after exceeding the configured limit, \
                     got {other:?} (the query must not stream all 100,000 rows nor stay \
                     Running indefinitely)"
                ),
            }
            assert_eq!(
                session.result().rows.len(),
                100,
                "accumulated rows must be capped at exactly the configured limit"
            );
        });
    }

    #[gpui::test]
    async fn connect_without_running_a_query_leaves_the_session_connected_when_configured(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let cfg = Config::default();
        let session = cx.new(|_cx| Session::new(&cfg));
        session
            .update(cx, |session, cx| {
                session.connect_to(in_memory_database_url(), cx)
            })
            .await;

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
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let cfg = Config::default();
        let session = cx.new(|_cx| Session::new(&cfg));
        session
            .update(cx, |session, cx| {
                session.connect_to(in_memory_database_url(), cx)
            })
            .await;

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
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let cfg = Config::default();
        let session = cx.new(|_cx| Session::new(&cfg));
        session
            .update(cx, |session, cx| {
                session.connect_to(in_memory_database_url(), cx)
            })
            .await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "connect should succeed against a reachable database, got {:?}",
                session.state()
            );
        });

        seed_test_data(cx, &session).await;

        session.update(cx, Session::introspect).await;

        session.read_with(cx, |session, _app| {
            let tree = match session.schema() {
                SchemaState::Ready(tree) => tree,
                other => panic!("expected SchemaState::Ready, got {other:?}"),
            };
            let main = tree
                .catalogs
                .iter()
                .flat_map(|catalog| &catalog.schemas)
                .find(|schema| schema.name == "main")
                .expect("expected the seeded main schema in the introspected schema");

            assert!(
                main.tables.iter().any(|r| r.name == "orders"),
                "expected the seeded orders table in the introspected schema"
            );
        });

        session
            .update(cx, |session, cx| {
                session.preview_relation("main", "orders", cx)
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

    #[gpui::test]
    async fn a_liveness_probe_completes_and_a_slow_query_still_finishes_when_configured(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let mut cfg = Config::default();
        cfg.liveness.probe_interval_ms = 100;
        cfg.liveness.probe_timeout_ms = 2_000;
        let interval = cfg.liveness.probe_interval();

        let session = cx.new(|_cx| Session::new(&cfg));
        session
            .update(cx, |session, cx| {
                session.connect_to(in_memory_database_url(), cx)
            })
            .await;

        session.read_with(cx, |session, _app| {
            assert!(matches!(session.state(), SessionState::Connected));
        });

        let run_task = session.update(cx, |session, cx| {
            session.run_query(
                "
            WITH RECURSIVE
              delay(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM delay WHERE i < 10000000)
            SELECT count(*) FROM delay;",
                cx,
            )
        });

        let reached_healthy_while_running =
            wait_for_liveness(cx, &session, interval, 50, |liveness| {
                matches!(liveness, LivenessState::Healthy)
            });
        assert!(
            reached_healthy_while_running,
            "a probe must complete (using its own connection) while the slow query streams"
        );
        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Running),
                "the slow query must still be in flight while the probe completed, got {:?}",
                session.state()
            );
        });

        // Just to be clear, explicitly drop the run_task
        drop(run_task);
    }
}
