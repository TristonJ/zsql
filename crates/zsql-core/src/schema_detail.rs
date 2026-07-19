//! A relation's full structural detail: columns with key/default detail,
//! indexes, and constraints -- beyond what [`crate::schema::Relation`]
//! carries.

/// A relation's full structural detail.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelationSchema {
    /// Columns, in ordinal position.
    pub columns: Vec<ColumnDetail>,
    /// Indexes defined on the relation.
    pub indexes: Vec<IndexInfo>,
    /// Constraints defined on the relation.
    pub constraints: Vec<ConstraintInfo>,
}

/// One column's full structural detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDetail {
    /// Column name.
    pub name: String,
    /// Backend type name (e.g. `int8`, `text`).
    pub type_name: String,
    /// Whether the column may be null.
    pub nullable: bool,
    /// The column's default expression, rendered as backend-native SQL text,
    /// or `None` if it has no default.
    pub default: Option<String>,
    /// Whether the column is (part of) the relation's primary key.
    pub is_primary_key: bool,
    /// Whether the column is constrained unique.
    pub is_unique: bool,
    /// The foreign key this column participates in, if any.
    pub foreign_key: Option<ForeignKeyRef>,
}

/// The target a foreign-key column (or column set) references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKeyRef {
    /// Schema of the referenced relation.
    pub schema: String,
    /// Name of the referenced relation.
    pub table: String,
    /// Referenced column(s), in the order the foreign key defines them.
    pub columns: Vec<String>,
}

/// One index defined on a relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    /// Index name.
    pub name: String,
    /// The access method the index uses (e.g. `btree`, `hash`).
    pub method: String,
    /// Whether the index enforces uniqueness.
    pub unique: bool,
    /// The index's definition, rendered as backend-native SQL text.
    pub definition: String,
}

/// One constraint defined on a relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintInfo {
    /// Constraint name.
    pub name: String,
    /// What kind of constraint this is.
    pub kind: ConstraintKind,
    /// The constraint's definition, rendered as backend-native SQL text.
    pub definition: String,
}

/// What kind of constraint a [`ConstraintInfo`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintKind {
    /// A primary key constraint.
    PrimaryKey,
    /// A foreign key constraint.
    ForeignKey,
    /// A unique constraint.
    Unique,
    /// A check constraint.
    Check,
}

/// A key role badge a [`ColumnDetail`] carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyBadge {
    /// The column is (part of) the relation's primary key.
    Primary,
    /// The column participates in a foreign key.
    Foreign,
    /// The column is constrained unique.
    Unique,
}

impl ColumnDetail {
    /// The key role badge(s) this column's flags indicate, in a fixed
    /// display order: primary key, then foreign key, then unique. Empty for
    /// a column with `is_primary_key` and `is_unique` both false and
    /// `foreign_key` `None`.
    #[must_use]
    pub fn key_badges(&self) -> Vec<KeyBadge> {
        let mut badges = Vec::new();
        if self.is_primary_key {
            badges.push(KeyBadge::Primary);
        }
        if self.foreign_key.is_some() {
            badges.push(KeyBadge::Foreign);
        }
        if self.is_unique {
            badges.push(KeyBadge::Unique);
        }
        badges
    }
}

#[cfg(test)]
mod tests {
    use super::{ColumnDetail, ForeignKeyRef, KeyBadge};

    fn plain_column() -> ColumnDetail {
        ColumnDetail {
            name: "n".to_owned(),
            type_name: "text".to_owned(),
            nullable: true,
            default: None,
            is_primary_key: false,
            is_unique: false,
            foreign_key: None,
        }
    }

    #[test]
    fn key_badges_returns_primary_for_a_primary_key_column() {
        let column = ColumnDetail {
            is_primary_key: true,
            ..plain_column()
        };
        assert_eq!(column.key_badges(), vec![KeyBadge::Primary]);
    }

    #[test]
    fn key_badges_returns_foreign_for_a_foreign_key_column() {
        let column = ColumnDetail {
            foreign_key: Some(ForeignKeyRef {
                schema: "public".to_owned(),
                table: "users".to_owned(),
                columns: vec!["id".to_owned()],
            }),
            ..plain_column()
        };
        assert_eq!(column.key_badges(), vec![KeyBadge::Foreign]);
    }

    #[test]
    fn key_badges_returns_unique_for_a_unique_column() {
        let column = ColumnDetail {
            is_unique: true,
            ..plain_column()
        };
        assert_eq!(column.key_badges(), vec![KeyBadge::Unique]);
    }

    #[test]
    fn key_badges_is_empty_for_a_column_with_no_key_role() {
        assert!(plain_column().key_badges().is_empty());
    }

    #[test]
    fn key_badges_combines_every_matching_role_in_a_fixed_order() {
        let column = ColumnDetail {
            is_primary_key: true,
            is_unique: true,
            foreign_key: Some(ForeignKeyRef {
                schema: "public".to_owned(),
                table: "users".to_owned(),
                columns: vec!["id".to_owned()],
            }),
            ..plain_column()
        };
        assert_eq!(
            column.key_badges(),
            vec![KeyBadge::Primary, KeyBadge::Foreign, KeyBadge::Unique]
        );
    }
}
