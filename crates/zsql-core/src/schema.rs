//! Engine-neutral schema model: catalog -> schema -> relation -> column.

use crate::value::ColumnMeta;

/// A snapshot of the reachable schema.
#[derive(Debug, Clone, Default)]
pub struct SchemaTree {
    /// Top-level catalogs/databases.
    pub catalogs: Vec<Catalog>,
}

/// A catalog (database).
#[derive(Debug, Clone)]
pub struct Catalog {
    /// Catalog name.
    pub name: String,
    /// Namespaces (schemas) within the catalog.
    pub schemas: Vec<SchemaNs>,
}

/// A schema namespace.
#[derive(Debug, Clone)]
pub struct SchemaNs {
    /// Schema name (e.g. `public`).
    pub name: String,
    /// Tables and views in the schema.
    pub tables: Vec<Relation>,
}

/// A table, view, or materialized view.
#[derive(Debug, Clone)]
pub struct Relation {
    /// Relation name.
    pub name: String,
    /// What kind of relation this is.
    pub kind: RelationKind,
    /// Columns of the relation.
    pub columns: Vec<ColumnMeta>,
}

/// Kind of a [`Relation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    /// An ordinary table.
    Table,
    /// A view.
    View,
    /// A materialized view.
    MatView,
    /// A partitioned table (the parent). Its partitions are ordinary tables
    /// in their own right and are enumerated as separate [`Relation`]s.
    Partitioned,
}
