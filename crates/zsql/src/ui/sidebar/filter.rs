//! Pure quick-find filtering for the sidebar: which schema-tree and
//! script rows survive a live query, their matched label byte ranges, and
//! the collapse-state bookkeeping the filtered tree needs while it is
//! live. Gpui-free so this logic is directly unit-testable.

use std::collections::HashSet;
use std::ops::Range;

use zsql_core::{RelationKind, SchemaTree};

use super::model::ScriptRow;

/// A byte range into a row's label that matched the live query, for
/// highlighting.
pub type MatchRange = Range<usize>;

/// The byte range of `needle`'s first case-insensitive occurrence in
/// `label`, or `None` if `needle` is empty or does not occur. Matching is
/// ASCII-case-insensitive: the range's endpoints always fall on `label`'s
/// char boundaries, so it is always safe to slice `label` with it.
#[must_use]
pub fn label_match(label: &str, needle: &str) -> Option<MatchRange> {
    if needle.is_empty() {
        return None;
    }
    let needle_len = needle.len();
    for (start, _) in label.char_indices() {
        let end = start + needle_len;
        if end > label.len() || !label.is_char_boundary(end) {
            continue;
        }
        if label.as_bytes()[start..end].eq_ignore_ascii_case(needle.as_bytes()) {
            return Some(start..end);
        }
    }
    None
}

/// One row the filtered schema tree renders: the same shape as
/// [`super::model::SidebarRow`], plus the row's own label match range (if
/// its own label matched the query) and, for a catalog/schema, how many of
/// its immediate children survived the filter.
#[derive(Debug, Clone, PartialEq)]
pub enum FilteredRow {
    Catalog {
        name: String,
        matched_schemas: usize,
        total_schemas: usize,
        label_match: Option<MatchRange>,
    },
    Schema {
        catalog: String,
        name: String,
        matched_relations: usize,
        total_relations: usize,
        label_match: Option<MatchRange>,
    },
    Relation {
        schema: String,
        name: String,
        kind: RelationKind,
        column_count: usize,
        label_match: Option<MatchRange>,
    },
}

/// Filter `tree` for `query`: a relation survives if its own label matches;
/// a schema or catalog survives if its own label matches or it has at
/// least one surviving descendant. Non-surviving catalogs/schemas are
/// omitted entirely rather than kept as empty rows. An empty `query`
/// matches nothing, by [`label_match`]'s own contract.
#[tracing::instrument(name = "sidebar_filter_schema_tree", skip(tree), fields(query_len = query.len()))]
pub fn flatten_schema_tree_filtered(tree: &SchemaTree, query: &str) -> Vec<FilteredRow> {
    let mut rows = Vec::new();
    for catalog in &tree.catalogs {
        let catalog_label_match = label_match(&catalog.name, query);
        let mut catalog_rows = Vec::new();
        let mut matched_schema_count = 0;

        for schema in &catalog.schemas {
            let schema_label_match = label_match(&schema.name, query);
            let relation_rows: Vec<FilteredRow> = schema
                .tables
                .iter()
                .filter_map(|relation| {
                    label_match(&relation.name, query).map(|range| FilteredRow::Relation {
                        schema: schema.name.clone(),
                        name: relation.name.clone(),
                        kind: relation.kind,
                        column_count: relation.columns.len(),
                        label_match: Some(range),
                    })
                })
                .collect();

            if schema_label_match.is_none() && relation_rows.is_empty() {
                continue;
            }
            matched_schema_count += 1;
            catalog_rows.push(FilteredRow::Schema {
                catalog: catalog.name.clone(),
                name: schema.name.clone(),
                matched_relations: relation_rows.len(),
                total_relations: schema.tables.len(),
                label_match: schema_label_match,
            });
            catalog_rows.extend(relation_rows);
        }

        if catalog_label_match.is_none() && catalog_rows.is_empty() {
            continue;
        }
        rows.push(FilteredRow::Catalog {
            name: catalog.name.clone(),
            matched_schemas: matched_schema_count,
            total_schemas: catalog.schemas.len(),
            label_match: catalog_label_match,
        });
        rows.extend(catalog_rows);
    }
    rows
}

