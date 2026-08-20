//! The staged-changes model behind results-grid row deletes: a driver- and
//! UI-agnostic queue of pending edits, keyed by a row's primary key rather
//! than its position on screen, plus pure DELETE statement generation.

use zsql_core::filter::quote_sql_string;
use zsql_core::schema_detail::RelationSchema;
use zsql_core::sql::quote_ident;
use zsql_core::{ColumnMeta, Row, Value};

use crate::ui::format::format_value;

/// Identifies one staged change within a [`StagedChangeQueue`] for its
/// lifetime, unique within that queue.
pub type StagedChangeId = u64;

/// One primary-key column's name, backend type, and the value a specific row
/// carries for it.
#[derive(Debug, Clone, PartialEq)]
pub struct PkColumnValue {
    pub column: String,
    pub type_name: String,
    pub value: Value,
}

/// A row's identity for staging purposes.
#[derive(Debug, Clone, PartialEq)]
pub struct RowIdentity {
    pub schema: String,
    pub relation: String,
    pub pk: Vec<PkColumnValue>,
}

/// One kind of pending change against a relation, targeting one row by its
/// [`RowIdentity`].
#[derive(Debug, Clone, PartialEq)]
pub enum StagedChange {
    Delete { target: RowIdentity },
}

impl StagedChange {
    /// The [`RowIdentity`] this change targets, whatever its variant.
    #[must_use]
    pub fn target(&self) -> &RowIdentity {
        match self {
            StagedChange::Delete { target } => target,
        }
    }
}

/// One entry in a [`StagedChangeQueue`]: a change plus the bookkeeping the
/// UI needs to show where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct StagedChangeEntry {
    pub id: StagedChangeId,
    /// The row index (within whatever result page was on screen) this
    /// change was staged from, for the ledger's "row N" reference only.
    pub source_row: usize,
    pub change: StagedChange,
}

/// A FIFO queue of staged changes.
#[derive(Debug, Clone, Default)]
pub struct StagedChangeQueue {
    entries: Vec<StagedChangeEntry>,
    next_id: StagedChangeId,
}

impl StagedChangeQueue {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Every staged entry, in FIFO staging order.
    #[must_use]
    pub fn entries(&self) -> &[StagedChangeEntry] {
        &self.entries
    }

    /// Stage a delete of `target`, read from `source_row`, appending it to
    /// the end of the queue and returning its new id.
    pub fn stage_delete(&mut self, source_row: usize, target: RowIdentity) -> StagedChangeId {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(StagedChangeEntry {
            id,
            source_row,
            change: StagedChange::Delete { target },
        });
        id
    }

    /// The id of the queued change targeting `target`, if any.
    #[must_use]
    pub fn find_staged(&self, target: &RowIdentity) -> Option<StagedChangeId> {
        self.entries
            .iter()
            .find(|entry| entry.change.target() == target)
            .map(|entry| entry.id)
    }

    /// Remove the entry with `id`. Returns whether an entry was actually
    /// removed.
    pub fn unstage(&mut self, id: StagedChangeId) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.id != id);
        self.entries.len() != before
    }

    /// Clear every staged entry.
    pub fn discard_all(&mut self) {
        self.entries.clear();
    }

    /// The exact SQL statements Apply will run, one per entry, in the same
    /// FIFO order [`StagedChangeQueue::entries`] lists them in.
    #[must_use]
    pub fn statements(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| statement_sql(&entry.change))
            .collect()
    }
}

/// This change's statement, as the exact plain text Apply executes.
#[must_use]
pub fn statement_sql(change: &StagedChange) -> String {
    match change {
        StagedChange::Delete { target } => delete_statement_sql(target),
    }
}

