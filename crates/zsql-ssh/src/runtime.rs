//! The single tokio runtime shared by every SSH tunnel. Isolated to this
//! module so `open_tunnel` never has to reason about runtime lifecycle.

use std::sync::OnceLock;

use tokio::runtime::{Builder, Handle};

/// Name of the dedicated OS thread the shared runtime drives its tasks on.
const RUNTIME_THREAD_NAME: &str = "zsql-ssh-runtime";

static RUNTIME_HANDLE: OnceLock<Handle> = OnceLock::new();

#[cfg(test)]
static INIT_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Returns a handle to the tokio runtime shared by every tunnel, starting it
/// on a dedicated OS thread the first time any tunnel needs it. Later calls
/// reuse the same runtime and thread.
pub(crate) fn handle() -> Handle {
    RUNTIME_HANDLE.get_or_init(start).clone()
}

fn start() -> Handle {
    let span = tracing::info_span!("ssh_runtime_start");
    let _enter = span.enter();

    #[cfg(test)]
    INIT_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(RUNTIME_THREAD_NAME.to_owned())
        .spawn(move || {
            let runtime = Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build the shared zsql-ssh tokio runtime");
            let handle = runtime.handle().clone();
            if ready_tx.send(handle).is_err() {
                return;
            }
            tracing::info!("shared zsql-ssh runtime thread running");
            runtime.block_on(std::future::pending::<()>());
        })
        .expect("failed to spawn the shared zsql-ssh runtime thread");

    ready_rx
        .recv()
        .expect("shared zsql-ssh runtime thread failed to start")
}

#[cfg(test)]
pub(crate) fn init_count() -> usize {
    INIT_COUNT.load(std::sync::atomic::Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::{handle, init_count};

    /// `RUNTIME_HANDLE` is a single process-wide `OnceLock` shared by every
    /// test in this binary, so a fresh call here may or may not be the
    /// first ever -- this test instead proves that once initialization has
    /// happened at least once (forced by the first `handle()` below), no
    /// further call anywhere increments the counter again.
    #[test]
    fn repeated_handle_calls_reuse_the_already_started_runtime() {
        let first = handle();
        let count_after_first_init = init_count();

        let second = handle();
        let third = handle();

        assert_eq!(
            init_count(),
            count_after_first_init,
            "further handle() calls must not start a new runtime"
        );

        // Same handle: spawning on one and observing via another proves
        // they drive the same runtime.
        let (tx, rx) = std::sync::mpsc::channel();
        second.spawn(async move {
            let _ = tx.send(());
        });
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("task spawned on a reused handle should run");
        drop(first);
        drop(third);
    }

    #[test]
    fn handle_is_usable_to_run_async_work() {
        let handle = handle();
        let (tx, rx) = std::sync::mpsc::channel();
        handle.spawn(async move {
            let _ = tx.send(2 + 2);
        });
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("spawned task should complete promptly");
        assert_eq!(result, 4);
    }
}
