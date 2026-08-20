//! `zsql-core`: UI- and driver-agnostic domain types plus the pluggable driver
//! contract

pub mod accumulate;
pub mod config;
pub mod connection_url;
pub mod driver;
pub mod error;
pub mod filter;
pub mod preview_state;
pub mod registry;
pub mod row_count;
pub mod schema;
pub mod schema_detail;
pub mod sql;
pub mod tls_verify;
pub mod transaction;
pub mod value;

pub use accumulate::{AccumulateStatus, ResultAccumulator, row_limit_reached};
pub use config::{ConnConfig, DEFAULT_QUERY_BATCH_SIZE};
pub use connection_url::{
    ConnectionUrl, SslModeSpelling, rewrite_for_tunnel, tunneled_connect_url_capping_verify_full,
};
pub use driver::{BatchSink, Connection, Driver, QueryEvent, QueryHandle};
pub use error::CoreError;
pub use filter::{
    FilterCondition, FilterConditionId, FilterConnector, FilterOperator, FilterState,
    FilterValueRender, MULTILINE_CONDITION_THRESHOLD, classify_filter_value, quote_sql_string,
    render_literal_value, render_where_body,
};
pub use preview_state::PreviewQueryState;
pub use registry::select_driver;
pub use row_count::{ESTIMATE_MARKER, RowCount, group_thousands};
pub use schema::{Catalog, Relation, RelationKind, SchemaNs, SchemaTree};
pub use schema_detail::{
    ColumnDetail, ConstraintInfo, ConstraintKind, DefaultKind, ForeignKeyRef, IndexInfo, KeyBadge,
    KeyCellBadge, RelationSchema, classify_default, key_cell_badge,
};
pub use sql::{
    PreviewQueryArgs, SortDirection, append_clause_line, append_where_clause,
    default_preview_query, quote_ident,
};
pub use tls_verify::TlsVerify;
pub use transaction::Transaction;
pub use value::{ColumnMeta, ResultSet, Row, RowBatch, Value};
