//! Connection lifecycle: opening a fresh connection
//! ([`Session::connect`]/[`Session::connect_to_with_ssh`]), switching the
//! active connection to a different database on the same server
//! ([`Session::switch_database`]), and the background work and outcome
//! handling both ride on. Independent of the query/preview lifecycle
//! [`super::Session`]'s own module otherwise tracks.

use std::sync::Arc;

use gpui::{Context, Task, prelude::*};
use zsql_core::{Connection, CoreError};

use super::Session;
use super::state::{LivenessState, SchemaState, SessionState};
use super::tunnel::{TunnelHandle, open_tunnel_and_connect};

impl Session {
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
    pub(super) fn connect_url(
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
pub(super) fn apply_connect_outcome(
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
pub(super) fn apply_switch_outcome(
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
pub(super) struct ConnectAttempt {
    pub(super) connection: Box<dyn Connection>,
    pub(super) tunnel: Option<Box<dyn TunnelHandle>>,
    pub(super) url: String,
    pub(super) current_database: Option<String>,
    pub(super) available_databases: Vec<String>,
}

/// Opens `url` (through `ssh`'s tunnel first, if given, via
/// [`open_tunnel_and_connect`]) and, once connected, lists the databases
/// available on its server, bundling both into a [`ConnectAttempt`].
pub(super) async fn connect_and_list_databases(
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
pub(super) fn current_database_from_url(url: &str) -> Option<String> {
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
pub(super) fn url_for_database(url: &str, database: &str) -> Result<String, CoreError> {
    let mut parsed = zsql_core::ConnectionUrl::parse(url)?;
    parsed.set_database(database);
    Ok(parsed.to_url_string())
}

/// The databases selectable on `connection`'s server, or an empty list if
/// the driver reports [`None`] (no switchable-database concept) or the
/// query itself fails -- a listing failure never fails the connect attempt
/// it rides alongside, it only leaves the database switcher without options.
pub(super) async fn fetch_available_databases(connection: &dyn Connection) -> Vec<String> {
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
pub(super) fn close_outgoing_connection(
    connection: Option<Arc<dyn Connection>>,
    cx: &mut Context<Session>,
) {
    let Some(connection) = connection else {
        return;
    };
    cx.background_spawn(async move {
        connection.close().await;
    })
    .detach();
}