/// `DELETE FROM "schema"."relation" WHERE ...`, targeting `target`'s primary
/// key: every PK column AND-joined, in `target.pk`'s own order. Identifiers
/// are quoted only via [`quote_ident`]; each value is rendered only via
/// [`quote_sql_string`] or as a bare numeric literal, never a hand-rolled
/// escaper and never treated as a SQL expression.
fn delete_statement_sql(target: &RowIdentity) -> String {
    let mut sql = format!(
        "DELETE FROM {}.{} WHERE ",
        quote_ident(&target.schema),
        quote_ident(&target.relation)
    );
    for (index, pk) in target.pk.iter().enumerate() {
        if index > 0 {
            sql.push_str(" AND ");
        }
        sql.push_str(&quote_ident(&pk.column));
        if matches!(pk.value, Value::Null) {
            sql.push_str(" IS NULL");
        } else {
            sql.push_str(" = ");
            sql.push_str(&pk_literal(pk));
        }
    }
    sql.push(';');
    sql
}

/// `pk`'s value as a SQL literal: a bare numeric literal for an
/// int/float/numeric value, or its exact display text (untrimmed, with no
/// attempt to detect an "expression") single-quoted via [`quote_sql_string`]
/// for everything else.
fn pk_literal(pk: &PkColumnValue) -> String {
    let text = format_value(&pk.value).text;
    if matches!(
        pk.value,
        Value::Int(_) | Value::Float(_) | Value::Numeric(_)
    ) {
        text
    } else {
        quote_sql_string(&text)
    }
}

/// Whether `relation_schema` carries a usable primary key: at least one
/// column with `is_primary_key` set.
#[must_use]
pub fn has_usable_primary_key(relation_schema: &RelationSchema) -> bool {
    relation_schema
        .columns
        .iter()
        .any(|column| column.is_primary_key)
}

/// Build `row`'s [`RowIdentity`] against `schema`.`relation`, from
/// `relation_schema`'s primary-key columns (in ordinal position) matched by
/// name to `columns`/`row`. `None` when the relation carries no primary key,
/// or `columns` is missing one of the PK columns by name (a relation schema
/// that no longer matches the query that produced `row`).
#[must_use]
pub fn row_identity(
    schema: &str,
    relation: &str,
    relation_schema: &RelationSchema,
    columns: &[ColumnMeta],
    row: &Row,
) -> Option<RowIdentity> {
    let pk_columns: Vec<_> = relation_schema
        .columns
        .iter()
        .filter(|column| column.is_primary_key)
        .collect();
    if pk_columns.is_empty() {
        return None;
    }
    let mut pk = Vec::with_capacity(pk_columns.len());
    for detail in pk_columns {
        let index = columns
            .iter()
            .position(|column| column.name == detail.name)?;
        let value = row.0.get(index)?.clone();
        pk.push(PkColumnValue {
            column: detail.name.clone(),
            type_name: detail.type_name.clone(),
            value,
        });
    }
    Some(RowIdentity {
        schema: schema.to_owned(),
        relation: relation.to_owned(),
        pk,
    })
}

#[cfg(test)]
mod tests {
    use zsql_core::schema_detail::ColumnDetail;

    use super::{
        PkColumnValue, RowIdentity, StagedChange, StagedChangeQueue, has_usable_primary_key,
        row_identity, statement_sql,
    };

    fn pk_column(name: &str, type_name: &str) -> ColumnDetail {
        ColumnDetail {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            nullable: false,
            default: None,
            is_primary_key: true,
            is_unique: false,
            foreign_key: None,
        }
    }

    fn plain_column(name: &str, type_name: &str) -> ColumnDetail {
        ColumnDetail {
            is_primary_key: false,
            ..pk_column(name, type_name)
        }
    }

    fn identity(pk: Vec<PkColumnValue>) -> RowIdentity {
        RowIdentity {
            schema: "public".to_owned(),
            relation: "orders".to_owned(),
            pk,
        }
    }

    fn int_pk(value: i64) -> Vec<PkColumnValue> {
        vec![PkColumnValue {
            column: "id".to_owned(),
            type_name: "int8".to_owned(),
            value: zsql_core::Value::Int(value),
        }]
    }

