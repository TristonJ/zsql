//! Pure sidebar state: flattening a `SchemaTree` (plus the view's collapse
//! state) into the rows the schema pane renders, mapping a relation's kind
//! to its icon/tint, and the small predicates the pane switcher, database
//! row, and scripts pane render from. Gpui-free so this logic is directly
//! unit-testable.

use std::collections::HashSet;

use zsql_core::{RelationKind, SchemaTree};
use zsql_ui::icon::IconName;
use zsql_ui::theme::Theme;

use crate::ui::open_modal::{LibraryScript, PickerTarget};
use crate::ui::tabs::TabId;

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

/// One named session script as the sidebar's scripts pane sees it -- built
/// from [`crate::session_store::list_session_scripts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionScript {
    /// The exact on-disk sibling file name (already carries `.sql`).
    pub file_name: String,
    pub relative_time: String,
}

/// Which of the scripts pane's groups a script row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptRowKind {
    Session,
    Library,
}

/// One row the scripts pane's connection/library groups render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRow {
    pub kind: ScriptRowKind,
    pub label: String,
    pub relative_time: String,
    pub target: PickerTarget,
    /// Whether this row's tab is the currently active tab
    pub selected: bool,
}

/// Build the scripts pane's connection then library rows: every named
/// session script in order, then every library file, each carrying its
/// relative-modified-time meta and whether it is the active tab.
#[must_use]
pub fn build_script_rows(
    active_tab_id: Option<TabId>,
    session_scripts: &[SessionScript],
    open_session_tabs: &[(String, TabId)],
    library_scripts: &[LibraryScript],
    open_library_tabs: &[(String, TabId)],
) -> Vec<ScriptRow> {
    let picker_sessions: Vec<crate::ui::open_modal::SessionScript> = session_scripts
        .iter()
        .map(|s| crate::ui::open_modal::SessionScript {
            file_name: s.file_name.clone(),
            relative_time: s.relative_time.clone(),
        })
        .collect();
    let picker_rows = crate::ui::open_modal::build_rows_with_open_sessions(
        "",
        &picker_sessions,
        open_session_tabs,
        library_scripts,
        open_library_tabs,
    );
    let mut rows = Vec::with_capacity(picker_rows.len());
    let mut sessions_iter = session_scripts.iter();
    let mut library_iter = library_scripts.iter();
    for picker_row in picker_rows {
        let selected = matches!(
            &picker_row.target,
            PickerTarget::FocusTab(id) if Some(*id) == active_tab_id
        );
        let (kind, relative_time) = match picker_row.section {
            crate::ui::open_modal::PickerSection::Connection => (
                ScriptRowKind::Session,
                sessions_iter
                    .next()
                    .map_or_else(String::new, |s| s.relative_time.clone()),
            ),
            crate::ui::open_modal::PickerSection::Library => (
                ScriptRowKind::Library,
                library_iter
                    .next()
                    .map_or_else(String::new, |s| s.relative_time.clone()),
            ),
        };
        rows.push(ScriptRow {
            kind,
            label: picker_row.label,
            relative_time,
            target: picker_row.target,
            selected,
        });
    }
    rows
}

/// Which full-height pane the sidebar currently shows
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarPane {
    #[default]
    Schema,
    Scripts,
}

/// The Scripts pane tab's trailing count
#[must_use]
pub fn scripts_count(rows: &[ScriptRow]) -> usize {
    rows.len()
}

/// Whether the sidebar's database row should render
#[must_use]
pub fn db_row_visible(pane: SidebarPane, available_database_count: usize) -> bool {
    pane == SidebarPane::Schema && available_database_count > 1
}

/// Whether a library row's script is currently open as a tab on this
/// connection
#[must_use]
pub fn library_row_is_open(row: &ScriptRow) -> bool {
    row.kind == ScriptRowKind::Library && matches!(row.target, PickerTarget::FocusTab(_))
}

