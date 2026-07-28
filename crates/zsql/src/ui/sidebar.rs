//! The schema sidebar: a tree of the connected database's catalog ->
//! schema -> relation structure, driven by a `Session`'s introspected
//! [`zsql_core::SchemaTree`]

use std::collections::HashSet;

use gpui::{
    ClickEvent, ClipboardItem, Context, Div, Entity, Focusable, MouseButton, MouseDownEvent,
    Pixels, Point, Render, Stateful, UniformListScrollHandle, Window, div, point, prelude::*, px,
    rgb, rgba, uniform_list,
};
use zsql_core::RelationKind;
use zsql_ui::context_menu::{ContextMenu, ContextMenuItem};
use zsql_ui::icon::{IconName, icon};
use zsql_ui::icon_button::icon_button_secondary;
use zsql_ui::scrollable::{Axis, ScrollSource, ScrollableState, ScrollbarStyle, WithScrollbars};
use zsql_ui::theme::{ActiveTheme, Theme};
// Imported by name rather than as `zsql_ui::tree::...`: this module already
// uses `tree` as a local variable/parameter name for a `SchemaTree`, and
// qualifying every call here would read as if it referred to that value.
use zsql_ui::tree::{
    META_TEXT_SIZE, ROW_HEIGHT, ROW_TEXT_SIZE, disclosure_glyph, disclosure_spacer, row_count,
    row_label, row_meta, row_shell,
};

use model::{SidebarRow, flatten_schema_tree, relation_icon_name, relation_tint};

use super::tabs::TabModel;
use super::theme;
use crate::session::{SchemaState, Session};

mod model;

/// The schema sidebar view.
pub struct SidebarView {
    session: Entity<Session>,
    tabs: Entity<TabModel>,
    collapsed_catalogs: HashSet<String>,
    collapsed_schemas: HashSet<(String, String)>,
    /// The relation most recently clicked, for highlighting its row.
    selected_relation: Option<(String, String)>,
    rows: Vec<SidebarRow>,
    /// The session's `schema_generation()` as of the last time `rows` was
    /// rebuilt from it
    synced_schema_generation: u64,
    /// Scroll state shared between the tree's `uniform_list` and the
    /// scrollbar overlay drawn on top of it, so both read/drive the same
    /// offset.
    tree_scroll_handle: UniformListScrollHandle,
    /// The tree's scrollbar state.
    scroll: Entity<ScrollableState>,
    /// The currently open relation-row context menu, if any.
    context_menu: Option<ContextMenuState>,
}

/// A relation row's open right-click context menu: which relation it
/// targets, the flattened index of its triggering row (so the menu can
/// anchor to that row's right edge), and the triggering click position used
/// as a fallback anchor before the tree viewport has been measured.
#[derive(Debug, Clone)]
struct ContextMenuState {
    schema: String,
    relation: String,
    kind: RelationKind,
    row_index: usize,
    fallback_position: Point<Pixels>,
}

impl SidebarView {
    /// Build a sidebar over `session`, previewing clicked relations by
    /// opening (or reusing) a generated tab in `tabs`.
    #[must_use]
    pub fn new(session: Entity<Session>, tabs: Entity<TabModel>, cx: &mut Context<Self>) -> Self {
        cx.observe(&session, |view: &mut Self, _session, cx| {
            if view.sync_rows_if_schema_changed(cx) {
                cx.notify();
            }
        })
        .detach();

        let mut view = Self {
            session,
            tabs,
            collapsed_catalogs: HashSet::new(),
            collapsed_schemas: HashSet::new(),
            selected_relation: None,
            rows: Vec::new(),
            synced_schema_generation: 0,
            tree_scroll_handle: UniformListScrollHandle::new(),
            scroll: cx.new(ScrollableState::new),
            context_menu: None,
        };
        view.sync_rows(cx);
        view
    }

    /// Rebuild `rows` from the session's current schema state and this
    /// view's collapse sets, and record the schema generation it was built
    /// from
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
    /// was rebuilt)
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
    /// open (or reuse) a generated tab for it -- running the preview query
    /// through `Session` and updating the results grid's source label --
    /// and move keyboard focus onto that tab's editor.
    fn preview(
        &mut self,
        schema: &str,
        relation: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_relation = Some((schema.to_owned(), relation.to_owned()));

        self.tabs.update(cx, |tabs, cx| {
            tabs.open_or_reuse_generated(schema, relation, cx);
        });
        if let Some(handle) = self
            .tabs
            .read(cx)
            .active_tab()
            .map(|tab| tab.editor().focus_handle(cx))
        {
            window.focus(&handle);
        }

        cx.notify();
    }

