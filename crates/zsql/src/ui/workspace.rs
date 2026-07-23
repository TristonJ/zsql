//! The root workspace view

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, Bounds, ClickEvent, Context, CursorStyle, Entity, FocusHandle, Focusable, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, Task, Window, canvas, div,
    prelude::*, px, rgb, rgba,
};
use zsql_ui::icon::{IconName, icon};
use zsql_ui::theme::ActiveTheme;

use super::connections::{ActiveConnection, ConnectionManagerView};
use super::footer::ConnectionFooterView;
use super::results::ResultsView;
use super::sidebar::SidebarView;
use super::tabs::{Tab, TabId, TabKind, TabModel};
use super::theme;
use crate::config::{LayoutConfig, ValuePanelConfig};
use crate::connections::ConnectionStore;
use crate::session::Session;
use crate::tab_session::{self, ConnectionKey, TabSessionSnapshot};

/// Which pane boundary a divider drag is currently resizing, and the pane
/// size/pointer position it started from. Tracking the drag's origin (not
/// just the last event) keeps each mouse-move computing the new size fresh
/// off the same starting point, so the size never drifts from accumulated
/// rounding across many small deltas.
enum DividerDrag {
    /// Dragging the divider between the sidebar and the editor/results
    /// column.
    Sidebar {
        origin_x: Pixels,
        start_width: Pixels,
    },
    /// Dragging the divider between the editor pane and the results grid.
    EditorResults {
        origin_y: Pixels,
        start_height: Pixels,
    },
}

pub struct WorkspaceView {
    connections: Entity<ConnectionManagerView>,
    footer: Entity<ConnectionFooterView>,
    sidebar: Entity<SidebarView>,
    tabs: Entity<TabModel>,
    results: Entity<ResultsView>,
    layout: LayoutConfig,
    sidebar_width: Pixels,
    editor_height: Pixels,
    drag: Option<DividerDrag>,
    /// Height of the editor/results column, refreshed on every layout pass
    /// by the measuring canvas rendered as that column's first child. Read
    /// (not written) while handling a divider drag, since the drag needs to
    /// know how much vertical space the editor and results panes have to
    /// share.
    column_height: Rc<Cell<Pixels>>,
    /// Where tab-session snapshots are persisted
    /// (`Config::tab_sessions_path`), if a config directory could be
    /// resolved at all. `None` disables tab-session persistence entirely
    /// rather than failing to start.
    tab_sessions_path: Option<PathBuf>,
    /// The tab-session key `tabs` currently holds state for, so a save
    /// triggered by a tab change knows which key to write under, and a
    /// connection switch knows which key's tabs it is replacing. `None`
    /// while no connection has ever been tracked as active.
    active_tab_session_key: Option<ConnectionKey>,
    /// The active connection last seen by [`Self::handle_active_connection_changed`],
    /// so that handler can tell an actual switch apart from an unrelated
    /// change on `connections` (e.g. the modal opening).
    last_active_connection: Option<ActiveConnection>,
    /// Set just before [`Self::handle_active_connection_changed`] reloads
    /// `tabs` from a snapshot, and consumed by the very next `tabs`
    /// notification (that reload's own `cx.notify()`) instead of triggering
    /// another save under the key that was just loaded. Without this, that
    /// redundant save would race the outgoing connection's own save (both
    /// dispatched onto the background executor around the same moment) for
    /// no benefit -- the freshly loaded tabs already match what is (or is
    /// not) on disk for that key.
    suppress_next_tab_save: bool,
    /// Every key's most recently dispatched-for-save snapshot, updated
    /// synchronously in [`Self::dispatch_tab_session_save`] before that
    /// save's write is handed to the background executor. Consulted first in
    /// [`Self::handle_active_connection_changed`] so switching back to a key
    /// whose write is still in flight sees this session's own latest tabs
    /// rather than racing the background write for whatever is currently on
    /// disk.
    session_cache: HashMap<ConnectionKey, TabSessionSnapshot>,
}

impl WorkspaceView {
    /// Build a workspace over `session`, with pane sizes seeded from
    /// `layout` (also sizing the results grid's value panel dock),
    /// `value_panel` configuring that panel's parse thresholds and hex-dump
    /// layout, `connection_store` backing the connection-manager modal,
    /// `probe_timeout` (typically [`crate::config::Config::liveness`]'s
    /// `probe_timeout()`) bounding the connection-manager form's Test
    /// button, and `tab_sessions_path` (typically
    /// [`crate::config::Config::tab_sessions_path`]) as where per-connection
    /// tab sessions are read from and saved to.
    #[must_use]
    pub fn new(
        session: Entity<Session>,
        layout: LayoutConfig,
        value_panel: ValuePanelConfig,
        connection_store: ConnectionStore,
        probe_timeout: Duration,
        tab_sessions_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let results = cx.new(|cx| ResultsView::new(session.clone(), "", cx));
        results.update(cx, |results, cx| {
            results.configure_value_panel(cx, &layout, value_panel);
        });
        let tabs = cx.new(|cx| TabModel::new(session.clone(), results.clone(), cx));
        // Every workspace opens with one empty script tab so the editor
        // pane is never blank; `TabModel::new` itself stays tab-less so its
        // own constructor never has to call back into an entity that has
        // not finished being built yet. A connection tracked as active by
        // the time this workspace is built (never true today, since
        // `connection_store` carries no notion of "last connected") would
        // otherwise leave this default tab in place until the next switch.
        tabs.update(cx, |tabs, cx| {
            tabs.new_script_tab(cx);
        });
        let sidebar = cx.new(|cx| SidebarView::new(session.clone(), tabs.clone(), cx));
        let connections = cx.new(|cx| {
            ConnectionManagerView::new(session.clone(), connection_store, probe_timeout, cx)
        });
        let footer = cx.new(|cx| ConnectionFooterView::new(session, connections.clone(), cx));
        let sidebar_width = layout.sidebar_default_width;
        let editor_height = layout.editor_default_height;

        // Opening/closing the modal (or switching its list/add-form panel)
        // lives entirely inside `connections`' own state; this workspace
        // must still re-render to mount or unmount that entity as the modal
        // overlay child below. A change to which connection is tracked as
        // active additionally swaps the tab session (see
        // `Self::handle_active_connection_changed`).
        cx.observe(&connections, |this, connections, cx| {
            let new_active = connections.read(cx).active().cloned();
            if new_active != this.last_active_connection {
                this.handle_active_connection_changed(cx);
            }
            cx.notify();
        })
        .detach();
        // Opening, reusing, converting, closing, or switching a tab lives
        // entirely inside `tabs`' own state; this workspace must still
        // re-render the tab bar and the active tab's body whenever any of
        // that changes, and persist the active connection's tab session so
        // the change survives a reconnect or restart.
        cx.observe(&tabs, |this, _tabs, cx| {
            if std::mem::take(&mut this.suppress_next_tab_save) {
                cx.notify();
                return;
            }
            this.save_active_tab_session(cx);
            cx.notify();
        })
        .detach();

        Self {
            connections,
            footer,
            sidebar,
            tabs,
            results,
            layout,
            sidebar_width,
            editor_height,
            drag: None,
            column_height: Rc::new(Cell::new(Pixels::ZERO)),
            tab_sessions_path,
            active_tab_session_key: None,
            last_active_connection: None,
            suppress_next_tab_save: false,
            session_cache: HashMap::new(),
        }
    }

