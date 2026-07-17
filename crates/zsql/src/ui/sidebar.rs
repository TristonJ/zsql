//! The schema sidebar: a tree of the connected database's catalog ->
//! schema -> relation structure, driven by a `Session`'s introspected
//! [`SchemaTree`]. Clicking a relation previews it into the results grid.
//!
//! Disclosure is ASCII-only (`v` expanded, `>` collapsed) and every kind
//! label is plain text (`table`/`view`/`matview`/`partitioned`) rather than
//! a glyph icon, per this crate's ASCII-only source convention; icons are a
//! later polish.
//!
//! This view holds an `Entity<Session>` and reads `session.schema()` fresh
//! on every sync rather than receiving a cloned `SchemaTree`, the same
//! entity-read pattern `ui::results::ResultsView` uses for its `Session`.
//! Expand/collapse state lives entirely in this view (`collapsed_catalogs`/
//! `collapsed_schemas`); the flattened, currently-visible row list
//! (`rows`) is cached and only recomputed when the session's schema
//! changes or a disclosure is toggled, and only the rows actually scrolled
//! into view are ever built into elements (`uniform_list`), so this stays
//! efficient for a schema with many relations.
//!
//! `Session` notifies on every `QueryEvent` a streaming preview query emits,
//! not only when its schema changes (e.g. once per batch while rows are
//! still arriving). This view's `cx.observe` callback therefore compares
//! `Session::schema_generation()` against the generation it last flattened
//! and skips re-flattening entirely when it is unchanged, so a streaming
//! preview never re-walks the (unchanged) schema tree once per batch.

use std::collections::HashSet;

use gpui::{
    ClickEvent, Context, Div, Entity, Render, SharedString, Stateful, Window, div, prelude::*, px,
    rgb, rgba, uniform_list,
};
use zsql_core::{RelationKind, SchemaTree};

use super::results::ResultsView;
use super::theme;
use crate::session::{SchemaState, Session};

/// One flattened, currently-visible sidebar row. Built by
/// [`flatten_schema_tree`] from a `SchemaTree` plus the view's collapse
/// state; owns small copies of the names it needs (never the whole tree),
/// so rebuilding this list is proportional to the number of *visible* rows,
/// not the size of a collapsed subtree.
#[derive(Debug, Clone, PartialEq)]
enum SidebarRow {
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

/// The schema sidebar view.
pub struct SidebarView {
    session: Entity<Session>,
    results: Entity<ResultsView>,
    collapsed_catalogs: HashSet<String>,
    collapsed_schemas: HashSet<(String, String)>,
    /// The relation most recently clicked, for highlighting its row.
    selected_relation: Option<(String, String)>,
    rows: Vec<SidebarRow>,
    /// The session's `schema_generation()` as of the last time `rows` was
    /// rebuilt from it. Lets the `cx.observe` callback tell "the schema
    /// itself changed" apart from "the session notified for some other
    /// reason" without diffing or cloning the `SchemaTree`.
    synced_schema_generation: u64,
}

impl SidebarView {
    /// Build a sidebar over `session`, previewing clicked relations into
    /// `results`.
    ///
    /// Subscribes to `session` for the lifetime of this view: whenever
    /// `session`'s `schema_generation()` has advanced since the last sync
    /// (including once introspection completes or fails), this re-syncs the
    /// flattened row list from its latest `schema()` state. A `cx.notify()`
    /// the session fires for an unrelated reason (e.g. a streaming preview
    /// query's batches) leaves the schema generation unchanged and is
    /// skipped.
    #[must_use]
    pub fn new(
        session: Entity<Session>,
        results: Entity<ResultsView>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&session, |view: &mut Self, _session, cx| {
            if view.sync_rows_if_schema_changed(cx) {
                cx.notify();
            }
        })
        .detach();

        let mut view = Self {
            session,
            results,
            collapsed_catalogs: HashSet::new(),
            collapsed_schemas: HashSet::new(),
            selected_relation: None,
            rows: Vec::new(),
            synced_schema_generation: 0,
        };
        view.sync_rows(cx);
        view
    }

