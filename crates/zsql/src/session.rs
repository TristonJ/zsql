//! `Session` owns the app's single active database connection and drives the
//! query lifecycle the results grid renders

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::Either;
use gpui::{BackgroundExecutor, Context, Task, prelude::*};
use zsql_core::{
    Connection, ConnectionUrl, CoreError, QueryEvent, RelationSchema, ResultSet, RowCount,
    SchemaTree,
};

use crate::config::Config;
use crate::drivers;

/// Anything with the same drop-driven lifecycle contract as an open SSH
/// tunnel: dropping it must tear the tunnel down, and it exposes the local
/// loopback address a driver should dial instead of the real remote host.
/// Kept as a trait object (rather than naming [`zsql_ssh::SshTunnel`]
/// directly in [`Session`]) so tests can substitute a lightweight fake
/// without opening a real SSH session.
pub(crate) trait TunnelHandle: Send + Sync {
    /// The loopback address a driver should dial to reach this tunnel's
    /// remote endpoint.
    fn local_addr(&self) -> SocketAddr;
}

impl TunnelHandle for zsql_ssh::SshTunnel {
    fn local_addr(&self) -> SocketAddr {
        zsql_ssh::SshTunnel::local_addr(self)
    }
}

/// What the session (and the results grid it drives) currently displays.
#[derive(Debug, Clone)]
pub enum SessionState {
    /// No URL is configured (`DATABASE_URL` unset and no
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
    /// The most recent query has hit its accumulated row limit but is still
    /// running
    Truncating { rows: u64 },
    /// The most recent query completed successfully but was truncated at
    /// the configured row limit
    Truncated { elapsed: Duration, rows: u64 },
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
    /// Resolved URL (`Config::resolve_url`), if any.
    url: Option<String>,
    /// The live connection, once `connect` succeeds
    connection: Option<Arc<dyn Connection>>,
    /// The active connection's SSH tunnel, if it was opened through one.
    /// `None` for a direct connection, or before any connect attempt.
    /// Replaced (dropping whatever was there before) at the same point
    /// `connection` is replaced: synchronously when a new attempt is
    /// dispatched (see [`Session::connect_url`]), and again once that
    /// attempt resolves.
    tunnel: Option<Box<dyn TunnelHandle>>,
    /// The current lifecycle state a view renders.
    state: SessionState,
    /// The schema sidebar's current state
    schema: SchemaState,
    /// Bumped every time `schema` is reassigned (see [`Session::set_schema`]).
    schema_generation: u64,
    /// Row limit applied to [`Session::preview_relation`]'s generated query.
    preview_limit: u64,
    /// Rows a connection's query stream batches at a time, from
    /// [`Config::query`]'s `batch_size`, threaded into
    /// [`open_tunnel_and_connect`] on every connect attempt.
    batch_size: usize,
    /// Upper bound on rows accumulated for the query currently streaming,
    /// from [`Config::query`]. Reaching this many rows cancels the query and
    /// moves `state` to [`SessionState::Limited`]; see [`Session::apply_query_event`].
    max_result_rows: u64,
    /// Cancellation handle for whichever query is currently streaming.
    active_query: Option<zsql_core::QueryHandle>,
    /// Columns/rows accumulated so far for the query currently streaming
    accumulating: ResultSet,
    /// The most recently previewed relation's total row count, once its
    /// background fetch (started by [`Session::preview_relation`]) has
    /// completed. Cleared at the start of every [`Session::run_query`] call
    /// (preview or not), and populated only by `preview_relation`'s own
    /// fetch -- a query typed into the editor never touches this.
    row_count: Option<RowCount>,
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
        Self {
            url: None,
            connection: None,
            tunnel: None,
            state: SessionState::Empty,
            schema: SchemaState::NotLoaded,
            schema_generation: 0,
            preview_limit: cfg.query.preview_limit,
            batch_size: cfg.query.batch_size,
            max_result_rows: cfg.query.max_result_rows,
            active_query: None,
            accumulating: ResultSet::default(),
            row_count: None,
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

    /// The click-to-preview SQL for `schema.relation`, in the active
    /// connection's dialect and capped at the configured preview row limit
    /// (from [`Config::query`]'s `preview_limit`). When there is no active
    /// connection (before the first successful connect, or after a connection
    /// switch fails), falls back to [`zsql_core::default_preview_query`], so
    /// a caller that wants to show (but not yet run) a preview never has to
    /// special-case the disconnected state.
    ///
    /// This is the single source of the preview query's text: both
    /// [`Session::preview_relation`] (what actually executes) and the
    /// generated tab's displayed buffer are built from it, so the two can
    /// never diverge.
    #[must_use]
    pub fn preview_sql(&self, schema: &str, relation: &str) -> String {
        self.connection.as_ref().map_or_else(
            || zsql_core::default_preview_query(schema, relation, self.preview_limit),
            |connection| connection.preview_query(schema, relation, self.preview_limit),
        )
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

    /// The most recently previewed relation's total row count, if its
    /// background fetch has completed. `None` before it completes, if it
    /// failed, or if the current result came from [`Session::run_query`]
    /// rather than [`Session::preview_relation`].
    #[must_use]
    pub fn row_count(&self) -> Option<RowCount> {
        self.row_count
    }

    /// Connect using the resolved URL (`DATABASE_URL`, or else
    /// `Config::connection.default_url`) as a fallback/seed when no saved
    /// connection has been explicitly chosen. If none is configured, sets
    /// [`SessionState::Empty`] and returns a completed task
    pub fn connect(&mut self, cx: &mut Context<Self>) -> Task<()> {
        let Some(url) = self.url.clone() else {
            self.state = SessionState::Empty;
            cx.notify();
            return Task::ready(());
        };
        self.connect_url(url, None, cx)
    }

    /// Connect to `url` through an SSH tunnel described by `ssh`, replacing
    /// whatever connection is currently active. `ssh` is `None` for a direct,
    /// tunnel-less connection, or when the chosen connection has no tunnel
    /// configured (or has one but it is disabled).
    ///
    /// When `ssh` is `Some`, [`zsql_ssh::open_tunnel`] is awaited and must
    /// succeed before the driver's own connect is ever attempted; a tunnel
    /// failure surfaces as [`SessionState::Error`] the same way a driver
    /// connect failure does, and no driver connect is attempted at all.
    pub fn connect_to_with_ssh(
        &mut self,
        url: impl Into<String>,
        ssh: Option<zsql_ssh::SshConfig>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        self.connect_url(url.into(), ssh, cx)
    }

    /// Shared implementation behind [`Session::connect`] and
    /// [`Session::connect_to_with_ssh`]: connect to `url` (through `ssh`'s
    /// tunnel first, if given) via [`drivers::connect`]/[`drivers::connect_tunneled`],
    /// replacing the current connection and tunnel and (re)starting the
    /// liveliness probe loop on success.
    fn connect_url(
        &mut self,
        url: String,
        ssh: Option<zsql_ssh::SshConfig>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        self.state = SessionState::Connecting;
        self.liveness = LivenessState::Unknown;
        // Every connect attempt targets a different (or not-yet-known)
        // database, so whatever schema tree belongs to the connection this
        // attempt is replacing must stop being shown as current immediately,
        // not only once (or if) the attempt succeeds.
        self.set_schema(SchemaState::NotLoaded);
        // The prior tunnel (if any) is torn down as part of this same
        // synchronous reset, not deferred until this attempt resolves: a
        // switch that never completes (or is itself superseded before it
        // does) must not leave the previous tunnel's listener/session
        // lingering any longer than the schema/tabs reset it rides alongside.
        self.tunnel = None;
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
        let batch_size = self.batch_size;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(open_tunnel_and_connect(url, ssh, batch_size))
                .await;

            let connected = this.update(cx, |session, cx| {
                apply_connect_outcome(session, generation, outcome, cx)
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
        self.row_count = None;
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
                            | SessionState::Truncated { .. }
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

    /// Preview a relation's rows via [`Session::preview_sql`] (capped at the
    /// configured preview limit, in the active connection's dialect), and
    /// separately fetch the relation's total row count via
    /// [`Connection::count_rows`].
    ///
    /// The count fetch runs as its own task, started here alongside (not
    /// sequenced before) the streaming preview `run_query` kicks off, so a
    /// slow count can never delay the first streamed rows painting. This is
    /// the only path that ever calls `count_rows`: SQL typed into the editor
    /// and run via a plain [`Session::run_query`] call never triggers a
    /// count fetch. A count failure is logged and leaves
    /// [`Session::row_count`] at `None`; it never moves `state` to
    /// [`SessionState::Error`] or otherwise disturbs the already-streaming
    /// preview.
    pub fn preview_relation(
        &mut self,
        schema: &str,
        relation: &str,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let sql = self.preview_sql(schema, relation);
        let preview_task = self.run_query(sql, cx);

        if let Some(connection) = self.connection.clone() {
            let generation = self.query_generation;
            let schema = schema.to_owned();
            let relation = relation.to_owned();
            cx.spawn(async move |this, cx| {
                let outcome = cx
                    .background_spawn(count_relation_rows(connection, schema, relation))
                    .await;

                let _ = this.update(cx, |session, cx| {
                    if session.query_generation != generation {
                        // A newer query has superseded this preview; a
                        // late-arriving count no longer belongs to what is
                        // currently displayed.
                        return;
                    }
                    match outcome {
                        Ok(row_count) => {
                            session.row_count = Some(row_count);
                            cx.notify();
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "session row count fetch failed");
                        }
                    }
                });
            })
            .detach();
        }

        preview_task
    }

    /// Fetch `schema.relation`'s full structural detail (columns, indexes,
    /// constraints) via the active connection's
    /// [`Connection::describe_relation`], as its own background task.
    ///
    /// Independent of [`Session::run_query`]/[`Session::preview_relation`]'s
    /// query-lifecycle state (`state`, `accumulating`, `row_count`): a
    /// describe never touches any of it, so any number of describes (e.g.
    /// for several open schema tabs) can be in flight at once, concurrently
    /// with each other and with a running query, without interfering with
    /// one another.
    ///
    /// # Errors
    /// The returned task resolves to an error if there is no active
    /// connection, or to whatever [`Connection::describe_relation`] itself
    /// returns.
    pub fn describe_relation(
        &self,
        schema: &str,
        relation: &str,
        cx: &Context<Self>,
    ) -> Task<Result<RelationSchema, CoreError>> {
        let Some(connection) = self.connection.clone() else {
            return Task::ready(Err(CoreError::connection(
                "cannot describe relation: not connected".to_owned(),
                false,
            )));
        };
        let schema = schema.to_owned();
        let relation = relation.to_owned();
        cx.background_spawn(describe_relation_via(connection, schema, relation))
    }

    /// Fetch `schema.relation`'s total row count via the active connection's
    /// [`Connection::count_rows`], as its own background task, independent
    /// of [`Session::preview_relation`]'s own count fetch.
    ///
    /// # Errors
    /// The returned task resolves to an error if there is no active
    /// connection, or to whatever [`Connection::count_rows`] itself returns.
    pub fn relation_row_count(
        &self,
        schema: &str,
        relation: &str,
        cx: &Context<Self>,
    ) -> Task<Result<RowCount, CoreError>> {
        let Some(connection) = self.connection.clone() else {
            return Task::ready(Err(CoreError::connection(
                "cannot fetch row count: not connected".to_owned(),
                false,
            )));
        };
        let schema = schema.to_owned();
        let relation = relation.to_owned();
        cx.background_spawn(count_relation_rows(connection, schema, relation))
    }

    /// Fold one `QueryEvent` (or a terminal error) into `state`/`accumulating`.
    ///
    /// Once `state` has moved to [`SessionState::Limited`] for the query
    /// currently streaming, every further event is a no-op: the flume
    /// channel can still hold `Batch`/`Columns`/`Done` events queued before
    /// the cancel took effect, and those must not resurrect rows or state
    /// the truncation already settled.
    fn apply_query_event(&mut self, event: Result<QueryEvent, CoreError>) {
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
                self.state = match self.state {
                    SessionState::Truncating { rows } => SessionState::Truncated { elapsed, rows },
                    _ => SessionState::Results(elapsed),
                };
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
    /// The moment the cap is reached, the state moves to [`SessionState::Truncating`]
    /// to signal the UI to show a truncation warning, and the active query is
    /// still running.
    fn append_batch_capped(&mut self, batch: zsql_core::RowBatch) {
        if let SessionState::Truncating { rows } = self.state {
            self.state = SessionState::Truncating {
                rows: rows + batch.rows.len() as u64,
            };
            return;
        }

        let limit = self.max_result_rows;
        let accumulated = row_count(&self.accumulating);
        let remaining = limit.saturating_sub(accumulated);
        let take = usize::try_from(remaining).unwrap_or(usize::MAX);
        let total_accumulated = accumulated.saturating_add(batch.rows.len() as u64);
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
        self.state = SessionState::Truncating {
            rows: total_accumulated,
        };
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

/// Applies a background connect attempt's outcome once back on the main
/// thread, returning whether the session ended up connected.
///
/// If `generation` no longer matches `session.connection_generation`, a
/// newer attempt has already superseded this one: installing its connection
/// and tunnel would leave a live tunnel held as current with no matching
/// probe loop, and clearing state on its failure would wipe the newer
/// attempt's state instead. The stale attempt's own tunnel is dropped as it
/// falls out of scope, but a stale `Ok` connection is explicitly closed
/// rather than left to a non-deterministic `Drop` -- the same guarantee a
/// non-stale replace gets from [`close_outgoing_connection`].
///
/// Otherwise this attempt is current: whatever connection it replaces (a
/// prior successful connect, or `None`) is closed via
/// [`close_outgoing_connection`] regardless of whether this attempt itself
/// succeeded or failed.
fn apply_connect_outcome(
    session: &mut Session,
    generation: u64,
    outcome: Result<TunneledConnectOutcome, CoreError>,
    cx: &mut Context<Session>,
) -> bool {
    if session.connection_generation != generation {
        tracing::debug!("discarding a superseded connect attempt's result");
        if let Ok((conn, _tunnel)) = outcome {
            close_outgoing_connection(Some(Arc::from(conn)), cx);
        }
        return false;
    }
    // Whatever connection this attempt is about to replace (a prior
    // successful connect, or `None`) is taken out here so its teardown can
    // be dispatched below regardless of which branch this attempt lands in.
    let outgoing = session.connection.take();
    let connected = match outcome {
        Ok((conn, tunnel)) => {
            tracing::info!("session connected");
            session.connection = Some(Arc::from(conn));
            session.tunnel = tunnel;
            session.state = SessionState::Connected;
            true
        }
        Err(err) => {
            tracing::warn!(error = %err, "session connect failed");
            // Drop any previously-active connection: the generation bump
            // already invalidated its probe loop, and leaving it in
            // `self.connection` would let `run_query` silently execute
            // against the database this failed switch was meant to
            // replace. Any tunnel this attempt itself opened was already
            // torn down inside `open_tunnel_and_connect` before this error
            // ever reached here.
            session.tunnel = None;
            session.state = SessionState::Error(err.to_string());
            false
        }
    };
    close_outgoing_connection(outgoing, cx);
    cx.notify();
    connected
}

/// Closes `connection` (if any) on a detached background task, so its
/// teardown never delays the state update replacing it.
fn close_outgoing_connection(connection: Option<Arc<dyn Connection>>, cx: &mut Context<Session>) {
    let Some(connection) = connection else {
        return;
    };
    cx.background_spawn(async move {
        connection.close().await;
    })
    .detach();
}

/// A successful connect's outcome: the live connection, and the tunnel it
/// was opened through, if any.
pub(crate) type TunneledConnectOutcome = (Box<dyn Connection>, Option<Box<dyn TunnelHandle>>);

/// Opens `ssh`'s tunnel (if given) before connecting to `url`, so a bad SSH
/// config surfaces as a connect failure before the driver is ever touched.
/// With no `ssh` config, this is exactly [`drivers::connect`]. `batch_size`
/// (typically [`Config::query`]'s `batch_size`) is threaded onto the
/// resulting connection.
///
/// Shared with `ui::connections`'s own Test and unsaved-Connect paths, which
/// need the identical tunnel-before-connect ordering outside of a
/// [`Session`].
#[tracing::instrument(name = "session_open_tunnel_before_connect", skip_all)]
pub(crate) async fn open_tunnel_and_connect(
    url: String,
    ssh: Option<zsql_ssh::SshConfig>,
    batch_size: usize,
) -> Result<TunneledConnectOutcome, CoreError> {
    let Some(ssh_cfg) = ssh else {
        let conn = drivers::connect(url, batch_size).await?;
        return Ok((conn, None));
    };

    let (remote_host, remote_port) = remote_target(&url)?;
    tracing::info!("opening ssh tunnel before connect");
    let tunnel = zsql_ssh::open_tunnel(ssh_cfg, remote_host, remote_port)
        .await
        .map_err(|err| CoreError::connection(err.to_string(), false))?;

    let (conn, tunnel) = connect_through_open_tunnel(url, Box::new(tunnel), batch_size).await?;
    Ok((conn, Some(tunnel)))
}

/// Connects to `url` through `tunnel`'s already-open local address, with the
/// resulting connection's row-batching set to `batch_size`. On failure,
/// `tunnel` is dropped as part of this same attempt (it is not returned in
/// the `Err` case), so a driver connect failure after a successfully opened
/// tunnel never leaves that tunnel outliving the failed attempt.
async fn connect_through_open_tunnel(
    url: String,
    tunnel: Box<dyn TunnelHandle>,
    batch_size: usize,
) -> Result<(Box<dyn Connection>, Box<dyn TunnelHandle>), CoreError> {
    let tunnel_addr = tunnel.local_addr();
    let conn = drivers::connect_tunneled(url, tunnel_addr, batch_size).await?;
    Ok((conn, tunnel))
}

/// The real remote host and port an SSH tunnel for `url` should forward to:
/// `url`'s own host and (explicit or driver-default) port.
///
/// # Errors
/// Returns [`CoreError::Url`] if `url` cannot be parsed or has no host (a
/// sqlite URL never reaches this: SSH tunneling only applies to network
/// connections).
fn remote_target(url: &str) -> Result<(String, u16), CoreError> {
    let parsed = ConnectionUrl::parse(url)?;
    let host = parsed.host().ok_or_else(|| {
        CoreError::Url("an SSH tunnel requires a network URL with a host".to_owned())
    })?;
    let port = match parsed.port() {
        Some(port) => port,
        None => drivers::detect_driver_default_port(url)?.ok_or_else(|| {
            CoreError::Url("an SSH tunnel requires an explicit port for this URL".to_owned())
        })?,
    };
    Ok((host, port))
}

/// Introspect `connection`'s reachable schema.
#[tracing::instrument(name = "session_introspect", skip_all)]
async fn introspect_connection(connection: Arc<dyn Connection>) -> Result<SchemaTree, CoreError> {
    connection.introspect().await
}

/// Fetch `schema.relation`'s total row count via `connection`.
#[tracing::instrument(name = "session_count_rows", skip(connection))]
async fn count_relation_rows(
    connection: Arc<dyn Connection>,
    schema: String,
    relation: String,
) -> Result<RowCount, CoreError> {
    connection.count_rows(&schema, &relation).await
}

/// Describe `schema.relation`'s full structure via `connection`.
#[tracing::instrument(name = "session_describe_relation", skip(connection))]
async fn describe_relation_via(
    connection: Arc<dyn Connection>,
    schema: String,
    relation: String,
) -> Result<RelationSchema, CoreError> {
    connection.describe_relation(&schema, &relation).await
}

/// Ping `connection`, failing the probe if it does not complete within
/// `timeout`. `timeout` races against `connection.ping()` on `executor`'s
/// clock (real wall time in the running app, the deterministic test clock
/// under `TestAppContext`) rather than a runtime timeout helper, since no
/// tokio runtime is available here.
#[tracing::instrument(name = "session_liveness_probe", skip_all)]
pub(crate) async fn probe_connection(
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
mod tests;