    /// Open `schema.relation`'s (or, if already open, reuse/activate) a
    /// read-only schema tab showing its columns, indexes, and constraints.
    fn view_schema(
        &mut self,
        schema: &str,
        relation: &str,
        kind: RelationKind,
        cx: &mut Context<Self>,
    ) {
        self.selected_relation = Some((schema.to_owned(), relation.to_owned()));
        self.tabs.update(cx, |tabs, cx| {
            tabs.open_or_reuse_schema(schema, relation, kind, cx);
        });
        cx.notify();
    }

    /// Open the right-click context menu for `schema.relation`, anchored to
    /// the right edge of its `row_index` row. `fallback_position` (window
    /// coordinates, from the triggering mouse event) anchors the menu until
    /// the tree viewport has been measured.
    fn open_context_menu(
        &mut self,
        schema: String,
        relation: String,
        kind: RelationKind,
        row_index: usize,
        fallback_position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = Some(ContextMenuState {
            schema,
            relation,
            kind,
            row_index,
            fallback_position,
        });
        cx.notify();
    }

    /// Where to anchor the context menu for the relation row at `row_index`:
    /// the top of that row at the tree viewport's right edge, in window
    /// coordinates. `None` before the tree viewport has been measured, when
    /// the row's on-screen position cannot yet be derived.
    #[allow(clippy::cast_precision_loss)]
    fn relation_row_anchor(&self, row_index: usize) -> Option<Point<Pixels>> {
        let bounds = self.tree_scroll_handle.0.borrow().base_handle.bounds();
        if bounds.size.height == Pixels::ZERO {
            return None;
        }
        let right_edge_x = bounds.origin.x + bounds.size.width;
        let row_top_y = bounds.origin.y + ROW_HEIGHT * row_index as f32 - self.tree_scroll_offset();
        Some(point(right_edge_x, row_top_y))
    }

