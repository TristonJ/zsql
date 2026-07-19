//! The schema sidebar: a tree of the connected database's catalog ->
//! schema -> relation structure, driven by a `Session`'s introspected
//! [`SchemaTree`]

use std::collections::HashSet;

use gpui::{
    ClickEvent, ClipboardItem, Context, Div, Entity, Focusable, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, Stateful, UniformListScrollHandle, Window,
    anchored, deferred, div, point, prelude::*, px, rgb, rgba, uniform_list,
};
use zsql_core::{RelationKind, SchemaTree};
use zsql_ui::colors;
use zsql_ui::icon::{IconName, icon};
use zsql_ui::scrollbar::{self, ScrollbarGeometry};
// Imported by name rather than as `zsql_ui::tree::...`: this module already
// uses `tree` as a local variable/parameter name for a `SchemaTree`, and
// qualifying every call here would read as if it referred to that value.
use zsql_ui::tree::{
    META_TEXT_SIZE, ROW_HEIGHT, ROW_TEXT_SIZE, disclosure_glyph, disclosure_spacer, row_count,
    row_label, row_meta, row_shell,
};

use super::tabs::TabModel;
use super::theme;
use crate::session::{SchemaState, Session};

/// One flattened, currently-visible sidebar row. Built by
/// [`flatten_schema_tree`] from a `SchemaTree` plus the view's collapse
/// state
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
    /// State of an in-progress scrollbar thumb drag; `None` when the thumb
    /// is not being dragged.
    thumb_drag: Option<ThumbDrag>,
    /// The currently open relation-row context menu, if any.
    context_menu: Option<ContextMenuState>,
}

/// A scrollbar thumb drag's starting point: the mouse position and the
/// tree's scroll offset at the moment the drag began, so later mouse-move
/// deltas can be converted to a new absolute scroll offset.
#[derive(Debug, Clone, Copy)]
struct ThumbDrag {
    start_mouse_y: Pixels,
    start_scroll_offset: Pixels,
}