    /// The active tab's editor focus handle, so the app can focus it on
    /// startup or after a tab switch. `None` when every tab has been closed,
    /// or when the active tab is a read-only Schema tab, whose editor is
    /// never rendered and so must not receive keyboard focus.
    #[must_use]
    pub fn editor_focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        self.tabs
            .read(cx)
            .active_tab()
            .filter(|tab| !tab.is_schema())
            .map(|tab| tab.editor().focus_handle(cx))
    }

    /// Move keyboard focus onto the active tab's editor, e.g. right after
    /// switching, opening, or closing a tab -- without this, typing and
    /// `RunQuery` (cmd/ctrl-enter) would keep targeting whatever was
    /// focused before, not the tab the user just switched to.
    fn focus_active_editor(&self, window: &mut Window, cx: &App) {
        if let Some(handle) = self.editor_focus_handle(cx) {
            window.focus(&handle);
        }
    }

    /// Set the tracked active connection
    #[cfg(test)]
    pub fn set_active_connection(&mut self, active: ActiveConnection, cx: &mut Context<Self>) {
        self.connections
            .update(cx, |view, cx| view.set_active(Some(active), cx));
    }

    /// React to `connections`' tracked active connection having changed
    /// (including to or from `None`, e.g. a disconnect or a deleted active
    /// row): persist the outgoing connection's tabs under its own key, then
    /// load the newly active connection's snapshot (or the default single
    /// script tab if it has none) into `tabs`, fully replacing whatever was
    /// open rather than merging with it.
    fn handle_active_connection_changed(&mut self, cx: &mut Context<Self>) {
        self.save_active_tab_session(cx);

        let (new_key, new_active) = {
            let connections = self.connections.read(cx);
            (
                connections.active_tab_session_key(),
                connections.active().cloned(),
            )
        };
        self.active_tab_session_key.clone_from(&new_key);
        self.last_active_connection = new_active;

        // A key this session has already dispatched a save for always wins
        // over disk: `dispatch_tab_session_save` records it here
        // synchronously, before handing the actual write to the background
        // executor, so this in-memory copy can never be older than whatever
        // is (or, for a write still in flight, will shortly be) on disk for
        // that key. Consulting disk instead in that case could observe the
        // pre-write file if the background write has not run yet -- e.g. a
        // quick switch back to a connection right after switching away from
        // it -- and wrongly show its default tabs.
        let snapshot = match &new_key {
            Some(key) if self.session_cache.contains_key(key) => {
                self.session_cache.get(key).cloned()
            }
            // Read synchronously rather than via a background executor: this
            // runs before the tab strip for the newly active connection is
            // ever shown, and the store is a small, local JSON file, so the
            // blocking read finishes long before it would be worth the
            // complexity of loading in the background and re-applying the
            // result once it completed a frame or more later (which would
            // show the wrong tabs briefly).
            Some(key) => match &self.tab_sessions_path {
                Some(path) => match tab_session::load_snapshot(path, key) {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            "failed to load tab session; falling back to the default tab"
                        );
                        None
                    }
                },
                None => None,
            },
            None => None,
        };

        self.suppress_next_tab_save = true;
        self.tabs.update(cx, |tabs, cx| {
            tabs.load_for_connection(snapshot.as_ref(), cx);
        });
    }

    /// Dispatch (but do not await) a background save of the active
    /// connection's current tab session, if one is tracked and a
    /// tab-session path could be resolved. A no-op otherwise: there is
    /// nothing meaningful to key the save under.
    fn save_active_tab_session(&mut self, cx: &mut Context<Self>) {
        if let Some(task) = self.dispatch_tab_session_save(cx) {
            task.detach();
        }
    }

    /// Flush the active connection's tab session to disk, returning the
    /// background [`Task`] the caller must await before the app actually
    /// exits. Used from `main.rs`'s `App::on_app_quit` hook so a change made
    /// just before quitting is not lost to a fire-and-forget write racing
    /// process exit.
    pub fn flush_tab_session_on_quit(&mut self, cx: &mut Context<Self>) -> Task<()> {
        self.dispatch_tab_session_save(cx)
            .unwrap_or_else(|| Task::ready(()))
    }

    /// Build the active connection's current snapshot, record it as this
    /// key's latest known state (see `session_cache`), and spawn its write
    /// to disk on a background executor -- never on this (render/update)
    /// thread -- returning the spawned [`Task`] so a caller that must
    /// observe completion (app quit) can await it, while a caller that only
    /// wants to fire-and-forget (a tab change) can detach it.
    fn dispatch_tab_session_save(&mut self, cx: &mut Context<Self>) -> Option<Task<()>> {
        let key = self.active_tab_session_key.clone()?;
        let path = self.tab_sessions_path.clone()?;
        let snapshot = self.tabs.read(cx).snapshot(cx);
        tracing::info!(
            key = ?key,
            tab_count = snapshot.tabs.len(),
            "dispatching tab session save"
        );
        self.session_cache.insert(key.clone(), snapshot.clone());
        Some(cx.background_spawn(async move {
            if let Err(err) = tab_session::save_snapshot(&path, &key, &snapshot) {
                tracing::warn!(error = %err, "failed to save tab session");
            }
        }))
    }

    fn start_sidebar_drag(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.drag = Some(DividerDrag::Sidebar {
            origin_x: event.position.x,
            start_width: self.sidebar_width,
        });
        cx.notify();
    }

    fn start_editor_results_drag(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.drag = Some(DividerDrag::EditorResults {
            origin_y: event.position.y,
            start_height: self.editor_height,
        });
        cx.notify();
    }

    fn end_drag(&mut self, _event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            cx.notify();
        }
    }

    fn drag_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match self.drag {
            Some(DividerDrag::Sidebar {
                origin_x,
                start_width,
            }) => {
                let delta = event.position.x - origin_x;
                self.sidebar_width = clamp_sidebar_width(
                    start_width,
                    delta,
                    self.layout.sidebar_min_width,
                    self.layout.sidebar_max_width,
                );
                cx.notify();
            }
            Some(DividerDrag::EditorResults {
                origin_y,
                start_height,
            }) => {
                let delta = event.position.y - origin_y;
                // The tab bar and the workspace header both sit above the
                // editor pane inside the same measured column, so their
                // fixed heights are not themselves resizable and must be
                // carved out of the container height before splitting the
                // rest between the editor and results.
                let available_height = self.column_height.get()
                    - zsql_ui::tabs::TAB_BAR_HEIGHT
                    - theme::WORKSPACE_HEADER_HEIGHT;
                self.editor_height = clamp_editor_height(
                    available_height,
                    start_height,
                    delta,
                    self.layout.editor_min_height,
                    self.layout.results_min_height,
                    self.layout.divider_thickness,
                );
                cx.notify();
            }
            None => {}
        }
    }

    fn activate_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.update(cx, |tabs, cx| tabs.set_active(id, cx));
        self.focus_active_editor(window, cx);
    }

    fn close_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.update(cx, |tabs, cx| tabs.close_tab(id, cx));
        self.focus_active_editor(window, cx);
    }

    fn open_new_script_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.update(cx, |tabs, cx| {
            tabs.new_script_tab(cx);
        });
        self.focus_active_editor(window, cx);
    }

    /// Run the active tab's query through the same seam its `RunQuery`
    /// keybinding uses, regardless of which element currently holds
    /// keyboard focus. The workspace header's Run button's click handler;
    /// a no-op when every tab has been closed.
    fn run_active_tab(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self
            .tabs
            .read(cx)
            .active_tab()
            .map(|tab| tab.editor().clone())
        else {
            return;
        };
        editor.update(cx, zsql_editor::EditorView::run_current_query);
    }

    /// The tab bar: one entry per open tab, in order, plus the trailing "+"
    /// affordance that opens a new script tab.
    fn render_tab_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let active_theme = cx.theme();
        let active_id = self.tabs.read(cx).active_id();
        let mut bar = zsql_ui::tabs::tab_bar_shell(active_theme);
        for tab in self.tabs.read(cx).tabs() {
            let active = active_id == Some(tab.id());
            bar = bar.child(Self::render_tab(tab, active, cx));
        }
        bar.child(
            zsql_ui::tabs::new_tab_glyph(active_theme)
                .id("workspace-new-tab")
                .cursor_pointer()
                .on_click(cx.listener(|view, _event: &ClickEvent, window, cx| {
                    view.open_new_script_tab(window, cx);
                })),
        )
    }

    /// One tab-bar entry, styled per its kind: a `Generated` tab gets the
    /// live dot, table icon, italic name, "auto" pill, and (only while
    /// active) a dashed underline; a `Script` tab gets a plain label with a
    /// trailing `*` while dirty, and (only while active) a solid underline.
    fn render_tab(tab: &Tab, active: bool, cx: &Context<Self>) -> impl IntoElement {
        let active_theme = cx.theme();
        let id = tab.id();
        let mut shell = zsql_ui::tabs::tab_shell(active, active_theme).id(("workspace-tab", id));

        shell = match tab.kind() {
            TabKind::Generated { .. } => {
                shell = shell
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(theme::TAB_ICON_TEXT_SIZE))
                            .text_color(rgb(active_theme.colors.accent))
                            .child("#"),
                    )
                    .child(div().italic().child(tab.title().to_owned()));
                if active {
                    shell = shell.child(zsql_ui::tabs::active_underline_solid(active_theme));
                }
                shell
            }
            TabKind::Script => {
                let mut label = tab.title().to_owned();
                if tab.dirty() {
                    label.push('*');
                }
                shell = shell.child(div().child(label));
                if active {
                    shell = shell.child(zsql_ui::tabs::active_underline_solid(active_theme));
                }
                shell
            }
            TabKind::Schema { .. } => {
                shell = shell
                    .child(icon(
                        IconName::Table,
                        px(theme::TAB_ICON_TEXT_SIZE),
                        active_theme.colors.accent,
                    ))
                    .child(div().child(tab.title().to_owned()));
                if active {
                    shell = shell.child(zsql_ui::tabs::active_underline_solid(active_theme));
                }
                shell
            }
        };

        shell
            .cursor_pointer()
            .on_click(cx.listener(move |view, _event: &ClickEvent, window, cx| {
                view.activate_tab(id, window, cx);
            }))
            .child(
                zsql_ui::tabs::close_glyph(active_theme)
                    .id(("workspace-tab-close", id))
                    .cursor_pointer()
                    .on_click(cx.listener(move |view, _event: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        view.close_tab(id, window, cx);
                    })),
            )
    }

    /// The header above the active tab's content: a pane label on the left
    /// and the Run button, with its keyboard-shortcut hint, on the right.
    /// Rendered once per frame regardless of the active tab's kind, so both
    /// a full `Script` editor and a compact `Generated` strip get a Run
    /// affordance.
    fn render_header(cx: &Context<Self>) -> impl IntoElement {
        let run_shortcut = if cfg!(target_os = "macos") {
            "Cmd+Enter"
        } else {
            "Ctrl+Enter"
        };
        let active_theme = cx.theme();
        let colors = active_theme.colors;

        div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .h(theme::WORKSPACE_HEADER_HEIGHT)
            .px(px(theme::WORKSPACE_HEADER_PADDING_X))
            .bg(rgb(colors.bg_panel))
            .border_b_1()
            .border_color(rgb(colors.border))
            .child(
                div()
                    .text_size(px(theme::WORKSPACE_HEADER_LABEL_TEXT_SIZE))
                    .text_color(rgb(colors.text_secondary))
                    .child("SQL"),
            )
            .child(
                div()
                    .id("workspace-run-query-button")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(theme::RUN_BUTTON_HEIGHT)
                    .px(px(theme::RUN_BUTTON_PADDING_X))
                    .rounded(px(theme::RUN_BUTTON_RADIUS))
                    .bg(rgb(colors.accent))
                    .text_color(rgb(colors.bg_app))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(theme::run_button_hover_bg(active_theme))))
                    .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                        view.run_active_tab(cx);
                    }))
                    .child(icon(
                        IconName::Run,
                        theme::RUN_BUTTON_ICON_SIZE,
                        colors.bg_app,
                    ))
                    .child(
                        div()
                            .text_size(px(theme::RUN_BUTTON_TEXT_SIZE))
                            .child("Run"),
                    )
                    .child(
                        div()
                            .text_size(px(theme::RUN_BUTTON_HINT_TEXT_SIZE))
                            .text_color(rgba(theme::run_button_hint(active_theme)))
                            .child(run_shortcut),
                    ),
            )
    }

    /// The active tab's editor body: a compact, teal-tinted strip for a
    /// live `Generated` tab, or the full-height editor pane for a `Script`
    /// tab (including a converted-from-generated one). Renders nothing when
    /// every tab has been closed. Never called for an active `Schema` tab --
    /// see [`Self::render_main_pane`].
    fn render_active_body(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let active_theme = cx.theme();
        let Some(active) = self.tabs.read(cx).active_tab() else {
            return div().flex_shrink_0().into_any_element();
        };

        if active.is_generated() {
            Self::render_generated_strip(active, active_theme).into_any_element()
        } else {
            div()
                .flex_shrink_0()
                .w_full()
                .h(self.editor_height)
                .child(active.editor().clone())
                .into_any_element()
        }
    }

    /// Everything below the tab bar: for an active `Schema` tab, its
    /// read-only structural view alone, filling all remaining space (no
    /// editor pane, divider, or shared results grid); for every other
    /// active tab (or none), the normal editor body, the resizable
    /// editor/results divider, and the shared results grid.
    fn render_main_pane(&self, cx: &Context<Self>) -> Vec<gpui::AnyElement> {
        if let Some(active) = self.tabs.read(cx).active_tab()
            && let Some(schema_view) = active.schema_view()
        {
            // A schema tab is read-only and carries its own header meta strip,
            // so the shared editor header (with its Run button) is deliberately
            // omitted; the schema view fills the whole pane below the tab bar.
            return vec![
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(schema_view.clone())
                    .into_any_element(),
            ];
        }

        vec![
            Self::render_header(cx).into_any_element(),
            self.render_active_body(cx),
            div()
                .id("editor-results-divider")
                .flex_shrink_0()
                .w_full()
                .h(self.layout.divider_thickness)
                .cursor(CursorStyle::ResizeUpDown)
                .bg(rgb(cx.theme().colors.border))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(Self::start_editor_results_drag),
                )
                .into_any_element(),
            div()
                .flex_1()
                .min_h_0()
                .child(self.results.clone())
                .into_any_element(),
        ]
    }

    /// The strip a live `Generated` tab renders instead of the full editor.
    fn render_generated_strip(tab: &Tab, active_theme: &zsql_ui::theme::Theme) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .w_full()
            .h(theme::GENERATED_STRIP_HEIGHT)
            .bg(gpui::rgba(theme::generated_strip_bg(active_theme)))
            .border_color(rgb(theme::generated_strip_accent(active_theme)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(tab.editor().clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(theme::GENERATED_STRIP_TRAILING_GAP))
                    .px(px(theme::GENERATED_STRIP_TRAILING_PADDING_X))
                    .child(zsql_ui::grid::type_tag("generated", active_theme))
                    .child(
                        div()
                            .text_size(px(theme::GENERATED_HINT_TEXT_SIZE))
                            .text_color(rgb(active_theme.colors.text_tertiary))
                            .child("edit to convert to a script"),
                    ),
            )
    }
}

