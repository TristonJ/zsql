//! The schema sidebar: a tree of the connected database's catalog ->
//! schema -> relation structure, driven by a `Session`'s introspected
//! [`zsql_core::SchemaTree`]

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use gpui::{
    App, ClickEvent, Context, Div, Entity, FocusHandle, Focusable, KeyBinding, MouseButton,
    MouseDownEvent, Pixels, Render, Stateful, UniformListScrollHandle, Window, actions, div, point,
    prelude::*, px, rgb, uniform_list,
};
use zsql_core::RelationKind;
use zsql_ui::context_menu::{ContextMenu, ContextMenuItem};
use zsql_ui::icon::{IconName, icon};
use zsql_ui::scrollable::{Axis, ScrollSource, ScrollableState, ScrollbarStyle, WithScrollbars};
use zsql_ui::theme::{ActiveTheme, Theme};
// Imported by name rather than as `zsql_ui::tree::...`: this module already
// uses `tree` as a local variable/parameter name for a `SchemaTree`, and
// qualifying every call here would read as if it referred to that value.
use zsql_ui::tree::{
    META_TEXT_SIZE, ROW_HEIGHT, ROW_TEXT_SIZE, disclosure_glyph, disclosure_spacer, row_count,
    row_label, row_meta, row_shell,
};

use model::{
    ScriptRow, SessionScript, SidebarPane, SidebarRow, build_script_rows, flatten_schema_tree,
    relation_icon_name, relation_tint,
};

use super::connections::UNSAVED_CONNECTION_LABEL;
use super::open_modal::{LibraryScript, PickerTarget};
use super::tabs::TabModel;
use super::theme;
use super::time_fmt;
use crate::session::{SchemaState, Session, SessionState};
use crate::session_store::{self, SessionDir};

mod context_menu;
mod db_row;
mod filter;
mod find;
mod model;
mod pane;
mod scripts;
mod scripts_refresh;

/// The key context the sidebar's own key bindings are scoped to.
pub const KEY_CONTEXT: &str = "Sidebar";

actions!(zsql_sidebar, [OpenFind, CloseFind]);

/// The context predicate the sidebar's open-find binding matches against:
/// the sidebar's own context, but never while keyboard focus sits inside a
/// text field (the find row's own input renders within this context).
const BINDING_CONTEXT: &str = "Sidebar && !TextField";

/// The keystroke that opens the sidebar's find row, shared with
/// [`crate::ui::workspace`]'s hover-based routing so the two paths can
/// never diverge.
pub(crate) const OPEN_FIND_KEYSTROKE: &str = "secondary-f";

/// Register the sidebar's key bindings. Call once at startup, before any
/// window that hosts a [`SidebarView`] is opened.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(OPEN_FIND_KEYSTROKE, OpenFind, Some(BINDING_CONTEXT)),
        KeyBinding::new("escape", CloseFind, Some(find::KEY_CONTEXT)),
    ]);
}

/// What [`SidebarView::render_body`] shows in place of the tree: `None`
/// means the tree itself should render instead.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SidebarPlaceholder {
    /// No connection has been attempted, or the last one failed before any
    /// schema tree existed.
    NotLoaded,
    /// A connect or an introspection is in flight.
    Loading,
    /// Introspection failed. The message is safe to show directly in the
    /// UI.
    Error(String),
    /// Introspection succeeded but the connected database reported no
    /// catalogs.
    EmptySchema,
}

/// The scripts pane's connection group label shows
/// [`UNSAVED_CONNECTION_LABEL`] until the workspace reports the active
/// connection's real display name via [`SidebarView::set_connection_name`]
/// -- the same fallback the Open Script picker's own header uses when no
/// connection is tracked as active.
const DEFAULT_CONNECTION_NAME: &str = UNSAVED_CONNECTION_LABEL;

/// The sidebar's placeholder for `state`/`schema`, in this precedence
/// order:
///
/// 1. [`SessionState::Connecting`] always renders
///    [`SidebarPlaceholder::Loading`], even while `schema` is still
///    [`SchemaState::NotLoaded`] -- a first connect resets the schema to
///    `NotLoaded` before the connect attempt starts, and without this
///    override the sidebar would show the "connect to a database" prompt
///    while a connection is actively being established. This mirrors
///    `footer::footer_display`'s and `results::status_indicator`'s own
///    Connecting override.
/// 2. Otherwise, `schema` alone decides: [`SchemaState::NotLoaded`] and
///    [`SchemaState::Loading`] map to [`SidebarPlaceholder::NotLoaded`] and
///    [`SidebarPlaceholder::Loading`] respectively, [`SchemaState::Error`]
///    maps to [`SidebarPlaceholder::Error`], an empty
///    [`SchemaState::Ready`] tree maps to [`SidebarPlaceholder::EmptySchema`],
///    and a populated [`SchemaState::Ready`] tree renders the tree itself
///    (`None`).
fn sidebar_placeholder(state: &SessionState, schema: &SchemaState) -> Option<SidebarPlaceholder> {
    if matches!(state, SessionState::Connecting) {
        return Some(SidebarPlaceholder::Loading);
    }
    match schema {
        SchemaState::NotLoaded => Some(SidebarPlaceholder::NotLoaded),
        SchemaState::Loading => Some(SidebarPlaceholder::Loading),
        SchemaState::Error(message) => Some(SidebarPlaceholder::Error(message.clone())),
        SchemaState::Ready(tree) if tree.catalogs.is_empty() => {
            Some(SidebarPlaceholder::EmptySchema)
        }
        SchemaState::Ready(_) => None,
    }
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
    /// The tree's scrollbar state.
    scroll: Entity<ScrollableState>,
    /// The currently open relation-row context menu, if any.
    context_menu: Option<context_menu::ContextMenuState>,
    /// Whether the database row's switcher dropdown (see
    /// [`Self::render_db_switcher_menu`]) is currently open.
    db_switcher_open: bool,
    /// Root directory the shared library's flat pool of `.sql` files lives
    /// under. `None` yields no library rows (the shared pool is
    /// unavailable)
    library_dir: Option<PathBuf>,
    /// The active connection's session directory, for resolving a named
    /// session script's last-modified time. `None` while no connection is
    /// tracked (or it has no resolvable session directory).
    session_dir: Option<PathBuf>,
    /// The SCRIPTS/LIBRARY rows currently shown, rebuilt whenever `tabs`
    /// changes (a script saved, renamed, closed, or the connection switched)
    script_rows: Vec<ScriptRow>,
    /// Which full-height pane is currently shown. In-memory only
    active_pane: SidebarPane,
    /// The active connection's display name, shown by the scripts pane's
    /// "THIS CONNECTION" group label. Updated by the workspace whenever the
    /// active connection switches
    connection_name: String,
    /// Scroll handle for the Scripts pane's row list, tracked by its own
    /// scrollbar overlay the same way [`Self::tree_scroll_handle`] backs the
    /// schema tree's.
    scripts_scroll_handle: gpui::ScrollHandle,
    /// The Scripts pane's scrollbar state.
    scripts_scroll: Entity<ScrollableState>,
    /// How often [`Self::scripts_refresh_task`] recomputes every script
    /// row's relative-modified-time label.
    scripts_refresh_interval: Duration,
    /// The self-rescheduling refresh loop spawned in [`Self::new`]. Held
    /// (not detached) so replacing it cancels the previous loop -- see
    /// [`Self::set_scripts_refresh_interval`].
    scripts_refresh_task: Option<gpui::Task<()>>,
    /// Bumped by every synchronous [`Self::sync_script_rows`] call. Each
    /// [`Self::spawn_scripts_refresh_loop`] iteration captures this value
    /// before dispatching its background scan and compares it again once
    /// the scan completes, dropping a result that no longer matches
    script_rows_generation: u64,
    /// The most recent disk listings, kept so a tabs notify (tab switch,
    /// edit debounce) can rebuild the rows' open/active markers without
    /// rescanning two directories on the render thread
    cached_session_scripts: Vec<SessionScript>,
    cached_library_scripts: Vec<LibraryScript>,
    /// The open quick-find session, if any. `None` shows the database row
    /// (schema pane) or nothing (scripts pane) in its slot instead.
    find: Option<find::SidebarFind>,
    /// The expand/collapse choices captured just before the live query went
    /// non-empty, restored exactly once it clears.
    pre_filter_collapse: Option<filter::CollapseSnapshot>,
    /// Whether the pointer currently sits over the sidebar, for Ctrl+F's
    /// hover-based routing against the results grid's own quick find.
    pointer_hovering: bool,
    /// This view's own focus target, so a tab or a click can move keyboard
    /// focus onto the sidebar and its find input registers as within it.
    focus_handle: FocusHandle,
}