/// A relation row's open right-click context menu: which relation it
/// targets and where (in window coordinates) to anchor the menu.
#[derive(Debug, Clone)]
struct ContextMenuState {
    schema: String,
    relation: String,
    kind: RelationKind,
    position: Point<Pixels>,
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
            thumb_drag: None,
            context_menu: None,
        };
        view.sync_rows(cx);
        view
    }

    /// The tree scrollbar's size is computed from the scroll viewport's
    /// laid-out height, which reads back as zero during the render that first
    /// lays the tree out (a scroll container's bounds are only known after
    /// that render). The tree only appears once the schema loads, so its first
    /// frame always starts unmeasured. When that state is detected - the tree
    /// is shown but its viewport has not been measured yet - schedule exactly
    /// one re-render so the scrollbar appears on the next frame instead of
    /// staying hidden until a wheel/keyboard scroll forces a repaint. This
    /// settles immediately: once the viewport is measured the condition is
    /// false. `request_animation_frame` cannot do this - it only queues a
    /// callback without forcing a draw, so on an idle window it never fires.
    fn nudge_scrollbar_when_tree_unmeasured(&mut self, cx: &mut Context<Self>) {
        let tree_shown = matches!(
            self.session.read(cx).schema(),
            SchemaState::Ready(tree) if !tree.catalogs.is_empty()
        );
        if tree_shown && self.tree_viewport_height() == Pixels::ZERO {
            cx.spawn(async move |this, cx| {
                this.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }
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

        let preview_limit = self.session.read(cx).preview_limit();
        self.tabs.update(cx, |tabs, cx| {
            tabs.open_or_reuse_generated(schema, relation, preview_limit, cx);
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

    /// Open the right-click context menu for `schema.relation`, anchored at
    /// `position` (window coordinates, from the triggering mouse event).
    fn open_context_menu(
        &mut self,
        schema: String,
        relation: String,
        kind: RelationKind,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = Some(ContextMenuState {
            schema,
            relation,
            kind,
            position,
        });
        cx.notify();
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

    /// The tree scroll region's most recently painted viewport height. Zero
    /// before the first paint.
    fn tree_viewport_height(&self) -> Pixels {
        self.tree_scroll_handle
            .0
            .borrow()
            .base_handle
            .bounds()
            .size
            .height
    }

    /// The tree's current downward scroll offset (zero at the top).
    /// `ScrollHandle::offset` is negative-down, matching `EditorView`'s
    /// scroll handle convention, so this negates it back to a
    /// positive-down offset for the scrollbar geometry.
    fn tree_scroll_offset(&self) -> Pixels {
        -self.tree_scroll_handle.0.borrow().base_handle.offset().y
    }

    /// Move the tree's scroll offset to `offset` (positive-down, clamping is
    /// the caller's responsibility).
    fn set_tree_scroll_offset(&self, offset: Pixels) {
        self.tree_scroll_handle
            .0
            .borrow()
            .base_handle
            .set_offset(point(px(0.0), -offset));
    }

    /// Begin dragging the scrollbar thumb, recording the drag's starting
    /// mouse position and scroll offset.
    fn on_thumb_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.thumb_drag = Some(ThumbDrag {
            start_mouse_y: event.position.y,
            start_scroll_offset: self.tree_scroll_offset(),
        });
        cx.notify();
    }

    /// While a thumb drag is in progress, convert the mouse's vertical
    /// travel since the drag started into a new tree scroll offset, using
    /// the same track-pixels-to-content-pixels ratio as the geometry
    /// function's offset formula.
    fn on_thumb_drag_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = self.thumb_drag else {
            return;
        };

        let content_height = f32::from(sidebar_tree_content_height(self.rows.len()));
        let viewport_height = f32::from(self.tree_viewport_height());
        let geometry = ScrollbarGeometry::compute(
            content_height,
            viewport_height,
            f32::from(drag.start_scroll_offset),
            viewport_height,
            scrollbar::MIN_THUMB_LENGTH,
        );
        if !geometry.visible {
            return;
        }

        let mouse_delta = f32::from(event.position.y - drag.start_mouse_y);
        let new_offset = ScrollbarGeometry::scroll_offset_for_drag(
            f32::from(drag.start_scroll_offset),
            mouse_delta,
            content_height,
            viewport_height,
            viewport_height,
            scrollbar::MIN_THUMB_LENGTH,
        );

        self.set_tree_scroll_offset(px(new_offset));
        cx.notify();
    }

    /// End a scrollbar thumb drag, if one was in progress.
    fn on_thumb_drag_end(
        &mut self,
        _: &MouseUpEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.thumb_drag = None;
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
            .border_color(rgb(colors::LINE_SOFT))
            .child(
                div()
                    .text_size(px(theme::SIDEBAR_HEADER_TEXT_SIZE))
                    .text_color(rgb(colors::FAINT))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("SCHEMA"),
            )
    }

    /// The main content area: the tree when a schema is loaded, or a
    /// centered prompt/status message for every other `SchemaState`.
    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let placeholder = {
            let session = self.session.read(cx);
            match session.schema() {
                SchemaState::NotLoaded => Some((
                    colors::FAINT,
                    "No schema",
                    "Connect to a database to browse its schema.".to_owned(),
                )),
                SchemaState::Loading => Some((
                    colors::FAINT,
                    "Loading schema...",
                    "Fetching catalogs, schemas, and relations.".to_owned(),
                )),
                SchemaState::Error(message) => {
                    Some((theme::STATUS_ERROR, "Schema unavailable", message.clone()))
                }
                SchemaState::Ready(tree) if tree.catalogs.is_empty() => Some((
                    colors::FAINT,
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
            None => self.render_tree(window, cx).into_any_element(),
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
                    .text_size(px(ROW_TEXT_SIZE))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(title_color))
                    .child(title.to_owned()),
            )
            .child(
                div()
                    .text_size(px(META_TEXT_SIZE))
                    .text_color(rgb(colors::FAINT))
                    .child(detail.to_owned()),
            )
    }

    /// The tree scrollbar's geometry, computed from the flattened row count's
    /// content height, the scroll viewport's laid-out height, and the current
    /// scroll offset. Read fresh each render so it stays in sync with wheel
    /// and keyboard scrolling and with rows appearing as the schema loads.
    fn tree_scrollbar_geometry(&self) -> ScrollbarGeometry {
        let viewport_height = f32::from(self.tree_viewport_height());
        ScrollbarGeometry::compute(
            f32::from(sidebar_tree_content_height(self.rows.len())),
            viewport_height,
            f32::from(self.tree_scroll_offset()),
            viewport_height,
            scrollbar::MIN_THUMB_LENGTH,
        )
    }

    /// The virtualized tree body: only rows scrolled into view are built.
    /// Wraps the `uniform_list` in a `relative` viewport div so the
    /// scrollbar overlay can be pinned to its right edge without shifting
    /// the tree rows' layout.
    fn render_tree(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let row_count = self.rows.len();
        let viewport_height = self.tree_viewport_height();
        let geometry = self.tree_scrollbar_geometry();

        div()
            .id("sidebar-tree")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .py(px(theme::SIDEBAR_TREE_PADDING_Y))
            .child(
                div()
                    .id("sidebar-tree-viewport")
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .on_mouse_move(cx.listener(Self::on_thumb_drag_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_thumb_drag_end))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_thumb_drag_end))
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
                        .flex_1()
                        .track_scroll(self.tree_scroll_handle.clone()),
                    )
                    .when(geometry.visible, |el| {
                        el.child(Self::render_scrollbar(
                            geometry,
                            f32::from(viewport_height),
                            cx,
                        ))
                    }),
            )
    }

    /// The scrollbar overlay: a track pinned to the right edge of the tree
    /// viewport, sized to the viewport height, holding a thumb positioned
    /// and sized from `geometry`.
    fn render_scrollbar(
        geometry: ScrollbarGeometry,
        track_length: f32,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id("sidebar-scrollbar-track")
            .absolute()
            .top_0()
            .right(theme::SIDEBAR_SCROLLBAR_GAP)
            .bottom_0()
            .w(theme::SIDEBAR_SCROLLBAR_WIDTH)
            .child(
                div()
                    .id("sidebar-scrollbar-thumb")
                    .absolute()
                    .top(px(geometry.thumb_offset(track_length)))
                    .w_full()
                    .h(px(geometry.thumb_length))
                    .rounded(px(theme::SIDEBAR_SCROLLBAR_RADIUS))
                    .bg(rgba(theme::SIDEBAR_SCROLLBAR_THUMB))
                    .hover(|el| el.bg(rgba(theme::SIDEBAR_SCROLLBAR_THUMB_HOVER)))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_thumb_mouse_down)),
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
                    .hover(|this| this.bg(rgb(colors::RAISE)))
                    .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                        view.toggle_catalog(&name_owned, cx);
                    }))
                    .child(disclosure_glyph(*expanded))
                    .child(icon(
                        IconName::Database,
                        theme::SIDEBAR_ROW_ICON_SIZE,
                        colors::FAINT,
                    ))
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
                    .hover(|this| this.bg(rgb(colors::RAISE)))
                    .on_click(cx.listener(move |view, _event: &ClickEvent, _window, cx| {
                        view.toggle_schema(&catalog_owned, &name_owned, cx);
                    }))
                    .child(disclosure_glyph(*expanded))
                    .child(icon(
                        IconName::Schema,
                        theme::SIDEBAR_ROW_ICON_SIZE,
                        colors::FAINT,
                    ))
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
        let schema_owned = schema.to_owned();
        let name_owned = name.to_owned();
        let schema_for_menu = schema.to_owned();
        let name_for_menu = name.to_owned();
        let selected = self
            .selected_relation
            .as_ref()
            .is_some_and(|(s, r)| s == schema && r == name);

        let mut shell = row_shell(theme::SIDEBAR_INDENT_L2)
            .id(ix)
            .cursor_pointer()
            .hover(|this| this.bg(rgb(colors::RAISE)))
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
                relation_tint(kind),
            ))
            .child(row_count(format!("{column_count} cols")));

        if selected {
            shell = shell
                .bg(rgba(theme::SIDEBAR_SELECTED_BG))
                .border_l_2()
                .border_color(rgb(colors::TEAL));
        }
        shell
    }
}

