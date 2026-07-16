//! `zsql-core`: UI- and driver-agnostic domain types plus the pluggable driver
//! contract. This crate depends on no UI framework and no database client, so
//! every backend and every view speaks only in these neutral types.

pub mod config;
pub mod driver;
pub mod error;
pub mod schema;
pub mod value;

pub use config::ConnConfig;
pub use driver::{BatchSink, Connection, Driver, QueryEvent, QueryHandle};
pub use error::CoreError;
pub use schema::{Catalog, Relation, RelationKind, SchemaNs, SchemaTree};
pub use value::{ColumnMeta, ResultSet, Row, RowBatch, Value};
