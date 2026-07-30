//! Postgres backend for zsql, built on sqlx with the **smol** runtime

mod databases;
mod describe;
mod driver;
mod introspect;
mod tunnel;
mod type_resolve;
mod values;

pub use driver::{PgConnection, PostgresDriver};