impl SidebarView {
    /// The right-click context menu overlay: `Preview Data`, `View Schema`,
    /// a separator, then `Copy Name`/`Copy Qualified Name`, anchored at the
    /// triggering click's position. Renders nothing when no menu is open.
    fn render_context_menu(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let menu = self.context_menu.clone()?;
        let schema = menu.schema.clone();
        let relation = menu.relation.clone();
        let kind = menu.kind;

        let preview_schema = schema.clone();
        let preview_relation = relation.clone();
        let view_schema_schema = schema.clone();
        let view_schema_relation = relation.clone();

        let content = div()
            .id("sidebar-context-menu")
            .w(theme::CONTEXT_MENU_WIDTH)
            .p(theme::CONTEXT_MENU_PADDING)
            .bg(rgb(colors::RAISE))
            .border_1()
            .border_color(rgb(colors::LINE))
            .rounded(px(theme::CONTEXT_MENU_RADIUS))
            .on_mouse_down_out(cx.listener(|view, _event, _window, cx| {
                view.close_context_menu(cx);
            }))
            .child(context_menu_item(
                cx,
                "Preview Data",
                move |view, window, cx| {
                    view.preview(&preview_schema, &preview_relation, window, cx);
                    view.close_context_menu(cx);
                },
            ))
            .child(context_menu_item(
                cx,
                "View Schema",
                move |view, _window, cx| {
                    view.view_schema(&view_schema_schema, &view_schema_relation, kind, cx);
                    view.close_context_menu(cx);
                },
            ))
            .child(context_menu_separator())
            .child(context_menu_item(cx, "Copy Name", |view, _window, cx| {
                view.copy_name(cx);
            }))
            .child(context_menu_item(
                cx,
                "Copy Qualified Name",
                |view, _window, cx| {
                    view.copy_qualified_name(cx);
                },
            ));

        Some(
            deferred(
                anchored()
                    .position(menu.position)
                    .snap_to_window()
                    .child(content),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}

/// One context menu row.
fn context_menu_item(
    cx: &Context<SidebarView>,
    label: &'static str,
    on_click: impl Fn(&mut SidebarView, &mut Window, &mut Context<SidebarView>) + 'static,
) -> Stateful<Div> {
    div()
        .id(label)
        .flex()
        .flex_row()
        .items_center()
        .h(theme::CONTEXT_MENU_ITEM_HEIGHT)
        .px(theme::CONTEXT_MENU_ITEM_PADDING_X)
        .rounded(px(theme::CONTEXT_MENU_ITEM_RADIUS))
        .cursor_pointer()
        .text_size(px(theme::CONTEXT_MENU_ITEM_TEXT_SIZE))
        .text_color(rgb(colors::TEXT))
        .hover(|el| el.bg(rgba(theme::SIDEBAR_SELECTED_BG)))
        .child(label)
        .on_click(cx.listener(move |view, _event: &ClickEvent, window, cx| {
            on_click(view, window, cx);
        }))
}

/// A thin horizontal divider between context menu item groups.
fn context_menu_separator() -> Div {
    div()
        .h(theme::CONTEXT_MENU_SEPARATOR_HEIGHT)
        .my(theme::CONTEXT_MENU_SEPARATOR_MARGIN_Y)
        .bg(rgb(colors::LINE_SOFT))
}

/// `schema.relation`, the text `Copy Qualified Name` writes to the
/// clipboard.
fn qualified_relation_name(schema: &str, relation: &str) -> String {
    format!("{schema}.{relation}")
}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.nudge_scrollbar_when_tree_unmeasured(cx);
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(colors::PANEL))
            .child(Self::render_header())
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

/// Map a [`RelationKind`] to the icon its sidebar row badge renders.
fn relation_icon_name(kind: RelationKind) -> IconName {
    match kind {
        RelationKind::Table => IconName::Table,
        RelationKind::View => IconName::View,
        RelationKind::MatView => IconName::MaterializedView,
        RelationKind::Partitioned => IconName::PartitionedTable,
    }
}

/// Map a [`RelationKind`] to the tint its sidebar row badge renders with.
fn relation_tint(kind: RelationKind) -> u32 {
    match kind {
        RelationKind::Table => colors::TEAL,
        RelationKind::View => colors::VIEW,
        RelationKind::MatView => colors::MATVIEW,
        RelationKind::Partitioned => colors::PARTITIONED,
    }
}

/// Flatten `tree` into the currently-visible sidebar rows, honoring which
/// catalogs/schemas are collapsed
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
    use zsql_ui::colors;
    use zsql_ui::icon::IconName;
    use zsql_ui::tree::ROW_HEIGHT;

    use super::{
        SidebarRow, flatten_schema_tree, relation_icon_name, relation_tint,
        sidebar_tree_content_height,
    };

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
        assert_eq!(relation_tint(RelationKind::Table), colors::TEAL);
        assert_eq!(relation_tint(RelationKind::View), colors::VIEW);
        assert_eq!(relation_tint(RelationKind::MatView), colors::MATVIEW);
        assert_eq!(
            relation_tint(RelationKind::Partitioned),
            colors::PARTITIONED
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

    #[test]
    fn tree_content_height_stacks_rows_at_row_height_with_no_padding() {
        // The padding around the tree viewport lives outside the
        // uniform_list's scrolled content, so it must not appear here: doing
        // so would overstate the scrollable extent against the
        // padding-excluded viewport height read from the list's bounds.
        assert_eq!(sidebar_tree_content_height(0), gpui::px(0.0));
        assert_eq!(sidebar_tree_content_height(7), ROW_HEIGHT * 7.0);
    }
}

#[cfg(test)]
mod render_tests {
    use gpui::AppContext as _;
    use zsql_core::{Catalog, ColumnMeta, Relation, RelationKind, SchemaNs, SchemaTree};

    use super::{SidebarView, qualified_relation_name};
    use crate::session::{SchemaState, Session};
    use crate::ui::results::ResultsView;
    use crate::ui::tabs::TabModel;

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

    fn build(cx: &mut gpui::TestAppContext, schema: SchemaState) {
        let session = cx.new(|_cx| Session::new_for_schema_test(schema));
        let tabs = build_tabs(session.clone(), cx);
        cx.add_window_view(|_window, cx| SidebarView::new(session, tabs, cx));
    }

    #[gpui::test]
    fn renders_a_populated_schema_tree_without_panicking(cx: &mut gpui::TestAppContext) {
        build(cx, SchemaState::Ready(sample_schema_tree()));
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

        sidebar.read_with(vcx, |view, _app| {
            assert!(
                view.tree_scrollbar_geometry().visible,
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
            build(cx, schema);
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
}