    /// Rebuild `rows` from the session's current schema state and this
    /// view's collapse sets, and record the schema generation it was built
    /// from. Everything is expanded by default (nothing in
    /// `collapsed_catalogs`/`collapsed_schemas` yet), so a freshly
    /// introspected tree shows its relations immediately without requiring
    /// any clicks. Always re-flattens regardless of generation -- callers
    /// that already know the schema (or the collapse state) changed, such as
    /// `toggle_catalog`/`toggle_schema` and the initial build in `new`,
    /// should call this directly; [`Self::sync_rows_if_schema_changed`] is
    /// for the generation-gated `cx.observe` path.
    fn sync_rows(&mut self, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        self.synced_schema_generation = session.schema_generation();
        self.rows = match session.schema() {
            SchemaState::Ready(tree) => {
                flatten_schema_tree(tree, &self.collapsed_catalogs, &self.collapsed_schemas)
            }
            SchemaState::NotLoaded | SchemaState::Loading | SchemaState::Error(_) => Vec::new(),
        };
    }

    /// Re-flatten `rows` only if the session's schema has actually changed
    /// since the last sync. Returns whether it did (and thus whether `rows`
    /// was rebuilt), so the `cx.observe` callback knows whether a
    /// `cx.notify()` of its own is warranted.
    fn sync_rows_if_schema_changed(&mut self, cx: &mut Context<Self>) -> bool {
        let current_generation = self.session.read(cx).schema_generation();
        if current_generation == self.synced_schema_generation {
            return false;
        }
        self.sync_rows(cx);
        true
    }

    fn toggle_catalog(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.collapsed_catalogs.remove(name) {
            self.collapsed_catalogs.insert(name.to_owned());
        }
        self.sync_rows(cx);
        cx.notify();
    }

    fn toggle_schema(&mut self, catalog: &str, schema: &str, cx: &mut Context<Self>) {
        let key = (catalog.to_owned(), schema.to_owned());
        if !self.collapsed_schemas.remove(&key) {
            self.collapsed_schemas.insert(key);
        }
        self.sync_rows(cx);
        cx.notify();
    }

    /// Preview `schema.relation`: mark it selected (for row highlighting),
    /// update the results grid's source label, and dispatch the preview
    /// query via `Session::preview_relation`.
    fn preview(&mut self, schema: &str, relation: &str, cx: &mut Context<Self>) {
        self.selected_relation = Some((schema.to_owned(), relation.to_owned()));

        let label = format!("{schema}.{relation}");
        self.results
            .update(cx, |results, cx| results.set_source_label(label, cx));

        self.session.update(cx, |session, cx| {
            session.preview_relation(schema, relation, cx).detach();
        });

        cx.notify();
    }

    /// The "SCHEMA" header bar.
    fn render_header() -> Div {
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .h(theme::SIDEBAR_HEADER_HEIGHT)
            .px_3()
            .border_b_1()
            .border_color(rgb(theme::LINE_SOFT))
            .child(
                div()
                    .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
                    .text_color(rgb(theme::FAINT))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("SCHEMA"),
            )
    }

    /// The main content area: the tree when a schema is loaded, or a
    /// centered prompt/status message for every other `SchemaState`.
    fn render_body(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let placeholder = {
            let session = self.session.read(cx);
            match session.schema() {
                SchemaState::NotLoaded => Some((
                    theme::FAINT,
                    "No schema",
                    "Connect to a database to browse its schema.".to_owned(),
                )),
                SchemaState::Loading => Some((
                    theme::FAINT,
                    "Loading schema...",
                    "Fetching catalogs, schemas, and relations.".to_owned(),
                )),
                SchemaState::Error(message) => {
                    Some((theme::STATUS_ERROR, "Schema unavailable", message.clone()))
                }
                SchemaState::Ready(tree) if tree.catalogs.is_empty() => Some((
                    theme::FAINT,
                    "No catalogs",
                    "The connected database reported no catalogs.".to_owned(),
                )),
                SchemaState::Ready(_) => None,
            }
        };

        match placeholder {
            Some((color, title, detail)) => {
                Self::render_placeholder(color, title, &detail).into_any_element()
            }
            None => self.render_tree(cx).into_any_element(),
        }
    }

