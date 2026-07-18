//! The pluggable driver contract

use async_trait::async_trait;

use crate::config::ConnConfig;
use crate::error::CoreError;
use crate::schema::SchemaTree;
use crate::value::{ColumnMeta, RowBatch};

/// An incremental event emitted while a query streams to the UI.
#[derive(Debug, Clone)]
pub enum QueryEvent {
    /// Column metadata, emitted once before any rows.
    Columns(Vec<ColumnMeta>),
    /// A batch of rows.
    Batch(RowBatch),
    /// Terminal event; carries rows-affected for non-SELECT statements.
    Done {
        /// Rows affected, if applicable.
        affected: Option<u64>,
    },
}

/// The channel a driver pushes [`QueryEvent`]s (or an error) into.
pub type BatchSink = flume::Sender<Result<QueryEvent, CoreError>>;

/// Handle to an in-flight query. Dropping it signals cooperative cancellation;
/// server-side cancellation (e.g. `pg_cancel_backend`) is driver-specific.
#[derive(Debug, Clone)]
pub struct QueryHandle {
    cancel: flume::Sender<()>,
}

impl QueryHandle {
    /// Create a handle wrapping a cancellation sender.
    #[must_use]
    pub fn new(cancel: flume::Sender<()>) -> Self {
        Self { cancel }
    }

    /// Request cancellation of the running query. Best-effort.
    pub fn cancel(&self) {
        let _ = self.cancel.send(());
    }
}

/// A pluggable database backend.
#[async_trait]
pub trait Driver: Send + Sync {
    /// Stable backend id, e.g. `"postgres"`.
    fn id(&self) -> &'static str;

    /// Human-readable backend name, e.g. `"PostgreSQL"`.
    fn display_name(&self) -> &'static str;

    /// Parse a DSN into a [`ConnConfig`] for this backend.
    ///
    /// # Errors
    /// Returns an error if the DSN is malformed for this backend.
    fn parse_dsn(&self, dsn: &str) -> Result<ConnConfig, CoreError>;

    /// Establish a connection.
    ///
    /// # Errors
    /// Returns an error if the connection cannot be established.
    async fn connect(&self, cfg: &ConnConfig) -> Result<Box<dyn Connection>, CoreError>;
}

/// A live connection to a database.
#[async_trait]
pub trait Connection: Send + Sync {
    /// Stream a query's results into `sink`, returning a handle for
    /// cancellation. `sql` may contain more than one statement (e.g.
    /// separated by `;`); implementations that support this concatenate
    /// each statement's rows into the same stream, reporting a single
    /// `Columns` taken from whichever statement's rows arrive first.
    fn stream_query(&self, sql: String, sink: BatchSink) -> QueryHandle;

    /// Snapshot the reachable schema.
    ///
    /// # Errors
    /// Returns an error if introspection fails.
    async fn introspect(&self) -> Result<SchemaTree, CoreError>;

    /// Cheaply verify the connection is still alive (e.g. a trivial `SELECT
    /// 1`-style round trip). Intended to be called on a bounded interval by
    /// a liveness probe; implementations should use a connection/pool
    /// distinct from the one [`Connection::stream_query`] draws from so a
    /// slow query in flight cannot block or be blocked by a probe.
    ///
    /// # Errors
    /// Returns an error if the connection is unreachable or the probe query
    /// fails.
    async fn ping(&self) -> Result<(), CoreError>;
}
