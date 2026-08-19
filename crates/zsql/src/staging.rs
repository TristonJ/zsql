//! The staged-changes model behind results-grid row changes.

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

/// How a staged cell edit's new value renders into generated SQL text,
/// mirroring [`zsql_core::filter::FilterValueRender`]'s classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateValue {
    /// Already rendered per [`zsql_core::filter::render_literal_value`]:
    /// either a single-quoted, escaped string or a bare numeric literal.
    Literal(String),
    /// Raw SQL, embedded unquoted (e.g. `now()`).
    Expression(String),
    Null,
}

/// One kind of pending change against a relation, targeting one row by its
/// [`RowIdentity`].
#[derive(Debug, Clone, PartialEq)]
pub enum StagedChange {
    Delete {
        target: RowIdentity,
    },
    Update {
        target: RowIdentity,
        column: String,
        new_value: UpdateValue,
    },
}

impl StagedChange {
    /// The [`RowIdentity`] this change targets, whatever its variant.
    #[must_use]
    pub fn target(&self) -> &RowIdentity {
        match self {
            StagedChange::Delete { target } | StagedChange::Update { target, .. } => target,
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

    /// The id of the queued delete targeting `target`, if any.
    #[must_use]
    pub fn find_staged_delete(&self, target: &RowIdentity) -> Option<StagedChangeId> {
        self.entries
            .iter()
            .find(
                |entry| matches!(&entry.change, StagedChange::Delete { target: t } if t == target),
            )
            .map(|entry| entry.id)
    }

    /// The new value of the queued update targeting `target`'s `column`, if
    /// any.
    #[must_use]
    pub fn staged_update_value(&self, target: &RowIdentity, column: &str) -> Option<&UpdateValue> {
        self.entries.iter().find_map(|entry| match &entry.change {
            StagedChange::Update {
                target: t,
                column: c,
                new_value,
            } if t == target && c == column => Some(new_value),
            _ => None,
        })
    }

    /// Stage `target`.`column` to `value`, read from `source_row`. Rejected
    /// (returns `None`, queue left untouched) while `target` already carries
    /// a staged delete: a row queued for deletion cannot also take a cell
    /// edit. When `target`.`column` is already staged for update, replaces
    /// that entry's value in place, keeping its id and FIFO position;
    /// otherwise appends a fresh entry at the end of the queue.
    pub fn stage_update(
        &mut self,
        source_row: usize,
        target: RowIdentity,
        column: String,
        value: UpdateValue,
    ) -> Option<StagedChangeId> {
        if self.find_staged_delete(&target).is_some() {
            return None;
        }
        if let Some(entry) = self.entries.iter_mut().find(|entry| {
            matches!(&entry.change, StagedChange::Update { target: t, column: c, .. } if *t == target && *c == column)
        }) {
            if let StagedChange::Update { new_value, .. } = &mut entry.change {
                *new_value = value;
            }
            return Some(entry.id);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(StagedChangeEntry {
            id,
            source_row,
            change: StagedChange::Update {
                target,
                column,
                new_value: value,
            },
        });
        Some(id)
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
        StagedChange::Update {
            target,
            column,
            new_value,
        } => update_statement_sql(target, column, new_value),
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
    push_pk_where_sql(&mut sql, &target.pk);
    sql.push(';');
    sql
}

/// `UPDATE "schema"."relation" SET "column" = <new_value> WHERE ...`,
/// targeting `target`'s primary key exactly like [`delete_statement_sql`]
/// (sharing [`push_pk_where_sql`], so a composite PK's `WHERE` clause can
/// never diverge between the two statement kinds).
fn update_statement_sql(target: &RowIdentity, column: &str, new_value: &UpdateValue) -> String {
    let mut sql = format!(
        "UPDATE {}.{} {} WHERE ",
        quote_ident(&target.schema),
        quote_ident(&target.relation),
        update_set_fragment_sql(column, new_value)
    );
    push_pk_where_sql(&mut sql, &target.pk);
    sql.push(';');
    sql
}

/// An update's `SET "column" = <new_value>` fragment alone, without the
/// statement around it.
#[must_use]
pub fn update_set_fragment_sql(column: &str, new_value: &UpdateValue) -> String {
    format!(
        "SET {} = {}",
        quote_ident(column),
        update_value_sql(new_value)
    )
}

/// `new_value` as SQL text: `NULL` bare for the null mode, raw text for an
/// expression, or `new_value`'s already-rendered literal.
fn update_value_sql(new_value: &UpdateValue) -> String {
    match new_value {
        UpdateValue::Null => "NULL".to_owned(),
        UpdateValue::Expression(text) | UpdateValue::Literal(text) => text.clone(),
    }
}

/// Appends `pk`'s columns, AND-joined in `pk`'s own order, as a `WHERE`
/// clause's body (no leading `WHERE` keyword).
fn push_pk_where_sql(sql: &mut String, pk: &[PkColumnValue]) {
    for (index, pk_column) in pk.iter().enumerate() {
        if index > 0 {
            sql.push_str(" AND ");
        }
        sql.push_str(&quote_ident(&pk_column.column));
        if matches!(pk_column.value, Value::Null) {
            sql.push_str(" IS NULL");
        } else {
            sql.push_str(" = ");
            sql.push_str(&pk_literal(pk_column));
        }
    }
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

/// Reverse of [`quote_sql_string`]: strips a literal's surrounding quotes
/// and un-doubles embedded ones, recovering the raw text before quoting.
/// Text with no surrounding quotes (a bare numeric literal) passes through
/// unchanged.
#[must_use]
pub fn unquote_sql_string(text: &str) -> String {
    match text
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        Some(inner) => inner.replace("''", "'"),
        None => text.to_owned(),
    }
}

/// `new_value`'s plain display text for the results grid cell it is staged
/// against: unquoted for a literal, raw for an expression, `NULL` for the
/// null mode.
#[must_use]
pub fn update_value_display_text(new_value: &UpdateValue) -> String {
    match new_value {
        UpdateValue::Null => "NULL".to_owned(),
        UpdateValue::Expression(text) => text.clone(),
        UpdateValue::Literal(text) => unquote_sql_string(text),
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
        PkColumnValue, RowIdentity, StagedChange, StagedChangeQueue, UpdateValue,
        has_usable_primary_key, row_identity, statement_sql, unquote_sql_string,
        update_set_fragment_sql, update_value_display_text,
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
    fn find_staged_delete_locates_the_entry_matching_a_row_identity() {
        let mut queue = StagedChangeQueue::new();
        let target = identity(int_pk(7));
        let id = queue.stage_delete(0, target.clone());
        assert_eq!(queue.find_staged_delete(&target), Some(id));
        assert_eq!(queue.find_staged_delete(&identity(int_pk(8))), None);
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

    // -- StagedChangeQueue::stage_update ---------------------------------

    #[test]
    fn stage_update_appends_an_entry_and_assigns_a_unique_id() {
        let mut queue = StagedChangeQueue::new();
        let id = queue
            .stage_update(
                0,
                identity(int_pk(1)),
                "status".to_owned(),
                UpdateValue::Literal("'shipped'".to_owned()),
            )
            .expect("staging an update against an unstaged row must succeed");
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.entries()[0].id, id);
    }

    #[test]
    fn staging_the_same_cell_twice_replaces_the_value_in_place_at_the_same_id_and_position() {
        let mut queue = StagedChangeQueue::new();
        queue.stage_delete(9, identity(int_pk(2)));
        let target = identity(int_pk(1));
        let first = queue
            .stage_update(
                0,
                target.clone(),
                "status".to_owned(),
                UpdateValue::Literal("'shipped'".to_owned()),
            )
            .expect("first stage must succeed");
        let second = queue
            .stage_update(
                0,
                target.clone(),
                "status".to_owned(),
                UpdateValue::Literal("'refunded'".to_owned()),
            )
            .expect("restaging the same cell must succeed");

        assert_eq!(first, second, "restaging the same cell must keep its id");
        assert_eq!(
            queue.len(),
            2,
            "restaging the same cell must not append a new entry"
        );
        assert_eq!(
            queue.entries()[0].change,
            StagedChange::Delete {
                target: identity(int_pk(2))
            },
            "restaging a different cell must not disturb an unrelated entry's FIFO position"
        );
        assert_eq!(
            queue.staged_update_value(&target, "status"),
            Some(&UpdateValue::Literal("'refunded'".to_owned())),
            "the replaced entry must carry the newest value"
        );
    }

    #[test]
    fn staging_two_different_columns_on_the_same_row_appends_two_entries() {
        let mut queue = StagedChangeQueue::new();
        let target = identity(int_pk(1));
        queue
            .stage_update(
                0,
                target.clone(),
                "status".to_owned(),
                UpdateValue::Literal("'shipped'".to_owned()),
            )
            .expect("staging the first column must succeed");
        queue
            .stage_update(
                0,
                target,
                "total_cents".to_owned(),
                UpdateValue::Literal("9000".to_owned()),
            )
            .expect("staging a different column on the same row must succeed");
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn stage_update_is_rejected_while_the_target_row_carries_a_staged_delete() {
        let mut queue = StagedChangeQueue::new();
        let target = identity(int_pk(1));
        queue.stage_delete(0, target.clone());

        let result = queue.stage_update(
            0,
            target,
            "status".to_owned(),
            UpdateValue::Literal("'shipped'".to_owned()),
        );

        assert_eq!(
            result, None,
            "a row staged for delete must not also accept a staged edit"
        );
        assert_eq!(
            queue.len(),
            1,
            "a rejected stage_update must leave the queue exactly as it was"
        );
    }

    #[test]
    fn stage_update_succeeds_again_once_the_blocking_delete_is_unstaged() {
        let mut queue = StagedChangeQueue::new();
        let target = identity(int_pk(1));
        let delete_id = queue.stage_delete(0, target.clone());
        queue.unstage(delete_id);

        let result = queue.stage_update(
            0,
            target,
            "status".to_owned(),
            UpdateValue::Literal("'shipped'".to_owned()),
        );

        assert!(
            result.is_some(),
            "unstaging the delete must make the row eligible for edits again"
        );
    }

    #[test]
    fn find_staged_delete_never_matches_a_staged_update_on_the_same_row() {
        let mut queue = StagedChangeQueue::new();
        let target = identity(int_pk(1));
        queue
            .stage_update(
                0,
                target.clone(),
                "status".to_owned(),
                UpdateValue::Literal("'shipped'".to_owned()),
            )
            .expect("staging the update must succeed");

        assert_eq!(
            queue.find_staged_delete(&target),
            None,
            "find_staged_delete must never match a staged update against the same row"
        );
    }

    // -- UPDATE statement generation --------------------------------------

    #[test]
    fn update_statement_renders_a_literal_value_quoted() {
        let change = StagedChange::Update {
            target: identity(int_pk(2)),
            column: "status".to_owned(),
            new_value: UpdateValue::Literal("'shipped'".to_owned()),
        };
        assert_eq!(
            statement_sql(&change),
            "UPDATE \"public\".\"orders\" SET \"status\" = 'shipped' WHERE \"id\" = 2;"
        );
    }

    #[test]
    fn update_statement_renders_a_bare_numeric_literal_value_unquoted() {
        let change = StagedChange::Update {
            target: identity(int_pk(2)),
            column: "total_cents".to_owned(),
            new_value: UpdateValue::Literal("9000".to_owned()),
        };
        assert_eq!(
            statement_sql(&change),
            "UPDATE \"public\".\"orders\" SET \"total_cents\" = 9000 WHERE \"id\" = 2;"
        );
    }

    #[test]
    fn update_statement_renders_an_expression_value_raw_and_unquoted() {
        let change = StagedChange::Update {
            target: identity(int_pk(8)),
            column: "placed_at".to_owned(),
            new_value: UpdateValue::Expression("now()".to_owned()),
        };
        assert_eq!(
            statement_sql(&change),
            "UPDATE \"public\".\"orders\" SET \"placed_at\" = now() WHERE \"id\" = 8;"
        );
    }

    #[test]
    fn update_statement_renders_null_mode_as_bare_null_not_the_string_null() {
        let change = StagedChange::Update {
            target: identity(int_pk(4)),
            column: "metadata".to_owned(),
            new_value: UpdateValue::Null,
        };
        let sql = statement_sql(&change);
        assert_eq!(
            sql,
            "UPDATE \"public\".\"orders\" SET \"metadata\" = NULL WHERE \"id\" = 4;"
        );
        assert!(
            !sql.contains("'NULL'"),
            "NULL mode must never render as the quoted string 'NULL'"
        );
    }

    #[test]
    fn update_statement_joins_a_composite_primary_key_with_and_in_column_order() {
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
        let change = StagedChange::Update {
            target,
            column: "status".to_owned(),
            new_value: UpdateValue::Literal("'shipped'".to_owned()),
        };
        assert_eq!(
            statement_sql(&change),
            "UPDATE \"public\".\"orders\" SET \"status\" = 'shipped' \
             WHERE \"tenant_id\" = 9 AND \"user_id\" = 4;"
        );
    }

    #[test]
    fn update_statement_quotes_identifiers_that_need_escaping() {
        let target = RowIdentity {
            schema: "we\"ird".to_owned(),
            relation: "orders".to_owned(),
            pk: int_pk(1),
        };
        let change = StagedChange::Update {
            target,
            column: "we\"ird col".to_owned(),
            new_value: UpdateValue::Literal("'x'".to_owned()),
        };
        let sql = statement_sql(&change);
        assert!(sql.starts_with("UPDATE \"we\"\"ird\".\"orders\" SET \"we\"\"ird col\""));
    }

    #[test]
    fn update_set_fragment_sql_renders_the_same_text_the_full_statement_embeds() {
        let fragment =
            update_set_fragment_sql("status", &UpdateValue::Literal("'shipped'".to_owned()));
        assert_eq!(fragment, "SET \"status\" = 'shipped'");

        let change = StagedChange::Update {
            target: identity(int_pk(2)),
            column: "status".to_owned(),
            new_value: UpdateValue::Literal("'shipped'".to_owned()),
        };
        assert!(statement_sql(&change).contains(&fragment));
    }

    // -- unquote_sql_string / update_value_display_text --------------------

    #[test]
    fn unquote_sql_string_strips_quotes_and_undoubles_an_embedded_quote() {
        assert_eq!(unquote_sql_string("'it''s'"), "it's");
    }

    #[test]
    fn unquote_sql_string_passes_a_bare_numeric_literal_through_unchanged() {
        assert_eq!(unquote_sql_string("9000"), "9000");
    }

    #[test]
    fn unquote_sql_string_reverses_quote_sql_string_for_arbitrary_text() {
        let raw = "shipped-with-a-note";
        assert_eq!(
            unquote_sql_string(&zsql_core::filter::quote_sql_string(raw)),
            raw
        );
    }

    #[test]
    fn update_value_display_text_unquotes_a_literal() {
        assert_eq!(
            update_value_display_text(&UpdateValue::Literal("'shipped'".to_owned())),
            "shipped"
        );
    }

    #[test]
    fn update_value_display_text_leaves_a_bare_numeric_literal_bare() {
        assert_eq!(
            update_value_display_text(&UpdateValue::Literal("9000".to_owned())),
            "9000"
        );
    }

    #[test]
    fn update_value_display_text_shows_an_expression_raw() {
        assert_eq!(
            update_value_display_text(&UpdateValue::Expression("now()".to_owned())),
            "now()"
        );
    }

    #[test]
    fn update_value_display_text_shows_null_mode_as_the_word_null() {
        assert_eq!(update_value_display_text(&UpdateValue::Null), "NULL");
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