/// New sidebar width after dragging its divider by `delta` from `current`,
/// clamped to `[min, max]`. Pure and gpui-free so drag math is unit
/// testable without a window.
///
/// `max` is widened to `min` first: `Pixels::clamp` asserts `min <= max`,
/// and a misconfigured `sidebar_max_width < sidebar_min_width` must not
/// crash the app on the first drag.
#[must_use]
fn clamp_sidebar_width(current: Pixels, delta: Pixels, min: Pixels, max: Pixels) -> Pixels {
    let max = max.max(min);
    (current + delta).clamp(min, max)
}

/// New editor-pane height after dragging the editor/results divider by
/// `delta` from `current`, given the column's total available height.
///
/// The editor is never allowed to grow past
/// `container_height - divider_thickness - min_results_height`, so the
/// results pane always keeps at least `min_results_height` regardless of how
/// far the drag requests. If the container itself is too small to fit both
/// panes' minimums, the editor's own minimum wins and the results pane
/// shrinks below its target -- there is no space left to honor both.
#[must_use]
fn clamp_editor_height(
    container_height: Pixels,
    current: Pixels,
    delta: Pixels,
    min_editor_height: Pixels,
    min_results_height: Pixels,
    divider_thickness: Pixels,
) -> Pixels {
    let max_editor_height =
        (container_height - divider_thickness - min_results_height).max(min_editor_height);
    (current + delta).clamp(min_editor_height, max_editor_height)
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let divider_thickness = self.layout.divider_thickness;
        let column_height = self.column_height.clone();
        let modal_open = self.connections.read(cx).is_open();
        let colors = cx.theme().colors;

        div()
            .id("workspace-root")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(colors.bg_app))
            .font_family(&cx.theme().fonts.ui)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .on_mouse_move(cx.listener(Self::drag_move))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::end_drag))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::end_drag))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_shrink_0()
                            .w(self.sidebar_width)
                            .h_full()
                            .child(div().flex_1().min_h_0().child(self.sidebar.clone()))
                            .child(self.footer.clone()),
                    )
                    .child(
                        div()
                            .id("sidebar-divider")
                            .flex_shrink_0()
                            .w(divider_thickness)
                            .h_full()
                            .cursor(CursorStyle::ResizeLeftRight)
                            .bg(rgb(colors.border))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(Self::start_sidebar_drag),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(
                                // Zero-size measuring probe: records this
                                // column's painted height into `column_height`
                                // on every layout pass, so a divider drag knows
                                // how much vertical space the editor and results
                                // panes have to split. Absolutely positioned so
                                // it never participates in the column's own flex
                                // layout.
                                canvas(
                                    move |bounds: Bounds<Pixels>, _window, _cx| {
                                        column_height.set(bounds.size.height);
                                    },
                                    |_bounds, (), _window, _cx| {},
                                )
                                .absolute()
                                .inset_0(),
                            )
                            .child(self.render_tab_bar(cx))
                            .children(self.render_main_pane(cx)),
                    ),
            )
            .when(modal_open, |el| el.child(self.connections.clone()))
    }
}

