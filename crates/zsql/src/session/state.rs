//! The state enums a [`super::Session`] exposes to its views: what the
//! results grid, connection footer, and schema sidebar each currently
//! render, tracked independently of one another.

use std::time::Duration;

use zsql_core::SchemaTree;

/// What the session (and the results grid it drives) currently displays.
#[derive(Debug, Clone)]
pub(crate) enum SessionState {
    /// No URL is configured (`DATABASE_URL` unset and no
    /// `connection.default_url` in the loaded [`Config`](crate::config::Config))
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
pub(crate) enum SchemaState {
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
pub(crate) enum LivenessState {
    /// No probe has completed yet for the current connection (including
    /// while there is no connection at all).
    Unknown,
    /// The most recent probe succeeded.
    Healthy,
    /// The most recent probe failed or timed out. The message is safe to
    /// show directly in the UI.
    Unreachable(String),
}
