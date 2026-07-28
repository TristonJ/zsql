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

/// Classification of a column's `default` expression, for coloring a
/// rendered Default cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultKind {
    /// A function call, e.g. `now()` or `nextval('orders_id_seq')`.
    Function,
    /// A literal value, e.g. `0` or `'pending'`.
    Literal,
    /// No default.
    None,
}

/// Classify `default` for [`DefaultKind`]-based coloring. A value is
/// classified as a function call when it both contains `(` and ends with
/// `)`; any other non-empty value is a literal.
#[must_use]
pub fn classify_default(default: Option<&str>) -> DefaultKind {
    let Some(text) = default else {
        return DefaultKind::None;
    };
    let trimmed = text.trim();
    if trimmed.contains('(') && trimmed.ends_with(')') {
        DefaultKind::Function
    } else {
        DefaultKind::Literal
    }
}

/// Which badge (if any) a column's Keys cell renders, in a fixed priority:
/// primary key, then foreign key, then unique, then check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyCellBadge {
    /// The column is (part of) the primary key.
    Primary,
    /// The column is a foreign key targeting the carried `table.column(s)`
    /// string.
    Foreign(String),
    /// The column is constrained unique.
    Unique,
    /// The column is mentioned by a `CHECK` constraint.
    Check,
}

/// Classify `column`'s Keys-cell badge from its own flags plus, for the
/// `CHECK` case only, whether any of `constraints` mentions its name.
///
/// # Panics
///
/// Never panics: a `Foreign` role from `column.key_badges()` only ever
/// arises when `column.foreign_key` is `Some`.
#[must_use]
pub fn key_cell_badge(
    column: &ColumnDetail,
    constraints: &[ConstraintInfo],
) -> Option<KeyCellBadge> {
    // The PK/FK/Unique roles come from the shared key_badges classifier, in
    // its priority order, so every consumer agrees on a column's key role.
    // The Check case is layered on top here because it is derived from the
    // relation's constraints, not the column itself.
    if let Some(badge) = column.key_badges().first() {
        return Some(match badge {
            KeyBadge::Primary => KeyCellBadge::Primary,
            KeyBadge::Foreign => KeyCellBadge::Foreign(foreign_key_target(
                column
                    .foreign_key
                    .as_ref()
                    .expect("a Foreign key badge implies the column carries a foreign key"),
            )),
            KeyBadge::Unique => KeyCellBadge::Unique,
        });
    }
    if column_has_check(&column.name, constraints) {
        return Some(KeyCellBadge::Check);
    }
    None
}

/// The `-> target.column` link chip's target string: `table.col1,col2` for
/// a composite key, `table.col` for a single-column one.
fn foreign_key_target(fk: &ForeignKeyRef) -> String {
    format!("{}.{}", fk.table, fk.columns.join(","))
}

/// Whether any `CHECK` constraint in `constraints` mentions `column_name` as
/// a whole identifier (not merely as a substring of a longer name).
fn column_has_check(column_name: &str, constraints: &[ConstraintInfo]) -> bool {
    constraints
        .iter()
        .filter(|constraint| constraint.kind == ConstraintKind::Check)
        .any(|constraint| definition_mentions_column(&constraint.definition, column_name))
}

/// Whether `definition` mentions `column_name` as a whole identifier token,
/// splitting on any character that cannot appear inside a SQL identifier.
fn definition_mentions_column(definition: &str, column_name: &str) -> bool {
    definition
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
        .any(|token| token == column_name)
}

#[cfg(test)]
mod tests {
    use super::{
        ColumnDetail, ConstraintInfo, ConstraintKind, DefaultKind, ForeignKeyRef, KeyBadge,
        KeyCellBadge, classify_default, column_has_check, foreign_key_target, key_cell_badge,
    };

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

    #[test]
    fn classify_default_none_for_no_default() {
        assert_eq!(classify_default(None), DefaultKind::None);
    }

    #[test]
    fn classify_default_recognizes_function_calls() {
        assert_eq!(classify_default(Some("now()")), DefaultKind::Function);
        assert_eq!(
            classify_default(Some("nextval('orders_id_seq')")),
            DefaultKind::Function
        );
    }

    #[test]
    fn classify_default_recognizes_literals() {
        assert_eq!(classify_default(Some("0")), DefaultKind::Literal);
        assert_eq!(classify_default(Some("'pending'")), DefaultKind::Literal);
        assert_eq!(classify_default(Some("'{}'::jsonb")), DefaultKind::Literal);
    }

    #[test]
    fn foreign_key_target_joins_a_single_column() {
        let fk = ForeignKeyRef {
            schema: "public".to_owned(),
            table: "users".to_owned(),
            columns: vec!["id".to_owned()],
        };
        assert_eq!(foreign_key_target(&fk), "users.id");
    }

    #[test]
    fn foreign_key_target_joins_a_composite_key() {
        let fk = ForeignKeyRef {
            schema: "public".to_owned(),
            table: "grants".to_owned(),
            columns: vec!["tenant_id".to_owned(), "user_id".to_owned()],
        };
        assert_eq!(foreign_key_target(&fk), "grants.tenant_id,user_id");
    }

    #[test]
    fn key_cell_badge_prioritizes_primary_key_over_everything_else() {
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
        assert_eq!(key_cell_badge(&column, &[]), Some(KeyCellBadge::Primary));
    }

    #[test]
    fn key_cell_badge_prefers_foreign_key_over_unique() {
        let column = ColumnDetail {
            is_unique: true,
            foreign_key: Some(ForeignKeyRef {
                schema: "public".to_owned(),
                table: "users".to_owned(),
                columns: vec!["id".to_owned()],
            }),
            ..plain_column()
        };
        assert_eq!(
            key_cell_badge(&column, &[]),
            Some(KeyCellBadge::Foreign("users.id".to_owned()))
        );
    }

    #[test]
    fn key_cell_badge_is_unique_for_a_unique_only_column() {
        let column = ColumnDetail {
            is_unique: true,
            ..plain_column()
        };
        assert_eq!(key_cell_badge(&column, &[]), Some(KeyCellBadge::Unique));
    }

    #[test]
    fn key_cell_badge_is_check_when_a_check_constraint_mentions_the_column() {
        let column = ColumnDetail {
            name: "total_cents".to_owned(),
            ..plain_column()
        };
        let constraints = [ConstraintInfo {
            name: "orders_total_cents_nonneg".to_owned(),
            kind: ConstraintKind::Check,
            definition: "total_cents >= 0".to_owned(),
        }];
        assert_eq!(
            key_cell_badge(&column, &constraints),
            Some(KeyCellBadge::Check)
        );
    }

    #[test]
    fn key_cell_badge_does_not_match_a_column_name_as_a_mere_substring() {
        let column = ColumnDetail {
            name: "total".to_owned(),
            ..plain_column()
        };
        let constraints = [ConstraintInfo {
            name: "orders_total_cents_nonneg".to_owned(),
            kind: ConstraintKind::Check,
            definition: "total_cents >= 0".to_owned(),
        }];
        assert_eq!(key_cell_badge(&column, &constraints), None);
    }

    #[test]
    fn key_cell_badge_is_none_for_a_plain_column() {
        assert_eq!(key_cell_badge(&plain_column(), &[]), None);
    }

    #[test]
    fn column_has_check_ignores_non_check_constraints() {
        let column_name = "id";
        let constraints = [ConstraintInfo {
            name: "orders_pkey".to_owned(),
            kind: ConstraintKind::PrimaryKey,
            definition: "id".to_owned(),
        }];
        assert!(!column_has_check(column_name, &constraints));
    }
}