#[cfg(test)]
mod resize_tests {
    use gpui::px;

    use super::{clamp_editor_height, clamp_sidebar_width};

    #[test]
    fn zero_delta_leaves_sidebar_width_unchanged() {
        let width = clamp_sidebar_width(px(300.0), px(0.0), px(180.0), px(560.0));
        assert_eq!(width, px(300.0));
    }

    #[test]
    fn dragging_sidebar_within_bounds_applies_the_delta_exactly() {
        let width = clamp_sidebar_width(px(300.0), px(50.0), px(180.0), px(560.0));
        assert_eq!(width, px(350.0));
    }

    #[test]
    fn sidebar_max_below_min_widens_to_min_instead_of_panicking() {
        // A misconfigured Config where max < min must not hit the
        // std Ord::clamp assertion that min <= max.
        let width = clamp_sidebar_width(px(300.0), px(1_000.0), px(400.0), px(200.0));
        assert_eq!(width, px(400.0));
    }

    #[test]
    fn dragging_sidebar_below_minimum_clamps_to_minimum() {
        let width = clamp_sidebar_width(px(300.0), px(-1_000.0), px(180.0), px(560.0));
        assert_eq!(width, px(180.0));
    }

    #[test]
    fn dragging_sidebar_above_maximum_clamps_to_maximum() {
        let width = clamp_sidebar_width(px(300.0), px(1_000.0), px(180.0), px(560.0));
        assert_eq!(width, px(560.0));
    }