    /// Close the open context menu, if any.
    fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            cx.notify();
        }
    }

    /// Write the open context menu's relation's bare name to the system
    /// clipboard, then close the menu. A no-op if no menu is open.
    fn copy_name(&mut self, cx: &mut Context<Self>) {
        if let Some(menu) = &self.context_menu {
            cx.write_to_clipboard(ClipboardItem::new_string(menu.relation.clone()));
        }
        self.close_context_menu(cx);
    }

    /// Write the open context menu's relation's qualified `schema.relation`
    /// name to the system clipboard, then close the menu. A no-op if no
    /// menu is open.
    fn copy_qualified_name(&mut self, cx: &mut Context<Self>) {
        if let Some(menu) = &self.context_menu {
            let qualified = qualified_relation_name(&menu.schema, &menu.relation);
            cx.write_to_clipboard(ClipboardItem::new_string(qualified));
        }
        self.close_context_menu(cx);
    }

    /// The tree's current downward scroll offset (zero at the top).
    /// `ScrollHandle::offset` is negative-down, matching `EditorView`'s
    /// scroll handle convention, so this negates it back to a
    /// positive-down offset for the scrollbar geometry.
    fn tree_scroll_offset(&self) -> Pixels {
        -self.tree_scroll_handle.0.borrow().base_handle.offset().y
    }

    /// Re-introspect the live connection so the tree picks up schema changes
    /// made since it was last loaded. A no-op while disconnected: there is no
    /// catalog to refresh, and introspecting without a connection would
    /// replace the "connect to browse" prompt with an error.
    fn refresh_schema(&mut self, cx: &mut Context<Self>) {
        if self.session.read(cx).is_connected() {
            self.session.update(cx, Session::introspect).detach();
        }
    }

    /// The "SCHEMA" header bar.
    fn render_header(window: &mut Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .flex_shrink_0()
            .h(theme::SIDEBAR_HEADER_HEIGHT)
            .px_3()
            .border_b_1()
            .border_color(rgb(cx.theme().colors.border_soft))
            .child(
                div()
                    .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
                    .text_color(rgb(cx.theme().colors.text_tertiary))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("SCHEMA"),
            )
            .child(
                icon_button_secondary("sidebar-refresh-schema", window, cx, IconName::Refresh)
                    .on_click(cx.listener(|view, _evt, _window, cx| view.refresh_schema(cx))),
            )
    }

    /// The main content area: the tree when a schema is loaded, or a
    /// centered prompt/status message for every other `SchemaState`.
    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let placeholder = {
            let session = self.session.read(cx);
            match session.schema() {
                SchemaState::NotLoaded => Some((
                    colors.text_tertiary,
                    "No schema",
                    "Connect to a database to browse its schema.".to_owned(),
                )),
                SchemaState::Loading => Some((
                    colors.text_tertiary,
                    "Loading schema...",
                    "Fetching catalogs, schemas, and relations.".to_owned(),
                )),
                SchemaState::Error(message) => {
                    Some((colors.status_error, "Schema unavailable", message.clone()))
                }
                SchemaState::Ready(tree) if tree.catalogs.is_empty() => Some((
                    colors.text_tertiary,
                    "No catalogs",
                    "The connected database reported no catalogs.".to_owned(),
                )),
                SchemaState::Ready(_) => None,
            }
        };

        match placeholder {
            Some((color, title, detail)) => {
                Self::render_placeholder(color, title, &detail, active_theme).into_any_element()
            }
            None => self.render_tree(window, cx).into_any_element(),
        }
    }

    /// A centered title + detail message shown in place of the tree for any
    /// non-ready `SchemaState`.
    fn render_placeholder(
        title_color: u32,
        title: &str,
        detail: &str,
        active_theme: &Theme,
    ) -> Div {
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
                    .text_size(px(ROW_TEXT_SIZE))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(title_color))
                    .child(title.to_owned()),
            )
            .child(
                div()
                    .text_size(px(META_TEXT_SIZE))
                    .text_color(rgb(active_theme.colors.text_tertiary))
                    .child(detail.to_owned()),
            )
    }

    /// The tree scrollbar's chrome, from the sidebar's own theme constants
    /// plus the active theme's scrollbar colors. The track paints no
    /// background.
    fn tree_scrollbar_style(active_theme: &Theme) -> ScrollbarStyle {
        ScrollbarStyle {
            track_width: f32::from(theme::SIDEBAR_SCROLLBAR_WIDTH),
            track_color: None,
            thumb_color: active_theme.colors.scrollbar_thumb,
            thumb_hover_color: Some(active_theme.colors.scrollbar_thumb_hover),
            radius: theme::SIDEBAR_SCROLLBAR_RADIUS,
            inset: f32::from(theme::SIDEBAR_SCROLLBAR_GAP),
            ..ScrollbarStyle::default()
        }
    }

    /// The virtualized tree body: only rows scrolled into view are built.
    fn render_tree(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let row_count = self.rows.len();
        let content_height = f32::from(sidebar_tree_content_height(row_count));
        let tree_scroll_handle = self.tree_scroll_handle.clone();

        self.scroll.update(cx, |scroll, _cx| {
            scroll.vertical(Axis::new(
                ScrollSource::UniformList(tree_scroll_handle),
                content_height,
            ));
        });

        let list = uniform_list(
            "sidebar-rows",
            row_count,
            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                range
                    .map(|ix| this.render_row(&this.rows[ix], ix, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .track_scroll(self.tree_scroll_handle.clone());

        div()
            .id("sidebar-tree")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .py(px(theme::SIDEBAR_TREE_PADDING_Y))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .with_scrollbars(&self.scroll, Self::tree_scrollbar_style(cx.theme()), cx),
            )
    }

    /// Render one flattened row, dispatching on its kind.
    fn render_row(&self, row: &SidebarRow, ix: usize, cx: &Context<Self>) -> Stateful<Div> {
        let active_theme = cx.theme();
        match row {
            SidebarRow::Catalog {
                name,
                expanded,
                schema_count,
            } => {
                let name_owned = name.clone();
                row_shell(theme::SIDEBAR_INDENT_L0, active_theme)
                    .id(ix)
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(active_theme.colors.bg_raised)))
                    .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                        view.toggle_catalog(&name_owned, cx);
                    }))
                    .child(disclosure_glyph(*expanded, active_theme))
                    .child(icon(
                        IconName::Database,
                        theme::SIDEBAR_ROW_ICON_SIZE,
                        active_theme.colors.text_tertiary,
                    ))
                    .child(row_label(name.clone()))
                    .when(!expanded, |el| {
                        el.child(row_meta(format!("{schema_count} schemas"), active_theme))
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
                row_shell(theme::SIDEBAR_INDENT_L1, active_theme)
                    .id(ix)
                    .cursor_pointer()
                    .hover(|this| this.bg(rgb(active_theme.colors.bg_raised)))
                    .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                        view.toggle_schema(&catalog_owned, &name_owned, cx);
                    }))
                    .child(disclosure_glyph(*expanded, active_theme))
                    .child(icon(
                        IconName::Schema,
                        theme::SIDEBAR_ROW_ICON_SIZE,
                        active_theme.colors.text_tertiary,
                    ))
                    .child(row_label(name.clone()))
                    .when(!expanded, |el| {
                        el.child(row_meta(format!("{relation_count} rel"), active_theme))
                    })
            }
            SidebarRow::Relation {
                schema,
                name,
                kind,
                column_count,
            } => self.render_relation_row(ix, schema, name, *kind, *column_count, cx),
        }
    }

    /// A relation row: left-click previews it, right-click opens its
    /// context menu, and a currently-selected relation gets a teal left
    /// border and tinted background.
    fn render_relation_row(
        &self,
        ix: usize,
        schema: &str,
        name: &str,
        kind: RelationKind,
        column_count: usize,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let active_theme = cx.theme();
        let schema_owned = schema.to_owned();
        let name_owned = name.to_owned();
        let schema_for_menu = schema.to_owned();
        let name_for_menu = name.to_owned();
        let selected = self
            .selected_relation
            .as_ref()
            .is_some_and(|(s, r)| s == schema && r == name);

        let mut shell = row_shell(theme::SIDEBAR_INDENT_L2, active_theme)
            .id(ix)
            .cursor_pointer()
            .hover(|this| this.bg(rgb(active_theme.colors.bg_raised)))
            .on_click(cx.listener(move |view, _event: &ClickEvent, window, cx| {
                view.preview(&schema_owned, &name_owned, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |view, event: &MouseDownEvent, _window, cx| {
                    view.open_context_menu(
                        schema_for_menu.clone(),
                        name_for_menu.clone(),
                        kind,
                        ix,
                        event.position,
                        cx,
                    );
                }),
            )
            .child(disclosure_spacer())
            .child(row_label(name.to_owned()))
            .child(icon(
                relation_icon_name(kind),
                theme::SIDEBAR_RELATION_ICON_SIZE,
                relation_tint(kind, active_theme),
            ))
            // left-pad the row count so that the icons are always aligned (
            // assuming <9999 columns)
            .child(row_count(format!("{column_count:>4} cols"), active_theme));

        if selected {
            shell = shell
                .bg(rgba(theme::sidebar_selected_bg(active_theme)))
                .border_l_2()
                .border_color(rgb(active_theme.colors.accent));
        }
        shell
    }
}

impl SidebarView {
    /// The right-click context menu overlay: `Preview Data`, `View Schema`,
    /// a separator, then `Copy Name`/`Copy Qualified Name`, anchored to the
    /// right edge of its triggering relation row. A full-window backdrop
    /// behind it absorbs off-menu clicks so closing the menu never doubles
    /// as activating whatever sits beneath it. Renders nothing when no menu
    /// is open.
    fn render_context_menu(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let menu = self.context_menu.clone()?;
        let schema = menu.schema.clone();
        let relation = menu.relation.clone();
        let kind = menu.kind;
        let anchor = self
            .relation_row_anchor(menu.row_index)
            .unwrap_or(menu.fallback_position);

        let preview_schema = schema.clone();
        let preview_relation = relation.clone();
        let view_schema_schema = schema.clone();
        let view_schema_relation = relation.clone();

        let menu = ContextMenu::new("sidebar-context-menu")
            .position(anchor)
            .on_close(cx.listener(|view, _event, _window, cx| {
                view.close_context_menu(cx);
            }))
            .add_item(ContextMenuItem::new("Preview Data").on_click(cx.listener(
                move |view, _event, window, cx| {
                    view.preview(&preview_schema, &preview_relation, window, cx);
                    view.close_context_menu(cx);
                },
            )))
            .add_item(ContextMenuItem::new("View Schema").on_click(cx.listener(
                move |view, _event, _window, cx| {
                    view.view_schema(&view_schema_schema, &view_schema_relation, kind, cx);
                    view.close_context_menu(cx);
                },
            )))
            .add_separator()
            .add_item(ContextMenuItem::new("Copy Name").on_click(cx.listener(
                |view, _event, _window, cx| {
                    view.copy_name(cx);
                    view.close_context_menu(cx);
                },
            )))
            .add_item(
                ContextMenuItem::new("Copy Qualified Name").on_click(cx.listener(
                    |view, _event, _window, cx| {
                        view.copy_qualified_name(cx);
                        view.close_context_menu(cx);
                    },
                )),
            );

        Some(menu.into_any_element())
    }
}

/// `schema.relation`, the text `Copy Qualified Name` writes to the
/// clipboard.
fn qualified_relation_name(schema: &str, relation: &str) -> String {
    format!("{schema}.{relation}")
}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_theme = cx.theme();
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(active_theme.colors.bg_panel))
            .child(Self::render_header(window, cx))
            .child(self.render_body(window, cx))
            .children(self.render_context_menu(cx))
    }
}

