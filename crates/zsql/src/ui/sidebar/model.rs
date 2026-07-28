//! Pure sidebar tree state: flattening a `SchemaTree` (plus the view's
//! collapse state) into the rows the sidebar renders, and mapping a
//! relation's kind to its icon/tint. Gpui-free so this logic is directly
//! unit-testable.

use std::collections::HashSet;

use zsql_core::{RelationKind, SchemaTree};
use zsql_ui::icon::IconName;
use zsql_ui::theme::Theme;

/// One flattened, currently-visible sidebar row. Built by
/// [`flatten_schema_tree`] from a `SchemaTree` plus the view's collapse
/// state
#[derive(Debug, Clone, PartialEq)]
pub enum SidebarRow {
    /// A catalog (database) row.
    Catalog {
        name: String,
        expanded: bool,
        schema_count: usize,
    },
    /// A schema (namespace) row, nested under a catalog.
    Schema {
        catalog: String,
        name: String,
        expanded: bool,
        relation_count: usize,
    },
    /// A table/view/matview/partitioned-table row, nested under a schema.
    Relation {
        schema: String,
        name: String,
        kind: RelationKind,
        column_count: usize,
    },
}

/// Map a [`RelationKind`] to the icon its sidebar row badge renders.
pub fn relation_icon_name(kind: RelationKind) -> IconName {
    match kind {
        RelationKind::Table => IconName::Table,
        RelationKind::View => IconName::View,
        RelationKind::MatView => IconName::MaterializedView,
        RelationKind::Partitioned => IconName::PartitionedTable,
    }
}

/// Map a [`RelationKind`] to the tint its sidebar row badge renders with.
pub fn relation_tint(kind: RelationKind, active_theme: &Theme) -> u32 {
    match kind {
        RelationKind::Table => active_theme.colors.accent,
        RelationKind::View => active_theme.colors.kind_view,
        RelationKind::MatView => active_theme.colors.kind_matview,
        RelationKind::Partitioned => active_theme.colors.kind_partitioned,
    }
}