    #[test]
    fn extreme_negative_sidebar_delta_does_not_panic_or_go_negative() {
        let width = clamp_sidebar_width(px(300.0), gpui::Pixels::MIN, px(180.0), px(560.0));
        assert!(width >= px(180.0));
        assert!(f32::from(width).is_finite());
    }

    #[test]
    fn extreme_positive_sidebar_delta_does_not_panic_or_overflow_past_max() {
        let width = clamp_sidebar_width(px(300.0), gpui::Pixels::MAX, px(180.0), px(560.0));
        assert!(width <= px(560.0));
        assert!(f32::from(width).is_finite());
    }

    #[test]
    fn zero_delta_leaves_editor_height_unchanged() {
        let height =
            clamp_editor_height(px(700.0), px(500.0), px(0.0), px(120.0), px(120.0), px(6.0));
        assert_eq!(height, px(500.0));
    }

    #[test]
    fn dragging_editor_divider_within_bounds_applies_the_delta_exactly() {
        let height = clamp_editor_height(
            px(700.0),
            px(500.0),
            px(50.0),
            px(120.0),
            px(120.0),
            px(6.0),
        );
        assert_eq!(height, px(550.0));
    }

    #[test]
    fn dragging_editor_divider_never_shrinks_results_below_its_minimum() {
        let container = px(700.0);
        let min_editor = px(120.0);
        let min_results = px(150.0);
        let divider = px(6.0);
        let height = clamp_editor_height(
            container,
            px(500.0),
            px(1_000.0),
            min_editor,
            min_results,
            divider,
        );
        let results_height = container - divider - height;
        assert!(results_height >= min_results);
    }

    #[test]
    fn huge_positive_delta_far_larger_than_container_still_respects_results_minimum() {
        let container = px(700.0);
        let min_editor = px(120.0);
        let min_results = px(150.0);
        let divider = px(6.0);
        let height = clamp_editor_height(
            container,
            px(500.0),
            gpui::Pixels::MAX,
            min_editor,
            min_results,
            divider,
        );
        let results_height = container - divider - height;
        assert!(results_height >= min_results);
        assert!(f32::from(height).is_finite());
    }

    #[test]
    fn dragging_editor_divider_upward_past_minimum_clamps_to_editor_minimum() {
        let container = px(700.0);
        let min_editor = px(120.0);
        let min_results = px(150.0);
        let divider = px(6.0);
        let height = clamp_editor_height(
            container,
            px(500.0),
            px(-1_000.0),
            min_editor,
            min_results,
            divider,
        );
        assert_eq!(height, min_editor);
    }

    #[test]
    fn extreme_negative_editor_delta_does_not_panic_or_go_negative() {
        let container = px(700.0);
        let height = clamp_editor_height(
            container,
            px(500.0),
            gpui::Pixels::MIN,
            px(120.0),
            px(150.0),
            px(6.0),
        );
        assert!(height >= px(120.0));
        assert!(f32::from(height).is_finite());
    }

    #[test]
    fn a_container_too_small_for_both_minimums_still_does_not_panic() {
        // The container is smaller than editor_min + divider + results_min:
        // there is no split that satisfies both minimums, so the editor's
        // own minimum wins rather than the clamp asserting min <= max.
        let height =
            clamp_editor_height(px(50.0), px(500.0), px(0.0), px(120.0), px(150.0), px(6.0));
        assert_eq!(height, px(120.0));
    }
}

#[cfg(test)]
mod render_tests {
    use std::time::Duration;

    use gpui::AppContext as _;
    use zsql_core::{Catalog, Relation, RelationKind, SchemaNs, SchemaTree};

    use super::WorkspaceView;
    use crate::config::{LayoutConfig, ValuePanelConfig};
    use crate::connections::{ConnectionArgs, ConnectionStore};
    use crate::session::{SchemaState, Session};
    use crate::tab_session::{self, ConnectionKey};
    use crate::ui::connections::ActiveConnection;

    /// An empty connection store backed by a path this test never writes
    /// to: `WorkspaceView`'s render test only exercises rendering, not
    /// persistence.
    fn empty_store_for_test() -> ConnectionStore {
        let path = std::env::temp_dir().join(format!(
            "zsql-workspace-render-test-{}.toml",
            std::process::id()
        ));
        ConnectionStore::load(&path).expect("loading a nonexistent path must succeed empty")
    }

    /// A connection store's temp file path, plus a tab-session store's own
    /// temp path, both owned exclusively by one test and removed on drop.
    struct PersistenceTestPaths {
        connections: std::path::PathBuf,
        tab_sessions: std::path::PathBuf,
    }

