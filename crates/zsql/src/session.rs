//! `Session` owns the app's single active database connection and drives the
//! query lifecycle the results grid renders

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::Either;
use gpui::{BackgroundExecutor, Context, Task, prelude::*};
use zsql_core::{Connection, CoreError, QueryEvent, ResultSet, SchemaTree};

use crate::config::Config;
use crate::drivers;

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
    /// The most recent query was cancelled after its accumulated row count
    /// reached the configured limit (`Config.query.max_result_rows`). The
    /// rows streamed up to that point remain in [`Session::result`]
    Limited { elapsed: Duration, rows: u64 },
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

/// The active connection's liveliness, as tracked by the recurring probe
/// loop. Kept independent of [`SessionState`] so a probe result never
/// overwrites query-lifecycle state such as `Running`/`Results(_)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LivenessState {
    /// No probe has completed yet for the current connection (including
    /// while there is no connection at all).
    Unknown,
    /// The most recent probe succeeded.
    Healthy,
    /// The most recent probe failed or timed out. The message is safe to
    /// show directly in the UI.
    Unreachable(String),
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
    /// Upper bound on rows accumulated for the query currently streaming,
    /// from [`Config::query`]. Reaching this many rows cancels the query and
    /// moves `state` to [`SessionState::Limited`]; see [`Session::apply_query_event`].
    max_result_rows: u64,
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
    /// The active connection's liveliness, as tracked by the recurring
    /// probe loop, independent of `state`.
    liveness: LivenessState,
    /// Incremented at the start of every `connect` attempt. A probe loop
    /// captures the generation of the connection it was started for and
    /// stops folding results into `liveness` once this no longer matches,
    /// so a stale probe from a superseded connection is ignored.
    connection_generation: u64,
    /// Whether a liveliness probe is currently awaiting its result. Guards
    /// against starting an overlapping probe if one is still outstanding
    /// when the next interval elapses; that tick is skipped instead.
    probe_in_flight: bool,
    /// How often the liveliness probe fires, from [`Config::liveness`].
    probe_interval: Duration,
    /// How long a single liveliness probe may run before it is treated as a
    /// failure, from [`Config::liveness`].
    probe_timeout: Duration,
}

