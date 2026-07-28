//! Postgres backend for zsql, built on sqlx with the **smol** runtime

mod describe;
mod driver;
mod introspect;
mod tunnel;
mod values;

pub use driver::{PgConnection, PostgresDriver};
