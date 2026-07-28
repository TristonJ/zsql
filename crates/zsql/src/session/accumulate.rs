//! Folds a running query's [`zsql_core::QueryEvent`] stream into
//! [`super::Session`]'s exposed lifecycle state, delegating the actual
//! row-cap and multi-statement fold to [`zsql_core::ResultAccumulator`].

use zsql_core::{AccumulateStatus, CoreError, QueryEvent};

use super::Session;
use super::state::SessionState;

impl Session {
    /// Fold one `QueryEvent` (or a terminal error) into `state`/the
    /// accumulated result.
    ///
    /// Once `state` has moved to a terminal variant for the query currently
    /// streaming, `active_query` is cleared; the flume channel can still
    /// hold `Batch`/`Columns`/`Done` events queued before a cancel took
    /// effect, but the consumer loop in [`Session::run_query`] stops
    /// forwarding them once it observes a terminal state.
    pub(super) fn apply_query_event(&mut self, event: Result<QueryEvent, CoreError>) {
        let status = self.accumulator.apply(event).clone();
        let reached_terminal_state = matches!(
            status,
            AccumulateStatus::Done { .. }
                | AccumulateStatus::Truncated { .. }
                | AccumulateStatus::Error(_)
        );
        self.state = match status {
            AccumulateStatus::Running => SessionState::Running,
            AccumulateStatus::Truncating { rows } => SessionState::Truncating { rows },
            AccumulateStatus::Done { elapsed } => SessionState::Results(elapsed),
            AccumulateStatus::Truncated { elapsed, rows } => {
                SessionState::Truncated { elapsed, rows }
            }
            AccumulateStatus::Error(message) => SessionState::Error(message),
        };
        if reached_terminal_state {
            self.active_query = None;
        }
    }
}