    /// A centered title + detail message shown in place of the tree for any
    /// non-ready `SchemaState`.
    fn render_placeholder(title_color: u32, title: &str, detail: &str) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_2()
            .px_4()
            .text_center()
            .child(
                div()
                    .text_size(px(theme::SIDEBAR_ROW_TEXT_SIZE))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(title_color))
                    .child(title.to_owned()),
            )
            .child(
                div()
                    .text_size(px(theme::SIDEBAR_META_TEXT_SIZE))
                    .text_color(rgb(theme::FAINT))
                    .child(detail.to_owned()),
            )
    }

    /// The virtualized tree body: only rows scrolled into view are built.
    fn render_tree(&mut self, cx: &mut Context<Self>) -> Stateful<Div> {
        let row_count = self.rows.len();
        div()
            .id("sidebar-tree")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .py(px(theme::SIDEBAR_TREE_PADDING_Y))
            .child(
                uniform_list(
                    "sidebar-rows",
                    row_count,
                    cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                        range
                            .map(|ix| this.render_row(&this.rows[ix], ix, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1(),
            )
    }

    /// Render one flattened row, dispatching on its kind.
    fn render_row(&self, row: &SidebarRow, ix: usize, cx: &Context<Self>) -> Stateful<Div> {
        match row {
            SidebarRow::Catalog {
                name,
                expanded,
                schema_count,
            } => {
                let name_owned = name.clone();
                row_shell(theme::SIDEBAR_INDENT_L0)
                    .id(ix)
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(theme::RAISE)))
                    .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                        view.toggle_catalog(&name_owned, cx);
                    }))
                    .child(disclosure_glyph(*expanded))
                    .child(row_label(name.clone()))
                    .when(!expanded, |el| {
                        el.child(row_meta(format!("{schema_count} schemas")))
                    })
            }
            SidebarRow::Schema {
                catalog,
                name,
                expanded,
                relation_count,
            } => {
                let catalog_owned = catalog.clone();
                let name_owned = name.clone();
                row_shell(theme::SIDEBAR_INDENT_L1)
                    .id(ix)
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(theme::RAISE)))
                    .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                        view.toggle_schema(&catalog_owned, &name_owned, cx);
                    }))
                    .child(disclosure_glyph(*expanded))
                    .child(row_label(name.clone()))
                    .when(!expanded, |el| {
                        el.child(row_meta(format!("{relation_count} rel")))
                    })
            }
            SidebarRow::Relation {
                schema,
                name,
                kind,
                column_count,
            } => {
                let schema_owned = schema.clone();
                let name_owned = name.clone();
                let selected = self
                    .selected_relation
                    .as_ref()
                    .is_some_and(|(s, r)| s == schema && r == name);

                let mut shell = row_shell(theme::SIDEBAR_INDENT_L2)
                    .id(ix)
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(theme::RAISE)))
                    .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                        view.preview(&schema_owned, &name_owned, cx);
                    }))
                    .child(disclosure_spacer())
                    .child(row_label(name.clone()))
                    .child(row_kind(kind_label(*kind)))
                    .child(row_count(format!("{column_count} cols")));

                if selected {
                    shell = shell
                        .bg(rgba(theme::SIDEBAR_SELECTED_BG))
                        .border_l_2()
                        .border_color(rgb(theme::TEAL));
                }
                shell
            }
        }
    }
}

impl Render for SidebarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(theme::PANEL))
            .child(Self::render_header())
            .child(self.render_body(cx))
    }
}

/// Shared chrome for a tree row: height, indent, gap, monospace text.
fn row_shell(indent: f32) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(theme::SIDEBAR_ROW_GAP))
        .h(theme::SIDEBAR_ROW_HEIGHT)
        .pl(px(indent))
        .pr_3()
        .flex_shrink_0()
        .font_family("monospace")
        .text_size(px(theme::SIDEBAR_ROW_TEXT_SIZE))
        .text_color(rgb(theme::TEXT))
}

/// The ASCII disclosure glyph: `v` expanded, `>` collapsed.
fn disclosure_glyph(expanded: bool) -> Div {
    div()
        .flex_shrink_0()
        .w(px(theme::SIDEBAR_DISCLOSURE_WIDTH))
        .text_color(rgb(theme::FAINT))
        .child(if expanded { "v" } else { ">" })
}

/// Blank space the width of a disclosure glyph, keeping a leaf relation
/// row's label aligned with its parent schema's disclosure-bearing rows.
fn disclosure_spacer() -> Div {
    div().flex_shrink_0().w(px(theme::SIDEBAR_DISCLOSURE_WIDTH))
}

/// A row's primary label, truncating rather than wrapping or overflowing.
fn row_label(text: impl Into<SharedString>) -> Div {
    div().flex_1().min_w_0().truncate().child(text.into())
}

