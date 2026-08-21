//! Test-only synchronization for tests that open a genuine database
//! connection (sqlite or postgres).
//!
//! sqlx's `runtime-smol` feature and `gpui`'s `TestDispatcher` (via
//! `cx.executor().allow_parking()`) both ultimately drive work through the
//! same process-global `async-io` reactor that `async-global-executor`
//! shares across every test in this binary (see the workspace `Cargo.toml`
//! for why that reactor is shared). `cargo test` runs each `#[test]`/
//! `#[gpui::test]` function on its own OS thread by default, so a full run
//! of this crate's test suite can put a dozen-plus real connects (sqlite in
//! memory, live postgres) in flight on that shared reactor at once.
//!
//! Serializing real connects to one at a time (below) makes that starvation
//! vastly less likely, but does not rule out a genuine lost-wakeup race in
//! gpui 0.2.2's test executor, reachable whenever parking is allowed: on a
//! `Pending` poll, `block_internal` ticks the queues, then registers its
//! unparker, then parks -- while `TestDispatcher::dispatch` pushes a
//! runnable and then calls `unpark_last`, which no-ops if no unparker is
//! registered yet. A background wake (a sqlite worker thread or the
//! `async-io` reactor thread) landing between the empty tick and
//! `set_unparker` leaves the runnable queued and the test thread parked
//! forever. Only one test thread and one background thread are needed, so
//! this fires even under `--test-threads=1`.
//!
//! Two mitigations, both scoped to the guarded section:
//!
//! * [`serialize_real_io_with_kicker`] runs a kicker thread that dispatches
//!   a no-op task onto the test executor every [`KICK_INTERVAL`]. Any
//!   dispatch after the unparker is registered unparks the test thread,
//!   which then re-ticks the queues and finds the lost runnable, so a hit
//!   of the race costs at most one kick interval instead of a hang.
//! * A watchdog remains as a backstop: if the guarded section does not
//!   finish within [`REAL_IO_TIMEOUT`], it prints a diagnostic (directly to
//!   fd 2, bypassing libtest's output capture, which would otherwise
//!   swallow it on abort) and exits the whole process with status 97,
//!   turning any remaining silent hang into a bounded, loud failure.
use std::fs::File;
use std::io::Write as _;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use gpui::BackgroundExecutor;

static REAL_IO_LOCK: Mutex<()> = Mutex::new(());

/// Generous relative to every observed real connect (sub-second even under
/// heavy contention) and far short of the multi-minute hangs this guards
/// against.
const REAL_IO_TIMEOUT: Duration = Duration::from_secs(20);

/// Upper bound on how long a lost-wakeup hit stalls the guarded section
/// before a kick rescues it; small enough to be invisible in a test run.
const KICK_INTERVAL: Duration = Duration::from_millis(100);

/// Held for the lifetime of a test that opens a real database connection.
/// Releases the serialization lock and stops the watchdog/kicker on drop.
pub(crate) struct RealIoGuard {
    _lock: MutexGuard<'static, ()>,
    _cancel: Sender<()>,
}

/// Serializes real database connects across the whole test binary and arms
/// a hang watchdog for the guarded section; see the module docs.
#[must_use]
pub(crate) fn serialize_real_io() -> RealIoGuard {
    arm(None)
}

/// [`serialize_real_io`] for gpui tests that park on real IO: additionally
/// kicks the test executor with periodic no-op dispatches so a lost wakeup
/// is rescued instead of hanging; see the module docs.
#[must_use]
pub(crate) fn serialize_real_io_with_kicker(executor: BackgroundExecutor) -> RealIoGuard {
    arm(Some(executor))
}

fn arm(kick: Option<BackgroundExecutor>) -> RealIoGuard {
    let lock = REAL_IO_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let armed_by = std::thread::current()
        .name()
        .unwrap_or("<unnamed thread>")
        .to_owned();

    let (cancel, cancel_rx) = channel::<()>();
    std::thread::spawn(move || {
        let deadline = Instant::now() + REAL_IO_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // libtest captures eprintln output per thread (inherited by
                // spawned threads) and discards it on process::exit, so
                // write the diagnostic straight to the stderr device.
                let mut stderr: Box<dyn std::io::Write> =
                    match File::options().write(true).open("/dev/stderr") {
                        Ok(f) => Box::new(f),
                        Err(_) => Box::new(std::io::stderr()),
                    };
                let _ = writeln!(
                    stderr,
                    "\ntest_support: the real-IO guarded test on thread `{armed_by}` did not \
                     finish within {REAL_IO_TIMEOUT:?} -- this looks like the gpui \
                     TestDispatcher lost-wakeup race documented in crate::test_support, not a \
                     genuine test failure. Aborting the process so the run fails fast instead \
                     of hanging indefinitely."
                );
                std::process::exit(97);
            }
            match cancel_rx.recv_timeout(remaining.min(KICK_INTERVAL)) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(executor) = &kick {
                        executor.spawn(async {}).detach();
                    }
                }
            }
        }
    });

    RealIoGuard {
        _lock: lock,
        _cancel: cancel,
    }
}
