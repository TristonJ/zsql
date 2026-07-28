//! The root workspace view

use std::cell::Cell;
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

use super::connections::ConnectionManagerView;
use super::footer::ConnectionFooterView;
use super::results::ResultsView;
use super::sidebar::SidebarView;
use super::tab_bar;
use super::tabs::{Tab, TabId, TabModel};
use super::theme;
use crate::config::{LayoutConfig, ValuePanelConfig};
use crate::connections::ConnectionStore;
use crate::session::Session;
use crate::tab_session::{self, TabSessionStore};

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
    /// The tab-session persistence state machine: where the store lives on
    /// disk, which key `tabs` currently holds state for, the save/load race
    /// protection described on [`TabSessionStore`], and the per-key cache of
    /// the latest dispatched-for-save snapshot.
    tab_session_store: TabSessionStore,
}

impl WorkspaceView {
    /// Build a workspace over `session`, with pane sizes seeded from
    /// `layout` (also sizing the results grid's value panel dock),
    /// `value_panel` configuring that panel's parse thresholds and hex-dump
    /// layout, `connection_store` backing the connection-manager modal,
    /// `probe_timeout` (typically [`crate::config::Config::liveness`]'s
    /// `probe_timeout()`) bounding the connection-manager form's Test
    /// button, `batch_size` (typically [`crate::config::Config::query`]'s
    /// `batch_size`) sizing that Test button's connection's row-batching,
    /// and `tab_sessions_path` (typically
    /// [`crate::config::Config::tab_sessions_path`]) as where per-connection
    /// tab sessions are read from and saved to.
    #[must_use]
    // Every parameter is an independent, already-resolved piece of `Config`
    // this workspace's own descendants need at construction; grouping them
    // into a wrapper struct would only move the field list, not shrink it.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session: Entity<Session>,
        layout: LayoutConfig,
        value_panel: ValuePanelConfig,
        connection_store: ConnectionStore,
        probe_timeout: Duration,
        batch_size: usize,
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
            ConnectionManagerView::new(
                session.clone(),
                connection_store,
                probe_timeout,
                batch_size,
                cx,
            )
        });
        results.update(cx, |results, _cx| {
            results.set_connections_modal(connections.clone());
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
            if this
                .tab_session_store
                .active_connection_changed(new_active.as_ref())
            {
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
            if this.tab_session_store.take_suppressed() {
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
            tab_session_store: TabSessionStore::new(tab_sessions_path),
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
        let snapshot = self.tab_session_store.begin_switch(new_key, new_active);

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

    /// Build the active connection's current snapshot and spawn its write to
    /// disk on a background executor -- never on this (render/update) thread
    /// -- returning the spawned [`Task`] so a caller that must observe
    /// completion (app quit) can await it, while a caller that only wants to
    /// fire-and-forget (a tab change) can detach it.
    fn dispatch_tab_session_save(&mut self, cx: &mut Context<Self>) -> Option<Task<()>> {
        if !self.tab_session_store.can_persist() {
            return None;
        }
        let snapshot = self.tabs.read(cx).snapshot(cx);
        let (path, key, snapshot) = self.tab_session_store.dispatch_save(snapshot)?;
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

    /// Visible to [`super::tab_bar`], which wires this up to a tab's click
    /// handler.
    pub(crate) fn activate_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.update(cx, |tabs, cx| tabs.set_active(id, cx));
        self.focus_active_editor(window, cx);
    }

    /// Visible to [`super::tab_bar`], which wires this up to a tab's close
    /// glyph.
    pub(crate) fn close_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.update(cx, |tabs, cx| tabs.close_tab(id, cx));
        self.focus_active_editor(window, cx);
    }

    /// Visible to [`super::tab_bar`], which wires this up to the new-tab
    /// glyph's click handler.
    pub(crate) fn open_new_script_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                    .child(zsql_ui::grid::type_tag_accent("generated", active_theme))
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
                            .child(tab_bar::render_tab_bar(
                                self.tabs.read(cx).active_id(),
                                self.tabs.read(cx).tabs(),
                                cx,
                            ))
                            .children(self.render_main_pane(cx)),
                    ),
            )
            .when(modal_open, |el| el.child(self.connections.clone()))
    }
}

#[cfg(test)]
mod tests;
