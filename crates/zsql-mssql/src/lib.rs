//! MSSQL backend for zsql, built on `tiberius` over `async-net` so it runs
//! on the same smol/async-io reactor as the rest of this workspace.

mod databases;
mod describe;
mod driver;
mod error;
mod introspect;
pub mod params;
mod quoting;
mod url;
mod values;

pub use driver::{MssqlConnection, MssqlDriver};
pub use quoting::bracket_quote_ident;