impl SidebarView {
    /// Build a sidebar over `session`, previewing clicked relations by
    /// opening (or reusing) a generated tab in `tabs`
    #[must_use]
    pub fn new(
        session: Entity<Session>,
        tabs: Entity<TabModel>,
        library_dir: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&session, |view: &mut Self, _session, cx| {
            if view.sync_rows_if_schema_changed(cx) {
                cx.notify();
            }
        })
        .detach();
        cx.observe(&tabs, |view: &mut Self, _tabs, cx| {
            view.rebuild_script_rows_from_cache(cx);
            cx.notify();
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
            db_switcher_open: false,
            library_dir,
            session_dir: None,
            script_rows: Vec::new(),
            active_pane: SidebarPane::default(),
            connection_name: DEFAULT_CONNECTION_NAME.to_owned(),
            scripts_scroll_handle: gpui::ScrollHandle::new(),
            scripts_scroll: cx.new(ScrollableState::new),
            scripts_refresh_interval: crate::config::SidebarConfig::default()
                .scripts_relative_time_refresh(),
            scripts_refresh_task: None,
            script_rows_generation: 0,
            cached_session_scripts: Vec::new(),
            cached_library_scripts: Vec::new(),
            find: None,
            pre_filter_collapse: None,
            pointer_hovering: false,
            focus_handle: cx.focus_handle(),
        };
        view.sync_rows(cx);
        view.sync_script_rows(cx);
        view.spawn_scripts_refresh_loop(cx);
        view
    }

    /// Update the active connection's session directory, so a named session
    /// script's row shows the right last-modified time
    pub fn set_session_dir(&mut self, session_dir: Option<PathBuf>, cx: &mut Context<Self>) {
        self.session_dir = session_dir;
        self.sync_script_rows(cx);
        cx.notify();
    }

    /// Update the active connection's display name shown by the scripts
    /// pane's "THIS CONNECTION" group label
    pub fn set_connection_name(&mut self, connection_name: String, cx: &mut Context<Self>) {
        self.connection_name = connection_name;
        cx.notify();
    }

    /// Rebuild the SCRIPTS/LIBRARY rows from disk right now
    pub fn resync_scripts(&mut self, cx: &mut Context<Self>) {
        self.sync_script_rows(cx);
        cx.notify();
    }

    /// The Scripts/Library rows currently shown
    #[cfg(test)]
    pub(crate) fn script_rows_for_test(&self) -> &[ScriptRow] {
        &self.script_rows
    }

    /// Whether the quick-find row is currently open.
    #[cfg(test)]
    pub(crate) fn find_is_open_for_test(&self) -> bool {
        self.find.is_some()
    }

    /// Switch which full-height pane the sidebar shows. A no-op (no
    /// re-render, no other state touched) when `pane` is already active.
    #[tracing::instrument(name = "sidebar_switch_pane", skip(self, cx))]
    fn switch_pane(&mut self, pane: SidebarPane, cx: &mut Context<Self>) {
        if self.active_pane == pane {
            return;
        }
        self.active_pane = pane;
        // The database row and its menu only ever render in the schema
        // pane; drop a stale open flag now rather than leaving it to
        // silently reappear next time the schema pane comes back.
        if pane != SidebarPane::Schema {
            self.close_db_switcher(cx);
        }
        find::sync_placeholder_for_pane(self, pane, cx);
        cx.notify();
    }

    /// Rebuild [`Self::script_rows`] from a disk scan of the active
    /// connection's session directory
    #[tracing::instrument(name = "sidebar_sync_script_rows", skip(self, cx))]
    fn sync_script_rows(&mut self, cx: &mut Context<Self>) {
        self.script_rows_generation += 1;
        let tabs = self.tabs.read(cx);
        let active_id = tabs.active_id();
        let open_session_tabs = tabs.named_open_scripts_by_file();
        let open_library_tabs = tabs.open_library_tabs();
        let now = SystemTime::now();

        let session_scripts: Vec<SessionScript> = self
            .session_dir
            .as_ref()
            .and_then(|dir| SessionDir::at(dir).list_scripts().ok())
            .unwrap_or_default()
            .into_iter()
            .map(|entry| SessionScript {
                file_name: entry.file_name,
                relative_time: time_fmt::relative_time(now, entry.modified),
            })
            .collect();

        let library_scripts: Vec<LibraryScript> = self
            .library_dir
            .as_ref()
            .and_then(|dir| session_store::LibraryDir::at(dir).list().ok())
            .unwrap_or_default()
            .into_iter()
            .map(|entry| LibraryScript {
                name: entry.name,
                relative_time: time_fmt::relative_time(now, entry.modified),
            })
            .collect();

        self.cached_session_scripts = session_scripts;
        self.cached_library_scripts = library_scripts;
        self.script_rows = build_script_rows(
            active_id,
            &self.cached_session_scripts,
            &open_session_tabs,
            &self.cached_library_scripts,
            &open_library_tabs,
        );
    }

    /// Rebuild [`Self::script_rows`]' open/active markers from the cached
    /// listings, without touching disk
    fn rebuild_script_rows_from_cache(&mut self, cx: &mut Context<Self>) {
        self.script_rows_generation += 1;
        let tabs = self.tabs.read(cx);
        let active_id = tabs.active_id();
        let open_session_tabs = tabs.named_open_scripts_by_file();
        let open_library_tabs = tabs.open_library_tabs();
        self.script_rows = build_script_rows(
            active_id,
            &self.cached_session_scripts,
            &open_session_tabs,
            &self.cached_library_scripts,
            &open_library_tabs,
        );
    }

    /// Open (or focus) the tab `target` names, and move keyboard focus onto
    /// it
    fn open_script_row(
        &mut self,
        target: PickerTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            PickerTarget::FocusTab(id) => {
                self.tabs.update(cx, |tabs, cx| tabs.set_active(id, cx));
            }
            PickerTarget::OpenLibrary(name) => {
                self.tabs
                    .update(cx, |tabs, cx| tabs.open_or_focus_library(&name, cx));
            }
            PickerTarget::OpenSessionScript(file_name) => {
                self.tabs.update(cx, |tabs, cx| {
                    tabs.open_or_focus_session_script(&file_name, cx);
                });
            }
        }
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
    /// open (or reuse) a generated tab for it and move keyboard focus onto
    /// that tab's editor.
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

    /// [`OpenFind`]'s handler: open the quick-find row and focus its input,
    /// or refocus it if already open.
    #[tracing::instrument(name = "sidebar_open_find", skip_all)]
    pub(crate) fn open_find(&mut self, _: &OpenFind, window: &mut Window, cx: &mut Context<Self>) {
        find::open(self, window, cx);
    }

    /// [`CloseFind`]'s handler: close the find row and restore the
    /// collapse state captured before filtering began.
    fn close_find(&mut self, _: &CloseFind, window: &mut Window, cx: &mut Context<Self>) {
        find::close(self, window, cx);
    }

    /// Whether the pointer currently sits over the sidebar, for Ctrl+F's
    /// hover-based routing between this view and the results grid.
    pub(crate) fn is_pointer_hovering(&self) -> bool {
        self.pointer_hovering
    }

    /// Open (or close, if already open) the database-switcher dropdown.
    fn toggle_db_switcher(&mut self, cx: &mut Context<Self>) {
        self.db_switcher_open = !self.db_switcher_open;
        cx.notify();
    }

