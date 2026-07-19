//! MSSQL backend for zsql, built on `tiberius` over `async-net` so it runs
//! on the same smol/async-io reactor as the rest of this workspace.

mod describe;
mod driver;
mod dsn;
mod error;
mod introspect;
mod quoting;
mod values;

pub use driver::{MssqlConnection, MssqlDriver};
pub use quoting::bracket_quote_ident;