    impl PersistenceTestPaths {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            Self {
                connections: std::env::temp_dir().join(format!(
                    "zsql-workspace-persistence-test-{label}-{pid}-{n}-connections.toml"
                )),
                tab_sessions: std::env::temp_dir().join(format!(
                    "zsql-workspace-persistence-test-{label}-{pid}-{n}-tab-sessions.json"
                )),
            }
        }
    }

    impl Drop for PersistenceTestPaths {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.connections);
            let _ = std::fs::remove_file(&self.tab_sessions);
        }
    }

    /// A connection store, persisted to `path`, holding two saved
    /// connections ("conn-a" and "conn-b") so `active_tab_session_key`
    /// resolves each to a stable [`ConnectionKey::Saved`] rather than the
    /// [`ConnectionKey::Unsaved`] fallback.
    fn store_with_two_saved_connections(path: &std::path::Path) -> ConnectionStore {
        let mut store =
            ConnectionStore::load(path).expect("loading a nonexistent path must succeed empty");
        store
            .add(ConnectionArgs {
                name: "conn-a".to_owned(),
                url: "postgres://localhost/a".to_owned(),
            })
            .expect("add conn-a must succeed");
        store
            .add(ConnectionArgs {
                name: "conn-b".to_owned(),
                url: "postgres://localhost/b".to_owned(),
            })
            .expect("add conn-b must succeed");
        store
    }

    fn sample_schema_session(cx: &mut gpui::TestAppContext) -> gpui::Entity<Session> {
        let tree = SchemaTree {
            catalogs: vec![Catalog {
                name: "zsql".to_owned(),
                schemas: vec![SchemaNs {
                    name: "public".to_owned(),
                    tables: vec![Relation {
                        name: "orders".to_owned(),
                        kind: RelationKind::Table,
                        columns: vec![],
                    }],
                }],
            }],
        };
        cx.new(|_cx| Session::new_for_schema_test(SchemaState::Ready(tree)))
    }

    #[gpui::test]
    fn renders_the_sidebar_editor_and_results_without_panicking(cx: &mut gpui::TestAppContext) {
        let session = sample_schema_session(cx);
        cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                None,
                cx,
            )
        });
    }

    /// Renders an active `Schema` tab: its own read-only structural view
    /// fills the pane in place of the editor, divider, and shared results
    /// grid.
    #[gpui::test]
    fn renders_an_active_schema_tab_without_panicking(cx: &mut gpui::TestAppContext) {
        let session = sample_schema_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                None,
                cx,
            )
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.open_or_reuse_schema("public", "orders", RelationKind::Table, cx);
            });
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            let active = tabs.active_tab().expect("a schema tab is active");
            assert!(active.is_schema());
            assert!(active.schema_view().is_some());
            // A schema tab's editor is never rendered, so it must not be
            // offered up for keyboard focus.
            assert!(
                workspace.editor_focus_handle(cx).is_none(),
                "a read-only schema tab must not expose an editor focus handle"
            );
        });
    }

    /// Renders a compact `Generated` tab (the active tab, shown as the
    /// compact SQL strip) alongside a `Script` tab, both listed in the tab
    /// bar above the results grid.
    #[gpui::test]
    fn renders_a_generated_and_a_script_tab_with_results_without_panicking(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = sample_schema_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                None,
                cx,
            )
        });

        let generated_id = workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                let generated_id = tabs.open_or_reuse_generated("public", "orders", cx);
                tabs.new_script_tab(cx);
                generated_id
            })
        });
        vcx.run_until_parked();

        // Re-focus the generated tab so this frame renders both the tab
        // bar's script tab entry and the active tab's compact strip body.
        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.set_active(generated_id, cx));
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            // Plus the one empty script tab every workspace opens with.
            assert_eq!(tabs.tabs().len(), 3);
            assert_eq!(
                tabs.tabs().iter().filter(|tab| tab.is_generated()).count(),
                1
            );
            assert_eq!(
                tabs.tabs().iter().filter(|tab| !tab.is_generated()).count(),
                2
            );
            assert_eq!(tabs.active_id(), Some(generated_id));
        });
    }

    /// Renders a dirty, converted-from-generated `Script` tab as the active
    /// tab: the trailing `*` unsaved marker, the solid (not dashed) active
    /// underline, the full-height editor body, and no leftover generated
    /// theming all have to render without panicking once a generated tab
    /// has been edited.
    #[gpui::test]
    fn renders_a_dirty_converted_script_tab_as_the_active_tab_without_panicking(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = sample_schema_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                None,
                cx,
            )
        });

        let (converted_id, editor) = workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                let id = tabs.open_or_reuse_generated("public", "orders", cx);
                let editor = tabs
                    .tabs()
                    .iter()
                    .find(|tab| tab.id() == id)
                    .unwrap()
                    .editor()
                    .clone();
                (id, editor)
            })
        });
        // The buffer's own `EditListener` reports back to the same
        // `TabModel` entity that owns it, so the edit must happen outside
        // any in-progress `tabs.update` call -- gpui forbids re-entrant
        // updates of the same entity.
        editor.update(vcx, |editor, cx| editor.insert_text_for_test("x", cx));
        workspace.update(vcx, |workspace, cx| {
            workspace
                .tabs
                .update(cx, |tabs, cx| tabs.set_active(converted_id, cx));
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            let tab = tabs
                .tabs()
                .iter()
                .find(|tab| tab.id() == converted_id)
                .unwrap();
            assert!(!tab.is_generated(), "the edit must have converted the tab");
            assert!(tab.dirty(), "an edited tab is dirty");
            assert_eq!(tabs.active_id(), Some(converted_id));
        });
    }

    #[gpui::test]
    fn initial_pane_sizes_match_the_layout_configs_defaults(cx: &mut gpui::TestAppContext) {
        let session = sample_schema_session(cx);
        let layout = LayoutConfig::default();
        let expected_sidebar_width = layout.sidebar_default_width;
        let expected_editor_height = layout.editor_default_height;
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                layout,
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                None,
                cx,
            )
        });
        workspace.read_with(vcx, |workspace, _cx| {
            assert_eq!(workspace.sidebar_width, expected_sidebar_width);
            assert_eq!(workspace.editor_height, expected_editor_height);
        });
    }

    /// Exercises the full connect-switch wiring, not just the pure
    /// `TabModel`/`tab_session` logic: connecting to "conn-a", mutating its
    /// tabs, then switching to "conn-b" must flush "conn-a"'s tabs to disk
    /// under its own key before "conn-b"'s (snapshot-less, default) tabs
    /// replace what `TabModel` shows -- the two saves this triggers must not
    /// race each other or corrupt the store.
    #[gpui::test]
    fn switching_the_active_connection_persists_the_outgoing_tabs_and_shows_the_incoming_default(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = sample_schema_session(cx);
        let paths = PersistenceTestPaths::new("switch");
        let store = store_with_two_saved_connections(&paths.connections);
        let conn_a = ActiveConnection {
            id: Some(store.connections()[0].id),
            name: "conn-a".to_owned(),
            url: "postgres://localhost/a".to_owned(),
        };
        let conn_b = ActiveConnection {
            id: Some(store.connections()[1].id),
            name: "conn-b".to_owned(),
            url: "postgres://localhost/b".to_owned(),
        };
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                store,
                Duration::from_secs(2),
                Some(paths.tab_sessions.clone()),
                cx,
            )
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(conn_a, cx);
        });
        vcx.run_until_parked();

        // Mutate connection A's tabs beyond the default single empty
        // script, so its persisted snapshot is distinguishable from B's.
        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.new_script_tab(cx);
            });
        });
        vcx.run_until_parked();

        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(conn_b, cx);
        });
        vcx.run_until_parked();

        let saved_a = tab_session::load_snapshot(
            &paths.tab_sessions,
            &ConnectionKey::Saved("conn-a".to_owned()),
        )
        .expect("load must succeed")
        .expect("conn-a's tabs must have been persisted before switching away from it");
        assert_eq!(
            saved_a.tabs.len(),
            2,
            "the persisted snapshot must reflect conn-a's mutated tab set"
        );

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(
                tabs.tabs().len(),
                1,
                "conn-b has no snapshot yet, so it must show the default single tab, \
                 not conn-a's leftover tabs"
            );
            assert!(!tabs.tabs()[0].dirty());
        });
    }

    /// A key this session has already loaded/saved must never lose to a
    /// disk read that happens to observe an older state than what this
    /// process already knows -- e.g. a background write still in flight
    /// when the user reconnects to a connection they only just switched
    /// away from. Forces disk out of sync with the in-memory cache directly
    /// (standing in for a write that has not landed yet) and asserts the
    /// switch back to "conn-a" still shows this session's own latest tabs.
    #[gpui::test]
    fn switching_back_to_a_cached_connection_ignores_a_stale_disk_snapshot(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = sample_schema_session(cx);
        let paths = PersistenceTestPaths::new("cache-wins");
        let store = store_with_two_saved_connections(&paths.connections);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                store,
                Duration::from_secs(2),
                Some(paths.tab_sessions.clone()),
                cx,
            )
        });

        let conn_a_id = uuid::Uuid::new_v4();
        let conn_a = ActiveConnection {
            id: Some(conn_a_id),
            name: "conn-a".to_owned(),
            url: "postgres://localhost/a".to_owned(),
        };
        let conn_b = ActiveConnection {
            id: Some(uuid::Uuid::new_v4()),
            name: "conn-b".to_owned(),
            url: "postgres://localhost/b".to_owned(),
        };

        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(conn_a, cx);
        });
        vcx.run_until_parked();
        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.new_script_tab(cx);
            });
        });
        vcx.run_until_parked();

        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(conn_b, cx);
        });
        vcx.run_until_parked();

        // Disk now disagrees with what this session already knows for
        // "conn-a" -- standing in for a background write dispatched earlier
        // that has not actually landed yet by the time the user reconnects.
        tab_session::save_snapshot(
            &paths.tab_sessions,
            &ConnectionKey::Saved("conn-a".to_owned()),
            &tab_session::TabSessionSnapshot::default(),
        )
        .expect("overwrite must succeed");

        let conn_a_again = ActiveConnection {
            id: Some(conn_a_id),
            name: "conn-a".to_owned(),
            url: "postgres://localhost/a".to_owned(),
        };
        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(conn_a_again, cx);
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(
                tabs.tabs().len(),
                2,
                "this session's own cached tabs for conn-a must win over the \
                 stale snapshot forced onto disk"
            );
        });
    }

    /// The app-restart path: a fresh workspace (empty in-memory cache) that
    /// connects to a saved connection whose tabs are already on disk must
    /// read that snapshot back from the file and rebuild the tab model from
    /// it -- order, kind, title, buffer text, and active tab all exactly as
    /// persisted. Exercises the cache-miss disk-read branch end to end, which
    /// the cache-wins and switch tests never reach.
    #[gpui::test]
    fn connecting_to_a_saved_connection_restores_its_tabs_from_disk(cx: &mut gpui::TestAppContext) {
        let session = sample_schema_session(cx);
        let paths = PersistenceTestPaths::new("restore-from-disk");
        let store = store_with_two_saved_connections(&paths.connections);
        let conn_a_id = store.connections()[0].id;

        // Seed conn-a's tab session on disk before the workspace exists, as
        // if written by a previous run of the app.
        let seeded = tab_session::TabSessionSnapshot {
            tabs: vec![
                tab_session::TabEntrySnapshot {
                    kind: tab_session::TabEntryKind::Script,
                    title: "query-1.sql".to_owned(),
                    buffer_text: "select 1;".to_owned(),
                },
                tab_session::TabEntrySnapshot {
                    kind: tab_session::TabEntryKind::Generated {
                        schema: "public".to_owned(),
                        relation: "orders".to_owned(),
                        edited: false,
                    },
                    title: "orders".to_owned(),
                    buffer_text: "SELECT * FROM \"public\".\"orders\" LIMIT 200".to_owned(),
                },
            ],
            active_index: Some(1),
        };
        tab_session::save_snapshot(
            &paths.tab_sessions,
            &ConnectionKey::Saved("conn-a".to_owned()),
            &seeded,
        )
        .expect("seeding conn-a's snapshot on disk must succeed");

        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                store,
                Duration::from_secs(2),
                Some(paths.tab_sessions.clone()),
                cx,
            )
        });

        // First-ever connect to conn-a: the in-memory cache has no entry, so
        // this must fall through to the on-disk snapshot.
        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(
                ActiveConnection {
                    id: Some(conn_a_id),
                    name: "conn-a".to_owned(),
                    url: "postgres://localhost/a".to_owned(),
                },
                cx,
            );
        });
        vcx.run_until_parked();

        workspace.read_with(vcx, |workspace, cx| {
            let tabs = workspace.tabs.read(cx);
            assert_eq!(tabs.tabs().len(), 2, "both persisted tabs must be restored");

            let script = &tabs.tabs()[0];
            assert!(!script.is_generated());
            assert_eq!(script.title(), "query-1.sql");
            assert_eq!(script.editor().read(cx).text(), "select 1;");

            let generated = &tabs.tabs()[1];
            assert!(generated.is_generated());
            assert_eq!(generated.title(), "orders");
            assert_eq!(
                generated.editor().read(cx).text(),
                "SELECT * FROM \"public\".\"orders\" LIMIT 200"
            );

            assert_eq!(
                tabs.active_id(),
                Some(generated.id()),
                "the persisted active tab (index 1) must be the active one after restore"
            );
        });
    }

    /// `flush_tab_session_on_quit` returns a `Task` the caller must await
    /// before the app actually exits; this proves awaiting it actually
    /// finishes the write rather than the caller racing process exit
    /// against a fire-and-forget save.
    #[gpui::test]
    async fn flush_tab_session_on_quit_persists_the_active_connections_current_tabs(
        cx: &mut gpui::TestAppContext,
    ) {
        let session = sample_schema_session(cx);
        let paths = PersistenceTestPaths::new("quit-flush");
        let store = store_with_two_saved_connections(&paths.connections);
        let conn_a_id = store.connections()[0].id;
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                store,
                Duration::from_secs(2),
                Some(paths.tab_sessions.clone()),
                cx,
            )
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.set_active_connection(
                ActiveConnection {
                    id: Some(conn_a_id),
                    name: "conn-a".to_owned(),
                    url: "postgres://localhost/a".to_owned(),
                },
                cx,
            );
        });
        vcx.run_until_parked();

        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace.tabs.read(cx).tabs()[0].editor().clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.insert_text_for_test("select 42;", cx);
        });
        vcx.run_until_parked();

        let flush = workspace.update(vcx, WorkspaceView::flush_tab_session_on_quit);
        flush.await;

        let saved = tab_session::load_snapshot(
            &paths.tab_sessions,
            &ConnectionKey::Saved("conn-a".to_owned()),
        )
        .expect("load must succeed")
        .expect("flush_tab_session_on_quit must have written conn-a's tabs");
        assert_eq!(saved.tabs.len(), 1);
        assert_eq!(saved.tabs[0].buffer_text, "select 42;");
    }
}