    /// Close the database-switcher dropdown, if open.
    fn close_db_switcher(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.db_switcher_open) {
            cx.notify();
        }
    }

    /// Switch the session to `database` via [`Session::switch_database`]
    /// and close the dropdown. While the switch (and its re-introspect) is
    /// in flight, the tree falls back to the existing
    /// [`SchemaState::Loading`] placeholder; a failure surfaces through
    /// [`Session::state`] the same way any other session error does, and
    /// [`Session::current_database`] simply never changes, so the trigger's
    /// displayed selection reverts on its own.
    fn select_database(&mut self, database: String, cx: &mut Context<Self>) {
        self.close_db_switcher(cx);
        self.session
            .update(cx, |session, cx| session.switch_database(database, cx))
            .detach();
    }

    /// The database-switcher's open dropdown: one item per
    /// [`Session::available_databases`] entry, the current database
    /// highlighted. `None` when the dropdown is closed. Anchored to the
    /// database row's own top-left corner, so it drops immediately below
    /// the row regardless of which position it renders at.
    fn render_db_switcher_menu(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        if !self.db_switcher_open {
            return None;
        }
        let session = self.session.read(cx);
        let current = session.current_database().map(str::to_owned);
        let databases = session.available_databases().to_owned();

        let mut menu = ContextMenu::new("sidebar-db-switcher-menu")
            .anchor(gpui::Corner::TopLeft)
            .offset(point(px(0.0), px(0.0)))
            .on_close(cx.listener(|view, _event, _window, cx| {
                view.close_db_switcher(cx);
            }));
        for database in databases {
            let selected = current.as_deref() == Some(database.as_str());
            let label = if selected {
                format!("\u{2022}\t{database}")
            } else {
                format!("  {database}")
            };
            let target = database.clone();
            let item_id = format!("sidebar-db-switcher-item-{database}");
            menu = menu.add_item(
                ContextMenuItem::with_id(item_id, label).on_click(cx.listener(
                    move |view, _event, _window, cx| {
                        view.select_database(target.clone(), cx);
                    },
                )),
            );
        }

        Some(menu.into_any_element())
    }

    /// The main content area: the tree when a schema is loaded, or a
    /// centered prompt/status message for every other `SchemaState` (see
    /// [`sidebar_placeholder`], including its `SessionState::Connecting`
    /// override).
    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active_theme = cx.theme();
        let colors = active_theme.colors;
        let placeholder = {
            let session = self.session.read(cx);
            sidebar_placeholder(session.state(), session.schema())
        };

        match placeholder {
            Some(SidebarPlaceholder::NotLoaded) => Self::render_placeholder(
                colors.text_tertiary,
                "No schema",
                "Connect to a database to browse its schema.",
                active_theme,
            )
            .into_any_element(),
            Some(SidebarPlaceholder::Loading) => Self::render_placeholder(
                colors.text_tertiary,
                "Loading schema...",
                "Fetching catalogs, schemas, and relations.",
                active_theme,
            )
            .into_any_element(),
            Some(SidebarPlaceholder::Error(message)) => Self::render_placeholder(
                colors.status_error,
                "Schema unavailable",
                &message,
                active_theme,
            )
            .into_any_element(),
            Some(SidebarPlaceholder::EmptySchema) => Self::render_placeholder(
                colors.text_tertiary,
                "No catalogs",
                "The connected database reported no catalogs.",
                active_theme,
            )
            .into_any_element(),
            None => self.render_tree(window, cx).into_any_element(),
        }
    }

    /// The schema pane's body, routed through the live find filter when a
    /// session is open with a non-empty query.
    fn render_schema_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        find::render_schema_body(self, window, cx)
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
    pub(super) fn tree_scrollbar_style(active_theme: &Theme) -> ScrollbarStyle {
        ScrollbarStyle::themed(
            &active_theme.colors,
            f32::from(theme::SIDEBAR_SCROLLBAR_WIDTH),
            theme::SIDEBAR_SCROLLBAR_RADIUS,
            f32::from(theme::SIDEBAR_SCROLLBAR_GAP),
        )
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
            } => self.render_relation_row(ix, schema, name, *kind, *column_count, None, cx),
        }
    }

    /// A relation row: left-click previews it, right-click opens its
    /// context menu, and a currently-selected relation gets a teal left
    /// border and tinted background. `label_match`, when the row is
    /// rendered under a live filter, washes that byte range of the label
    /// in the shared quick-find amber.
    #[allow(clippy::too_many_arguments)]
    fn render_relation_row(
        &self,
        ix: usize,
        schema: &str,
        name: &str,
        kind: RelationKind,
        column_count: usize,
        label_match: Option<&filter::MatchRange>,
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
                    context_menu::open(
                        view,
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
            .child(find::highlighted_row_label(name, label_match, active_theme))
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
                .bg(theme::sidebar_selected_bg(active_theme))
                .border_l_2()
                .border_color(rgb(active_theme.colors.accent));
        }
        shell
    }
}

impl Focusable for SidebarView {
    fn focus_handle(&self, _cx: &gpui::App) -> gpui::FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_theme = cx.theme();
        let mut root = div()
            .id("sidebar-root")
            .debug_selector(|| "sidebar-root".to_owned())
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::open_find))
            .on_action(cx.listener(Self::close_find))
            .on_hover(cx.listener(|view, hovered: &bool, _window, _cx| {
                view.pointer_hovering = *hovered;
            }))
            .bg(rgb(active_theme.colors.bg_panel))
            .child(pane::render_pane_tabs(self, window, cx))
            .children(find::render_top_slot(self, cx));

        root = match self.active_pane {
            SidebarPane::Schema => root.child(self.render_schema_pane(window, cx)),
            SidebarPane::Scripts => root.child(scripts::render_scripts_pane(self, window, cx)),
        };

        root.children(context_menu::render(self, cx))
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
    use zsql_core::{Catalog, SchemaNs, SchemaTree};
    use zsql_ui::theme::Theme;
    use zsql_ui::tree::ROW_HEIGHT;

    use super::{
        SidebarPlaceholder, SidebarView, sidebar_placeholder, sidebar_tree_content_height,
    };
    use crate::session::{SchemaState, SessionState};
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

    #[test]
    fn a_first_connect_in_flight_shows_the_loading_placeholder_not_the_connect_prompt() {
        assert_eq!(
            sidebar_placeholder(&SessionState::Connecting, &SchemaState::NotLoaded),
            Some(SidebarPlaceholder::Loading),
            "Connecting must override the stale NotLoaded schema left by a first connect"
        );
    }

    #[test]
    fn a_database_switch_in_flight_still_shows_the_loading_placeholder() {
        // `switch_database` resets the schema to `Loading` (not `NotLoaded`)
        // before moving `state` to `Connecting`; the Connecting override
        // must not change what is already the correct placeholder here.
        assert_eq!(
            sidebar_placeholder(&SessionState::Connecting, &SchemaState::Loading),
            Some(SidebarPlaceholder::Loading)
        );
    }

    #[test]
    fn a_failed_first_connect_reverts_to_the_not_loaded_placeholder() {
        assert_eq!(
            sidebar_placeholder(
                &SessionState::Error("connection refused".to_owned()),
                &SchemaState::NotLoaded
            ),
            Some(SidebarPlaceholder::NotLoaded),
            "a failed connect must not leave the sidebar stuck on the loading placeholder"
        );
    }

    #[test]
    fn a_query_error_on_an_already_loaded_schema_does_not_touch_the_tree_placeholder() {
        // A query error (as opposed to a connect failure) never touches
        // `SchemaState`, so the tree must keep rendering rather than
        // falling back to any placeholder.
        let tree = SchemaTree {
            catalogs: vec![Catalog {
                name: "zsql".to_owned(),
                schemas: vec![SchemaNs {
                    name: "public".to_owned(),
                    tables: vec![],
                }],
            }],
        };
        assert_eq!(
            sidebar_placeholder(
                &SessionState::Error("syntax error".to_owned()),
                &SchemaState::Ready(tree)
            ),
            None
        );
    }

    #[test]
    fn a_successful_connect_proceeds_from_not_loaded_through_loading_to_the_tree() {
        assert_eq!(
            sidebar_placeholder(&SessionState::Connected, &SchemaState::NotLoaded),
            Some(SidebarPlaceholder::NotLoaded)
        );
        assert_eq!(
            sidebar_placeholder(&SessionState::Connected, &SchemaState::Loading),
            Some(SidebarPlaceholder::Loading)
        );
        assert_eq!(
            sidebar_placeholder(
                &SessionState::Connected,
                &SchemaState::Ready(SchemaTree::default())
            ),
            Some(SidebarPlaceholder::EmptySchema)
        );
    }

    #[test]
    fn a_schema_introspection_error_maps_to_error_placeholder() {
        assert_eq!(
            sidebar_placeholder(
                &SessionState::Connected,
                &SchemaState::Error("boom".to_owned())
            ),
            Some(SidebarPlaceholder::Error("boom".to_owned()))
        );
    }
}

