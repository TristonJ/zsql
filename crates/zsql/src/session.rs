//! `Session` owns the app's single active database connection and drives the
//! query lifecycle the results grid renders

use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, Task, prelude::*};
use zsql_core::{
    Connection, CoreError, FilterState, PreviewQueryArgs, RelationSchema, ResultSet, RowCount,
    SchemaTree,
};

use crate::config::Config;

mod accumulate;
mod connect;
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

    /// Whether a query is currently running
    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(
            self.state,
            SessionState::Running | SessionState::Truncating { .. }
        )
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
    /// direction)` sort, a `LIMIT`/`OFFSET` page, and `filters`' `WHERE`
    /// conditions, in the active connection's dialect (or the shared default
    /// when there is none). The sort column must come from
    /// [`zsql_core::ColumnMeta::name`], the same contract
    /// [`Session::preview_sql`] holds for `schema`/`relation`.
    ///
    /// This is the single source of a sort/page/filter-driven rerun's SQL
    /// text: both [`Session::preview_relation_windowed`] (what actually
    /// executes) and the generated tab's rewritten buffer are built from it.
    #[must_use]
    pub fn preview_sql_windowed(
        &self,
        schema: &str,
        relation: &str,
        sort: Option<(&str, zsql_core::SortDirection)>,
        limit: u64,
        offset: u64,
        filters: &FilterState,
    ) -> String {
        let mut args = PreviewQueryArgs::from_limit(limit)
            .offset(offset)
            .filters(filters.clone());
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

    /// Abort / cancel the currently running query, if any. A no-op if there is none.
    pub fn cancel_query(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.active_query.take() {
            tracing::debug!("session cancelling active query");
            handle.cancel();
            self.query_generation += 1;
            self.state = SessionState::Error("Query canceled".to_string());
            cx.notify();
        }
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
                    .background_spawn(count_relation_rows(
                        connection,
                        schema,
                        relation,
                        FilterState::new(),
                    ))
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

    /// Re-run a live generated preview after a sort, page, or filter change,
    /// via [`Session::preview_sql_windowed`].
    ///
    /// Unlike [`Session::preview_relation`], this only conditionally issues
    /// its own `count_rows` fetch, via `refetch_count`: a sort or page step
    /// previews the same relation the initial `preview_relation` call (or
    /// the most recent filter change) already counted, so passing `false`
    /// restores that still-valid total after [`Session::run_query`] (which
    /// unconditionally clears [`Session::row_count`] for the general case of
    /// an unrelated query replacing it) rather than re-querying `COUNT(*)`
    /// on every pager click. A filter add/remove/edit/connector-toggle
    /// changes which rows exist at all, so its caller passes `true`,
    /// dispatching a fresh `WHERE`-qualified count fetch the same way
    /// `preview_relation` does for the initial open.
    #[allow(clippy::too_many_arguments)] // mirrors preview_sql_windowed's own window+filter args
    pub fn preview_relation_windowed(
        &mut self,
        schema: &str,
        relation: &str,
        sort: Option<(&str, zsql_core::SortDirection)>,
        limit: u64,
        offset: u64,
        filters: &FilterState,
        refetch_count: bool,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let sql = self.preview_sql_windowed(schema, relation, sort, limit, offset, filters);
        let preserved_row_count = self.row_count;
        let task = self.run_query(sql, cx);

        if refetch_count {
            if let Some(connection) = self.connection.clone() {
                let generation = self.query_generation;
                let schema = schema.to_owned();
                let relation = relation.to_owned();
                let filters = filters.clone();
                cx.spawn(async move |this, cx| {
                    let outcome = cx
                        .background_spawn(count_relation_rows(
                            connection, schema, relation, filters,
                        ))
                        .await;

                    let _ = this.update(cx, |session, cx| {
                        if session.query_generation != generation {
                            return;
                        }
                        match outcome {
                            Ok(row_count) => {
                                session.row_count = Some(row_count);
                                cx.notify();
                            }
                            Err(err) => {
                                tracing::warn!(
                                    error = %err,
                                    "session filtered row count fetch failed"
                                );
                            }
                        }
                    });
                })
                .detach();
            }
        } else {
            self.row_count = preserved_row_count;
        }

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

    /// Fetch `schema.relation`'s total, unfiltered row count via the active
    /// connection's [`Connection::count_rows`], as its own background task,
    /// independent of [`Session::preview_relation`]'s own count fetch. Used
    /// by a `Schema` tab, which carries no filter bar of its own.
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
        cx.background_spawn(count_relation_rows(
            connection,
            schema,
            relation,
            FilterState::new(),
        ))
    }
}

/// Introspect `connection`'s reachable schema.
#[tracing::instrument(name = "session_introspect", skip_all)]
async fn introspect_connection(connection: Arc<dyn Connection>) -> Result<SchemaTree, CoreError> {
    connection.introspect().await
}

/// Fetch `schema.relation`'s total row count via `connection`, restricted to
/// rows matching `filters`.
#[tracing::instrument(
    name = "session_count_rows",
    skip(connection, filters),
    fields(filtered = !filters.is_empty())
)]
async fn count_relation_rows(
    connection: Arc<dyn Connection>,
    schema: String,
    relation: String,
    filters: FilterState,
) -> Result<RowCount, CoreError> {
    connection.count_rows(&schema, &relation, &filters).await
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
