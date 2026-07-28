//! `zsql-core`: UI- and driver-agnostic domain types plus the pluggable driver
//! contract

pub mod config;
pub mod connection_url;
pub mod driver;
pub mod error;
pub mod registry;
pub mod row_count;
pub mod schema;
pub mod schema_detail;
pub mod sql;
pub mod tls_verify;
pub mod value;

pub use config::ConnConfig;
pub use connection_url::{ConnectionUrl, rewrite_for_tunnel};
pub use driver::{BatchSink, Connection, Driver, QueryEvent, QueryHandle};
pub use error::CoreError;
pub use registry::select_driver;
pub use row_count::{ESTIMATE_MARKER, RowCount, group_thousands};
pub use schema::{Catalog, Relation, RelationKind, SchemaNs, SchemaTree};
pub use schema_detail::{
    ColumnDetail, ConstraintInfo, ConstraintKind, DefaultKind, ForeignKeyRef, IndexInfo, KeyBadge,
    KeyCellBadge, RelationSchema, classify_default, key_cell_badge,
};
pub use sql::{default_preview_query, quote_ident};
pub use tls_verify::TlsVerify;
pub use value::{ColumnMeta, ResultSet, Row, RowBatch, Value};