#[cfg(test)]
mod render_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use gpui::AppContext as _;
    use zsql_core::{
        BatchSink, Catalog, ColumnMeta, Connection, CoreError, QueryHandle, RelationSchema,
        RowCount, SchemaNs, SchemaTree,
    };
    use zsql_core::{Relation, RelationKind};

    use zsql_ui::scrollable::vertical_thumb_debug_selector;

    use super::{
        SidebarPane, SidebarPlaceholder, SidebarView, context_menu, db_row, filter, find,
        sidebar_placeholder,
    };
    use crate::session::{SchemaState, Session, SessionState};
    use crate::ui::results::ResultsView;
    use crate::ui::tabs::{OpenRequested, ResultsChanged, TabModel};

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

        async fn count_rows(
            &self,
            _schema: &str,
            _relation: &str,
            _filters: &zsql_core::FilterState,
        ) -> Result<RowCount, CoreError> {
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
        cx.new(|cx| TabModel::new(session, cx))
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
        cx.add_window_view(|_window, cx| SidebarView::new(session, tabs, None, cx))
    }

    /// Like [`build`], but over a session in `state` rather than always
    /// `SessionState::Connected` -- for tests that need control over the
    /// session's connect lifecycle state, not just its schema.
    fn build_with_state(
        cx: &mut gpui::TestAppContext,
        state: SessionState,
        schema: SchemaState,
    ) -> (gpui::Entity<SidebarView>, &mut gpui::VisualTestContext) {
        let session = cx.new(|_cx| {
            let mut session = Session::new_for_render_test(state, zsql_core::ResultSet::default());
            session.set_schema_for_test(schema);
            session
        });
        let tabs = build_tabs(session.clone(), cx);
        cx.add_window_view(|_window, cx| SidebarView::new(session, tabs, None, cx))
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
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session, tabs, None, cx));
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

    /// Regression test for a first-time connect: while `SessionState` is
    /// `Connecting` and the schema has not yet started loading, the sidebar
    /// must show the loading placeholder, not the stale "connect to a
    /// database" prompt.
    #[gpui::test]
    fn a_connect_in_flight_renders_the_loading_placeholder_not_the_connect_prompt(
        cx: &mut gpui::TestAppContext,
    ) {
        let (sidebar, vcx) = build_with_state(cx, SessionState::Connecting, SchemaState::NotLoaded);
        sidebar.read_with(vcx, |view, app| {
            assert!(
                view.rows.is_empty(),
                "no catalog/table rows are known while a first connect is in flight"
            );
            let session = view.session.read(app);
            assert_eq!(
                sidebar_placeholder(session.state(), session.schema()),
                Some(SidebarPlaceholder::Loading),
                "a connect in flight must show the loading placeholder, not the connect prompt"
            );
        });
    }

    /// Regression test: once a first connect fails, the sidebar must revert
    /// to the "connect to a database" placeholder rather than staying stuck
    /// on the loading placeholder shown while it was in flight.
    #[gpui::test]
    fn a_failed_first_connect_renders_the_connect_prompt_placeholder(
        cx: &mut gpui::TestAppContext,
    ) {
        let (sidebar, vcx) = build_with_state(
            cx,
            SessionState::Error("connection refused".to_owned()),
            SchemaState::NotLoaded,
        );
        sidebar.read_with(vcx, |view, app| {
            assert!(view.rows.is_empty());
            let session = view.session.read(app);
            assert_eq!(
                sidebar_placeholder(session.state(), session.schema()),
                Some(SidebarPlaceholder::NotLoaded),
                "a failed first connect must not stay stuck on the loading placeholder"
            );
        });
    }

    #[gpui::test]
    fn an_unrelated_session_notify_does_not_reflatten_the_row_cache(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

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
        let tabs = cx.new(|cx| TabModel::new(session.clone(), cx));
        // Mirror the workspace's wiring: the results view learns what to
        // show from the tab model's events, not from the model directly.
        let results_for_events = results.clone();
        cx.update(|cx| {
            cx.subscribe(&tabs, move |_tabs, evt: &ResultsChanged, cx| {
                results_for_events.update(cx, |results, cx| match evt {
                    ResultsChanged::Live(label) => results.show_live(label, cx),
                    ResultsChanged::LiveWindowChanged(label) => {
                        results.show_live_window(label, cx);
                    }
                    ResultsChanged::Snapshot(snap) => results.show_snapshot(snap.clone(), cx),
                });
            })
            .detach();
        });
        let tabs_for_view = tabs.clone();
        let (sidebar, vcx) = cx.add_window_view(|_window, cx| {
            SidebarView::new(session_for_view, tabs_for_view, None, cx)
        });

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
        let (sidebar, vcx) = cx.add_window_view(|_window, cx| {
            SidebarView::new(session_for_view, tabs_for_view, None, cx)
        });

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
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

        sidebar.update(vcx, |view, cx| {
            context_menu::open(
                view,
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

        sidebar.update(vcx, context_menu::close);
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
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

        sidebar.update(vcx, |view, cx| {
            context_menu::open(
                view,
                "public".to_owned(),
                "orders".to_owned(),
                RelationKind::Table,
                0,
                gpui::point(gpui::px(10.0), gpui::px(20.0)),
                cx,
            );
            context_menu::copy_name(view, cx);
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
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

        sidebar.update(vcx, |view, cx| {
            context_menu::open(
                view,
                "public".to_owned(),
                "orders".to_owned(),
                RelationKind::Table,
                0,
                gpui::point(gpui::px(10.0), gpui::px(20.0)),
                cx,
            );
            context_menu::copy_qualified_name(view, cx);
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
        assert_eq!(
            context_menu::qualified_relation_name("public", "orders"),
            "public.orders"
        );
    }

    #[gpui::test]
    fn the_rendered_context_menu_does_not_panic_while_open(cx: &mut gpui::TestAppContext) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

        sidebar.update(vcx, |view, cx| {
            context_menu::open(
                view,
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
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));
        vcx.run_until_parked();

        // Once the tree viewport is measured, a row anchor is derived from
        // the row's laid-out geometry (its top at the viewport's right edge),
        // and successive rows anchor lower than their predecessors.
        sidebar.read_with(vcx, |view, _app| {
            let first = context_menu::relation_row_anchor(view, 0)
                .expect("a measured tree yields a row anchor");
            let second = context_menu::relation_row_anchor(view, 1)
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
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

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
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

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
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

        sidebar.update(vcx, SidebarView::refresh_schema);
        vcx.run_until_parked();

        session.read_with(vcx, |session, _app| {
            assert!(
                matches!(session.schema(), SchemaState::NotLoaded),
                "refreshing without a connection must not change the schema state"
            );
        });
    }

    // -- database switcher ----------------------------------------

    /// A session already connected (via a fake connection, no real network
    /// I/O) with `databases` as its available-databases list and `current`
    /// as its current database.
    fn session_with_databases(
        databases: &[&str],
        current: Option<&str>,
        cx: &mut gpui::TestAppContext,
    ) -> gpui::Entity<Session> {
        use std::sync::Arc;

        cx.new(|_cx| {
            let mut session =
                Session::new_for_switch_test(Arc::new(RefreshingConnection), "sqlite::memory:");
            session.set_schema_for_test(SchemaState::Ready(sample_schema_tree()));
            session.set_available_databases_for_test(
                databases.iter().map(|d| (*d).to_owned()).collect(),
            );
            session.set_current_database_for_test(current.map(str::to_owned));
            session
        })
    }

    #[gpui::test]
    fn the_database_row_is_absent_with_zero_or_one_available_databases(
        cx: &mut gpui::TestAppContext,
    ) {
        for databases in [Vec::<&str>::new(), vec!["only_db"]] {
            let session = session_with_databases(&databases, databases.first().copied(), cx);
            let session_for_view = session.clone();
            let tabs = build_tabs(session.clone(), cx);
            let (sidebar, vcx) = cx
                .add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

            sidebar.update(vcx, |view, cx| {
                assert!(
                    db_row::render_db_row(view, cx).is_none(),
                    "expected no database row with {} available database(s)",
                    databases.len()
                );
            });
        }
    }

    #[gpui::test]
    fn the_database_row_is_shown_with_more_than_one_available_database(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = session_with_databases(&["alpha", "beta"], Some("alpha"), cx);
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

        sidebar.update(vcx, |view, cx| {
            assert!(
                db_row::render_db_row(view, cx).is_some(),
                "expected a database row with more than one available database"
            );
        });
    }

    #[gpui::test]
    fn the_database_row_stays_shown_while_a_database_switch_is_in_flight(
        cx: &mut gpui::TestAppContext,
    ) {
        // `select_database` moves the session synchronously into
        // `SessionState::Connecting` (see `selecting_a_database_...` below);
        // the row must not disappear just because a switch is in flight.
        let session = session_with_databases(&["alpha", "beta"], Some("alpha"), cx);
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

        sidebar.update(vcx, |view, cx| {
            view.select_database("beta".to_owned(), cx);
        });

        session.read_with(vcx, |session, _app| {
            assert_eq!(session.state(), &SessionState::Connecting);
        });
        sidebar.update(vcx, |view, cx| {
            assert!(
                db_row::render_db_row(view, cx).is_some(),
                "the database row must stay visible while a switch is in flight"
            );
        });
    }

    #[gpui::test]
    fn the_database_row_is_absent_when_the_scripts_pane_is_active_even_with_many_databases(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = session_with_databases(&["alpha", "beta"], Some("alpha"), cx);
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

        sidebar.update(vcx, |view, cx| {
            view.switch_pane(SidebarPane::Scripts, cx);
            assert!(
                db_row::render_db_row(view, cx).is_none(),
                "the database row is schema-pane-only, regardless of database count"
            );
        });
    }

    #[gpui::test]
    fn toggling_the_switcher_opens_and_closes_its_menu(cx: &mut gpui::TestAppContext) {
        let session = session_with_databases(&["alpha", "beta"], Some("alpha"), cx);
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

        sidebar.read_with(vcx, |view, _app| assert!(!view.db_switcher_open));

        sidebar.update(vcx, SidebarView::toggle_db_switcher);
        sidebar.read_with(vcx, |view, _app| assert!(view.db_switcher_open));

        sidebar.update(vcx, SidebarView::close_db_switcher);
        sidebar.read_with(vcx, |view, _app| assert!(!view.db_switcher_open));
    }

    /// Selecting a database from the switcher closes the menu synchronously
    /// and dispatches the switch through `Session::switch_database`: the
    /// session's state moves to `Connecting` in that same call, exactly as
    /// calling `switch_database` directly does (see `session::tests`).
    #[gpui::test]
    fn selecting_a_database_closes_the_menu_and_dispatches_the_switch(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = session_with_databases(&["alpha", "beta"], Some("alpha"), cx);
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

        sidebar.update(vcx, SidebarView::toggle_db_switcher);
        sidebar.read_with(vcx, |view, _app| assert!(view.db_switcher_open));

        sidebar.update(vcx, |view, cx| {
            view.select_database("beta".to_owned(), cx);
        });

        sidebar.read_with(vcx, |view, _app| {
            assert!(
                !view.db_switcher_open,
                "selecting an entry must close the menu synchronously"
            );
        });
        session.read_with(vcx, |session, _app| {
            assert!(
                matches!(session.state(), SessionState::Connecting),
                "expected switch_database to have been dispatched synchronously, got {:?}",
                session.state()
            );
        });
    }

    #[gpui::test]
    fn the_rendered_database_switcher_menu_does_not_panic_while_open(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = session_with_databases(&["alpha", "beta", "gamma"], Some("beta"), cx);
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

        sidebar.update(vcx, SidebarView::toggle_db_switcher);
        // Forces a render pass with the switcher's deferred/anchored
        // overlay on screen, catching a panic in the overlay itself.
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| assert!(view.db_switcher_open));
    }

    #[gpui::test]
    fn set_connection_name_updates_the_scripts_panes_connection_group_label_source(
        cx: &mut gpui::TestAppContext,
    ) {
        let (sidebar, vcx) = build(cx, SchemaState::Ready(sample_schema_tree()));
        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(view.connection_name, "Unsaved");
        });

        sidebar.update(vcx, |view, cx| {
            view.set_connection_name("zsql-dev".to_owned(), cx);
        });
        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(view.connection_name, "zsql-dev");
        });
    }

    // -- pane switcher ----------------------------------------------

    #[gpui::test]
    fn a_new_sidebar_always_starts_on_the_schema_pane(cx: &mut gpui::TestAppContext) {
        let (sidebar, vcx) = build(cx, SchemaState::Ready(sample_schema_tree()));
        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(view.active_pane, SidebarPane::Schema);
        });
    }

    #[gpui::test]
    fn switching_the_pane_updates_active_pane_and_is_idempotent_once_active(
        cx: &mut gpui::TestAppContext,
    ) {
        let (sidebar, vcx) = build(cx, SchemaState::Ready(sample_schema_tree()));

        sidebar.update(vcx, |view, cx| view.switch_pane(SidebarPane::Scripts, cx));
        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(view.active_pane, SidebarPane::Scripts);
        });

        // Switching to the pane that is already active must not panic or
        // change anything further.
        sidebar.update(vcx, |view, cx| view.switch_pane(SidebarPane::Scripts, cx));
        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(view.active_pane, SidebarPane::Scripts);
        });
    }

    #[gpui::test]
    fn leaving_the_schema_pane_closes_an_open_database_switcher(cx: &mut gpui::TestAppContext) {
        let (sidebar, vcx) = build(cx, SchemaState::Ready(sample_schema_tree()));

        sidebar.update(vcx, super::SidebarView::toggle_db_switcher);
        sidebar.read_with(vcx, |view, _app| {
            assert!(view.db_switcher_open);
        });

        sidebar.update(vcx, |view, cx| view.switch_pane(SidebarPane::Scripts, cx));
        sidebar.read_with(vcx, |view, _app| {
            assert!(
                !view.db_switcher_open,
                "a db switcher left open must not silently reappear when the \
                 schema pane comes back"
            );
        });
    }

    /// Switching panes must not reset any of the schema pane's own state --
    /// the collapsed tree nodes and the selected relation survive a round
    /// trip through the scripts pane and back.
    #[gpui::test]
    fn switching_panes_preserves_collapsed_tree_state_and_the_selected_relation(
        cx: &mut gpui::TestAppContext,
    ) {
        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let tabs_for_view = tabs.clone();
        let (sidebar, vcx) = cx.add_window_view(|_window, cx| {
            SidebarView::new(session_for_view, tabs_for_view, None, cx)
        });

        sidebar.update_in(vcx, |view, window, cx| {
            view.toggle_catalog("zsql", cx);
            view.preview("public", "orders", window, cx);
        });
        vcx.run_until_parked();

        let (collapsed_before, selected_before, rows_before) =
            sidebar.read_with(vcx, |view, _app| {
                (
                    view.collapsed_catalogs.clone(),
                    view.selected_relation.clone(),
                    view.script_rows.clone(),
                )
            });

        sidebar.update(vcx, |view, cx| {
            view.switch_pane(SidebarPane::Scripts, cx);
            view.switch_pane(SidebarPane::Schema, cx);
        });

        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(view.collapsed_catalogs, collapsed_before);
            assert_eq!(view.selected_relation, selected_before);
            assert_eq!(view.script_rows, rows_before);
        });
    }

    #[gpui::test]
    fn the_schema_pane_renders_without_panicking_with_the_database_row_present(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = session_with_databases(&["alpha", "beta"], Some("alpha"), cx);
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(view.active_pane, SidebarPane::Schema);
        });
        sidebar.update(vcx, |view, cx| {
            assert!(
                db_row::render_db_row(view, cx).is_some(),
                "the schema pane with more than one database must show the database row"
            );
        });
    }

    #[gpui::test]
    fn the_scripts_pane_renders_without_panicking_and_hides_the_schema_tree_and_database_row(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = session_with_databases(&["alpha", "beta"], Some("alpha"), cx);
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));

        sidebar.update(vcx, |view, cx| view.switch_pane(SidebarPane::Scripts, cx));
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(view.active_pane, SidebarPane::Scripts);
        });
        sidebar.update(vcx, |view, cx| {
            assert!(
                db_row::render_db_row(view, cx).is_none(),
                "the database row never renders while the scripts pane is active"
            );
        });
    }

    /// A temp directory this test owns exclusively, removed on drop.
    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "zsql-sidebar-test-{label}-{}-{n}",
                std::process::id()
            ));
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Forces the scripts pane's non-empty render path: a named session
    /// script row (selected, since it ends as the active tab) and a library
    /// row already open as a tab (so it renders the open-tab accent dot).
    /// The stacked-section smoke tests above only ever reach the empty-state
    /// branch, so neither `render_script_row` nor its open-dot element is
    /// otherwise exercised by a render pass.
    #[gpui::test]
    fn the_scripts_pane_renders_a_named_session_row_and_an_open_library_row(
        cx: &mut gpui::TestAppContext,
    ) {
        let library_dir = TempDir::new("scripts-pane-rows");
        crate::session_store::LibraryDir::at(&library_dir.0)
            .save(
                &crate::session_store::LibraryName::new("revenue-report").unwrap(),
                "select 1;",
            )
            .expect("seeding the library file must succeed");
        // The scripts pane's session rows come from a disk scan, never from
        // open tabs alone, so this named script needs a real sibling file
        // on disk, not just an in-memory tab title.
        let session_dir = TempDir::new("scripts-pane-rows-session");
        std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
        std::fs::write(
            session_dir.0.join("scripts").join("top-customers.sql"),
            "select * from customers;",
        )
        .expect("must write the session script");

        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);

        let session_tab_id = tabs.update(cx, |tabs, cx| {
            tabs.set_library_dir(Some(library_dir.0.clone()));
            let id = tabs.new_script_tab(cx);
            tabs.apply_renamed_title(id, "top-customers.sql".to_owned(), cx);
            id
        });
        tabs.update(cx, |tabs, cx| {
            tabs.open_or_focus_library("revenue-report", cx);
        });
        // Reactivate the session script so its row renders the selected
        // treatment alongside the library row's open-tab dot.
        tabs.update(cx, |tabs, cx| tabs.set_active(session_tab_id, cx));

        let tabs_for_view = tabs.clone();
        let (sidebar, vcx) = cx.add_window_view(|_window, cx| {
            SidebarView::new(
                session_for_view,
                tabs_for_view,
                Some(library_dir.0.clone()),
                cx,
            )
        });
        sidebar.update(vcx, |view, cx| {
            view.set_session_dir(Some(session_dir.0.clone()), cx);
            view.switch_pane(SidebarPane::Scripts, cx);
        });
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(
                view.script_rows.len(),
                2,
                "expected exactly one session row and one library row"
            );
            let session_row = view
                .script_rows
                .iter()
                .find(|row| row.kind == super::model::ScriptRowKind::Session)
                .expect("a named session script must produce a session row");
            assert!(
                session_row.selected,
                "the reactivated session tab's row must render the selected treatment"
            );
            let library_row = view
                .script_rows
                .iter()
                .find(|row| row.kind == super::model::ScriptRowKind::Library)
                .expect("the seeded library file must produce a library row");
            assert!(
                super::model::library_row_is_open(library_row),
                "a library file open as a tab must render the open-tab accent dot"
            );
        });
    }

    /// A script row's relative-time label must recompute from the file's
    /// current on-disk modified time each time the periodic refresh loop
    /// (see `SidebarView::spawn_scripts_refresh_loop`) runs, not stay frozen
    /// at whatever it read the first time the pane synced -- otherwise a
    /// label like "2m" would read "2m" forever, even overnight.
    #[gpui::test]
    fn a_script_rows_relative_time_recomputes_on_the_periodic_refresh_not_only_at_sync_time(
        cx: &mut gpui::TestAppContext,
    ) {
        let session_dir = TempDir::new("scripts-pane-relative-time-refresh");
        std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
        let script_path = session_dir.0.join("scripts").join("top-customers.sql");
        std::fs::write(&script_path, "select * from customers;")
            .expect("must write the session script");

        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);

        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));
        let refresh_interval = Duration::from_millis(20);
        sidebar.update(vcx, |view, cx| {
            view.set_scripts_refresh_interval(refresh_interval, cx);
            view.set_session_dir(Some(session_dir.0.clone()), cx);
            view.switch_pane(SidebarPane::Scripts, cx);
        });
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(
                view.script_rows[0].relative_time, "now",
                "a just-written file's row must read as recently modified"
            );
        });

        // Back-date the file on disk without going through this view at
        // all -- standing in for real time passing between the initial sync
        // and the periodic refresh's next scan.
        let three_days_ago = std::time::SystemTime::now() - Duration::from_hours(3 * 24);
        std::fs::File::open(&script_path)
            .expect("script file must still exist")
            .set_modified(three_days_ago)
            .expect("must back-date the script file's modified time");

        vcx.executor().advance_clock(refresh_interval * 2);
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(
                view.script_rows[0].relative_time, "3d",
                "the periodic refresh must re-read the file's modified time from disk, \
                 not keep showing the label computed at the original sync"
            );
        });
    }

    /// A synchronous resync (a script saved, renamed, closed, or the
    /// connection switching -- anything that fires the tabs observer) that
    /// lands while the periodic background refresh's scan is still in
    /// flight must win: the background scan's own result, captured under
    /// the generation the resync just superseded, must be dropped rather
    /// than clobbering the newer synchronous rows once it completes.
    #[gpui::test]
    fn a_background_refresh_result_is_dropped_if_a_sync_interleaves_before_it_completes(
        cx: &mut gpui::TestAppContext,
    ) {
        let session_dir = TempDir::new("scripts-pane-refresh-race");
        std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
        std::fs::write(
            session_dir.0.join("scripts").join("top-customers.sql"),
            "select 1;",
        )
        .expect("must write the session script");

        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);

        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));
        sidebar.update(vcx, |view, cx| {
            view.set_session_dir(Some(session_dir.0.clone()), cx);
        });
        vcx.run_until_parked();

        let (captured_generation, expected_rows) = sidebar.update(vcx, |view, cx| {
            // Standing in for the periodic loop's own capture-then-dispatch
            // step (`SidebarView::spawn_scripts_refresh_loop`): note the
            // generation as it stands right before a background scan would
            // have been dispatched.
            let generation = view.script_rows_generation;

            // The interleaving synchronous resync: something else (a save,
            // a tab close, a connection switch) triggers a resync while
            // that scan is still in flight, bumping the generation and
            // rebuilding rows fresh from disk.
            std::fs::write(session_dir.0.join("second.sql"), "select 2;")
                .expect("must write a second session script");
            view.resync_scripts(cx);

            (generation, view.script_rows.clone())
        });

        // The stale scan's result finally lands, carrying whatever it read
        // before the resync -- an empty scan stands in for a moment before
        // "second.sql" existed at all.
        sidebar.update(vcx, |view, cx| {
            view.apply_background_script_rows((Vec::new(), Vec::new()), captured_generation, cx);
        });

        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(
                view.script_rows, expected_rows,
                "a background scan captured before an interleaving sync must never overwrite \
                 that sync's newer rows once it completes"
            );
        });
    }

    /// The footer must render even when the scripts pane shows the
    /// empty-connection-group state, since it starts a flow unrelated to
    /// whether any scripts exist yet.
    #[gpui::test]
    fn the_scripts_pane_footer_is_present_in_the_empty_state(cx: &mut gpui::TestAppContext) {
        let (sidebar, vcx) = build(cx, SchemaState::Ready(sample_schema_tree()));
        sidebar.update(vcx, |view, cx| view.switch_pane(SidebarPane::Scripts, cx));
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert!(
                super::model::scripts_pane_shows_empty_state(&view.script_rows),
                "this test's fixture must reach the empty-scripts state"
            );
        });
        assert!(
            vcx.debug_bounds("sidebar-scripts-open-external").is_some(),
            "the open-external-file footer must render in the empty-scripts state"
        );
    }

    /// The footer must render alongside a populated script list too.
    #[gpui::test]
    fn the_scripts_pane_footer_is_present_with_populated_scripts(cx: &mut gpui::TestAppContext) {
        let session_dir = TempDir::new("scripts-pane-footer-populated");
        std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
        std::fs::write(
            session_dir.0.join("scripts").join("top-customers.sql"),
            "select * from customers;",
        )
        .expect("must write the session script");

        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));
        sidebar.update(vcx, |view, cx| {
            view.set_session_dir(Some(session_dir.0.clone()), cx);
            view.switch_pane(SidebarPane::Scripts, cx);
        });
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert!(
                !super::model::scripts_pane_shows_empty_state(&view.script_rows),
                "this test's fixture must reach the populated-scripts state"
            );
        });
        assert!(
            vcx.debug_bounds("sidebar-scripts-open-external").is_some(),
            "the open-external-file footer must render alongside a populated script list"
        );
    }

    /// The footer is a structural sibling of the scrollable rows, not one of
    /// them, so scrolling an overflowing list must never move, hide, or
    /// clip it.
    #[gpui::test]
    fn the_scripts_pane_footer_stays_put_while_an_overflowing_list_scrolls(
        cx: &mut gpui::TestAppContext,
    ) {
        let session_dir = TempDir::new("scripts-pane-footer-overflow");
        std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
        for i in 0..80 {
            std::fs::write(
                session_dir
                    .0
                    .join("scripts")
                    .join(format!("script-{i:03}.sql")),
                format!("select {i};"),
            )
            .expect("must write a session script");
        }

        let session =
            cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(sample_schema_tree())));
        let session_for_view = session.clone();
        let tabs = build_tabs(session.clone(), cx);
        let (sidebar, vcx) =
            cx.add_window_view(|_window, cx| SidebarView::new(session_for_view, tabs, None, cx));
        sidebar.update(vcx, |view, cx| {
            view.set_session_dir(Some(session_dir.0.clone()), cx);
            view.switch_pane(SidebarPane::Scripts, cx);
        });
        vcx.run_until_parked();

        let scroll = sidebar.read_with(vcx, |view, _app| view.scripts_scroll.clone());
        let thumb_selector = vertical_thumb_debug_selector(&scroll);
        let thumb_bounds_before = vcx.debug_bounds(thumb_selector).expect(
            "this test's fixture must overflow the pane's viewport enough to show a \
             scrollbar thumb, or the scroll below would prove nothing",
        );
        let footer_bounds_before = vcx
            .debug_bounds("sidebar-scripts-open-external")
            .expect("the footer must be painted before scrolling");

        sidebar.update(vcx, |view, cx| {
            view.scripts_scroll_handle
                .set_offset(gpui::point(gpui::px(0.0), gpui::px(-3000.0)));
            cx.notify();
        });
        vcx.run_until_parked();

        let thumb_bounds_after = vcx
            .debug_bounds(thumb_selector)
            .expect("the scrollbar thumb must still be painted after scrolling");
        assert_ne!(
            thumb_bounds_before, thumb_bounds_after,
            "the scroll offset change must actually reach the paint pipeline, or the \
             footer assertion below would prove nothing"
        );

        let footer_bounds_after = vcx
            .debug_bounds("sidebar-scripts-open-external")
            .expect("the footer must still be painted after scrolling");
        assert_eq!(
            footer_bounds_before, footer_bounds_after,
            "the footer must stay a structural sibling of the scrolled rows, unmoved by \
             scrolling to any position or list length"
        );
    }

    /// Clicking the footer must raise the exact event Ctrl+Shift+O raises on
    /// the same tabs model, not a parallel path into the file dialog.
    #[gpui::test]
    fn clicking_the_footer_emits_the_same_browse_files_event_as_ctrl_shift_o(
        cx: &mut gpui::TestAppContext,
    ) {
        let (sidebar, vcx) = build(cx, SchemaState::Ready(sample_schema_tree()));
        let tabs = sidebar.read_with(vcx, |view, _app| view.tabs.clone());

        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let events_for_sub = events.clone();
        vcx.update(|_window, cx| {
            cx.subscribe(&tabs, move |_tabs, evt: &OpenRequested, _cx| {
                events_for_sub.borrow_mut().push(*evt);
            })
            .detach();
        });

        sidebar.update(vcx, |view, cx| view.switch_pane(SidebarPane::Scripts, cx));
        vcx.run_until_parked();

        let bounds = vcx
            .debug_bounds("sidebar-scripts-open-external")
            .expect("the footer must be painted before it can be clicked");
        vcx.simulate_click(bounds.center(), gpui::Modifiers::default());
        vcx.run_until_parked();

        assert_eq!(
            *events.borrow(),
            vec![OpenRequested::BrowseFiles],
            "clicking the footer must emit exactly the BrowseFiles request"
        );
    }

    // -- quick find -------------------------------------------------

    /// Like [`build`], but registers the sidebar's own key bindings first,
    /// so `vcx.simulate_keystrokes` actually dispatches through the real
    /// keymap instead of only through direct method calls.
    fn build_with_find_init(
        cx: &mut gpui::TestAppContext,
        schema: SchemaState,
    ) -> (gpui::Entity<SidebarView>, &mut gpui::VisualTestContext) {
        cx.update(|cx| {
            super::init(cx);
            zsql_ui::text_field::init(cx);
        });
        build(cx, schema)
    }

    #[gpui::test]
    fn secondary_f_opens_the_find_row_with_its_input_focused_when_the_sidebar_has_focus(
        cx: &mut gpui::TestAppContext,
    ) {
        let (sidebar, vcx) = build_with_find_init(cx, SchemaState::Ready(sample_schema_tree()));
        let focus_handle = sidebar.read_with(vcx, |view, _app| view.focus_handle.clone());
        vcx.update(|window, _cx| window.focus(&focus_handle));
        vcx.run_until_parked();

        vcx.simulate_keystrokes("secondary-f");
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert!(view.find.is_some(), "Ctrl+F must open the find row");
        });
        let input_focus = sidebar
            .read_with(vcx, find::input_focus_handle_for_test)
            .expect("the find row must be open");
        vcx.update(|window, _cx| {
            assert!(
                input_focus.is_focused(window),
                "opening the find row must move window focus into its query input"
            );
        });
    }

    #[gpui::test]
    fn typing_a_query_captures_and_auto_expands_a_previously_collapsed_matching_schema(
        cx: &mut gpui::TestAppContext,
    ) {
        let (sidebar, vcx) = build_with_find_init(cx, SchemaState::Ready(sample_schema_tree()));
        sidebar.update(vcx, |view, cx| view.toggle_schema("zsql", "public", cx));
        sidebar.read_with(vcx, |view, _app| {
            assert!(
                view.collapsed_schemas
                    .contains(&("zsql".to_owned(), "public".to_owned())),
                "the schema must start collapsed for this test to prove anything"
            );
        });

        sidebar.update_in(vcx, find::open);
        vcx.run_until_parked();
        vcx.simulate_keystrokes("o r d e r s");
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert!(
                view.pre_filter_collapse.is_some(),
                "the pre-filter collapse state must be captured on the first keystroke"
            );
            assert!(
                !view
                    .collapsed_schemas
                    .contains(&("zsql".to_owned(), "public".to_owned())),
                "a schema holding a match must auto-expand while filtering"
            );
        });
    }

    #[gpui::test]
    fn esc_closes_the_row_and_restores_the_exact_pre_filter_collapse_state(
        cx: &mut gpui::TestAppContext,
    ) {
        let (sidebar, vcx) = build_with_find_init(cx, SchemaState::Ready(sample_schema_tree()));
        sidebar.update(vcx, |view, cx| view.toggle_schema("zsql", "public", cx));

        sidebar.update_in(vcx, find::open);
        vcx.run_until_parked();
        vcx.simulate_keystrokes("o r d e r s");
        vcx.run_until_parked();

        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert!(view.find.is_none(), "Esc must close the find row");
            assert!(
                view.pre_filter_collapse.is_none(),
                "the snapshot must be consumed once restored"
            );
            assert!(
                view.collapsed_schemas
                    .contains(&("zsql".to_owned(), "public".to_owned())),
                "the user's own collapse choice from before filtering must be restored exactly"
            );
        });
    }

    #[gpui::test]
    fn emptying_the_query_restores_collapse_state_without_closing_the_row(
        cx: &mut gpui::TestAppContext,
    ) {
        let (sidebar, vcx) = build_with_find_init(cx, SchemaState::Ready(sample_schema_tree()));
        sidebar.update(vcx, |view, cx| view.toggle_schema("zsql", "public", cx));

        sidebar.update_in(vcx, find::open);
        vcx.run_until_parked();
        vcx.simulate_keystrokes("o r d e r s");
        vcx.run_until_parked();

        for _ in 0.."orders".len() {
            vcx.simulate_keystrokes("backspace");
        }
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert!(
                view.find.is_some(),
                "emptying the query must not close the row, only Esc does"
            );
            assert!(
                view.pre_filter_collapse.is_none(),
                "the snapshot must be consumed once the query empties back out"
            );
            assert!(
                view.collapsed_schemas
                    .contains(&("zsql".to_owned(), "public".to_owned())),
                "the pre-filter collapse state must be restored once the query empties"
            );
        });
    }

    #[gpui::test]
    fn enter_opens_the_first_visible_match(cx: &mut gpui::TestAppContext) {
        let (sidebar, vcx) = build_with_find_init(cx, SchemaState::Ready(sample_schema_tree()));
        let tabs = sidebar.read_with(vcx, |view, _app| view.tabs.clone());

        sidebar.update_in(vcx, find::open);
        vcx.run_until_parked();
        vcx.simulate_keystrokes("o r d e r s");
        vcx.run_until_parked();

        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(
                view.selected_relation,
                Some(("public".to_owned(), "orders".to_owned())),
                "Enter must open the first visible match, the same as clicking it"
            );
        });
        tabs.read_with(vcx, |tabs, _app| {
            assert_eq!(
                tabs.tabs().len(),
                1,
                "Enter must open exactly one generated tab"
            );
        });
    }

    #[gpui::test]
    fn enter_with_zero_visible_matches_is_a_no_op(cx: &mut gpui::TestAppContext) {
        let (sidebar, vcx) = build_with_find_init(cx, SchemaState::Ready(sample_schema_tree()));

        sidebar.update_in(vcx, find::open);
        vcx.run_until_parked();
        vcx.simulate_keystrokes("z z z z z");
        vcx.run_until_parked();

        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(view.selected_relation, None);
        });
    }

    #[gpui::test]
    fn filtered_relation_rows_remain_interactive_for_preview_and_context_menu(
        cx: &mut gpui::TestAppContext,
    ) {
        let (sidebar, vcx) = build_with_find_init(cx, SchemaState::Ready(sample_schema_tree()));

        sidebar.update_in(vcx, find::open);
        vcx.run_until_parked();
        vcx.simulate_keystrokes("o r d e r s");
        vcx.run_until_parked();

        sidebar.update_in(vcx, |view, window, cx| {
            view.preview("public", "orders", window, cx);
        });
        sidebar.read_with(vcx, |view, _app| {
            assert_eq!(
                view.selected_relation,
                Some(("public".to_owned(), "orders".to_owned())),
                "a filtered relation row's click handler must still preview normally"
            );
        });

        sidebar.update(vcx, |view, cx| {
            context_menu::open(
                view,
                "public".to_owned(),
                "orders".to_owned(),
                RelationKind::Table,
                0,
                gpui::point(gpui::px(10.0), gpui::px(20.0)),
                cx,
            );
        });
        sidebar.read_with(vcx, |view, _app| {
            assert!(
                view.context_menu.is_some(),
                "a filtered relation row's context menu must still open normally"
            );
        });
    }

    #[gpui::test]
    fn a_query_matching_nothing_renders_the_empty_state_without_panicking(
        cx: &mut gpui::TestAppContext,
    ) {
        let (sidebar, vcx) = build_with_find_init(cx, SchemaState::Ready(sample_schema_tree()));

        sidebar.update_in(vcx, find::open);
        vcx.run_until_parked();
        vcx.simulate_keystrokes("z z z z z");
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, cx| {
            let tree = match view.session.read(cx).schema() {
                SchemaState::Ready(tree) => tree.clone(),
                other => panic!("expected a Ready schema, got {other:?}"),
            };
            assert!(
                filter::flatten_schema_tree_filtered(&tree, "zzzzz").is_empty(),
                "this test's fixture must reach the zero-match empty state"
            );
        });
        assert!(
            vcx.debug_bounds("sidebar-filter-empty").is_some(),
            "a zero-match query must render the empty-state element, not a blank pane"
        );
    }

    #[gpui::test]
    fn the_scripts_pane_is_filtered_by_the_same_find_row(cx: &mut gpui::TestAppContext) {
        let session_dir = TempDir::new("sidebar-find-scripts-pane");
        std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
        std::fs::write(
            session_dir.0.join("scripts").join("top-customers.sql"),
            "select 1;",
        )
        .expect("must write a session script");
        std::fs::write(
            session_dir.0.join("scripts").join("cohort-debug.sql"),
            "select 2;",
        )
        .expect("must write a session script");

        let (sidebar, vcx) = build_with_find_init(cx, SchemaState::Ready(sample_schema_tree()));
        sidebar.update(vcx, |view, cx| {
            view.set_session_dir(Some(session_dir.0.clone()), cx);
            view.switch_pane(SidebarPane::Scripts, cx);
        });
        vcx.run_until_parked();

        sidebar.update_in(vcx, find::open);
        vcx.run_until_parked();
        vcx.simulate_keystrokes("t o p");
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, _app| {
            let matches = filter::filter_script_rows(&view.script_rows, "top");
            assert_eq!(matches.len(), 1);
            assert_eq!(
                view.script_rows[matches[0].index].label,
                "top-customers.sql"
            );
        });
    }

    #[gpui::test]
    fn enter_opens_the_first_visible_match_in_the_scripts_pane(cx: &mut gpui::TestAppContext) {
        let session_dir = TempDir::new("sidebar-find-scripts-enter");
        std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
        std::fs::write(
            session_dir.0.join("scripts").join("top-customers.sql"),
            "select 1;",
        )
        .expect("must write a session script");
        std::fs::write(
            session_dir.0.join("scripts").join("cohort-debug.sql"),
            "select 2;",
        )
        .expect("must write a session script");

        let (sidebar, vcx) = build_with_find_init(cx, SchemaState::Ready(sample_schema_tree()));
        let tabs = sidebar.read_with(vcx, |view, _app| view.tabs.clone());
        // `TabModel`'s own session directory (distinct from the sidebar's,
        // which only drives the scripts pane's disk scan) is what
        // `open_or_focus_session_script` reads the file from.
        tabs.update(vcx, |tabs, _cx| {
            tabs.set_session_dir(Some(session_dir.0.clone()));
        });
        sidebar.update(vcx, |view, cx| {
            view.set_session_dir(Some(session_dir.0.clone()), cx);
            view.switch_pane(SidebarPane::Scripts, cx);
        });
        vcx.run_until_parked();

        sidebar.update_in(vcx, find::open);
        vcx.run_until_parked();
        vcx.simulate_keystrokes("t o p");
        vcx.run_until_parked();

        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();

        tabs.read_with(vcx, |tabs, _app| {
            assert_eq!(
                tabs.active_tab().map(crate::ui::tabs::Tab::title),
                Some("top-customers.sql"),
                "Enter must activate the first visible matching script"
            );
        });
    }

    /// Switching panes with a filter active must reapply the same query to
    /// the newly active pane's own rows, rather than clearing the filter or
    /// leaving it stuck showing the old pane's stale matches.
    #[gpui::test]
    fn switching_panes_while_filtered_reapplies_the_same_query_to_the_new_panes_rows(
        cx: &mut gpui::TestAppContext,
    ) {
        let session_dir = TempDir::new("sidebar-find-pane-switch");
        std::fs::create_dir_all(session_dir.0.join("scripts")).expect("must create scripts dir");
        std::fs::write(
            session_dir.0.join("scripts").join("orders-report.sql"),
            "select 1;",
        )
        .expect("must write a session script");

        let (sidebar, vcx) = build_with_find_init(cx, SchemaState::Ready(sample_schema_tree()));
        sidebar.update(vcx, |view, cx| {
            view.set_session_dir(Some(session_dir.0.clone()), cx);
        });
        vcx.run_until_parked();

        sidebar.update_in(vcx, find::open);
        vcx.run_until_parked();
        vcx.simulate_keystrokes("o r d e r s");
        vcx.run_until_parked();

        sidebar.update(vcx, |view, cx| view.switch_pane(SidebarPane::Scripts, cx));
        vcx.run_until_parked();

        sidebar.read_with(vcx, |view, cx| {
            assert!(
                view.find.is_some(),
                "switching panes must not close an active filter"
            );
            let query = find::current_query(view, cx);
            assert_eq!(query, "orders");
            let matches = filter::filter_script_rows(&view.script_rows, &query);
            assert_eq!(
                matches.len(),
                1,
                "the same query must re-filter the new pane's own rows"
            );
            assert_eq!(
                view.script_rows[matches[0].index].label,
                "orders-report.sql"
            );
        });
    }
}
