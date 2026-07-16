//! Logging / tracing setup. Called once at startup so every subsystem can
//! instrument from the first line of code.

use tracing_subscriber::EnvFilter;

/// Initialize the global tracing subscriber. Honors `RUST_LOG`; defaults to a
/// sensible dev filter when it is unset.
pub fn init() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,zsql=debug,zsql_postgres=debug"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}
