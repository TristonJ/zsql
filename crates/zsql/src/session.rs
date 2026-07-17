//! `Session` owns the app's single active database connection and drives the
//! query lifecycle the results grid renders: resolving a DSN, connecting,
//! streaming a query's `QueryEvent`s, and accumulating them into the
//! `ResultSet` the UI displays.
//!
//! Every fallible or async step funnels through [`SessionState`], so a view
//! rendering a `Session` never has to infer what is happening from the
//! presence or absence of other fields — there is always exactly one current
//! state.
//!
//! All async work follows the crate's no-tokio model: the actual connect and
//! query calls run on `sqlx`'s `runtime-smol` feature, so they are driven
//! from gpui's own executors (`cx.background_spawn` for the connect future,
//! `cx.spawn` to consume the query's `flume` event stream) rather than any
//! tokio runtime.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{Context, Task, prelude::*};
use zsql_core::{ConnConfig, Connection, CoreError, Driver, QueryEvent, ResultSet, SchemaTree};
use zsql_postgres::PostgresDriver;

use crate::config::Config;

/// What the session (and the results grid it drives) currently displays.
///
/// Neither `Running` nor `Results` carries a `ResultSet` by value: the
/// accumulated result set lives exactly once, in [`Session::accumulating`],
/// and a view reads it through [`Session::result`]. Cloning `SessionState`
/// (e.g. to snapshot it for a render pass) is therefore always O(1) — it
/// never copies row data — no matter how many rows a streaming query has
/// accumulated so far.
#[derive(Debug, Clone)]
pub enum SessionState {
    /// No DSN is configured (`DATABASE_URL` unset and no
    /// `connection.default_url` in the loaded [`Config`]): there is nothing
    /// to connect to yet, so the UI shows a prompt instead of an error.
    Empty,
    /// A connection attempt is in flight.
    Connecting,
    /// Connected, idle: the connection succeeded and no query has run yet
    /// (or the most recent one hasn't been dispatched). Distinct from
    /// `Connecting` so a view stops showing "Connecting…" once the
    /// connection is actually up.
    Connected,
    /// Connected; a query is currently streaming results. A view reads the
    /// columns/rows folded in so far via [`Session::result`] and paints them
    /// as they arrive rather than waiting for the terminal `Done` event:
    /// [`Session::accumulating`] starts empty the moment the query is
    /// dispatched, gains its columns on the first `QueryEvent::Columns`,
    /// then grows a batch at a time as `QueryEvent::Batch`es arrive.
    Running,
    /// The most recent query completed successfully. The accumulated result
    /// set is available via [`Session::result`]; this variant carries only
    /// how long the query took end to end.
    Results(Duration),
    /// Connecting or running a query failed. The message is safe to show
    /// directly in the UI: `zsql-postgres`'s error mapping never embeds a
    /// DSN or other secret in it.
    Error(String),
}

/// What the schema sidebar currently has to render, tracked independently of
/// [`SessionState`]: introspection is best-effort metadata about the
/// connection, not a precondition for using it, so a broken or in-flight
/// schema fetch never turns into a fatal [`SessionState::Error`] and never
/// blocks [`Session::run_query`]/[`Session::preview_relation`].
#[derive(Debug, Clone)]
pub enum SchemaState {
    /// No introspection has completed yet (a fresh session, or one that
    /// hasn't connected/introspected).
    NotLoaded,
    /// Introspection is in flight.
    Loading,
    /// Introspection succeeded; the sidebar renders this tree.
    Ready(SchemaTree),
    /// Introspection failed. The message is safe to show directly in the
    /// UI, same as [`SessionState::Error`].
    Error(String),
}

