//! Postgres backend for zsql, built on sqlx with the **smol** runtime so its
//! futures await directly on gpui's executor — no tokio runtime, no bridge
//! thread.
//!
//! This crate is the sole place `sqlx` types are visible; every public
//! signature that crosses into `zsql-core` speaks only in that crate's
//! neutral types ([`zsql_core::CoreError`] and friends).

mod driver;
mod error;
mod introspect;
mod values;

pub use driver::{PgConnection, PostgresDriver, spike_select_one};