    // -- StagedChangeQueue ----------------------------------------------

    #[test]
    fn a_fresh_queue_is_empty() {
        let queue = StagedChangeQueue::new();
        assert!(queue.is_empty());
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn stage_delete_appends_an_entry_and_assigns_a_unique_id() {
        let mut queue = StagedChangeQueue::new();
        let first = queue.stage_delete(0, identity(int_pk(1)));
        let second = queue.stage_delete(1, identity(int_pk(2)));
        assert_ne!(first, second);
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn entries_are_listed_in_fifo_staging_order() {
        let mut queue = StagedChangeQueue::new();
        queue.stage_delete(2, identity(int_pk(30)));
        queue.stage_delete(0, identity(int_pk(10)));
        queue.stage_delete(1, identity(int_pk(20)));

        let source_rows: Vec<usize> = queue.entries().iter().map(|e| e.source_row).collect();
        assert_eq!(source_rows, vec![2, 0, 1]);
    }

    #[test]
    fn find_staged_locates_the_entry_matching_a_row_identity() {
        let mut queue = StagedChangeQueue::new();
        let target = identity(int_pk(7));
        let id = queue.stage_delete(0, target.clone());
        assert_eq!(queue.find_staged(&target), Some(id));
        assert_eq!(queue.find_staged(&identity(int_pk(8))), None);
    }

    #[test]
    fn unstage_removes_exactly_the_matching_entry() {
        let mut queue = StagedChangeQueue::new();
        let keep = queue.stage_delete(0, identity(int_pk(1)));
        let remove = queue.stage_delete(1, identity(int_pk(2)));

        assert!(queue.unstage(remove));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.entries()[0].id, keep);
    }

    #[test]
    fn unstage_reports_false_for_an_id_not_in_the_queue() {
        let mut queue = StagedChangeQueue::new();
        assert!(!queue.unstage(999));
    }

    #[test]
    fn restaging_a_row_after_unstaging_it_appends_a_new_entry_at_the_end() {
        let mut queue = StagedChangeQueue::new();
        let target = identity(int_pk(1));
        let other = identity(int_pk(2));

        let first = queue.stage_delete(0, target.clone());
        queue.stage_delete(1, other);
        queue.unstage(first);
        let restaged = queue.stage_delete(2, target.clone());

        let ids: Vec<_> = queue.entries().iter().map(|e| e.id).collect();
        assert_eq!(ids.last(), Some(&restaged));
        assert_ne!(
            restaged, first,
            "a restage must mint a fresh id, not reuse the old one"
        );
    }

    #[test]
    fn discard_all_clears_every_entry() {
        let mut queue = StagedChangeQueue::new();
        queue.stage_delete(0, identity(int_pk(1)));
        queue.stage_delete(1, identity(int_pk(2)));
        queue.discard_all();
        assert!(queue.is_empty());
    }

    #[test]
    fn apply_executes_statements_in_the_same_order_the_ledger_lists_them() {
        let mut queue = StagedChangeQueue::new();
        queue.stage_delete(0, identity(int_pk(3)));
        queue.stage_delete(1, identity(int_pk(10)));

        let statements = queue.statements();
        assert_eq!(
            statements,
            vec![
                "DELETE FROM \"public\".\"orders\" WHERE \"id\" = 3;".to_owned(),
                "DELETE FROM \"public\".\"orders\" WHERE \"id\" = 10;".to_owned(),
            ]
        );
    }

    // -- statement generation ---------------------------------------------

    #[test]
    fn delete_statement_quotes_identifiers_and_leaves_a_numeric_pk_bare() {
        let sql = statement_sql(&StagedChange::Delete {
            target: identity(int_pk(42)),
        });
        assert_eq!(sql, "DELETE FROM \"public\".\"orders\" WHERE \"id\" = 42;");
    }

    #[test]
    fn delete_statement_quotes_a_text_pk_value_and_escapes_embedded_quotes() {
        let target = identity(vec![PkColumnValue {
            column: "code".to_owned(),
            type_name: "text".to_owned(),
            value: zsql_core::Value::Text("it's".to_owned()),
        }]);
        let sql = statement_sql(&StagedChange::Delete { target });
        assert_eq!(
            sql,
            "DELETE FROM \"public\".\"orders\" WHERE \"code\" = 'it''s';"
        );
    }

    #[test]
    fn delete_statement_quotes_a_text_pk_value_containing_parens_instead_of_treating_it_as_an_expression()
     {
        let target = identity(vec![PkColumnValue {
            column: "code".to_owned(),
            type_name: "text".to_owned(),
            value: zsql_core::Value::Text("substr('a',1,1)".to_owned()),
        }]);
        let sql = statement_sql(&StagedChange::Delete { target });
        assert_eq!(
            sql,
            "DELETE FROM \"public\".\"orders\" WHERE \"code\" = 'substr(''a'',1,1)';"
        );
    }

    #[test]
    fn delete_statement_quotes_a_text_pk_value_containing_a_plus_sign_instead_of_treating_it_as_an_expression()
     {
        let target = identity(vec![PkColumnValue {
            column: "code".to_owned(),
            type_name: "text".to_owned(),
            value: zsql_core::Value::Text("total + 500".to_owned()),
        }]);
        let sql = statement_sql(&StagedChange::Delete { target });
        assert_eq!(
            sql,
            "DELETE FROM \"public\".\"orders\" WHERE \"code\" = 'total + 500';"
        );
    }

    #[test]
    fn delete_statement_preserves_leading_and_trailing_whitespace_in_a_text_pk_value() {
        let target = identity(vec![PkColumnValue {
            column: "code".to_owned(),
            type_name: "text".to_owned(),
            value: zsql_core::Value::Text("  padded  ".to_owned()),
        }]);
        let sql = statement_sql(&StagedChange::Delete { target });
        assert_eq!(
            sql,
            "DELETE FROM \"public\".\"orders\" WHERE \"code\" = '  padded  ';"
        );
    }

    #[test]
    fn delete_statement_quotes_identifiers_that_need_escaping() {
        let target = RowIdentity {
            schema: "we\"ird".to_owned(),
            relation: "orders".to_owned(),
            pk: int_pk(1),
        };
        let sql = statement_sql(&StagedChange::Delete { target });
        assert!(sql.starts_with("DELETE FROM \"we\"\"ird\".\"orders\""));
    }

    #[test]
    fn delete_statement_renders_a_null_pk_value_as_is_null() {
        let target = identity(vec![PkColumnValue {
            column: "id".to_owned(),
            type_name: "int8".to_owned(),
            value: zsql_core::Value::Null,
        }]);
        let sql = statement_sql(&StagedChange::Delete { target });
        assert_eq!(
            sql,
            "DELETE FROM \"public\".\"orders\" WHERE \"id\" IS NULL;"
        );
    }

    #[test]
    fn delete_statement_joins_a_composite_primary_key_with_and_in_column_order() {
        let target = identity(vec![
            PkColumnValue {
                column: "tenant_id".to_owned(),
                type_name: "int8".to_owned(),
                value: zsql_core::Value::Int(9),
            },
            PkColumnValue {
                column: "user_id".to_owned(),
                type_name: "int8".to_owned(),
                value: zsql_core::Value::Int(4),
            },
        ]);
        let sql = statement_sql(&StagedChange::Delete { target });
        assert_eq!(
            sql,
            "DELETE FROM \"public\".\"orders\" WHERE \"tenant_id\" = 9 AND \"user_id\" = 4;"
        );
    }

    // -- has_usable_primary_key ---------------------------------------------

    #[test]
    fn has_usable_primary_key_is_true_with_at_least_one_pk_column() {
        let schema = zsql_core::schema_detail::RelationSchema {
            columns: vec![pk_column("id", "int8"), plain_column("name", "text")],
            indexes: vec![],
            constraints: vec![],
        };
        assert!(has_usable_primary_key(&schema));
    }

    #[test]
    fn has_usable_primary_key_is_false_with_no_pk_columns() {
        let schema = zsql_core::schema_detail::RelationSchema {
            columns: vec![plain_column("name", "text")],
            indexes: vec![],
            constraints: vec![],
        };
        assert!(!has_usable_primary_key(&schema));
    }

    #[test]
    fn has_usable_primary_key_is_false_for_a_relation_with_no_columns() {
        assert!(!has_usable_primary_key(
            &zsql_core::schema_detail::RelationSchema::default()
        ));
    }

    // -- row_identity ---------------------------------------------------

    fn columns(names: &[(&str, &str)]) -> Vec<zsql_core::ColumnMeta> {
        names
            .iter()
            .map(|(name, type_name)| zsql_core::ColumnMeta {
                name: (*name).to_owned(),
                type_name: (*type_name).to_owned(),
                nullable: false,
            })
            .collect()
    }

    #[test]
    fn row_identity_builds_a_single_column_pk_from_matching_row_values() {
        let schema = zsql_core::schema_detail::RelationSchema {
            columns: vec![pk_column("id", "int8"), plain_column("status", "text")],
            indexes: vec![],
            constraints: vec![],
        };
        let cols = columns(&[("id", "int8"), ("status", "text")]);
        let row = zsql_core::Row(vec![
            zsql_core::Value::Int(3),
            zsql_core::Value::Text("paid".to_owned()),
        ]);

        let got = row_identity("public", "orders", &schema, &cols, &row).unwrap();
        assert_eq!(got, identity(int_pk(3)));
    }

    #[test]
    fn row_identity_orders_a_composite_pk_by_the_relation_schema_column_order() {
        let schema = zsql_core::schema_detail::RelationSchema {
            columns: vec![
                pk_column("tenant_id", "int8"),
                pk_column("user_id", "int8"),
                plain_column("name", "text"),
            ],
            indexes: vec![],
            constraints: vec![],
        };
        // The result set's own column order differs from the relation
        // schema's, proving the PK values are matched by name, not position.
        let cols = columns(&[("name", "text"), ("user_id", "int8"), ("tenant_id", "int8")]);
        let row = zsql_core::Row(vec![
            zsql_core::Value::Text("alice".to_owned()),
            zsql_core::Value::Int(4),
            zsql_core::Value::Int(9),
        ]);

        let got = row_identity("public", "grants", &schema, &cols, &row).unwrap();
        assert_eq!(got.pk[0].column, "tenant_id");
        assert_eq!(got.pk[0].value, zsql_core::Value::Int(9));
        assert_eq!(got.pk[1].column, "user_id");
        assert_eq!(got.pk[1].value, zsql_core::Value::Int(4));
    }

    #[test]
    fn row_identity_is_none_with_no_primary_key() {
        let schema = zsql_core::schema_detail::RelationSchema {
            columns: vec![plain_column("id", "int8")],
            indexes: vec![],
            constraints: vec![],
        };
        let cols = columns(&[("id", "int8")]);
        let row = zsql_core::Row(vec![zsql_core::Value::Int(1)]);
        assert!(row_identity("public", "v", &schema, &cols, &row).is_none());
    }

    #[test]
    fn row_identity_is_none_when_a_pk_column_is_missing_from_the_result_columns() {
        let schema = zsql_core::schema_detail::RelationSchema {
            columns: vec![pk_column("id", "int8")],
            indexes: vec![],
            constraints: vec![],
        };
        let cols = columns(&[("name", "text")]);
        let row = zsql_core::Row(vec![zsql_core::Value::Text("x".to_owned())]);
        assert!(row_identity("public", "orders", &schema, &cols, &row).is_none());
    }

    #[test]
    fn two_identities_with_the_same_relation_and_pk_values_are_equal_even_from_different_rows() {
        assert_eq!(identity(int_pk(5)), identity(int_pk(5)));
    }
}