/// Tests for the workspace header's Run button: it dispatches a run for the
/// active tab through the same `Session`/`QueryRunner` path
/// `TabModel::run_for_tab` already uses, for both a `Script` and a
/// `Generated` active tab, independent of keyboard focus.
#[cfg(test)]
mod header_tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use gpui::{AppContext as _, Entity, TestAppContext};
    use zsql_core::{BatchSink, Connection, CoreError, QueryHandle, RowCount, SchemaTree};

    use super::WorkspaceView;
    use crate::config::{LayoutConfig, ValuePanelConfig};
    use crate::connections::ConnectionStore;
    use crate::session::Session;

    /// A `Connection` double that records every SQL string `stream_query` is
    /// called with, in place of a real session/database, so a test can
    /// assert the header's Run button reached `Session::run_query`.
    struct RecordingConnection {
        queries: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Connection for RecordingConnection {
        fn stream_query(&self, sql: String, _sink: BatchSink) -> QueryHandle {
            self.queries
                .lock()
                .expect("queries lock poisoned")
                .push(sql);
            let (cancel_tx, _cancel_rx) = flume::unbounded();
            QueryHandle::new(cancel_tx)
        }

        async fn introspect(&self) -> Result<SchemaTree, CoreError> {
            Ok(SchemaTree::default())
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
        ) -> Result<zsql_core::RelationSchema, CoreError> {
            Ok(zsql_core::RelationSchema::default())
        }
    }

    /// An empty connection store backed by a path this test never writes to.
    fn empty_store_for_test() -> ConnectionStore {
        let path = std::env::temp_dir().join(format!(
            "zsql-workspace-header-test-{}.toml",
            std::process::id()
        ));
        ConnectionStore::load(&path).expect("loading a nonexistent path must succeed empty")
    }

    /// A session backed by a [`RecordingConnection`], and the queries it
    /// records.
    fn recording_session(cx: &mut TestAppContext) -> (Entity<Session>, Arc<Mutex<Vec<String>>>) {
        let queries: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let connection: Arc<dyn Connection> = Arc::new(RecordingConnection {
            queries: queries.clone(),
        });
        let session = cx.update(|cx| cx.new(|_cx| Session::new_for_query_test(connection)));
        (session, queries)
    }

    #[gpui::test]
    fn header_run_button_dispatches_run_for_the_active_script_tab(cx: &mut TestAppContext) {
        let (session, queries) = recording_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                None,
                cx,
            )
        });

        let editor = workspace.read_with(vcx, |workspace, cx| {
            workspace
                .tabs
                .read(cx)
                .active_tab()
                .expect("a workspace always opens with an active script tab")
                .editor()
                .clone()
        });
        editor.update(vcx, |editor, cx| {
            editor.set_text("select * from orders", cx);
        });

        workspace.update(vcx, WorkspaceView::run_active_tab);
        vcx.run_until_parked();

        assert_eq!(
            queries.lock().expect("queries lock poisoned").as_slice(),
            ["select * from orders"],
            "the header's Run button must dispatch the active script tab's SQL"
        );
    }

    #[gpui::test]
    fn header_run_button_dispatches_run_for_the_active_generated_tab(cx: &mut TestAppContext) {
        let (session, queries) = recording_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                None,
                cx,
            )
        });

        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.open_or_reuse_generated("public", "orders", cx);
            });
        });
        vcx.run_until_parked();
        // Opening a generated tab already dispatches its own preview run;
        // isolate what the header's own click contributes.
        queries.lock().expect("queries lock poisoned").clear();

        workspace.update(vcx, WorkspaceView::run_active_tab);
        vcx.run_until_parked();

        assert_eq!(
            queries.lock().expect("queries lock poisoned").as_slice(),
            ["SELECT * FROM \"public\".\"orders\" LIMIT 200"],
            "the header's Run button must dispatch the active generated tab's SQL"
        );
    }

    #[gpui::test]
    fn header_renders_above_both_a_script_and_a_generated_active_tab_without_panicking(
        cx: &mut TestAppContext,
    ) {
        let (session, _queries) = recording_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                None,
                cx,
            )
        });
        // A fresh workspace's active tab is a Script tab: this first frame
        // already exercises the header above a Script tab's full editor.
        vcx.run_until_parked();

        workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.open_or_reuse_generated("public", "orders", cx);
            });
        });
        vcx.run_until_parked();
        // The newly opened generated tab is now active: this frame exercises
        // the same header above a Generated tab's compact strip instead.
        workspace.read_with(vcx, |workspace, cx| {
            assert!(
                workspace
                    .tabs
                    .read(cx)
                    .active_tab()
                    .is_some_and(super::Tab::is_generated),
                "the generated tab must be active for this frame to cover it"
            );
        });
    }

    #[gpui::test]
    fn header_run_button_is_a_no_op_when_all_tabs_are_closed(cx: &mut TestAppContext) {
        let (session, queries) = recording_session(cx);
        let (workspace, vcx) = cx.add_window_view(|_window, cx| {
            WorkspaceView::new(
                session,
                LayoutConfig::default(),
                ValuePanelConfig::default(),
                empty_store_for_test(),
                Duration::from_secs(2),
                None,
                cx,
            )
        });

        // Close the default script tab, leaving no active tab
        workspace.update(vcx, |workspace, cx| {
            let tab_id = workspace
                .tabs
                .read(cx)
                .active_id()
                .expect("workspace should have an active tab initially");
            workspace.tabs.update(cx, |tabs, cx| {
                tabs.close_tab(tab_id, cx);
            });
        });
        vcx.run_until_parked();

        // Verify there's no active tab
        workspace.read_with(vcx, |workspace, cx| {
            assert_eq!(
                workspace.tabs.read(cx).active_id(),
                None,
                "all tabs should be closed"
            );
        });

        // Attempt to run the active tab when none exists
        workspace.update(vcx, WorkspaceView::run_active_tab);
        vcx.run_until_parked();

        // Verify no queries were recorded (it was a no-op)
        let locked_queries = queries.lock().expect("queries lock poisoned");
        assert!(
            locked_queries.is_empty(),
            "running when no active tab exists must be a no-op"
        );
    }
}
