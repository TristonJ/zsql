//! `SQLite` backend for zsql, built on sqlx with the **smol** runtime

mod describe;
mod driver;
mod error;
mod introspect;
mod values;

pub use driver::{SqliteConnectionImpl, SqliteDriver};