/// Height of the sidebar tree's scrollable content: every row, stacked at
/// `ROW_HEIGHT`. Used as the scrollbar geometry's content extent, computed
/// explicitly from the row count rather than measured post-layout (mirroring
/// `editor.rs`'s `editor_content_height`).
///
/// Excludes `SIDEBAR_TREE_PADDING_Y`: that padding is applied by the
/// `sidebar-tree` div that wraps the `uniform_list`'s viewport, not by
/// anything inside the scrolled `uniform_list` itself, so it shrinks the
/// viewport rather than extending the scrollable content. Baking it into
/// this content height would overstate the scrollable range relative to the
/// viewport height read from the (padding-excluded) `uniform_list` bounds.
#[allow(clippy::cast_precision_loss)]
fn sidebar_tree_content_height(row_count: usize) -> Pixels {
    ROW_HEIGHT * row_count as f32
}

#[cfg(test)]
mod tests {
    use zsql_ui::theme::Theme;
    use zsql_ui::tree::ROW_HEIGHT;

    use super::{SidebarView, sidebar_tree_content_height};
    use crate::ui::theme;

    #[test]
    fn tree_content_height_stacks_rows_at_row_height_with_no_padding() {
        // The padding around the tree viewport lives outside the
        // uniform_list's scrolled content, so it must not appear here: doing
        // so would overstate the scrollable extent against the
        // padding-excluded viewport height read from the list's bounds.
        assert_eq!(sidebar_tree_content_height(0), gpui::px(0.0));
        assert_eq!(sidebar_tree_content_height(7), ROW_HEIGHT * 7.0);
    }