/// What a liveliness probe loop's tick did, decided under a single
/// `Session::update` so the in-flight guard and generation check are
/// atomic with respect to the rest of `Session`'s state.
enum ProbeTick {
    /// A probe was dispatched (as its own task, so this loop's next timer
    /// starts on schedule regardless of how long the probe takes).
    Started,
    /// A probe is already outstanding; this tick was skipped.
    Skipped,
    /// The connection this loop was started for has been superseded (or is
    /// gone entirely); the loop must stop.
    Stale,
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
            max_result_rows: cfg.query.max_result_rows,
            active_query: None,
            accumulating: ResultSet::default(),
            query_started_at: None,
            query_generation: 0,
            liveness: LivenessState::Unknown,
            connection_generation: 0,
            probe_in_flight: false,
            probe_interval: cfg.liveness.probe_interval(),
            probe_timeout: cfg.liveness.probe_timeout(),
        }
    }

    /// The session's current lifecycle state.
    #[must_use]
    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// Whether a live connection is currently held, independent of
    /// [`SessionState`]: a query error (see [`Session::run_query`]) moves
    /// `state` to [`SessionState::Error`] without dropping the connection
    /// itself, so callers that need to know "is the database actually
    /// reachable" (e.g. the connection footer) must check this rather than
    /// pattern-matching `state` alone.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connection.is_some()
    }

    /// The active connection's liveliness, as tracked by the recurring
    /// probe loop, independent of [`Session::state`].
    #[must_use]
    pub fn liveness(&self) -> &LivenessState {
        &self.liveness
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

    /// The `LIMIT` [`Session::preview_relation`] applies to a relation
    /// preview, from [`Config::query`]'s `preview_limit`.
    #[must_use]
    pub fn preview_limit(&self) -> u64 {
        self.preview_limit
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

    /// Connect using the resolved DSN (`DATABASE_URL`, or else
    /// `Config::connection.default_url`) as a fallback/seed when no saved
    /// connection has been explicitly chosen. If none is configured, sets
    /// [`SessionState::Empty`] and returns a completed task
    pub fn connect(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let Some(dsn) = self.dsn.clone() else {
            self.state = SessionState::Empty;
            cx.notify();
            return Task::ready(());
        };
        self.connect_url(dsn, cx)
    }

    /// Connect to an explicitly chosen URL (e.g. a saved connection picked
    /// from the connection manager), replacing whatever connection is
    /// currently active. The driver is resolved from `url`'s scheme via
    /// [`drivers::connect`]; this crate never picks a driver directly.
    pub fn connect_to(&mut self, url: impl Into<String>, cx: &mut Context<Self>) -> Task<()> {
        self.connect_url(url.into(), cx)
    }

    /// Shared implementation behind [`Session::connect`] and
    /// [`Session::connect_to`]: connect to `url` through
    /// [`drivers::connect`], replacing the current connection and
    /// (re)starting the liveliness probe loop on success.
    fn connect_url(&mut self, url: String, cx: &mut Context<Self>) -> Task<()> {
        self.state = SessionState::Connecting;
        self.liveness = LivenessState::Unknown;
        // A fresh connect attempt invalidates any liveness probe loop tied
        // to whatever connection preceded it, even if this attempt goes on
        // to fail: that prior loop's next tick (or in-flight probe) must
        // not fold a stale result into this attempt's state. `probe_in_flight`
        // is reset too, since it tracks the *current* generation's probe:
        // a stale probe's own completion knows not to touch it (see
        // `spawn_probe_and_apply`), so nothing else ever would.
        self.connection_generation = self.connection_generation.wrapping_add(1);
        self.probe_in_flight = false;
        let generation = self.connection_generation;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let connect_result = cx.background_spawn(drivers::connect(url)).await;

            let connected = this.update(cx, |session, cx| {
                let connected = match connect_result {
                    Ok(conn) => {
                        tracing::info!("session connected");
                        session.connection = Some(Arc::from(conn));
                        session.state = SessionState::Connected;
                        true
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "session connect failed");
                        // Drop any previously-active connection: the generation
                        // bump already invalidated its probe loop, and leaving it
                        // in `self.connection` would let `run_query` silently
                        // execute against the database this failed switch was
                        // meant to replace.
                        session.connection = None;
                        session.state = SessionState::Error(err.to_string());
                        false
                    }
                };
                cx.notify();
                connected
            });

            // Only a successful connect starts the probe loop: there is
            // nothing to ping while `state` is `Connecting`/`Error`.
            if matches!(connected, Ok(true)) {
                let _ = this.update(cx, |session, cx| {
                    session.spawn_liveness_probe_loop(generation, cx);
                });
            }
        })
    }

    /// Start the recurring liveliness probe loop for the connection tied to
    /// `generation`, on the gpui executor. The loop's own timer fires on a
    /// fixed cadence of [`Session::probe_interval`](Config::liveness)
    /// regardless of how long any individual probe takes (each probe runs
    /// as its own task, dispatched by [`Session::spawn_probe_and_apply`]);
    /// a tick that lands while a probe is still outstanding is skipped
    /// rather than starting an overlapping one. The loop stops as soon as
    /// `generation` no longer matches [`Session::connection_generation`] (a
    /// fresh `connect` superseded it) or the session itself is dropped.
    fn spawn_liveness_probe_loop(&mut self, generation: u64, cx: &mut Context<Self>) {
        let interval = self.probe_interval;
        let timeout = self.probe_timeout;

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(interval).await;

                let tick = this.update(cx, |session, cx| {
                    if session.connection_generation != generation {
                        return ProbeTick::Stale;
                    }
                    let Some(connection) = session.connection.clone() else {
                        return ProbeTick::Stale;
                    };
                    if session.probe_in_flight {
                        return ProbeTick::Skipped;
                    }
                    session.probe_in_flight = true;
                    Session::spawn_probe_and_apply(generation, connection, timeout, cx);
                    ProbeTick::Started
                });

                match tick {
                    Ok(ProbeTick::Started | ProbeTick::Skipped) => {}
                    Ok(ProbeTick::Stale) | Err(_) => break,
                }
            }
        })
        .detach();
    }

    /// Run one probe against `connection` and fold its outcome into
    /// `liveness`, as an independent task from the interval loop above so a
    /// slow probe cannot delay that loop's next tick. Ignores the result
    /// entirely (without touching `probe_in_flight`) if `generation` has
    /// since been superseded: that flag belongs to whatever generation is
    /// current now, not to this stale probe.
    fn spawn_probe_and_apply(
        generation: u64,
        connection: Arc<dyn Connection>,
        timeout: Duration,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let executor = cx.background_executor().clone();
            let outcome = cx
                .background_spawn(probe_connection(connection, timeout, executor))
                .await;

            let _ = this.update(cx, |session, cx| {
                if session.connection_generation != generation {
                    return;
                }
                session.probe_in_flight = false;
                session.liveness = match outcome {
                    Ok(()) => LivenessState::Healthy,
                    Err(message) => LivenessState::Unreachable(message),
                };
                cx.notify();
            });
        })
        .detach();
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
                        SessionState::Results(..)
                            | SessionState::Error(_)
                            | SessionState::Limited { .. }
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
    ///
    /// Once `state` has moved to [`SessionState::Limited`] for the query
    /// currently streaming, every further event is a no-op: the flume
    /// channel can still hold `Batch`/`Columns`/`Done` events queued before
    /// the cancel took effect, and those must not resurrect rows or state
    /// the truncation already settled.
    fn apply_query_event(&mut self, event: Result<QueryEvent, CoreError>) {
        if matches!(self.state, SessionState::Limited { .. }) {
            return;
        }
        match event {
            Ok(QueryEvent::Columns(columns)) => {
                // Each Columns event begins a fresh result set. A run of
                // several statements only shows the last one, so discard any
                // rows accumulated for a prior set rather than appending the
                // new set's rows onto mismatched columns.
                self.accumulating = ResultSet {
                    columns,
                    ..ResultSet::default()
                };
                self.state = SessionState::Running;
            }
            Ok(QueryEvent::Batch(batch)) => {
                self.append_batch_capped(batch);
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

    /// Append `batch`'s rows to `accumulating`, capping at exactly
    /// `max_result_rows`. A batch can straddle the limit (batches hold up to
    /// the driver's own batch size, which may be far larger than a small
    /// configured limit), so only as many rows as still fit are appended -
    /// never the whole batch followed by a truncation of the vector, which
    /// would momentarily overshoot the limit.
    ///
    /// The moment the cap is reached, the query is cancelled through the
    /// existing [`zsql_core::QueryHandle`] cancellation path (the same one
    /// that drives cooperative-drop and server-side `pg_cancel_backend`
    /// cancellation) and `state` moves to [`SessionState::Limited`].
    fn append_batch_capped(&mut self, batch: zsql_core::RowBatch) {
        let limit = self.max_result_rows;
        let accumulated = row_count(&self.accumulating);
        let remaining = limit.saturating_sub(accumulated);
        let take = usize::try_from(remaining).unwrap_or(usize::MAX);
        self.accumulating
            .rows
            .extend(batch.rows.into_iter().take(take));
        self.state = SessionState::Running;

        let accumulated = row_count(&self.accumulating);
        if !row_limit_reached(accumulated, limit) {
            return;
        }

        let elapsed = self
            .query_started_at
            .take()
            .map_or(Duration::ZERO, |started| started.elapsed());
        tracing::warn!(
            limit,
            rows = accumulated,
            elapsed_ms = elapsed.as_millis(),
            "session query result truncated at the configured row limit"
        );
        self.state = SessionState::Limited {
            elapsed,
            rows: accumulated,
        };
        if let Some(handle) = self.active_query.take() {
            handle.cancel();
        }
    }
}

/// `accumulating.rows.len()` as a `u64`, for comparison against the
/// configured row limit (also `u64`).
fn row_count(accumulating: &ResultSet) -> u64 {
    u64::try_from(accumulating.rows.len()).unwrap_or(u64::MAX)
}

/// Whether an accumulated row count of `accumulated` rows has reached or
/// exceeded `limit`. Pure and cheap (a single comparison) so it can run once
/// per streamed batch without touching the executor or blocking anything
/// else in flight, such as the liveness probe or an unrelated query.
fn row_limit_reached(accumulated: u64, limit: u64) -> bool {
    accumulated >= limit
}

/// Test-only constructors used by the UI views' render and action tests.
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
            max_result_rows: Config::default().query.max_result_rows,
            active_query: None,
            accumulating: result,
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
}

/// Introspect `connection`'s reachable schema.
#[tracing::instrument(name = "session_introspect", skip_all)]
async fn introspect_connection(connection: Arc<dyn Connection>) -> Result<SchemaTree, CoreError> {
    connection.introspect().await
}

/// Ping `connection`, failing the probe if it does not complete within
/// `timeout`. `timeout` races against `connection.ping()` on `executor`'s
/// clock (real wall time in the running app, the deterministic test clock
/// under `TestAppContext`) rather than a runtime timeout helper, since no
/// tokio runtime is available here.
#[tracing::instrument(name = "session_liveness_probe", skip_all)]
async fn probe_connection(
    connection: Arc<dyn Connection>,
    timeout: Duration,
    executor: BackgroundExecutor,
) -> Result<(), String> {
    let started = Instant::now();
    let ping = Box::pin(connection.ping());
    let timed_out = executor.timer(timeout);

    match futures::future::select(ping, timed_out).await {
        Either::Left((Ok(()), _)) => {
            tracing::debug!(
                elapsed_ms = started.elapsed().as_millis(),
                "liveness probe succeeded"
            );
            Ok(())
        }
        Either::Left((Err(err), _)) => {
            tracing::warn!(error = %err, "liveness probe failed");
            Err(err.to_string())
        }
        Either::Right(((), _)) => {
            tracing::warn!(timeout_ms = timeout.as_millis(), "liveness probe timed out");
            Err(format!(
                "liveness probe timed out after {}ms",
                timeout.as_millis()
            ))
        }
    }
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
    fn a_second_result_set_replaces_the_first_keeping_only_the_last() {
        let mut session = session_with_no_dsn();
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
    fn a_batch_crossing_the_limit_is_capped_exactly_and_cancels_the_query() {
        let mut cfg = Config::default();
        cfg.query.max_result_rows = 5;
        let mut session = Session::new(&cfg);
        session.state = SessionState::Running;

        let (cancel_tx, cancel_rx) = flume::unbounded();
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
            SessionState::Limited { rows, .. } => assert_eq!(*rows, 5),
            other => panic!("expected SessionState::Limited, got {other:?}"),
        }
        assert!(
            session.active_query.is_none(),
            "active_query must be cleared once the limit is reached, as on Done/Error"
        );
        assert!(
            cancel_rx.try_recv().is_ok(),
            "the existing QueryHandle cancellation path must have been invoked"
        );

        // Events already queued for this generation when cancellation fired
        // must be ignored, not resurrect rows or flip state away from Limited.
        session.apply_query_event(Ok(QueryEvent::Batch(RowBatch {
            rows: vec![Row(vec![Value::Int(99)])],
        })));
        assert_eq!(
            session.result().rows.len(),
            5,
            "a late batch after truncation must not grow the capped result"
        );
        session.apply_query_event(Ok(QueryEvent::Done { affected: None }));
        assert!(
            matches!(session.state(), SessionState::Limited { .. }),
            "a late Done after truncation must not flip state away from Limited"
        );
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
        assert!(matches!(session.state(), SessionState::Limited { .. }));
    }
}

/// `TestAppContext`-driven `Session` tests that need no live database
#[cfg(test)]
mod gpui_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use gpui::{AppContext as _, TestAppContext};
    use zsql_core::{
        BatchSink, Catalog, ColumnMeta, Connection, CoreError, QueryEvent, QueryHandle, Relation,
        RelationKind, Row, RowBatch, SchemaNs, SchemaTree, Value,
    };

    use super::{Config, LivenessState, SchemaState, Session, SessionState};

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
        let _guard = crate::test_support::serialize_real_io();

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

    /// Proves a `SQLite` connection now works end-to-end through the same
    /// selection-based connect path the app uses, where before this feature
    /// `SQLite` could not be connected at all. Unconditional: an in-memory
    /// database needs no external service.
    #[gpui::test]
    async fn connect_resolves_a_sqlite_url_and_actually_opens_it(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let mut cfg = Config::default();
        cfg.connection.default_url = Some("sqlite::memory:".to_owned());
        let session = cx.new(|_cx| Session::new(&cfg));

        session.update(cx, Session::connect).await;

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
    /// same selection-based path as `connect`, independent of whatever DSN
    /// (if any) `Config` resolved at startup.
    #[gpui::test]
    async fn connect_to_opens_a_sqlite_url_regardless_of_the_configured_dsn(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        // No configured DSN at all: `connect_to` must still work on its own.
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

        async fn ping(&self) -> Result<(), CoreError> {
            self.ping_calls.fetch_add(1, Ordering::SeqCst);
            self.ping_rx
                .recv_async()
                .await
                .unwrap_or_else(|_| Err(CoreError::Connection("fake connection closed".to_owned())))
        }
    }

    #[gpui::test]
    fn liveness_probe_does_not_run_before_a_connection_exists(cx: &mut TestAppContext) {
        let session = cx.new(|_cx| session_with_no_dsn());

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
            let mut session = session_with_no_dsn();
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
            .send(Err(CoreError::Connection("connection reset".to_owned())))
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
            let mut session = session_with_no_dsn();
            session.connection = Some(Arc::new(connection));
            session.state = SessionState::Connected;
            session
        });

        let interval = session.update(cx, |session, cx| {
            session.start_liveness_probe_for_test(cx);
            session.probe_interval_for_test()
        });

        ping_sender
            .send(Err(CoreError::Connection("connection reset".to_owned())))
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
            let mut session = session_with_no_dsn();
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
            .send(Err(CoreError::Connection("fresh probe failed".to_owned())))
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
                !matches!(session.state(), SessionState::Limited { .. }),
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
            let mut session = session_with_no_dsn();
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
        sink.send(Err(CoreError::Query("syntax error".to_owned())))
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
    use std::time::Duration;

    use gpui::{AppContext as _, Entity, TestAppContext};
    use zsql_core::{Driver as _, Value};

    use super::{Config, LivenessState, SchemaState, Session, SessionState};

    fn live_database_url() -> Option<String> {
        let Ok(url) = std::env::var("ZSQL_TEST_DATABASE_URL") else {
            eprintln!("skipping live test: ZSQL_TEST_DATABASE_URL not set");
            return None;
        };
        Some(url)
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
        let Some(url) = live_database_url() else {
            return;
        };
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

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

    /// A query producing far more rows than the configured
    /// `max_result_rows` must be cancelled the moment the limit is reached:
    /// the session's `run_query` task itself stops awaiting further events
    /// as soon as `state` becomes `Limited` (its terminal-state check now
    /// includes it), so `run_task.await` below returns promptly instead of
    /// waiting for `generate_series` to actually finish producing 100,000
    /// rows.
    #[gpui::test]
    async fn a_runaway_result_is_cancelled_and_capped_at_the_configured_limit_when_configured(
        cx: &mut TestAppContext,
    ) {
        let Some(url) = live_database_url() else {
            return;
        };
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let mut cfg = Config::default();
        cfg.connection.default_url = Some(url);
        cfg.query.max_result_rows = 100;

        let session = cx.new(|_cx| Session::new(&cfg));
        session.update(cx, Session::connect).await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "connect should succeed against a reachable database, got {:?}",
                session.state()
            );
        });

        let run_task = session.update(cx, |session, cx| {
            session.run_query("SELECT * FROM generate_series(1, 100000)", cx)
        });
        run_task.await;

        session.read_with(cx, |session, _app| {
            match session.state() {
                SessionState::Limited { rows, .. } => {
                    assert_eq!(
                        *rows, 100,
                        "the truncated state must report exactly the configured limit"
                    );
                }
                other => panic!(
                    "expected SessionState::Limited after exceeding the configured limit, \
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

    #[gpui::test]
    async fn liveness_probe_detects_a_dropped_connection_and_recovers_when_configured(
        cx: &mut TestAppContext,
    ) {
        let Some(url) = live_database_url() else {
            return;
        };
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        // Tag this session's own connections with a unique `application_name`
        // so the disconnect below can target exactly this test's backends,
        // not every backend on the database - other live tests may be
        // running against the same server concurrently.
        let tagged_url = tag_dsn_with_application_name(&url, "zsql_test_liveness_disconnect");

        let mut cfg = Config::default();
        cfg.connection.default_url = Some(tagged_url);
        // Fast enough that this test doesn't burn real wall-clock time
        // waiting out several intervals, but still comfortably separated
        // from the timeout below.
        cfg.liveness.probe_interval_ms = 100;
        cfg.liveness.probe_timeout_ms = 2_000;
        let interval = cfg.liveness.probe_interval();

        let session = cx.new(|_cx| Session::new(&cfg));
        session.update(cx, Session::connect).await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connected),
                "connect should succeed against a reachable database, got {:?}",
                session.state()
            );
        });

        let became_healthy = wait_for_liveness(cx, &session, interval, 50, |liveness| {
            matches!(liveness, LivenessState::Healthy)
        });
        assert!(
            became_healthy,
            "the probe should report Healthy at least once before the connection is severed"
        );

        // Sever only this test's own tagged backends, simulating a genuine
        // dropped connection out from under this session's pools without
        // disturbing any other live test's connections to the same server.
        // Goes through `zsql_postgres::PostgresDriver` and the `Connection`
        // contract, like the rest of this test file, rather than naming
        // `sqlx` directly in this crate.
        terminate_backends_tagged(&url, "zsql_test_liveness_disconnect").await;

        let became_unreachable = wait_for_liveness(cx, &session, interval, 50, |liveness| {
            matches!(liveness, LivenessState::Unreachable(_))
        });
        assert!(
            became_unreachable,
            "liveness should flip to Unreachable within a bounded number of intervals \
             after the connection is severed"
        );

        // No explicit reconnect: sqlx's pool opens a fresh connection the
        // next time it needs one, so the very next successful probe should
        // recover liveness on its own.
        let recovered = wait_for_liveness(cx, &session, interval, 50, |liveness| {
            matches!(liveness, LivenessState::Healthy)
        });
        assert!(
            recovered,
            "liveness should revert to Healthy automatically once the pool recovers, \
             with no reconnect/restart from the app"
        );
    }

    /// Append `?application_name=<tag>` to `url`, so every connection built
    /// from the result is identifiable in `pg_stat_activity` by `tag` alone.
    fn tag_dsn_with_application_name(url: &str, tag: &str) -> String {
        let separator = if url.contains('?') { '&' } else { '?' };
        format!("{url}{separator}application_name={tag}")
    }

    /// Open a throwaway connection to `url` and terminate every backend
    /// tagged with `application_name = tag` (see
    /// [`tag_dsn_with_application_name`]), simulating a dropped connection
    /// from outside the app. Scoped to `tag` rather than every backend on
    /// the database, so this cannot disturb another live test running
    /// concurrently against the same server. Runs entirely through
    /// `zsql_postgres`/`zsql_core` so this test file never names `sqlx`
    /// directly.
    async fn terminate_backends_tagged(url: &str, tag: &str) {
        let cfg = zsql_core::ConnConfig::from_dsn(url).expect("a valid test DSN");
        let conn = zsql_postgres::PostgresDriver
            .connect(&cfg)
            .await
            .expect("a separate verification connection must succeed");

        // `tag` is always one of this file's own string literals, never
        // externally supplied, so inlining it here carries no injection risk.
        let sql = format!(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE application_name = '{tag}' AND pid <> pg_backend_pid()"
        );
        let (tx, rx) = flume::unbounded();
        let _handle = conn.stream_query(sql, tx);
        loop {
            match rx.recv_async().await {
                Ok(Ok(zsql_core::QueryEvent::Done { .. })) | Err(_) => break,
                Ok(Ok(_)) => {}
                Ok(Err(err)) => panic!("terminating tagged backends failed: {err}"),
            }
        }
    }

    #[gpui::test]
    async fn a_liveness_probe_completes_and_a_slow_query_still_finishes_when_configured(
        cx: &mut TestAppContext,
    ) {
        let Some(url) = live_database_url() else {
            return;
        };
        cx.executor().allow_parking();
        let _guard = crate::test_support::serialize_real_io();

        let mut cfg = Config::default();
        cfg.connection.default_url = Some(url);
        cfg.liveness.probe_interval_ms = 100;
        cfg.liveness.probe_timeout_ms = 2_000;
        let interval = cfg.liveness.probe_interval();

        let session = cx.new(|_cx| Session::new(&cfg));
        session.update(cx, Session::connect).await;

        session.read_with(cx, |session, _app| {
            assert!(matches!(session.state(), SessionState::Connected));
        });

        // `pg_sleep` produces no output until it returns, so it is
        // genuinely running server-side (not just queued) for its whole
        // 2-second duration; let the probe tick while it's in flight.
        let run_task = session.update(cx, |session, cx| {
            session.run_query("SELECT pg_sleep(2)", cx)
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

        run_task.await;

        session.read_with(cx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Results(_)),
                "the slow query must still reach its normal terminal state, unaffected \
                 by the probe that ran alongside it, got {:?}",
                session.state()
            );
        });
    }
}