/// Flatten `tree` into the currently-visible sidebar rows, honoring which
/// catalogs/schemas are collapsed
pub fn flatten_schema_tree(
    tree: &SchemaTree,
    collapsed_catalogs: &HashSet<String>,
    collapsed_schemas: &HashSet<(String, String)>,
) -> Vec<SidebarRow> {
    let mut rows = Vec::new();
    for catalog in &tree.catalogs {
        let catalog_expanded = !collapsed_catalogs.contains(&catalog.name);
        rows.push(SidebarRow::Catalog {
            name: catalog.name.clone(),
            expanded: catalog_expanded,
            schema_count: catalog.schemas.len(),
        });
        if !catalog_expanded {
            continue;
        }

        for schema in &catalog.schemas {
            let key = (catalog.name.clone(), schema.name.clone());
            let schema_expanded = !collapsed_schemas.contains(&key);
            rows.push(SidebarRow::Schema {
                catalog: catalog.name.clone(),
                name: schema.name.clone(),
                expanded: schema_expanded,
                relation_count: schema.tables.len(),
            });
            if !schema_expanded {
                continue;
            }

            for relation in &schema.tables {
                rows.push(SidebarRow::Relation {
                    schema: schema.name.clone(),
                    name: relation.name.clone(),
                    kind: relation.kind,
                    column_count: relation.columns.len(),
                });
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use zsql_core::{Catalog, ColumnMeta, Relation, RelationKind, SchemaNs, SchemaTree};
    use zsql_ui::icon::IconName;
    use zsql_ui::theme::Theme;

    use super::{SidebarRow, flatten_schema_tree, relation_icon_name, relation_tint};

    fn sample_tree() -> SchemaTree {
        SchemaTree {
            catalogs: vec![Catalog {
                name: "zsql".to_owned(),
                schemas: vec![
                    SchemaNs {
                        name: "public".to_owned(),
                        tables: vec![
                            Relation {
                                name: "orders".to_owned(),
                                kind: RelationKind::Table,
                                columns: vec![
                                    ColumnMeta {
                                        name: "id".to_owned(),
                                        type_name: "int8".to_owned(),
                                        nullable: false,
                                    },
                                    ColumnMeta {
                                        name: "status".to_owned(),
                                        type_name: "text".to_owned(),
                                        nullable: false,
                                    },
                                ],
                            },
                            Relation {
                                name: "recent_orders".to_owned(),
                                kind: RelationKind::View,
                                columns: vec![],
                            },
                            Relation {
                                name: "recent_orders_mv".to_owned(),
                                kind: RelationKind::MatView,
                                columns: vec![],
                            },
                            Relation {
                                name: "events".to_owned(),
                                kind: RelationKind::Partitioned,
                                columns: vec![],
                            },
                        ],
                    },
                    SchemaNs {
                        name: "empty_ns".to_owned(),
                        tables: vec![],
                    },
                ],
            }],
        }
    }

    #[test]
    fn relation_icon_name_maps_every_relation_kind_to_a_distinct_icon() {
        let icons = [
            relation_icon_name(RelationKind::Table),
            relation_icon_name(RelationKind::View),
            relation_icon_name(RelationKind::MatView),
            relation_icon_name(RelationKind::Partitioned),
        ];
        assert_eq!(icons[0], IconName::Table);
        assert_eq!(icons[1], IconName::View);
        assert_eq!(icons[2], IconName::MaterializedView);
        assert_eq!(icons[3], IconName::PartitionedTable);
        for (i, a) in icons.iter().enumerate() {
            for b in &icons[i + 1..] {
                assert_ne!(a, b, "every relation kind must map to a distinct icon");
            }
        }
    }

    #[test]
    fn relation_tint_maps_every_relation_kind_to_a_named_color_constant() {
        let theme = Theme::default();
        assert_eq!(
            relation_tint(RelationKind::Table, &theme),
            theme.colors.accent
        );
        assert_eq!(
            relation_tint(RelationKind::View, &theme),
            theme.colors.kind_view
        );
        assert_eq!(
            relation_tint(RelationKind::MatView, &theme),
            theme.colors.kind_matview
        );
        assert_eq!(
            relation_tint(RelationKind::Partitioned, &theme),
            theme.colors.kind_partitioned
        );
    }

    #[test]
    fn everything_expanded_by_default_shows_the_full_tree() {
        let tree = sample_tree();
        let rows = flatten_schema_tree(&tree, &HashSet::new(), &HashSet::new());

        assert_eq!(
            rows,
            vec![
                SidebarRow::Catalog {
                    name: "zsql".to_owned(),
                    expanded: true,
                    schema_count: 2,
                },
                SidebarRow::Schema {
                    catalog: "zsql".to_owned(),
                    name: "public".to_owned(),
                    expanded: true,
                    relation_count: 4,
                },
                SidebarRow::Relation {
                    schema: "public".to_owned(),
                    name: "orders".to_owned(),
                    kind: RelationKind::Table,
                    column_count: 2,
                },
                SidebarRow::Relation {
                    schema: "public".to_owned(),
                    name: "recent_orders".to_owned(),
                    kind: RelationKind::View,
                    column_count: 0,
                },
                SidebarRow::Relation {
                    schema: "public".to_owned(),
                    name: "recent_orders_mv".to_owned(),
                    kind: RelationKind::MatView,
                    column_count: 0,
                },
                SidebarRow::Relation {
                    schema: "public".to_owned(),
                    name: "events".to_owned(),
                    kind: RelationKind::Partitioned,
                    column_count: 0,
                },
                SidebarRow::Schema {
                    catalog: "zsql".to_owned(),
                    name: "empty_ns".to_owned(),
                    expanded: true,
                    relation_count: 0,
                },
            ]
        );
    }

    #[test]
    fn a_collapsed_catalog_hides_every_descendant() {
        let tree = sample_tree();
        let mut collapsed_catalogs = HashSet::new();
        collapsed_catalogs.insert("zsql".to_owned());

        let rows = flatten_schema_tree(&tree, &collapsed_catalogs, &HashSet::new());

        assert_eq!(
            rows,
            vec![SidebarRow::Catalog {
                name: "zsql".to_owned(),
                expanded: false,
                schema_count: 2,
            }]
        );
    }

    #[test]
    fn a_collapsed_schema_hides_its_relations_but_not_sibling_schemas() {
        let tree = sample_tree();
        let mut collapsed_schemas = HashSet::new();
        collapsed_schemas.insert(("zsql".to_owned(), "public".to_owned()));

        let rows = flatten_schema_tree(&tree, &HashSet::new(), &collapsed_schemas);

        assert_eq!(
            rows,
            vec![
                SidebarRow::Catalog {
                    name: "zsql".to_owned(),
                    expanded: true,
                    schema_count: 2,
                },
                SidebarRow::Schema {
                    catalog: "zsql".to_owned(),
                    name: "public".to_owned(),
                    expanded: false,
                    relation_count: 4,
                },
                SidebarRow::Schema {
                    catalog: "zsql".to_owned(),
                    name: "empty_ns".to_owned(),
                    expanded: true,
                    relation_count: 0,
                },
            ]
        );
    }

    #[test]
    fn an_empty_tree_produces_no_rows() {
        let rows = flatten_schema_tree(&SchemaTree::default(), &HashSet::new(), &HashSet::new());
        assert!(rows.is_empty());
    }
}
