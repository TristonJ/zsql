//! `zsql-core`: UI- and driver-agnostic domain types plus the pluggable driver
//! contract

pub mod config;
pub mod driver;
pub mod error;
pub mod registry;
pub mod row_count;
pub mod schema;
pub mod schema_detail;
pub mod sql;
pub mod value;

pub use config::ConnConfig;
pub use driver::{BatchSink, Connection, Driver, QueryEvent, QueryHandle};
pub use error::CoreError;
pub use registry::select_driver;
pub use row_count::{ESTIMATE_MARKER, RowCount};
pub use schema::{Catalog, Relation, RelationKind, SchemaNs, SchemaTree};
pub use schema_detail::{
    ColumnDetail, ConstraintInfo, ConstraintKind, ForeignKeyRef, IndexInfo, KeyBadge,
    RelationSchema,
};
pub use sql::{default_preview_query, quote_ident};
pub use value::{ColumnMeta, ResultSet, Row, RowBatch, Value};