/// Owns the active connection and the current query's lifecycle.
///
/// A gpui `Entity`: methods that touch the connection or run a query take
/// `&mut Context<Self>` so they can hop between gpui's foreground executor
/// (to call `cx.notify()`) and its background executor (for the actual sqlx
/// I/O).
pub struct Session {
    /// Resolved DSN (`Config::resolve_url`), if any.
    dsn: Option<String>,
    /// The live connection, once `connect` succeeds. An `Arc` (rather than a
    /// plain `Box`) so [`Session::introspect`] can clone a handle to it into
    /// a `'static` future for `cx.background_spawn`, the same way
    /// `connect`'s own background future works, without borrowing `self`
    /// across the `.await`.
    connection: Option<Arc<dyn Connection>>,
    /// The current lifecycle state a view renders.
    state: SessionState,
    /// The schema sidebar's current state, tracked independently of `state`
    /// -- see [`SchemaState`]'s doc comment for why.
    schema: SchemaState,
    /// Bumped every time `schema` is reassigned (see [`Session::set_schema`]).
    /// Lets a view such as `ui::sidebar::SidebarView` cheaply tell "the
    /// schema actually changed" apart from "the session merely notified for
    /// an unrelated reason" (e.g. a preview query's `QueryEvent`s streaming
    /// in) without diffing or cloning the `SchemaTree` itself.
    schema_generation: u64,
    /// `LIMIT` applied to [`Session::preview_relation`]'s generated query,
    /// resolved once from `Config` at construction rather than
    /// hardcoded at each call site.
    preview_limit: u64,
    /// Cancellation handle for whichever query is currently streaming.
    /// Holding this alive for the query's duration matters: dropping a
    /// `QueryHandle` is itself a cancellation signal (see
    /// `zsql_core::QueryHandle`), so this must not be dropped between
    /// `run_query` starting the query and it reaching a terminal state.
    active_query: Option<zsql_core::QueryHandle>,
    /// Columns/rows accumulated so far for the query currently streaming;
    /// promoted into `SessionState::Results` once `Done` arrives.
    accumulating: ResultSet,
    /// When the currently-streaming query started, for computing elapsed time.
    query_started_at: Option<Instant>,
    /// Incremented every `run_query` call. Each query's consumer loop
    /// captures the generation it was started with and compares it against
    /// this field before folding an event into `state`/`accumulating`.
    ///
    /// This matters because dropping `active_query`'s old `QueryHandle`
    /// cancels the *driver's* work but not the *consumer* task spawned in
    /// the previous `run_query` call: that task is returned to the caller as
    /// its own `Task<()>`, so `run_query` has no way to stop it from here if
    /// the caller detached it rather than awaiting/dropping it. Without this
    /// check, a `QueryEvent` still in flight on the superseded query's
    /// `flume` channel when a new query starts would be folded into the
    /// *new* query's state, clobbering it with stale data.
    query_generation: u64,
}

impl Session {
    /// Build a session for `cfg`'s resolved connection URL. Does not connect
    /// — call [`Session::connect`] to do that.
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

    /// Monotonically increases every time `schema` is reassigned; unchanged
    /// between calls that don't touch it. A view can cache the value it last
    /// saw and skip re-deriving anything from `schema()` when this is still
    /// the same, without needing to compare `SchemaTree`s for equality.
    #[must_use]
    pub fn schema_generation(&self) -> u64 {
        self.schema_generation
    }

    /// Replace `schema` and bump [`Session::schema_generation`] in the same
    /// step, so the two can never drift out of sync.
    fn set_schema(&mut self, schema: SchemaState) {
        self.schema = schema;
        self.schema_generation = self.schema_generation.wrapping_add(1);
    }

    /// The result set accumulated by the most recently dispatched query.
    /// Grows batch-by-batch while `state()` is `Running`, and holds the
    /// completed result once `state()` is `Results` (it is not moved out or
    /// cloned at that point — this always returns the same underlying
    /// storage). Empty for every other state, including right after a fresh
    /// `Session::new` and immediately after `connect` succeeds.
    #[must_use]
    pub fn result(&self) -> &ResultSet {
        &self.accumulating
    }

