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
//! vastly less likely, but does not fully rule out a genuine lost-wakeup
//! race between a real I/O completion (a sqlite worker thread or the
//! `async-io` reactor thread) and `gpui`'s `TestDispatcher` parking a test
//! thread to wait for it (observed directly: under heavy concurrent load
//! the parked test thread and the I/O thread that should wake it both sit
//! idle on a futex, each waiting on the other). That race lives inside a
//! pinned third-party dependency, not this crate, so [`serialize_real_io`]
//! also arms a watchdog: if the guarded section does not finish within
//! [`REAL_IO_TIMEOUT`], the watchdog prints a diagnostic and aborts the
//! whole test process, turning a silent multi-minute hang into a bounded,
//! loud failure.
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

static REAL_IO_LOCK: Mutex<()> = Mutex::new(());

/// Generous relative to every observed real connect (sub-second even under
/// heavy contention) and far short of the multi-minute hangs this guards
/// against.
const REAL_IO_TIMEOUT: Duration = Duration::from_secs(20);

/// Held for the lifetime of a test that opens a real database connection.
/// Releases the serialization lock and disarms the watchdog on drop.
pub(crate) struct RealIoGuard {
    _lock: MutexGuard<'static, ()>,
    _cancel: Sender<()>,
}

/// Serializes real database connects across the whole test binary and arms
/// a hang watchdog for the guarded section; see the module docs.
#[must_use]
pub(crate) fn serialize_real_io() -> RealIoGuard {
    let lock = REAL_IO_LOCK.lock().unwrap_or_else(PoisonError::into_inner);

    let (cancel, cancel_rx) = channel::<()>();
    std::thread::spawn(move || match cancel_rx.recv_timeout(REAL_IO_TIMEOUT) {
        Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
        Err(RecvTimeoutError::Timeout) => {
            eprintln!(
                "test_support: a real-IO guarded test did not finish within {REAL_IO_TIMEOUT:?} \
                 -- this looks like the gpui TestDispatcher / sqlx real-IO lost-wakeup race \
                 documented in crate::test_support, not a genuine test failure. Aborting the \
                 process so the run fails fast instead of hanging indefinitely."
            );
            std::process::exit(97);
        }
    });

    RealIoGuard {
        _lock: lock,
        _cancel: cancel,
    }
}
