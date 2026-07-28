//! Folds a query's `QueryEvent` stream into a capped `ResultSet`, shared by
//! every front end built on `BatchSink` so the row-cap and multi-statement
//! semantics stay identical across them.

use std::time::{Duration, Instant};

use crate::{CoreError, QueryEvent, ResultSet, RowBatch};

/// A query-event fold's status after the most recently applied event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccumulateStatus {
    /// Still streaming, under the configured row cap.
    Running,
    /// The row cap was reached by the most recently applied batch; `rows` is
    /// the total row count the source has produced for this query so far,
    /// which may exceed what was actually retained.
    Truncating {
        /// Rows the source has produced for this query so far.
        rows: u64,
    },
    /// Completed normally, never reaching the row cap.
    Done {
        /// Time from the accumulator's construction to this `Done` event.
        elapsed: Duration,
    },
    /// Completed after being truncated at the row cap.
    Truncated {
        /// Time from the accumulator's construction to this `Done` event.
        elapsed: Duration,
        /// Total rows the source produced for this query.
        rows: u64,
    },
    /// The query failed; the message is safe to show directly in the UI.
    Error(String),
}

/// Folds one query's `QueryEvent` stream into a `ResultSet`, capping
/// accumulated rows at a configured limit.
pub struct ResultAccumulator {
    max_rows: u64,
    result: ResultSet,
    status: AccumulateStatus,
    started_at: Option<Instant>,
}

impl ResultAccumulator {
    /// Start folding a new query's events, capping accumulated rows at
    /// `max_rows`.
    #[must_use]
    pub fn new(max_rows: u64) -> Self {
        Self {
            max_rows,
            result: ResultSet::default(),
            status: AccumulateStatus::Running,
            started_at: Some(Instant::now()),
        }
    }

    /// Build an accumulator already holding `result`, at rest (as if a fold
    /// had never run against it). Lets a caller seed a known result set
    /// directly -- e.g. a render test, or restoring a previously fetched
    /// result -- without folding a real event stream.
    #[must_use]
    pub fn with_result(max_rows: u64, result: ResultSet) -> Self {
        Self {
            max_rows,
            result,
            status: AccumulateStatus::Running,
            started_at: None,
        }
    }

    /// The result set accumulated so far.
    #[must_use]
    pub fn result(&self) -> &ResultSet {
        &self.result
    }

    /// The fold's status after the most recently applied event.
    #[must_use]
    pub fn status(&self) -> &AccumulateStatus {
        &self.status
    }

    /// Fold one `QueryEvent` (or a terminal error) into the accumulated
    /// result, returning the resulting status.
    ///
    /// Once `status` has moved to [`AccumulateStatus::Truncating`], a
    /// further batch still folds its row count into that variant's running
    /// total but no longer grows the retained rows: a source can still emit
    /// `Batch`/`Done` events queued before a cancellation took effect, and
    /// those must not resurrect rows the truncation already settled.
    pub fn apply(&mut self, event: Result<QueryEvent, CoreError>) -> &AccumulateStatus {
        match event {
            Ok(QueryEvent::Columns(columns)) => {
                // Each Columns event begins a fresh result set. A run of
                // several statements only shows the last one, so discard any
                // rows accumulated for a prior set rather than appending the
                // new set's rows onto mismatched columns.
                self.result = ResultSet {
                    columns,
                    ..ResultSet::default()
                };
                self.status = AccumulateStatus::Running;
            }
            Ok(QueryEvent::Batch(batch)) => self.append_batch_capped(batch),
            Ok(QueryEvent::Done { affected }) => {
                self.result.affected = affected;
                let elapsed = self
                    .started_at
                    .take()
                    .map_or(Duration::ZERO, |started| started.elapsed());
                tracing::info!(
                    rows = self.result.rows.len(),
                    elapsed_ms = elapsed.as_millis(),
                    "session query completed"
                );
                self.status = match self.status {
                    AccumulateStatus::Truncating { rows } => {
                        AccumulateStatus::Truncated { elapsed, rows }
                    }
                    _ => AccumulateStatus::Done { elapsed },
                };
            }
            Err(err) => {
                tracing::warn!(error = %err, "session query failed");
                self.started_at = None;
                self.status = AccumulateStatus::Error(err.to_string());
            }
        }
        &self.status
    }

    /// Append `batch`'s rows, capping at exactly `max_rows`. A batch can
    /// straddle the limit (batches hold up to the driver's own batch size,
    /// which may be far larger than a small configured limit), so only as
    /// many rows as still fit are appended -- never the whole batch followed
    /// by a truncation of the vector, which would momentarily overshoot the
    /// limit.
    fn append_batch_capped(&mut self, batch: RowBatch) {
        if let AccumulateStatus::Truncating { rows } = self.status {
            self.status = AccumulateStatus::Truncating {
                rows: rows + batch.rows.len() as u64,
            };
            return;
        }

        let accumulated = row_count(&self.result);
        let remaining = self.max_rows.saturating_sub(accumulated);
        let take = usize::try_from(remaining).unwrap_or(usize::MAX);
        let total_accumulated = accumulated.saturating_add(batch.rows.len() as u64);
        self.result.rows.extend(batch.rows.into_iter().take(take));
        self.status = AccumulateStatus::Running;

        let accumulated = row_count(&self.result);
        if !row_limit_reached(accumulated, self.max_rows) {
            return;
        }

        let elapsed = self
            .started_at
            .take()
            .map_or(Duration::ZERO, |started| started.elapsed());
        tracing::warn!(
            limit = self.max_rows,
            rows = accumulated,
            elapsed_ms = elapsed.as_millis(),
            "session query result truncated at the configured row limit"
        );
        self.status = AccumulateStatus::Truncating {
            rows: total_accumulated,
        };
    }
}

