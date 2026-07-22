//! `MySQL` and `MariaDB` backend for zsql, built on sqlx's `MySql` backend
//! with the **smol** runtime. sqlx has no separate `MariaDB` backend;
//! `MariaDB` speaks the `MySQL` wire protocol, so this one driver connects
//! to both.

mod describe;
mod driver;
mod error;
mod introspect;
mod quoting;
mod url;
mod values;

pub use driver::{MySqlConnection, MysqlDriver};