/// A row's trailing affordance (a relation/column count), right-aligned via
/// `ml_auto` when it is the first such child, and otherwise simply
/// following the previous one.
fn row_meta(text: impl Into<SharedString>) -> Div {
    div()
        .flex_shrink_0()
        .ml_auto()
        .pl_2()
        .text_size(px(theme::SIDEBAR_META_TEXT_SIZE))
        .text_color(rgb(theme::FAINT))
        .font_family("monospace")
        .child(text.into())
}

/// A relation row's kind label (table/view/matview/partitioned): smaller
/// than [`row_count`]'s text, and always the first trailing affordance, so
/// it alone carries the `ml_auto` that pushes the whole trailing group
/// (kind label + column count) to the row's right edge.
fn row_kind(text: impl Into<SharedString>) -> Div {
    div()
        .flex_shrink_0()
        .ml_auto()
        .pl_2()
        .text_size(px(theme::SIDEBAR_KIND_TEXT_SIZE))
        .text_color(rgb(theme::FAINT))
        .font_family("monospace")
        .child(text.into())
}

/// A relation row's column count, following [`row_kind`] in normal flow
/// (no `ml_auto` of its own -- `row_kind` already pushed the pair to the
/// right edge).
fn row_count(text: impl Into<SharedString>) -> Div {
    div()
        .flex_shrink_0()
        .pl_2()
        .text_size(px(theme::SIDEBAR_META_TEXT_SIZE))
        .text_color(rgb(theme::FAINT))
        .font_family("monospace")
        .child(text.into())
}

/// Map a [`RelationKind`] to its ASCII text label.
fn kind_label(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Table => "table",
        RelationKind::View => "view",
        RelationKind::MatView => "matview",
        RelationKind::Partitioned => "partitioned",
    }
}