/// Whether the scripts pane's connection group should show the empty-state
/// invitation instead of a row list
#[must_use]
pub fn scripts_pane_shows_empty_state(rows: &[ScriptRow]) -> bool {
    !rows.iter().any(|row| row.kind == ScriptRowKind::Session)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use zsql_core::{Catalog, ColumnMeta, Relation, RelationKind, SchemaNs, SchemaTree};
    use zsql_ui::icon::IconName;
    use zsql_ui::theme::Theme;

    use super::{
        ScriptRow, ScriptRowKind, SessionScript, SidebarPane, build_script_rows, db_row_visible,
        library_row_is_open, scripts_count, scripts_pane_shows_empty_state,
    };
    use crate::ui::open_modal::{LibraryScript, PickerTarget};

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

    fn session_script(file_name: &str, relative_time: &str) -> SessionScript {
        SessionScript {
            file_name: file_name.to_owned(),
            relative_time: relative_time.to_owned(),
        }
    }

    fn library_script(name: &str, relative_time: &str) -> LibraryScript {
        LibraryScript {
            name: name.to_owned(),
            relative_time: relative_time.to_owned(),
        }
    }

    #[test]
    fn script_rows_list_session_scripts_before_library_scripts() {
        let sessions = vec![session_script("top-customers.sql", "2s")];
        let library = vec![library_script("revenue-report", "2w")];

        let rows = build_script_rows(None, &sessions, &[], &library, &[]);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, ScriptRowKind::Session);
        assert_eq!(rows[0].label, "top-customers.sql");
        assert_eq!(rows[0].relative_time, "2s");
        assert_eq!(rows[1].kind, ScriptRowKind::Library);
        assert_eq!(rows[1].label, "revenue-report.sql");
        assert_eq!(rows[1].relative_time, "2w");
    }

    #[test]
    fn a_library_row_never_carries_the_accent_type_badge_role() {
        // This model carries no color itself (that lives in the render
        // layer), but every `Library` row must still resolve to a plain
        // `ScriptRowKind::Library` classification so the render layer has
        // no accent-eligible signal to key off for these rows -- the
        // accent color stays reserved for a relation's type badge and the
        // open-tab dot.
        let library = vec![library_script("revenue-report", "2w")];
        let rows = build_script_rows(None, &[], &[], &library, &[]);
        assert_eq!(rows[0].kind, ScriptRowKind::Library);
    }

    #[test]
    fn the_row_for_the_active_open_tab_carries_the_selected_flag() {
        let sessions = vec![
            session_script("top-customers.sql", "2s"),
            session_script("cohort-debug.sql", "1w"),
        ];
        let open_session_tabs = vec![
            ("top-customers.sql".to_owned(), 1u64),
            ("cohort-debug.sql".to_owned(), 2u64),
        ];

        let rows = build_script_rows(Some(2), &sessions, &open_session_tabs, &[], &[]);

        assert!(!rows[0].selected);
        assert!(rows[1].selected);
    }

    #[test]
    fn no_row_is_selected_when_the_active_tab_is_not_a_named_script() {
        let sessions = vec![session_script("top-customers.sql", "2s")];
        let open_session_tabs = vec![("top-customers.sql".to_owned(), 1u64)];
        let rows = build_script_rows(Some(99), &sessions, &open_session_tabs, &[], &[]);
        assert!(!rows[0].selected);
    }

    #[test]
    fn a_named_session_script_not_open_anywhere_targets_open_session_script() {
        let sessions = vec![session_script("top-customers.sql", "2s")];
        let rows = build_script_rows(None, &sessions, &[], &[], &[]);
        assert_eq!(
            rows[0].target,
            PickerTarget::OpenSessionScript("top-customers.sql".to_owned())
        );
        assert_eq!(rows[0].relative_time, "2s");
        assert!(!rows[0].selected);
    }

    #[test]
    fn a_library_rows_target_dedupes_against_an_already_open_library_tab() {
        let library = vec![library_script("revenue-report", "2w")];
        let open_library_tabs = vec![("revenue-report".to_owned(), 7u64)];

        let rows = build_script_rows(Some(7), &[], &[], &library, &open_library_tabs);

        assert_eq!(rows[0].target, PickerTarget::FocusTab(7));
        assert!(rows[0].selected);
    }

    #[test]
    fn a_library_row_not_open_anywhere_targets_open_library_and_is_never_selected() {
        let library = vec![library_script("revenue-report", "2w")];
        let rows = build_script_rows(None, &[], &[], &library, &[]);
        assert_eq!(
            rows[0].target,
            PickerTarget::OpenLibrary("revenue-report".to_owned())
        );
        assert!(!rows[0].selected);
    }

    #[test]
    fn empty_session_and_library_lists_produce_no_rows() {
        let rows = build_script_rows(None, &[], &[], &[], &[]);
        assert!(rows.is_empty());
    }

    #[test]
    fn sidebar_pane_defaults_to_schema() {
        assert_eq!(SidebarPane::default(), SidebarPane::Schema);
    }

    #[test]
    fn scripts_count_sums_session_and_library_rows() {
        assert_eq!(scripts_count(&[]), 0);

        let sessions = vec![session_script("top-customers.sql", "2s")];
        let library = vec![
            library_script("revenue-report", "2w"),
            library_script("slow-queries", "1mo"),
        ];
        let rows = build_script_rows(None, &sessions, &[], &library, &[]);
        assert_eq!(scripts_count(&rows), 3);
    }

    #[test]
    fn db_row_is_visible_only_in_the_schema_pane_with_more_than_one_database() {
        assert!(!db_row_visible(SidebarPane::Schema, 0));
        assert!(!db_row_visible(SidebarPane::Schema, 1));
        assert!(db_row_visible(SidebarPane::Schema, 2));
        // Even with multiple databases, the row never shows in the scripts
        // pane -- scripts are connection-scoped, not database-scoped.
        assert!(!db_row_visible(SidebarPane::Scripts, 2));
    }

    #[test]
    fn library_row_is_open_only_for_a_library_row_already_resolved_to_a_focus_tab_target() {
        let open_row = ScriptRow {
            kind: ScriptRowKind::Library,
            label: "revenue-report.sql".to_owned(),
            relative_time: "2w".to_owned(),
            target: PickerTarget::FocusTab(7),
            selected: false,
        };
        assert!(library_row_is_open(&open_row));

        let closed_row = ScriptRow {
            target: PickerTarget::OpenLibrary("revenue-report".to_owned()),
            ..open_row.clone()
        };
        assert!(!library_row_is_open(&closed_row));

        // A session row's target is always `FocusTab`, but a connection
        // script is always open by the discard-on-close rule, so it must
        // never carry the dot regardless of its target shape.
        let session_row = ScriptRow {
            kind: ScriptRowKind::Session,
            ..open_row
        };
        assert!(!library_row_is_open(&session_row));
    }

    #[test]
    fn scripts_pane_empty_state_triggers_only_with_zero_named_session_scripts() {
        let library = vec![library_script("revenue-report", "2w")];
        let no_sessions = build_script_rows(None, &[], &[], &library, &[]);
        assert!(scripts_pane_shows_empty_state(&no_sessions));

        let sessions = vec![session_script("top-customers.sql", "2s")];
        let with_sessions = build_script_rows(None, &sessions, &[], &library, &[]);
        assert!(!scripts_pane_shows_empty_state(&with_sessions));

        assert!(scripts_pane_shows_empty_state(&[]));
    }
}