/// `result.rows.len()` as a `u64`, for comparison against the configured row
/// cap (also `u64`).
fn row_count(result: &ResultSet) -> u64 {
    u64::try_from(result.rows.len()).unwrap_or(u64::MAX)
}

/// Whether an accumulated row count of `accumulated` rows has reached or
/// exceeded `limit`. Pure and cheap (a single comparison) so it can run once
/// per streamed batch without blocking anything else in flight.
#[must_use]
pub fn row_limit_reached(accumulated: u64, limit: u64) -> bool {
    accumulated >= limit
}

#[cfg(test)]
mod tests {
    use super::{AccumulateStatus, ResultAccumulator};
    use crate::{ColumnMeta, QueryEvent, Row, RowBatch, Value};

    fn col(name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.to_owned(),
            type_name: "int8".to_owned(),
            nullable: false,
        }
    }

    fn row_int(n: i64) -> Row {
        Row(vec![Value::Int(n)])
    }

    #[test]
    fn a_columns_event_discards_the_prior_sets_rows_and_columns() {
        let mut acc = ResultAccumulator::new(100);
        acc.apply(Ok(QueryEvent::Columns(vec![col("a")])));
        acc.apply(Ok(QueryEvent::Batch(RowBatch {
            rows: vec![row_int(1), row_int(2)],
        })));
        assert_eq!(acc.result().rows.len(), 2);

        acc.apply(Ok(QueryEvent::Columns(vec![col("b"), col("c")])));

        assert!(
            acc.result().rows.is_empty(),
            "a fresh Columns event must discard the prior set's rows"
        );
        let names: Vec<&str> = acc
            .result()
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["b", "c"],
            "a fresh Columns event must replace the prior set's columns"
        );
    }

    #[test]
    fn a_batch_crossing_the_cap_is_truncated_and_reports_the_full_source_count() {
        let mut acc = ResultAccumulator::new(3);
        acc.apply(Ok(QueryEvent::Columns(vec![col("n")])));

        let status = acc
            .apply(Ok(QueryEvent::Batch(RowBatch {
                rows: (0..5).map(row_int).collect(),
            })))
            .clone();

        assert_eq!(
            acc.result().rows.len(),
            3,
            "retained rows must be capped at exactly max_rows"
        );
        match status {
            AccumulateStatus::Truncating { rows } => assert_eq!(rows, 5),
            other => panic!("expected Truncating, got {other:?}"),
        }

        // A further batch after truncation must not grow the retained rows,
        // but its count still folds into the running source total.
        let status = acc
            .apply(Ok(QueryEvent::Batch(RowBatch {
                rows: vec![row_int(99)],
            })))
            .clone();
        assert_eq!(acc.result().rows.len(), 3);
        match status {
            AccumulateStatus::Truncating { rows } => assert_eq!(rows, 6),
            other => panic!("expected Truncating, got {other:?}"),
        }
    }

    #[test]
    fn done_with_only_affected_is_distinguished_from_done_after_columns_and_batch() {
        let mut dml = ResultAccumulator::new(10);
        let status = dml
            .apply(Ok(QueryEvent::Done { affected: Some(3) }))
            .clone();
        assert!(dml.result().columns.is_empty());
        assert!(dml.result().rows.is_empty());
        assert_eq!(dml.result().affected, Some(3));
        assert!(
            matches!(status, AccumulateStatus::Done { .. }),
            "a Done with no prior Columns must still complete normally"
        );

        let mut select = ResultAccumulator::new(10);
        select.apply(Ok(QueryEvent::Columns(vec![col("id")])));
        select.apply(Ok(QueryEvent::Batch(RowBatch {
            rows: vec![row_int(1)],
        })));
        let status = select
            .apply(Ok(QueryEvent::Done { affected: None }))
            .clone();
        assert_eq!(select.result().rows.len(), 1);
        assert_eq!(select.result().affected, None);
        assert!(matches!(status, AccumulateStatus::Done { .. }));
    }

    #[test]
    fn an_error_event_terminates_the_fold_without_touching_the_accumulated_result() {
        let mut acc = ResultAccumulator::new(10);
        acc.apply(Ok(QueryEvent::Columns(vec![col("id")])));
        acc.apply(Ok(QueryEvent::Batch(RowBatch {
            rows: vec![row_int(1)],
        })));

        let status = acc
            .apply(Err(crate::CoreError::query("syntax error".to_owned())))
            .clone();

        match status {
            AccumulateStatus::Error(message) => assert!(message.contains("syntax error")),
            other => panic!("expected Error, got {other:?}"),
        }
        assert_eq!(
            acc.result().rows.len(),
            1,
            "an error event must leave whatever had already accumulated untouched"
        );
    }
}