/// Flatten `tree` into the currently-visible sidebar rows, honoring which
/// catalogs/schemas are collapsed. A collapsed node's descendants are never
/// walked, so this costs time proportional to the rows actually produced,
/// not the full size of a schema with many collapsed relations.
fn flatten_schema_tree(
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

    use super::{SidebarRow, flatten_schema_tree, kind_label};

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
    fn kind_label_maps_every_relation_kind_to_ascii_text() {
        assert_eq!(kind_label(RelationKind::Table), "table");
        assert_eq!(kind_label(RelationKind::View), "view");
        assert_eq!(kind_label(RelationKind::MatView), "matview");
        assert_eq!(kind_label(RelationKind::Partitioned), "partitioned");
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

/// gpui headless render-smoke tests: no assertion on rendered content, only
/// that building and painting one frame over each `SchemaState` (and a
/// populated `SchemaTree`) never panics. Uses `TestAppContext`'s
/// `TestPlatform` (a headless mock of the platform layer), so this needs no
/// real display server, matching `ui::results`'s render tests.
#[cfg(test)]
mod render_tests {
    use gpui::AppContext as _;
    use zsql_core::{Catalog, ColumnMeta, Relation, RelationKind, SchemaNs, SchemaTree};

    use super::SidebarView;
    use crate::session::{SchemaState, Session};
    use crate::ui::results::ResultsView;

    fn sample_schema_tree() -> SchemaTree {
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
                                columns: vec![ColumnMeta {
                                    name: "id".to_owned(),
                                    type_name: "int8".to_owned(),
                                    nullable: false,
                                }],
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

    fn build(cx: &mut gpui::TestAppContext, schema: SchemaState) {
        let session = cx.new(|_cx| Session::new_for_schema_test(schema));
        let session_for_results = session.clone();
        cx.add_window_view(|_window, cx| {
            let results = cx.new(|cx| ResultsView::new(session_for_results, "", cx));
            SidebarView::new(session, results, cx)
        });
    }

    #[gpui::test]
    fn renders_a_populated_schema_tree_without_panicking(cx: &mut gpui::TestAppContext) {
        build(cx, SchemaState::Ready(sample_schema_tree()));
    }

    #[gpui::test]
    fn renders_every_non_ready_schema_state_without_panicking(cx: &mut gpui::TestAppContext) {
        for schema in [
            SchemaState::NotLoaded,
            SchemaState::Loading,
            SchemaState::Error("permission denied for schema pg_catalog".to_owned()),
            SchemaState::Ready(SchemaTree::default()),
        ] {
            build(cx, schema);
        }
    }

    /// Pins the guard `sync_rows_if_schema_changed` adds: a session
    /// `cx.notify()` that does not advance `schema_generation` (e.g. one
    /// fired mid-stream by a preview query's `QueryEvent`s) must leave the
    /// sidebar's cached `rows`/`synced_schema_generation` exactly as they
    /// were, not re-flatten the (unchanged) schema tree.
    #[gpui::test]
    fn an_unrelated_session_notify_does_not_reflatten_the_row_cache(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_results = session.clone();
        let session_for_view = session.clone();
        let (sidebar, vcx) = cx.add_window_view(|_window, cx| {
            let results = cx.new(|cx| ResultsView::new(session_for_results, "", cx));
            SidebarView::new(session_for_view, results, cx)
        });

        let (generation_after_build, row_count_after_build) = sidebar.update(vcx, |view, _cx| {
            (view.synced_schema_generation, view.rows.len())
        });
        assert!(
            row_count_after_build > 0,
            "the sample tree should have flattened into at least one row"
        );

        // A notify that does not touch `schema` -- standing in for one of
        // the per-batch notifies `Session::apply_query_event` fires while a
        // preview query streams.
        session.update(vcx, |_session, cx| cx.notify());
        vcx.run_until_parked();

        let (generation_after_notify, row_count_after_notify) = sidebar.update(vcx, |view, _cx| {
            (view.synced_schema_generation, view.rows.len())
        });
        assert_eq!(
            generation_after_notify, generation_after_build,
            "an unrelated notify must not advance the sidebar's synced schema generation"
        );
        assert_eq!(
            row_count_after_notify, row_count_after_build,
            "an unrelated notify must not change the cached row count"
        );
    }

    /// Pins AC3's click-to-preview glue end to end at the view layer: a
    /// clicked relation must both mark itself selected on the sidebar and
    /// set the results grid's source label to the qualified
    /// `schema.relation` name, not just dispatch the preview query.
    #[gpui::test]
    fn preview_selects_the_relation_and_sets_the_results_source_label(
        cx: &mut gpui::TestAppContext,
    ) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_results = session.clone();
        let session_for_view = session.clone();
        let results = cx.new(|cx| ResultsView::new(session_for_results, "", cx));
        let results_for_view = results.clone();
        let (sidebar, vcx) = cx.add_window_view(|_window, cx| {
            SidebarView::new(session_for_view, results_for_view, cx)
        });

        sidebar.update(vcx, |view, cx| view.preview("public", "orders", cx));
        vcx.run_until_parked();

        sidebar.update(vcx, |view, _cx| {
            assert_eq!(
                view.selected_relation,
                Some(("public".to_owned(), "orders".to_owned()))
            );
        });
        results.update(vcx, |view, _cx| {
            assert_eq!(view.source_label_for_test(), "public.orders");
        });
    }

    /// Pins AC2's "expand/collapse state lives in the view": collapsing a
    /// catalog or schema drops its descendants from `rows` and records the
    /// collapse; toggling again restores both.
    #[gpui::test]
    fn toggling_a_catalog_or_schema_collapses_then_re_expands(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_results = session.clone();
        let session_for_view = session.clone();
        let (sidebar, vcx) = cx.add_window_view(|_window, cx| {
            let results = cx.new(|cx| ResultsView::new(session_for_results, "", cx));
            SidebarView::new(session_for_view, results, cx)
        });

        let expanded = sidebar.update(vcx, |view, _cx| view.rows.len());
        assert!(expanded > 1);

        let catalog_collapsed = sidebar.update(vcx, |view, cx| {
            view.toggle_catalog("zsql", cx);
            view.rows.len()
        });
        assert!(catalog_collapsed < expanded);
        sidebar.update(vcx, |view, _cx| {
            assert!(view.collapsed_catalogs.contains("zsql"));
        });

        sidebar.update(vcx, |view, cx| view.toggle_catalog("zsql", cx));
        sidebar.update(vcx, |view, _cx| {
            assert_eq!(view.rows.len(), expanded);
            assert!(view.collapsed_catalogs.is_empty());
        });

        let schema_collapsed = sidebar.update(vcx, |view, cx| {
            view.toggle_schema("zsql", "public", cx);
            view.rows.len()
        });
        assert!(schema_collapsed < expanded);
        sidebar.update(vcx, |view, _cx| {
            assert!(
                view.collapsed_schemas
                    .contains(&("zsql".to_owned(), "public".to_owned()))
            );
        });

        sidebar.update(vcx, |view, cx| view.toggle_schema("zsql", "public", cx));
        sidebar.update(vcx, |view, _cx| {
            assert_eq!(view.rows.len(), expanded);
            assert!(view.collapsed_schemas.is_empty());
        });
    }
}
