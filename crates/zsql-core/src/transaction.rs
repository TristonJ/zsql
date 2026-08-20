//! An explicit database transaction over a driver [`Connection`].

use std::sync::Arc;

use crate::driver::{Connection, QueryEvent};
use crate::error::CoreError;

/// An open transaction on a connection: statements executed through it are
/// committed or rolled back together. Finish it with
/// [`Transaction::commit`] or [`Transaction::rollback`]; dropping it without
/// either leaves the transaction open on the connection.
pub struct Transaction {
    connection: Arc<dyn Connection>,
}

impl Transaction {
    /// Open a transaction on `connection` by issuing `BEGIN`.
    ///
    /// # Errors
    /// Whatever the database reports for the `BEGIN` itself.
    pub async fn begin(connection: Arc<dyn Connection>) -> Result<Self, CoreError> {
        run_statement(&connection, "BEGIN".to_owned()).await?;
        Ok(Self { connection })
    }

    /// Run one statement inside this transaction, resolving once it
    /// completes.
    ///
    /// # Errors
    /// Whatever the database reports for the statement. The transaction is
    /// left open either way; the caller decides whether to roll back.
    pub async fn execute(&self, sql: &str) -> Result<(), CoreError> {
        run_statement(&self.connection, sql.to_owned()).await
    }

    /// Commit everything executed in this transaction.
    ///
    /// # Errors
    /// Whatever the database reports for the `COMMIT` itself.
    pub async fn commit(self) -> Result<(), CoreError> {
        run_statement(&self.connection, "COMMIT".to_owned()).await
    }

    /// Discard everything executed in this transaction.
    ///
    /// # Errors
    /// Whatever the database reports for the `ROLLBACK` itself.
    pub async fn rollback(self) -> Result<(), CoreError> {
        run_statement(&self.connection, "ROLLBACK".to_owned()).await
    }
}

/// Run one statement to completion on `connection`, resolving once its
/// terminal event arrives (or the stream closes without one).
async fn run_statement(connection: &Arc<dyn Connection>, sql: String) -> Result<(), CoreError> {
    let (tx, rx) = flume::unbounded();
    let _handle = connection.stream_query(sql, tx);
    while let Ok(event) = rx.recv_async().await {
        match event {
            Ok(QueryEvent::Done { .. }) => return Ok(()),
            Ok(_) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}
