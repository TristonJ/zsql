//! The recurring liveliness probe loop: pings the active connection on a
//! fixed cadence and folds the result into [`super::state::LivenessState`],
//! independent of the query lifecycle a [`super::Session`] otherwise tracks.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::Either;
use gpui::{BackgroundExecutor, Context, prelude::*};
use zsql_core::Connection;

use super::Session;
use super::state::LivenessState;

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
    /// Start the recurring liveliness probe loop for the connection tied to
    /// `generation`, on the gpui executor. The loop's own timer fires on a
    /// fixed cadence of [`Session::probe_interval`](Config::liveness)
    /// regardless of how long any individual probe takes (each probe runs
    /// as its own task, dispatched by [`Session::spawn_probe_and_apply`]);
    /// a tick that lands while a probe is still outstanding is skipped
    /// rather than starting an overlapping one. The loop stops as soon as
    /// `generation` no longer matches [`Session::connection_generation`] (a
    /// fresh `connect` superseded it) or the session itself is dropped.
    pub(super) fn spawn_liveness_probe_loop(&mut self, generation: u64, cx: &mut Context<Self>) {
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
    pub(super) fn spawn_probe_and_apply(
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