/// The total relation count across every catalog and schema in `tree`,
/// regardless of any filter: the "of m" half of the find row's own match
/// counter.
#[must_use]
pub fn total_relation_count(tree: &SchemaTree) -> usize {
    tree.catalogs
        .iter()
        .flat_map(|catalog| &catalog.schemas)
        .map(|schema| schema.tables.len())
        .sum()
}

/// How many rows in `rows` matched the query directly by their own label
/// (a relation match, or a catalog/schema whose own name matched rather
/// than only holding a matching descendant): the "n" half of the find
/// row's own match counter, so a query that only matches a schema or
/// catalog name still reports a non-zero count.
#[must_use]
pub fn matched_label_count(rows: &[FilteredRow]) -> usize {
    rows.iter()
        .filter(|row| match row {
            FilteredRow::Catalog { label_match, .. } | FilteredRow::Schema { label_match, .. } => {
                label_match.is_some()
            }
            FilteredRow::Relation { .. } => true,
        })
        .count()
}

/// Which catalogs and schemas hold at least one relation matching `query`,
/// and so must render expanded even if the user had them collapsed before
/// filtering began.
#[must_use]
pub fn expanded_ancestors_for_query(
    tree: &SchemaTree,
    query: &str,
) -> (HashSet<String>, HashSet<(String, String)>) {
    let mut catalogs = HashSet::new();
    let mut schemas = HashSet::new();
    for catalog in &tree.catalogs {
        for schema in &catalog.schemas {
            let has_match = schema
                .tables
                .iter()
                .any(|relation| label_match(&relation.name, query).is_some());
            if has_match {
                catalogs.insert(catalog.name.clone());
                schemas.insert((catalog.name.clone(), schema.name.clone()));
            }
        }
    }
    (catalogs, schemas)
}

/// The sidebar's expand/collapse choices, captured just before a filter
/// starts narrowing the tree, so they can be restored exactly once the
/// filter clears rather than merely re-collapsing everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollapseSnapshot {
    catalogs: HashSet<String>,
    schemas: HashSet<(String, String)>,
}

impl CollapseSnapshot {
    /// Capture `catalogs`/`schemas` as they stand right now.
    #[must_use]
    pub fn capture(catalogs: &HashSet<String>, schemas: &HashSet<(String, String)>) -> Self {
        Self {
            catalogs: catalogs.clone(),
            schemas: schemas.clone(),
        }
    }

    /// The exact collapsed sets this snapshot captured.
    #[must_use]
    pub fn into_parts(self) -> (HashSet<String>, HashSet<(String, String)>) {
        (self.catalogs, self.schemas)
    }
}

/// One row the filtered Scripts pane renders: `index` into the unfiltered
/// row list this was filtered from, plus the label's own match range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilteredScriptRow {
    pub index: usize,
    pub label_match: MatchRange,
}