    #[test]
    fn tree_scrollbar_style_matches_every_one_of_the_sidebars_theme_constants() {
        let active_theme = Theme::default();
        let style = SidebarView::tree_scrollbar_style(&active_theme);
        assert!(
            (style.track_width - f32::from(theme::SIDEBAR_SCROLLBAR_WIDTH)).abs() < f32::EPSILON
        );
        assert_eq!(
            style.track_color, None,
            "the tree scrollbar's track paints no background"
        );
        assert_eq!(style.thumb_color, active_theme.colors.scrollbar_thumb);
        assert_eq!(
            style.thumb_hover_color,
            Some(active_theme.colors.scrollbar_thumb_hover)
        );
        assert!((style.radius - theme::SIDEBAR_SCROLLBAR_RADIUS).abs() < f32::EPSILON);
        assert!((style.inset - f32::from(theme::SIDEBAR_SCROLLBAR_GAP)).abs() < f32::EPSILON);
    }
}

#[cfg(test)]
mod render_tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use gpui::AppContext as _;
    use zsql_core::{
        BatchSink, Catalog, ColumnMeta, Connection, CoreError, QueryHandle, RelationSchema,
        RowCount, SchemaNs, SchemaTree,
    };
    use zsql_core::{Relation, RelationKind};

    use super::{SidebarView, qualified_relation_name};
    use crate::session::{SchemaState, Session};
    use crate::ui::results::ResultsView;
    use crate::ui::tabs::TabModel;

    /// A `Connection` double whose `introspect` hands back a fixed,
    /// distinctively-named tree, so a test can tell a fresh introspection
    /// apart from whatever schema the session held beforehand. Its other
    /// methods are inert -- these tests only exercise the refresh path.
    struct RefreshingConnection;

    /// The catalog name [`RefreshingConnection::introspect`] returns, chosen
    /// so it cannot be confused with any other fixture tree in this module.
    const REFRESHED_CATALOG: &str = "refreshed_catalog";

    #[async_trait]
    impl Connection for RefreshingConnection {
        fn stream_query(&self, _sql: String, _sink: BatchSink) -> QueryHandle {
            let (cancel_tx, _cancel_rx) = flume::unbounded();
            QueryHandle::new(cancel_tx)
        }

        async fn introspect(&self) -> Result<SchemaTree, CoreError> {
            Ok(SchemaTree {
                catalogs: vec![Catalog {
                    name: REFRESHED_CATALOG.to_owned(),
                    schemas: vec![SchemaNs {
                        name: "public".to_owned(),
                        tables: vec![],
                    }],
                }],
            })
        }

        async fn ping(&self) -> Result<(), CoreError> {
            Ok(())
        }

        async fn count_rows(&self, _schema: &str, _relation: &str) -> Result<RowCount, CoreError> {
            Ok(RowCount::Exact(0))
        }

        async fn describe_relation(
            &self,
            _schema: &str,
            _relation: &str,
        ) -> Result<RelationSchema, CoreError> {
            Ok(RelationSchema::default())
        }
    }

    /// A `TabModel` over a fresh `ResultsView`, for tests that only care
    /// about the sidebar's own state and do not inspect results/tabs
    /// directly.
    fn build_tabs(
        session: gpui::Entity<Session>,
        cx: &mut gpui::TestAppContext,
    ) -> gpui::Entity<TabModel> {
        let session_for_results = session.clone();
        let results = cx.new(|cx| ResultsView::new(session_for_results, "", cx));
        cx.new(|cx| TabModel::new(session, results, cx))
    }

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

    fn build(
        cx: &mut gpui::TestAppContext,
        schema: SchemaState,
    ) -> (gpui::Entity<SidebarView>, &mut gpui::VisualTestContext) {
        let session = cx.new(|_cx| Session::new_for_schema_test(schema));
        let tabs = build_tabs(session.clone(), cx);
        cx.add_window_view(|_window, cx| SidebarView::new(session, tabs, cx))
    }

    #[gpui::test]
    fn renders_a_populated_schema_tree_without_panicking(cx: &mut gpui::TestAppContext) {
        let (sidebar, vcx) = build(cx, SchemaState::Ready(sample_schema_tree()));
        sidebar.read_with(vcx, |view, _app| {
            assert!(
                !view.rows.is_empty(),
                "a populated schema tree must flatten into at least one visible row"
            );
        });
    }

    /// A schema with one catalog, one schema, and `table_count` tables.
    fn tall_schema_tree(table_count: usize) -> SchemaTree {
        let tables = (0..table_count)
            .map(|i| Relation {
                name: format!("t{i}"),
                kind: RelationKind::Table,
                columns: vec![],
            })
            .collect();
        SchemaTree {
            catalogs: vec![Catalog {
                name: "zsql".to_owned(),
                schemas: vec![SchemaNs {
                    name: "public".to_owned(),
                    tables,
                }],
            }],
        }
    }

    /// A tree taller than any reasonable sidebar viewport must show its
    /// scrollbar after the first frame. This guards the regression where the
    /// scrollbar stayed hidden because the scroll viewport's bounds are zero
    /// during the first render and nothing forced the follow-up re-render once
    /// they became known.
    #[gpui::test]
    fn tree_scrollbar_is_shown_after_the_first_frame_when_rows_overflow(
        cx: &mut gpui::TestAppContext,
    ) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(tall_schema_tree(300))));
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) = cx.add_window_view(|_window, cx| SidebarView::new(session, tabs, cx));
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, app| {
            assert!(
                view.scroll.read(app).vertical_visible(),
                "the tree scrollbar must be visible when 300 rows overflow the sidebar viewport"
            );
        });
    }

    #[gpui::test]
    fn renders_every_non_ready_schema_state_without_panicking(cx: &mut gpui::TestAppContext) {
        for schema in [
            SchemaState::NotLoaded,
            SchemaState::Loading,
            SchemaState::Error("permission denied for schema pg_catalog".to_owned()),
            SchemaState::Ready(SchemaTree::default()),
        ] {
            let (sidebar, vcx) = build(cx, schema);
            sidebar.read_with(vcx, |view, _app| {
                assert!(
                    view.rows.is_empty(),
                    "no catalog/table rows are known without a populated schema tree"
                );
            });
        }
    }

    #[gpui::test]
    fn an_unrelated_session_notify_does_not_reflatten_the_row_cache(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, cx));

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

    #[gpui::test]
    fn preview_selects_the_relation_and_opens_a_generated_tab(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let session_for_results = session.clone();
        let results = cx.new(|cx| ResultsView::new(session_for_results, "", cx));
        let results_for_tabs = results.clone();
        let tabs = cx.new(|cx| TabModel::new(session.clone(), results_for_tabs, cx));
        let tabs_for_view = tabs.clone();
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs_for_view, cx));

        sidebar.update_in(vcx, |view, window, cx| {
            view.preview("public", "orders", window, cx);
        });
        vcx.run_until_parked();

        sidebar.update(vcx, |view, _cx| {
            assert_eq!(
                view.selected_relation,
                Some(("public".to_owned(), "orders".to_owned()))
            );
        });
        tabs.read_with(vcx, |tabs, _app| {
            assert_eq!(
                tabs.tabs().len(),
                1,
                "preview opens exactly one generated tab"
            );
            assert!(tabs.tabs()[0].is_generated());
        });
        results.update(vcx, |view, _cx| {
            assert_eq!(view.source_label_for_test(), "public.orders");
        });
    }

    #[gpui::test]
    fn view_schema_selects_the_relation_and_opens_a_schema_tab(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let tabs_for_view = tabs.clone();
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs_for_view, cx));

        sidebar.update(vcx, |view, cx| {
            view.view_schema("public", "orders", RelationKind::Table, cx);
        });
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(
                view.selected_relation,
                Some(("public".to_owned(), "orders".to_owned()))
            );
        });
        tabs.read_with(vcx, |tabs, _app| {
            assert_eq!(tabs.tabs().len(), 1, "View Schema opens exactly one tab");
            assert!(tabs.tabs()[0].is_schema());
        });
    }

    #[gpui::test]
    fn opening_and_closing_the_context_menu_toggles_its_state(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, cx));

        sidebar.update(vcx, |view, cx| {
            view.open_context_menu(
                "public".to_owned(),
                "orders".to_owned(),
                RelationKind::Table,
                0,
                gpui::point(gpui::px(10.0), gpui::px(20.0)),
                cx,
            );
        });
        sidebar.read_with(vcx, |view, _app| {
            assert!(view.context_menu.is_some());
        });

        sidebar.update(vcx, SidebarView::close_context_menu);
        sidebar.read_with(vcx, |view, _app| {
            assert!(view.context_menu.is_none());
        });
    }

    #[gpui::test]
    fn copy_name_writes_the_relations_bare_name_to_the_clipboard(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, cx));

        sidebar.update(vcx, |view, cx| {
            view.open_context_menu(
                "public".to_owned(),
                "orders".to_owned(),
                RelationKind::Table,
                0,
                gpui::point(gpui::px(10.0), gpui::px(20.0)),
                cx,
            );
            view.copy_name(cx);
        });

        let copied =
            vcx.update(|_window, cx| cx.read_from_clipboard().and_then(|item| item.text()));
        assert_eq!(copied.as_deref(), Some("orders"));
        sidebar.read_with(vcx, |view, _app| {
            assert!(
                view.context_menu.is_none(),
                "Copy Name must also close the menu it was chosen from"
            );
        });
    }

    #[gpui::test]
    fn copy_qualified_name_writes_schema_dot_relation_to_the_clipboard(
        cx: &mut gpui::TestAppContext,
    ) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, cx));

        sidebar.update(vcx, |view, cx| {
            view.open_context_menu(
                "public".to_owned(),
                "orders".to_owned(),
                RelationKind::Table,
                0,
                gpui::point(gpui::px(10.0), gpui::px(20.0)),
                cx,
            );
            view.copy_qualified_name(cx);
        });

        let copied =
            vcx.update(|_window, cx| cx.read_from_clipboard().and_then(|item| item.text()));
        assert_eq!(copied.as_deref(), Some("public.orders"));
        sidebar.read_with(vcx, |view, _app| {
            assert!(
                view.context_menu.is_none(),
                "Copy Qualified Name must also close the menu it was chosen from"
            );
        });
    }

    #[test]
    fn qualified_relation_name_joins_schema_and_relation_with_a_dot() {
        assert_eq!(qualified_relation_name("public", "orders"), "public.orders");
    }

    #[gpui::test]
    fn the_rendered_context_menu_does_not_panic_while_open(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, cx));

        sidebar.update(vcx, |view, cx| {
            view.open_context_menu(
                "public".to_owned(),
                "orders".to_owned(),
                RelationKind::Table,
                0,
                gpui::point(gpui::px(10.0), gpui::px(20.0)),
                cx,
            );
        });
        // Forces a render pass with `render_context_menu`'s deferred/anchored
        // overlay on screen, catching a panic in the overlay itself (e.g. an
        // element-id collision or anchoring failure) that a state-only
        // assertion would miss.
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert!(view.context_menu.is_some());
        });
    }

    #[gpui::test]
    fn context_menu_anchors_to_the_triggering_rows_right_edge(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, cx));
        vcx.run_until_parked();

        // Once the tree viewport is measured, a row anchor is derived from
        // the row's laid-out geometry (its top at the viewport's right edge),
        // and successive rows anchor lower than their predecessors.
        sidebar.read_with(vcx, |view, _app| {
            let first = view
                .relation_row_anchor(0)
                .expect("a measured tree yields a row anchor");
            let second = view
                .relation_row_anchor(1)
                .expect("a measured tree yields a row anchor");
            assert!(
                second.y > first.y,
                "a later row must anchor below an earlier one"
            );
            assert_eq!(
                first.x, second.x,
                "every row anchors at the same right-edge x"
            );
        });
    }

    #[gpui::test]
    fn toggling_a_catalog_or_schema_collapses_then_re_expands(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, cx));

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

    #[gpui::test]
    fn refreshing_a_connected_session_re_introspects_and_replaces_the_tree(
        cx: &mut gpui::TestAppContext,
    ) {
        // Start Ready with a stale tree, then refresh: the connection's own
        // introspection must overwrite it with the freshly-fetched catalog.
        let session = cx.new(|_cx| {
            let mut session = Session::new_for_query_test(Arc::new(RefreshingConnection));
            session.set_schema_for_test(SchemaState::Ready(sample_schema_tree()));
            session
        });
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, cx));

        sidebar.update(vcx, SidebarView::refresh_schema);
        vcx.run_until_parked();

        session.read_with(vcx, |session, _app| match session.schema() {
            SchemaState::Ready(tree) => {
                assert_eq!(
                    tree.catalogs
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>(),
                    vec![REFRESHED_CATALOG],
                    "refresh must replace the stale tree with the re-introspected one"
                );
            }
            other => panic!("expected a Ready schema after refresh, got {other:?}"),
        });
    }

    #[gpui::test]
    fn refreshing_while_disconnected_leaves_the_schema_untouched(cx: &mut gpui::TestAppContext) {
        // With no live connection there is nothing to introspect, so refresh
        // must be a no-op rather than flipping the "connect to browse" prompt
        // into a "not connected" error.
        let session = cx.new(|_cx| Session::new_for_schema_test(SchemaState::NotLoaded));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, cx));

        sidebar.update(vcx, SidebarView::refresh_schema);
        vcx.run_until_parked();

        session.read_with(vcx, |session, _app| {
            assert!(
                matches!(session.schema(), SchemaState::NotLoaded),
                "refreshing without a connection must not change the schema state"
            );
        });
    }
}