    /// Connect using the resolved DSN. If none is configured, sets
    /// [`SessionState::Empty`] and returns a completed task (no fabricated
    /// connection attempt). Otherwise connects on gpui's background executor
    /// and hops back to the foreground to apply the result.
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
    /// [`SessionState`]. If there is no active connection, sets
    /// [`SessionState::Error`] instead of attempting a query with nothing to
    /// run it on.
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
        // cancels it. That stops the *driver* side of the superseded query,
        // but not the previous call's consumer loop below if the caller
        // detached its `Task` rather than dropping it — the `generation`
        // check inside the loop is what keeps a late event from that old
        // consumer out of this query's state.
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
    /// `Connection::introspect`, running on gpui's background executor like
    /// `connect` and `run_query` do (no tokio, no UI-thread block).
    ///
    /// A failure here only updates [`Session::schema`], never
    /// [`Session::state`]: a sidebar that cannot introspect the catalog must
    /// not stop a connection that can still run queries just fine. If there
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
    /// LIMIT <configured preview limit>`, dispatched the same way any other
    /// query runs, via [`Session::run_query`]. Both identifiers are quoted
    /// (see [`crate::sql`]), so a name that needs quoting -- or contains a
    /// double quote -- cannot break out of the identifier position.
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
    /// Pure state reduction with no gpui or I/O dependency, so it is unit
    /// testable without a database or an app context (see the tests below).
    ///
    /// Every arm here is O(1) plus the cost of the event's own payload (e.g.
    /// `extend`ing by one batch's rows) — none of them clone or rescan the
    /// full `accumulating` result set, so folding N events this way is O(N)
    /// total rather than O(N^2). `accumulating` is mutated in place and read
    /// by a view through [`Session::result`]; `state` only ever carries a
    /// cheap marker (or, for `Results`, the elapsed `Duration`), never the
    /// row data itself.
    fn apply_query_event(&mut self, event: Result<QueryEvent, CoreError>) {
        match event {
            Ok(QueryEvent::Columns(columns)) => {
                self.accumulating.columns = columns;
                // A view rendering `state` sees `Running` and reads the
                // freshly-set columns straight off `accumulating` via
                // `Session::result`, so the header/grid can paint as soon as
                // columns are known, before the first row has even arrived —
                // no need to re-publish a snapshot into `state` itself.
                self.state = SessionState::Running;
            }
            Ok(QueryEvent::Batch(batch)) => {
                self.accumulating.rows.extend(batch.rows);
                // Same reasoning as the `Columns` arm above: `state` stays
                // `Running` and a view picks up the newly-extended rows
                // straight from `accumulating`, so rows paint incrementally
                // instead of only once the whole query has finished
                // streaming.
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
                // `accumulating` is left in place (not taken/reset) so
                // `Session::result` keeps returning the completed result set
                // for as long as `state` stays `Results`; the next
                // `run_query` call resets it when a new query starts.
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

/// Test-only constructors used by `ui::results`'s render tests, which need a
/// `Session` entity already parked in a specific `SessionState` with a
/// specific accumulated result set, without driving it through
/// `connect`/`run_query`. `pub(crate)` visibility (rather than plain
/// private) is needed even though this whole block only exists under
/// `#[cfg(test)]`: `crate::ui::results`'s test module is a sibling of this
/// module, not a descendant, so private items here would otherwise be out of
/// its reach.
#[cfg(test)]
impl Session {
    /// Build a session already in `state`, with `result` as its accumulated
    /// result set (readable back via [`Session::result`]). Every other field
    /// starts at its normal "nothing happening" default: no DSN, no live
    /// connection, no in-flight query.
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
    /// batch (or a fresh result) having landed without going through a real
    /// `QueryEvent` stream. Used by tests that need to drive a `ResultsView`
    /// through more than one snapshot of a session's data.
    pub(crate) fn set_result_for_test(&mut self, result: ResultSet) {
        self.accumulating = result;
    }

    /// Build a session already holding `schema` as its introspected schema
    /// state, connected but idle, with no result set -- used by
    /// `ui::sidebar`'s render tests, which need a `Session` entity parked in
    /// a specific `SchemaState` without driving it through a real
    /// `introspect()` call.
    pub(crate) fn new_for_schema_test(schema: SchemaState) -> Self {
        let mut session = Self::new_for_render_test(SessionState::Connected, ResultSet::default());
        session.set_schema(schema);
        session
    }
}

/// Connect to Postgres via [`PostgresDriver`]. A free function (rather than
/// inlined into `Session::connect`'s spawned closure) so it carries its own
/// tracing span across the `.await` inside `cx.background_spawn`.
///
/// # Errors
/// Returns whatever [`PostgresDriver::connect`] returns on failure.
#[tracing::instrument(name = "session_connect_postgres", skip_all)]
async fn connect_postgres(cfg: ConnConfig) -> Result<Box<dyn Connection>, CoreError> {
    PostgresDriver.connect(&cfg).await
}

/// Introspect `connection`'s reachable schema. A free function (rather than
/// inlined into `Session::introspect`'s spawned closure) so it carries its
/// own tracing span across the `.await` inside `cx.background_spawn`, same
/// as [`connect_postgres`] above.
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
        // Guards the streaming refactor: `accumulating` is mutated in place
        // across every event rather than being cloned/replaced wholesale, so
        // this pins that folding three separate batches still produces
        // exactly the concatenation of their rows, in arrival order, with no
        // duplication or loss.
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
        // Pins the streaming behavior `apply_query_event` is responsible
        // for: a view reading `Session::result` while `state()` is `Running`
        // must see the columns and each batch's rows as soon as they are
        // folded in, not only once the query reaches its terminal `Done`
        // event.
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

/// `TestAppContext`-driven `Session` tests that need no live database:
/// `Session`'s public async surface (`connect`/`run_query`, both of which
/// require `&mut Context<Self>` and so can only be exercised through a gpui
/// entity) rather than the pure `apply_query_event` reduction covered above.
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
        // `resolve_url` can itself yield `Some(String::new())` (e.g. an
        // empty `connection.default_url`), which is distinct from `None`:
        // `Session::new` still starts `Connecting` since a DSN *is*
        // configured, and it is only `connect`'s call into
        // `ConnConfig::from_dsn` that discovers it is unusable.
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
        // Real DNS/network I/O for a host on the reserved `.invalid` TLD
        // (RFC 2606: guaranteed never to resolve) takes real async time that
        // gpui's deterministic test dispatcher cannot simulate on its own,
        // same as the live end-to-end tests below — see
        // `BackgroundExecutor::allow_parking`'s own doc comment. This needs
        // no database at all, live or otherwise: it only exercises
        // `connect`'s failure -> `SessionState::Error` mapping.
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

    /// What [`FakeConnection::introspect`] returns. A plain enum (rather
    /// than storing a `Result<SchemaTree, CoreError>` directly) because
    /// `CoreError` has no `Clone` impl and this needs to be returned
    /// (potentially more than once) from a `&self` method.
    enum FakeIntrospectOutcome {
        Ready(SchemaTree),
        Failed(String),
    }

    /// A `Connection` double that records every `stream_query` call's SQL
    /// text and sink instead of running anything, so tests can drive the
    /// resulting `QueryEvent`s by hand, assert on exactly what SQL a
    /// `Session` method generated, and control timing relative to other
    /// calls. Never touches the network -- no `sqlx`/Postgres types appear
    /// here.
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

        // Start the first query and detach its consumer task, the way a
        // caller that does not need the returned `Task<()>`'s completion
        // would (e.g. a future "Run" action). Detaching (rather than
        // holding/dropping it) is what lets the old consumer loop keep
        // running concurrently with the next query below, reproducing the
        // scenario `query_generation` guards against.
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

        // A late `Columns` event from the superseded first query must not
        // land in the second query's accumulating state.
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

        // The second (current) query completing normally must still work.
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

    /// Pins the contract `ui::sidebar::SidebarView` relies on to avoid
    /// re-flattening its schema tree on every session notify: introspection
    /// advances `schema_generation`, but a streaming query's `QueryEvent`s
    /// (which also call `cx.notify()`) must not.
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

    /// Pins the core "sidebar-level, not fatal" contract: an introspection
    /// failure must land in `SchemaState::Error` and leave `SessionState`
    /// completely untouched, and a query dispatched afterwards on the same
    /// connection must still succeed.
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
/// `cargo test` passes with no database present. Unlike the pure reduction
/// tests above, these drive a real `Session` gpui `Entity` through
/// `TestAppContext`, connecting and streaming a query against the seeded dev
/// database (`scripts/pg-dev.sh` + `dev/seed.sql`).
#[cfg(test)]
mod live_tests {
    use gpui::{AppContext as _, TestAppContext};
    use zsql_core::Value;

    use super::{Config, SchemaState, Session, SessionState};

    /// Reads `ZSQL_TEST_DATABASE_URL`, or returns `None` (after printing why)
    /// so callers can skip.
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
        // Real network I/O to Postgres takes real async time that gpui's
        // deterministic test dispatcher cannot simulate on its own; this
        // opts the test into genuinely parking the thread and waiting for
        // it, per `BackgroundExecutor::allow_parking`'s own doc comment
        // ("integrating other (non-GPUI) futures, like disk access, that do
        // take real async time to run").
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

            // `placed_at` is not asserted on for order: every seeded order is
            // inserted in a single `INSERT` statement, so Postgres' `now()`
            // (transaction start time) can give them an identical
            // `placed_at`, making `ORDER BY placed_at DESC`
            // non-deterministic between ties. The unordered set of
            // `total_cents` values is exact and stable regardless.
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
        // Pins the fix for a session that connects but never runs a query:
        // it must land in `SessionState::Connected` (idle, connected), not
        // stay stuck showing `Connecting…` forever.
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

        // Connect must actually succeed here: otherwise the `Error` state
        // asserted on below could come from `run_query`'s own "not
        // connected" guard instead of from the invalid query this test is
        // meant to exercise, and would pass for the wrong reason.
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

    /// End-to-end: connect, introspect against the seeded dev database
    /// (`scripts/pg-dev.sh` + `dev/seed.sql`), and confirm the resulting
    /// `SchemaTree` carries the seeded `public` schema's `orders`/`users`
    /// tables and `recent_orders` view; then preview one of those relations
    /// and confirm it lands in `SessionState::Results` with rows.
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