/// Filter `rows` for `query`: a script row survives iff its own label
/// matches, in original order. An empty `query` matches nothing, by
/// [`label_match`]'s own contract.
#[tracing::instrument(name = "sidebar_filter_script_rows", skip(rows), fields(query_len = query.len()))]
pub fn filter_script_rows(rows: &[ScriptRow], query: &str) -> Vec<FilteredScriptRow> {
    rows.iter()
        .enumerate()
        .filter_map(|(index, row)| {
            label_match(&row.label, query)
                .map(|label_match| FilteredScriptRow { index, label_match })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use zsql_core::{Catalog, ColumnMeta, Relation, RelationKind, SchemaNs, SchemaTree};

    use super::{
        CollapseSnapshot, FilteredRow, FilteredScriptRow, expanded_ancestors_for_query,
        filter_script_rows, flatten_schema_tree_filtered, label_match, matched_label_count,
        total_relation_count,
    };
    use crate::ui::open_modal::PickerTarget;
    use crate::ui::sidebar::model::{ScriptRow, ScriptRowKind};

    fn relation(name: &str, kind: RelationKind, column_count: usize) -> Relation {
        Relation {
            name: name.to_owned(),
            kind,
            columns: (0..column_count)
                .map(|i| ColumnMeta {
                    name: format!("c{i}"),
                    type_name: "text".to_owned(),
                    nullable: false,
                })
                .collect(),
        }
    }

    fn two_schema_tree() -> SchemaTree {
        SchemaTree {
            catalogs: vec![Catalog {
                name: "zsql".to_owned(),
                schemas: vec![
                    SchemaNs {
                        name: "public".to_owned(),
                        tables: vec![
                            relation("orders", RelationKind::Table, 6),
                            relation("order_items", RelationKind::Table, 5),
                            relation("recent_orders", RelationKind::View, 0),
                        ],
                    },
                    SchemaNs {
                        name: "analytics".to_owned(),
                        tables: vec![relation("page_events", RelationKind::Table, 7)],
                    },
                ],
            }],
        }
    }

    #[test]
    fn label_match_is_case_insensitive_and_returns_a_byte_range() {
        assert_eq!(label_match("orders", "ORD"), Some(0..3));
        assert_eq!(label_match("recent_orders", "ord"), Some(7..10));
        assert_eq!(label_match("orders", "xyz"), None);
    }

    #[test]
    fn label_match_on_a_multibyte_label_lands_on_char_boundaries() {
        let label = "orders_caf\u{e9}";

        let range = label_match(label, "caf\u{e9}").expect("the needle occurs in the label");
        assert!(label.is_char_boundary(range.start));
        assert!(label.is_char_boundary(range.end));
        assert_eq!(&label[range], "caf\u{e9}");

        // For the char-boundary start one byte before the match above, this
        // same-length needle's end would land inside the two-byte final
        // character; that candidate must be skipped rather than panicking,
        // and no other start matches.
        assert_eq!(label_match(label, "afex"), None);
    }

    #[test]
    fn label_match_of_an_empty_query_never_matches() {
        assert_eq!(label_match("orders", ""), None);
    }

    #[test]
    fn an_empty_query_filters_out_every_row() {
        let tree = two_schema_tree();
        assert!(flatten_schema_tree_filtered(&tree, "").is_empty());
    }

    #[test]
    fn a_query_matching_nothing_produces_no_rows() {
        let tree = two_schema_tree();
        assert!(flatten_schema_tree_filtered(&tree, "invoices").is_empty());
    }

    #[test]
    fn a_relation_level_match_keeps_its_schema_and_catalog_ancestors() {
        let tree = two_schema_tree();
        let rows = flatten_schema_tree_filtered(&tree, "order_items");

        assert_eq!(
            rows,
            vec![
                FilteredRow::Catalog {
                    name: "zsql".to_owned(),
                    matched_schemas: 1,
                    total_schemas: 2,
                    label_match: None,
                },
                FilteredRow::Schema {
                    catalog: "zsql".to_owned(),
                    name: "public".to_owned(),
                    matched_relations: 1,
                    total_relations: 3,
                    label_match: None,
                },
                FilteredRow::Relation {
                    schema: "public".to_owned(),
                    name: "order_items".to_owned(),
                    kind: RelationKind::Table,
                    column_count: 5,
                    label_match: Some(0..11),
                },
            ]
        );
    }

    /// A tree whose two schemas each hold exactly one relation matching
    /// "event", so its counts are trivial to assert against.
    fn cross_schema_match_tree() -> SchemaTree {
        SchemaTree {
            catalogs: vec![Catalog {
                name: "zsql".to_owned(),
                schemas: vec![
                    SchemaNs {
                        name: "public".to_owned(),
                        tables: vec![relation("event_log", RelationKind::Table, 4)],
                    },
                    SchemaNs {
                        name: "analytics".to_owned(),
                        tables: vec![relation("page_events", RelationKind::Table, 7)],
                    },
                ],
            }],
        }
    }

    #[test]
    fn a_match_present_in_two_different_schemas_keeps_both_with_their_own_counts() {
        let tree = cross_schema_match_tree();
        let rows = flatten_schema_tree_filtered(&tree, "event");

        let schema_counts: Vec<(String, usize, usize)> = rows
            .iter()
            .filter_map(|row| match row {
                FilteredRow::Schema {
                    name,
                    matched_relations,
                    total_relations,
                    ..
                } => Some((name.clone(), *matched_relations, *total_relations)),
                _ => None,
            })
            .collect();
        assert_eq!(
            schema_counts,
            vec![("public".to_owned(), 1, 1), ("analytics".to_owned(), 1, 1)]
        );

        let relation_names: Vec<&str> = rows
            .iter()
            .filter_map(|row| match row {
                FilteredRow::Relation { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(relation_names, vec!["event_log", "page_events"]);
        assert_eq!(matched_label_count(&rows), 2);
        assert_eq!(total_relation_count(&tree), 2);
    }

    #[test]
    fn matched_label_count_counts_a_schema_only_match_that_holds_no_matching_relation() {
        let tree = two_schema_tree();
        let rows = flatten_schema_tree_filtered(&tree, "analytics");
        assert!(
            rows.iter()
                .all(|row| !matches!(row, FilteredRow::Relation { .. })),
            "this query must match only the schema's own name, no relation inside it"
        );
        assert_eq!(
            matched_label_count(&rows),
            1,
            "a schema-name-only match must still count toward the find row's counter"
        );
    }

    #[test]
    fn matching_is_case_insensitive() {
        let tree = two_schema_tree();
        let rows = flatten_schema_tree_filtered(&tree, "ORDER_ITEMS");
        let relation_names: Vec<&str> = rows
            .iter()
            .filter_map(|row| match row {
                FilteredRow::Relation { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(relation_names, vec!["order_items"]);
    }

    #[test]
    fn expanded_ancestors_for_query_names_only_catalogs_and_schemas_with_a_relation_match() {
        let tree = two_schema_tree();
        let (catalogs, schemas) = expanded_ancestors_for_query(&tree, "page_events");
        assert_eq!(catalogs, HashSet::from(["zsql".to_owned()]));
        assert_eq!(
            schemas,
            HashSet::from([("zsql".to_owned(), "analytics".to_owned())])
        );
    }

    #[test]
    fn expanded_ancestors_for_query_is_empty_with_no_matches() {
        let tree = two_schema_tree();
        let (catalogs, schemas) = expanded_ancestors_for_query(&tree, "nope");
        assert!(catalogs.is_empty());
        assert!(schemas.is_empty());
    }

    #[test]
    fn collapse_snapshot_round_trips_the_exact_sets_it_captured() {
        let mut catalogs = HashSet::new();
        catalogs.insert("zsql".to_owned());
        let mut schemas = HashSet::new();
        schemas.insert(("zsql".to_owned(), "analytics".to_owned()));

        let snapshot = CollapseSnapshot::capture(&catalogs, &schemas);

        // Mutating the originals after capture must not affect the snapshot:
        // it owns an independent copy.
        catalogs.clear();
        schemas.clear();

        let (restored_catalogs, restored_schemas) = snapshot.into_parts();
        assert_eq!(restored_catalogs, HashSet::from(["zsql".to_owned()]));
        assert_eq!(
            restored_schemas,
            HashSet::from([("zsql".to_owned(), "analytics".to_owned())])
        );
    }

    fn script_row(label: &str) -> ScriptRow {
        ScriptRow {
            kind: ScriptRowKind::Session,
            label: label.to_owned(),
            relative_time: "2s".to_owned(),
            target: PickerTarget::OpenSessionScript(label.to_owned()),
            selected: false,
        }
    }

    #[test]
    fn filter_script_rows_keeps_only_matching_rows_in_original_order() {
        let rows = vec![
            script_row("top-customers.sql"),
            script_row("revenue-report.sql"),
            script_row("cohort-debug.sql"),
        ];
        let filtered = filter_script_rows(&rows, "rev");
        assert_eq!(
            filtered,
            vec![FilteredScriptRow {
                index: 1,
                label_match: 0..3,
            }]
        );
    }

    #[test]
    fn filter_script_rows_is_case_insensitive() {
        let rows = vec![script_row("Revenue-Report.sql")];
        let filtered = filter_script_rows(&rows, "revenue");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].label_match, 0..7);
    }

    #[test]
    fn filter_script_rows_with_an_empty_query_matches_nothing() {
        let rows = vec![script_row("top-customers.sql")];
        assert!(filter_script_rows(&rows, "").is_empty());
    }
}
