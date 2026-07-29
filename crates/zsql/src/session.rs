//! `Session` owns the app's single active database connection and drives the
//! query lifecycle the results grid renders

use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, Task, prelude::*};
use zsql_core::{
    Connection, CoreError, PreviewQueryArgs, RelationSchema, ResultSet, RowCount, SchemaTree,
};

use crate::config::Config;

mod accumulate;
mod probe;
mod state;
mod tunnel;

use tunnel::TunnelHandle;

pub(crate) use probe::probe_connection;
pub(crate) use state::{LivenessState, SchemaState, SessionState};
pub(crate) use tunnel::open_tunnel_and_connect;

#[cfg(test)]
use tunnel::{connect_through_open_tunnel, remote_target};

#[cfg(test)]
mod tests;

/// Owns the active connection and the current query's lifecycle.
pub struct Session {
    /// Resolved URL (`Config::resolve_url`), if any.
    url: Option<String>,
    /// The live connection, once `connect` succeeds
    connection: Option<Arc<dyn Connection>>,
    /// The URL the active connection was actually opened with, kept
    /// alongside `connection` so [`Session::switch_database`] can derive a
    /// same-server URL for a different database without re-deriving it from
    /// anything else. `None` before any successful connect, and cleared
    /// again on a failed [`Session::connect_url`] attempt (but left
    /// untouched by a failed [`Session::switch_database`] attempt -- see its
    /// own doc comment).
    current_url: Option<String>,
    /// The active connection's current database, if the backend has one
    /// (derived from `current_url`'s path). `None` before any successful
    /// connect, for a sqlite connection (no database concept), or if the
    /// URL carries no path segment.
    current_database: Option<String>,
    /// Every database selectable on the active connection's server, sorted,
    /// populated from [`zsql_core::Connection::list_databases`] immediately
    /// after each successful connect. Empty when the driver reports `None`
    /// (a single-database backend, or one that already exposes databases as
    /// schemas) or before any successful connect.
    available_databases: Vec<String>,
    /// The SSH tunnel config the active connection was opened through, if
    /// any, kept so [`Session::switch_database`] can reopen an equivalent
    /// tunnel for the same server rather than dropping to a direct connect.
    active_ssh: Option<zsql_ssh::SshConfig>,
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
    /// [`tunnel::open_tunnel_and_connect`] on every connect attempt.
    batch_size: usize,
    /// Page-size choices a generated preview's pager cycles through, from
    /// [`Config::query`]'s `preview_page_sizes`.
    preview_page_sizes: Vec<u64>,
    /// Upper bound on rows accumulated for the query currently streaming,
    /// from [`Config::query`]. Reaching this many rows moves `state` to
    /// [`SessionState::Truncating`]; see [`Session::apply_query_event`].
    max_result_rows: u64,
    /// Cancellation handle for whichever query is currently streaming.
    active_query: Option<zsql_core::QueryHandle>,
    /// Columns/rows accumulated so far for the query currently streaming,
    /// folded via [`zsql_core::ResultAccumulator`] and capped at
    /// `max_result_rows`.
    accumulator: zsql_core::ResultAccumulator,
    /// The most recently previewed relation's total row count, once its
    /// background fetch (started by [`Session::preview_relation`]) has
    /// completed. Cleared at the start of every [`Session::run_query`] call
    /// (preview or not), and populated only by `preview_relation`'s own
    /// fetch -- a query typed into the editor never touches this.
    row_count: Option<RowCount>,
    /// Incremented every `run_query` call. Each query's consumer loop
    /// captures the generation it was started with and compares it against
    /// this field before folding an event into `state`/`accumulator`.
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

impl Session {
    /// Build a session for `cfg`'s resolved connection URL
    #[must_use]
    pub fn new(cfg: &Config) -> Self {
        Self {
            url: None,
            connection: None,
            current_url: None,
            current_database: None,
            available_databases: Vec::new(),
            active_ssh: None,
            tunnel: None,
            state: SessionState::Empty,
            schema: SchemaState::NotLoaded,
            schema_generation: 0,
            preview_limit: cfg.query.preview_limit,
            batch_size: cfg.query.batch_size,
            preview_page_sizes: cfg.query.preview_page_sizes.clone(),
            max_result_rows: cfg.query.max_result_rows,
            active_query: None,
            accumulator: zsql_core::ResultAccumulator::new(cfg.query.max_result_rows),
            row_count: None,
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

    /// The active connection's current database, if the backend has one.
    /// `None` before any successful connect, for a backend with no
    /// database concept (e.g. `SQLite`), or if the connected URL carries no
    /// database path segment.
    #[must_use]
    pub fn current_database(&self) -> Option<&str> {
        self.current_database.as_deref()
    }

    /// Every database selectable on the active connection's server, sorted.
    /// Empty when the driver has no switchable-database concept (see
    /// [`zsql_core::Connection::list_databases`]) or before any successful
    /// connect.
    #[must_use]
    pub fn available_databases(&self) -> &[String] {
        &self.available_databases
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
            || {
                zsql_core::default_preview_query(
                    schema,
                    relation,
                    PreviewQueryArgs::from_limit(self.preview_limit),
                )
            },
            |connection| {
                connection.preview_query(
                    schema,
                    relation,
                    PreviewQueryArgs::from_limit(self.preview_limit),
                )
            },
        )
    }

    /// The configured default preview row limit (`Config::query`'s
    /// `preview_limit`), and the page size a fresh generated tab's pager
    /// starts at.
    #[must_use]
    pub fn preview_limit(&self) -> u64 {
        self.preview_limit
    }

    /// The page sizes a generated preview's pager cycles through, from
    /// `Config::query`'s `preview_page_sizes`.
    #[must_use]
    pub fn preview_page_sizes(&self) -> &[u64] {
        &self.preview_page_sizes
    }

    /// [`Session::preview_sql`], windowed by an optional `(column,
    /// direction)` sort and a `LIMIT`/`OFFSET` page, in the active
    /// connection's dialect (or the shared default when there is none). The
    /// sort column must come from [`zsql_core::ColumnMeta::name`], the same
    /// contract [`Session::preview_sql`] holds for `schema`/`relation`.
    ///
    /// This is the single source of a sort/page-driven rerun's SQL text:
    /// both [`Session::preview_relation_windowed`] (what actually executes)
    /// and the generated tab's rewritten buffer are built from it.
    #[must_use]
    pub fn preview_sql_windowed(
        &self,
        schema: &str,
        relation: &str,
        sort: Option<(&str, zsql_core::SortDirection)>,
        limit: u64,
        offset: u64,
    ) -> String {
        let mut args = PreviewQueryArgs::from_limit(limit).offset(offset);
        if let Some((column, direction)) = sort {
            args = args.sort(column, direction);
        }
        let conn_args = args.clone();
        self.connection.as_ref().map_or_else(
            move || zsql_core::default_preview_query(schema, relation, args),
            move |connection| connection.preview_query(schema, relation, conn_args),
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
        self.accumulator.result()
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
    /// tunnel first, if given) via [`crate::drivers::connect`]/[`crate::drivers::connect_tunneled`],
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
        // `Session::spawn_probe_and_apply`), so nothing else ever would.
        self.connection_generation = self.connection_generation.wrapping_add(1);
        self.probe_in_flight = false;
        let generation = self.connection_generation;
        let batch_size = self.batch_size;
        self.active_ssh.clone_from(&ssh);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(connect_and_list_databases(url, ssh, batch_size))
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

    /// Switch the active connection to `database`, on the same server:
    /// derives a new URL from the current connection's own URL (via
    /// [`zsql_core::ConnectionUrl::set_database`], so credentials, host,
    /// port, query parameters, and any SSH tunnel configuration are carried
    /// through unchanged) and performs the same cancel/reconnect/reset
    /// sequence as [`Session::connect_url`]: the in-flight query (if any) is
    /// cancelled, the schema tree is reset to
    /// [`SchemaState::Loading`](crate::session::SchemaState::Loading)
    /// immediately, `connection_generation` is bumped, and a fresh
    /// connection is opened against the new database, followed by
    /// re-introspection on success.
    ///
    /// Unlike `connect_url`, a failed switch leaves this session exactly as
    /// it was before the attempt: the active connection, current database,
    /// and schema tree are all restored rather than cleared, so a bad
    /// target (e.g. no `CONNECT` right on it) never disconnects the
    /// session from a server it was already talking to. The failure is
    /// still surfaced via [`Session::state`], the same way a query error
    /// is -- see [`Session::is_connected`].
    ///
    /// A no-op (an immediately completed task reporting an error) if there
    /// is no active connection to switch from.
    #[tracing::instrument(
        name = "session_switch_database",
        skip_all,
        fields(database = tracing::field::Empty)
    )]
    pub fn switch_database(
        &mut self,
        database: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let database = database.into();
        tracing::Span::current().record("database", database.as_str());
        let Some(current_url) = self.current_url.clone() else {
            self.state = SessionState::Error("cannot switch database: not connected".to_owned());
            cx.notify();
            return Task::ready(());
        };
        let new_url = match url_for_database(&current_url, &database) {
            Ok(url) => url,
            Err(err) => {
                self.state = SessionState::Error(err.to_string());
                cx.notify();
                return Task::ready(());
            }
        };

        // Replacing `active_query` drops its handle, cooperatively
        // cancelling whatever query was streaming for the connection this
        // switch is about to replace, exactly as a fresh `run_query` call
        // would.
        self.active_query = None;

        let previous_schema = self.schema.clone();
        self.set_schema(SchemaState::Loading);
        self.state = SessionState::Connecting;
        self.liveness = LivenessState::Unknown;

        self.connection_generation = self.connection_generation.wrapping_add(1);
        self.probe_in_flight = false;
        let generation = self.connection_generation;
        let batch_size = self.batch_size;
        let ssh = self.active_ssh.clone();
        cx.notify();

        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(connect_and_list_databases(new_url, ssh, batch_size))
                .await;

            let (probe_generation, introspect_task) = this
                .update(cx, |session, cx| {
                    let probe_generation =
                        apply_switch_outcome(session, generation, previous_schema, outcome, cx);
                    let introspect_task =
                        (probe_generation == Some(generation)).then(|| session.introspect(cx));
                    (probe_generation, introspect_task)
                })
                .unwrap_or((None, None));

            if let Some(probe_generation) = probe_generation {
                let _ = this.update(cx, |session, cx| {
                    session.spawn_liveness_probe_loop(probe_generation, cx);
                });
            }
            if let Some(task) = introspect_task {
                task.await;
            }
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
        self.accumulator = zsql_core::ResultAccumulator::new(self.max_result_rows);
        self.row_count = None;
        self.state = SessionState::Running;
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

    /// Re-run a live generated preview after a sort or page change, via
    /// [`Session::preview_sql_windowed`].
    ///
    /// Unlike [`Session::preview_relation`], this never issues its own
    /// `count_rows` fetch: a sort or page step previews the same relation
    /// the initial `preview_relation` call already counted, so that total
    /// is still valid and is restored after [`Session::run_query`] (which
    /// unconditionally clears [`Session::row_count`] for the general case
    /// of an unrelated query replacing it) rather than re-querying
    /// `COUNT(*)` on every pager click.
    pub fn preview_relation_windowed(
        &mut self,
        schema: &str,
        relation: &str,
        sort: Option<(&str, zsql_core::SortDirection)>,
        limit: u64,
        offset: u64,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let sql = self.preview_sql_windowed(schema, relation, sort, limit, offset);
        let row_count = self.row_count;
        let task = self.run_query(sql, cx);
        self.row_count = row_count;
        task
    }

    /// Fetch `schema.relation`'s full structural detail (columns, indexes,
    /// constraints) via the active connection's
    /// [`Connection::describe_relation`], as its own background task.
    ///
    /// Independent of [`Session::run_query`]/[`Session::preview_relation`]'s
    /// query-lifecycle state (`state`, the accumulated result, `row_count`):
    /// a describe never touches any of it, so any number of describes (e.g.
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
    outcome: Result<ConnectAttempt, CoreError>,
    cx: &mut Context<Session>,
) -> bool {
    if session.connection_generation != generation {
        tracing::debug!("discarding a superseded connect attempt's result");
        if let Ok(attempt) = outcome {
            close_outgoing_connection(Some(Arc::from(attempt.connection)), cx);
        }
        return false;
    }
    // Whatever connection this attempt is about to replace (a prior
    // successful connect, or `None`) is taken out here so its teardown can
    // be dispatched below regardless of which branch this attempt lands in.
    let outgoing = session.connection.take();
    let connected = match outcome {
        Ok(attempt) => {
            tracing::info!("session connected");
            session.connection = Some(Arc::from(attempt.connection));
            session.tunnel = attempt.tunnel;
            session.current_url = Some(attempt.url);
            session.current_database = attempt.current_database;
            session.available_databases = attempt.available_databases;
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
            session.current_url = None;
            session.current_database = None;
            session.available_databases = Vec::new();
            session.state = SessionState::Error(err.to_string());
            false
        }
    };
    close_outgoing_connection(outgoing, cx);
    cx.notify();
    connected
}

/// Applies a background database-switch attempt's outcome once back on the
/// main thread, returning the generation a liveness probe loop should be
/// (re)started for, or `None` if this attempt's result was discarded because
/// a newer attempt has already superseded it.
///
/// Unlike [`apply_connect_outcome`], a failed attempt here does not clear
/// the session's connection: `session.connection`, `current_url`, and
/// `current_database` are left exactly as they were before the switch was
/// attempted (this function never touches them on the `Err` branch), and
/// `schema` is restored to `previous_schema` rather than left at the
/// `NotLoaded` [`Session::switch_database`] set synchronously before
/// dispatching the attempt. `connection_generation` is bumped again so a
/// liveness probe loop can resume for the still-active connection under a
/// generation this (now-resolved) attempt no longer owns.
fn apply_switch_outcome(
    session: &mut Session,
    generation: u64,
    previous_schema: SchemaState,
    outcome: Result<ConnectAttempt, CoreError>,
    cx: &mut Context<Session>,
) -> Option<u64> {
    if session.connection_generation != generation {
        tracing::debug!("discarding a superseded database switch's result");
        if let Ok(attempt) = outcome {
            close_outgoing_connection(Some(Arc::from(attempt.connection)), cx);
        }
        return None;
    }
    match outcome {
        Ok(attempt) => {
            tracing::info!("session switched database");
            let outgoing = session.connection.take();
            session.connection = Some(Arc::from(attempt.connection));
            session.tunnel = attempt.tunnel;
            session.current_url = Some(attempt.url);
            session.current_database = attempt.current_database;
            session.available_databases = attempt.available_databases;
            session.state = SessionState::Connected;
            close_outgoing_connection(outgoing, cx);
            cx.notify();
            Some(generation)
        }
        Err(err) => {
            tracing::warn!(error = %err, "session database switch failed; reverting");
            session.set_schema(previous_schema);
            session.state = SessionState::Error(err.to_string());
            // The generation bump in `switch_database` already invalidated
            // whatever probe loop was watching the connection this failed
            // attempt would have replaced; bump again so a fresh loop can
            // resume probing it under a generation this settled attempt no
            // longer owns.
            session.connection_generation = session.connection_generation.wrapping_add(1);
            session.probe_in_flight = false;
            cx.notify();
            Some(session.connection_generation)
        }
    }
}

/// A background connect attempt's successful result: the live connection
/// and its tunnel (if any), the URL it was actually opened with, that URL's
/// database (if the backend has one), and the databases available on its
/// server (see [`zsql_core::Connection::list_databases`]).
struct ConnectAttempt {
    connection: Box<dyn Connection>,
    tunnel: Option<Box<dyn TunnelHandle>>,
    url: String,
    current_database: Option<String>,
    available_databases: Vec<String>,
}

/// Opens `url` (through `ssh`'s tunnel first, if given, via
/// [`open_tunnel_and_connect`]) and, once connected, lists the databases
/// available on its server, bundling both into a [`ConnectAttempt`].
async fn connect_and_list_databases(
    url: String,
    ssh: Option<zsql_ssh::SshConfig>,
    batch_size: usize,
) -> Result<ConnectAttempt, CoreError> {
    let current_database = current_database_from_url(&url);
    let (connection, tunnel) = open_tunnel_and_connect(url.clone(), ssh, batch_size).await?;
    let available_databases = fetch_available_databases(connection.as_ref()).await;
    Ok(ConnectAttempt {
        connection,
        tunnel,
        url,
        current_database,
        available_databases,
    })
}

/// `url`'s database, if it names one: `None` for a sqlite URL (no database
/// concept) or a network URL with an empty path.
fn current_database_from_url(url: &str) -> Option<String> {
    let parsed = zsql_core::ConnectionUrl::parse(url).ok()?;
    let database = parsed.database();
    (!database.is_empty()).then_some(database)
}

/// `url` with its database path segment rewritten to `database` (via
/// [`zsql_core::ConnectionUrl::set_database`], which percent-encodes it),
/// leaving credentials, host, port, and query parameters -- and so, since
/// none of those carry the tunnel's dial target, any SSH tunnel -- untouched.
///
/// # Errors
/// Returns [`CoreError::Url`] if `url` cannot be parsed.
fn url_for_database(url: &str, database: &str) -> Result<String, CoreError> {
    let mut parsed = zsql_core::ConnectionUrl::parse(url)?;
    parsed.set_database(database);
    Ok(parsed.to_url_string())
}

/// The databases selectable on `connection`'s server, or an empty list if
/// the driver reports [`None`] (no switchable-database concept) or the
/// query itself fails -- a listing failure never fails the connect attempt
/// it rides alongside, it only leaves the database switcher without options.
async fn fetch_available_databases(connection: &dyn Connection) -> Vec<String> {
    match connection.list_databases().await {
        Ok(Some(databases)) => databases,
        Ok(None) => Vec::new(),
        Err(err) => {
            tracing::warn!(error = %err, "listing available databases failed");
            Vec::new()
        }
    }
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
