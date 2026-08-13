//! The root workspace view

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    App, Bounds, ClickEvent, Context, CursorStyle, Entity, FocusHandle, Focusable, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels, Render, Task, Window,
    canvas, div, prelude::*, px, rems, rgb,
};
use zsql_ui::button::secondary_link_button;
use zsql_ui::icon::{IconName, icon};
use zsql_ui::theme::ActiveTheme;

use super::appearance::AppearanceModalView;
use super::connections::{ConnectionManagerView, UNSAVED_CONNECTION_LABEL};
use super::footer::ConnectionFooterView;
use super::open_modal::OpenModalView;
use super::results::ResultsView;
use super::sidebar::SidebarView;
use super::tab_bar;
use super::tabs::{Tab, TabId, TabModel};
use super::theme;
use crate::config::{LayoutConfig, ValuePanelConfig};
use crate::connections::ConnectionStore;
use crate::session::Session;
use crate::session_store::{self, SessionStore};
use crate::ui::tabs::{PreviewControlsChanged, ResultsChanged};
pub use startup::WorkspaceStartup;

/// The platform open-file dialog function "Browse files..." invokes
pub type OpenFilesPrompt = Box<dyn Fn(&mut App) -> Task<Option<Vec<PathBuf>>>>;
/// The platform save-file dialog function "Somewhere else..." invokes
pub type SaveFilePrompt =
    Box<dyn Fn(&mut App, &std::path::Path, Option<&str>) -> Task<Option<PathBuf>>>;

/// The default [`OpenFilesPrompt`]: `gpui`'s own native path-prompt API
/// (`App::prompt_for_paths`, backed by the platform's real open-file
/// dialog).
fn default_open_files_prompt(cx: &mut App) -> Task<Option<Vec<PathBuf>>> {
    let receiver = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: None,
    });
    cx.spawn(async move |_cx| match receiver.await {
        Ok(Ok(Some(paths))) => Some(paths),
        _ => None,
    })
}

/// The default [`SaveFilePrompt`]: `App::prompt_for_new_path`, gpui's own
/// native save-file dialog. See [`default_open_files_prompt`].
fn default_save_file_prompt(
    cx: &mut App,
    directory: &std::path::Path,
    suggested_name: Option<&str>,
) -> Task<Option<PathBuf>> {
    let receiver = cx.prompt_for_new_path(directory, suggested_name);
    cx.spawn(async move |_cx| match receiver.await {
        Ok(Ok(Some(path))) => Some(path),
        _ => None,
    })
}

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
    /// Kept alongside the sub-entities that also hold their own clone, so
    /// the header can read [`Session::is_connected`] for the Run button's
    /// enabled state without reaching into `tabs`.
    session: Entity<Session>,
    connections: Entity<ConnectionManagerView>,
    appearance: Entity<AppearanceModalView>,
    footer: Entity<ConnectionFooterView>,
    sidebar: Entity<SidebarView>,
    pub(crate) tabs: Entity<TabModel>,
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
    /// protection described on [`SessionStore`], and the per-key cache of
    /// the latest dispatched-for-save snapshot.
    session_store: SessionStore,
    /// The tab strip's horizontal scroll state; see [`tab_bar::TabBarState`].
    pub(crate) tab_bar: tab_bar::TabBarState,
    /// Width every tab-bar entry renders at; see [`LayoutConfig::tab_width`].
    tab_width: Pixels,
    /// Root directory the shared library lives under; see
    /// [`WorkspaceStartup::library_root`].
    library_dir: Option<PathBuf>,
    /// The Save Script / Save as / Rename modal.
    pub(crate) save_modal: Entity<crate::ui::save_modal::SaveModalView>,
    /// The Open Script picker.
    pub(crate) open_modal: Entity<OpenModalView>,
    /// The platform open-file dialog seam; see [`OpenFilesPrompt`].
    open_files_prompt: OpenFilesPrompt,
    /// The platform save-file dialog seam; see [`SaveFilePrompt`].
    save_file_prompt: SaveFilePrompt,
    /// How long the footer's post-save confirmation stays visible.
    save_confirmation_duration: Duration,
    /// Invalidates a pending save-confirmation clear timer once a newer
    /// confirmation (or an unrelated footer update) supersedes it, so two
    /// saves in quick succession cannot have the first one's timer clear
    /// the second's message early.
    save_confirmation_generation: Rc<Cell<u64>>,
    /// Set when the save or open modal is dismissed without confirming
    /// (Escape, the close icon, or Cancel), consumed by the next `render`
    pub(crate) refocus_editor_on_next_render: bool,
    /// Set while a "Browse files..." native dialog is open
    pub(crate) browse_dialog_in_flight: Rc<Cell<bool>>,
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
    /// and `startup` bundling the remaining persisted settings (see
    /// [`WorkspaceStartup`]).
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
        startup: WorkspaceStartup,
        cx: &mut Context<Self>,
    ) -> Self {
        let WorkspaceStartup {
            sessions_root,
            library_root,
            active_theme_name,
            themes_dir,
            config_path,
            save_confirmation_duration,
            edit_debounce,
            scripts_relative_time_refresh,
        } = startup;
        let header_session = session.clone();
        let results = cx.new(|cx| ResultsView::new(session.clone(), "", cx));
        results.update(cx, |results, cx| {
            results.configure_value_panel(cx, &layout, value_panel);
        });
        let session_store = SessionStore::new(sessions_root);
        let tabs = Self::build_tabs(
            &session,
            library_root.clone(),
            edit_debounce,
            session_store.claim_factory(),
            cx,
        );
        let save_modal = cx.new(crate::ui::save_modal::SaveModalView::new);
        let open_modal = cx.new(OpenModalView::new);

        let sidebar = Self::build_sidebar(
            &session,
            &tabs,
            library_root.clone(),
            scripts_relative_time_refresh,
            cx,
        );
        let (connections, appearance, footer) = Self::build_connection_chrome(
            session,
            &results,
            connection_store,
            probe_timeout,
            batch_size,
            active_theme_name,
            themes_dir,
            config_path,
            cx,
        );

        Self::subscribe_to_tab_events(&tabs, &results, &footer, cx);
        Self::subscribe_to_save_events(&tabs, &save_modal, cx);
        Self::subscribe_to_open_events(&tabs, &open_modal, cx);

        // Every workspace opens with one empty script tab so the editor
        // pane is never blank
        tabs.update(cx, |tabs, cx| {
            tabs.new_script_tab(cx);
        });

        let sidebar_width = layout.sidebar_default_width;
        let editor_height = layout.editor_default_height;
        let tab_width = layout.tab_width;
        let tab_bar_state = tab_bar::TabBarState::new(cx);

        Self::subscribe_to_connection_and_tab_changes(&connections, &appearance, &tabs, cx);

        Self {
            session: header_session,
            connections,
            appearance,
            footer,
            sidebar,
            tabs,
            results,
            layout,
            sidebar_width,
            editor_height,
            drag: None,
            column_height: Rc::new(Cell::new(Pixels::ZERO)),
            session_store,
            tab_bar: tab_bar_state,
            tab_width,
            library_dir: library_root,
            save_modal,
            open_modal,
            open_files_prompt: Box::new(default_open_files_prompt),
            save_file_prompt: Box::new(default_save_file_prompt),
            save_confirmation_duration,
            save_confirmation_generation: Rc::new(Cell::new(0)),
            refocus_editor_on_next_render: false,
            browse_dialog_in_flight: Rc::new(Cell::new(false)),
        }
    }

    fn subscribe_to_tab_events(
        tabs: &Entity<TabModel>,
        results: &Entity<ResultsView>,
        footer: &Entity<ConnectionFooterView>,
        cx: &mut Context<Self>,
    ) {
        let changed_results = results.clone();
        cx.subscribe(tabs, move |_v, _tabs, evt: &ResultsChanged, cx| {
            changed_results.update(cx, |results, cx| match evt {
                ResultsChanged::Live(label) => results.show_live(label, cx),
                ResultsChanged::Snapshot(snap) => results.show_snapshot(snap.clone(), cx),
            });
        })
        .detach();
        let preview_results = results.clone();
        cx.subscribe(tabs, move |_v, _tabs, evt: &PreviewControlsChanged, cx| {
            preview_results.update(cx, |results, cx| {
                results.set_preview_controls(evt.0.clone(), cx);
            });
        })
        .detach();
        let changed_footer = footer.clone();
        cx.subscribe(tabs, move |_v, _tabs, evt: &ResultsChanged, cx| {
            changed_footer.update(cx, |footer, cx| match evt {
                ResultsChanged::Live(_) => footer.set_result_snapshot(None, cx),
                ResultsChanged::Snapshot(snap) => {
                    footer.set_result_snapshot(Some(snap.clone()), cx);
                }
            });
        })
        .detach();
        let preview_footer = footer.clone();
        cx.subscribe(tabs, move |_v, _tabs, evt: &PreviewControlsChanged, cx| {
            preview_footer.update(cx, |footer, cx| {
                footer.set_row_count(evt.0.as_ref().and_then(|c| c.state.total_rows()), cx);
            });
        })
        .detach();
    }

    fn subscribe_to_connection_and_tab_changes(
        connections: &Entity<ConnectionManagerView>,
        appearance: &Entity<AppearanceModalView>,
        tabs: &Entity<TabModel>,
        cx: &mut Context<Self>,
    ) {
        cx.observe(connections, |this, connections, cx| {
            let new_active = connections.read(cx).active().cloned();
            if this
                .session_store
                .active_connection_changed(new_active.as_ref())
            {
                this.handle_active_connection_changed(cx);
            }
            cx.notify();
        })
        .detach();
        cx.observe(appearance, |_this, _appearance, cx| {
            cx.notify();
        })
        .detach();
        cx.observe(tabs, |this, _tabs, cx| {
            if this.session_store.take_suppressed() {
                cx.notify();
                return;
            }
            this.save_active_tab_session(cx);
            cx.notify();
        })
        .detach();
    }

    fn subscribe_to_save_events(
        tabs: &Entity<TabModel>,
        save_modal: &Entity<crate::ui::save_modal::SaveModalView>,
        cx: &mut Context<Self>,
    ) {
        cx.subscribe(tabs, |this, _tabs, evt: &super::tabs::SaveRequested, cx| {
            this.handle_save_requested(evt, cx);
        })
        .detach();
        cx.subscribe(
            save_modal,
            |this, _modal, evt: &crate::ui::save_modal::SaveModalEvent, cx| {
                this.handle_save_modal_event(evt, cx);
            },
        )
        .detach();
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
        let connection_name = new_active.as_ref().map_or_else(
            || UNSAVED_CONNECTION_LABEL.to_owned(),
            |active| active.name.clone(),
        );
        let snapshot = self.session_store.begin_switch(new_key, new_active);

        self.tabs.update(cx, |tabs, cx| {
            tabs.load_for_connection(snapshot.as_deref(), cx);
        });
        let session_dir = self.session_store.active_session_dir();
        self.tabs.update(cx, |tabs, _cx| {
            tabs.set_session_dir(session_dir.clone());
        });
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_session_dir(session_dir, cx);
            sidebar.set_connection_name(connection_name, cx);
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

    /// Flush a theme selected in the Appearance modal but not yet committed to
    /// disk (the modal persists on dismiss, so a choice made and then quit with
    /// the modal still open would otherwise be lost). Used from `main.rs`'s
    /// `App::on_app_quit` hook.
    pub fn flush_theme_on_quit(&mut self, cx: &mut Context<Self>) {
        self.appearance
            .update(cx, |appearance, _cx| appearance.flush_theme_on_quit());
    }

    /// Build the active connection's current snapshot and spawn its write to
    /// disk on a background executor -- never on this (render/update) thread
    /// -- returning the spawned [`Task`] so a caller that must observe
    /// completion (app quit) can await it, while a caller that only wants to
    /// fire-and-forget (a tab change) can detach it.
    fn dispatch_tab_session_save(&mut self, cx: &mut Context<Self>) -> Option<Task<()>> {
        if !self.session_store.can_persist() {
            return None;
        }
        let snapshot = self.tabs.read(cx).snapshot(cx);
        let (path, key, snapshot, claim) = self.session_store.dispatch_save(snapshot)?;
        Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    session_store::SessionDir::new(&path, key)
                        .save_snapshot_if_current(&snapshot, claim)
                })
                .await;
            if let Err(err) = result {
                tracing::warn!(error = %err, "failed to save tab session");
                let _ = this.update(cx, |this, cx| {
                    this.show_save_error("Failed to save tab session", cx);
                });
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
    /// affordance. The Run button is disabled -- muted fill, non-interactive
    /// -- whenever `session` holds no live connection; see
    /// [`Session::is_connected`].
    fn render_header(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let run_shortcut = if cfg!(target_os = "macos") {
            "Cmd+Enter"
        } else {
            "Ctrl+Enter"
        };
        let active_theme = cx.theme().clone();
        let colors = active_theme.colors;
        let is_connected = self.session.read(cx).is_connected();
        let is_running = self.session.read(cx).is_running();
        let can_run = is_connected && !is_running;
        let can_cancel = is_connected && is_running;
        let run_button_bg = if can_run {
            colors.accent
        } else {
            theme::run_button_disabled_bg(&active_theme)
        };

        let cancel_button = secondary_link_button("workspace-cancel-query-button", window, cx)
            .ml_auto()
            .text_size(rems(0.75))
            .child("cancel query")
            .px_4()
            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                view.session
                    .update(cx, super::super::session::Session::cancel_query);
            }));

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
            .when(can_cancel, |el| el.child(cancel_button))
            .child(
                div()
                    .id("workspace-run-query-button")
                    .debug_selector(|| "workspace-run-query-button".to_owned())
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .h(theme::RUN_BUTTON_HEIGHT)
                    .px(px(theme::RUN_BUTTON_PADDING_X))
                    .rounded(px(theme::RUN_BUTTON_RADIUS))
                    .bg(rgb(run_button_bg))
                    .text_color(rgb(colors.bg_app))
                    .when(can_run, |el| {
                        el.cursor_pointer()
                            .hover(|style| style.bg(rgb(theme::run_button_hover_bg(&active_theme))))
                            .on_click(cx.listener(|view, _event: &ClickEvent, _window, cx| {
                                view.run_active_tab(cx);
                            }))
                    })
                    .when(!can_run, gpui::Styled::cursor_not_allowed)
                    .child(icon(
                        IconName::Run,
                        theme::RUN_BUTTON_ICON_SIZE,
                        colors.bg_app,
                    ))
                    .child(
                        div()
                            .text_size(px(theme::RUN_BUTTON_TEXT_SIZE))
                            .when(is_running, |el| el.child("Running..."))
                            .when(!is_running, |el| el.child("Run")),
                    )
                    .when(!is_running, |el| {
                        el.child(
                            div()
                                .text_size(px(theme::RUN_BUTTON_HINT_TEXT_SIZE))
                                .text_color(theme::run_button_hint(&active_theme))
                                .child(run_shortcut),
                        )
                    }),
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
            let line_count = active.editor().read(cx).line_count();
            Self::render_generated_strip(active, line_count, active_theme).into_any_element()
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
    fn render_main_pane(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
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
            self.render_header(window, cx).into_any_element(),
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
    fn render_generated_strip(
        tab: &Tab,
        line_count: usize,
        active_theme: &zsql_ui::theme::Theme,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .w_full()
            .h(theme::generated_strip_height(line_count))
            .bg(theme::generated_strip_bg(active_theme))
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.refocus_editor_on_next_render) {
            self.focus_active_editor(window, cx);
        }
        let divider_thickness = self.layout.divider_thickness;
        let column_height = self.column_height.clone();
        let modal_open = self.connections.read(cx).is_open();
        let appearance_open = self.appearance.read(cx).is_open();
        let colors = cx.theme().colors;

        div()
            .id("workspace-root")
            .debug_selector(|| "workspace-root".to_owned())
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
                            .child(div().flex_1().min_h_0().child(self.sidebar.clone())),
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
                                &self.tabs,
                                &self.tab_bar,
                                self.tab_width,
                                cx,
                            ))
                            .children(self.render_main_pane(window, cx)),
                    ),
            )
            .child(self.footer.clone())
            .when(modal_open, |el| el.child(self.connections.clone()))
            .when(appearance_open, |el| el.child(self.appearance.clone()))
            .when(self.save_modal.read(cx).is_open(), |el| {
                el.child(self.save_modal.clone())
            })
            .when(self.open_modal.read(cx).is_open(), |el| {
                el.child(self.open_modal.clone())
            })
    }
}

/// Test-only accessors/injection points for this view's own tests and for
/// `ui::workspace::tests`' end-to-end keybinding coverage.
#[cfg(test)]
impl WorkspaceView {
    /// Replace the "Browse files..." seam with `prompt`, so a test can fake
    /// the picked paths without invoking the real platform dialog (which
    /// `gpui`'s test platform does not implement and would panic on).
    pub(crate) fn set_open_files_prompt_for_test(&mut self, prompt: OpenFilesPrompt) {
        self.open_files_prompt = prompt;
    }

    /// Replace the "Somewhere else..." save-file seam with `prompt`, for the
    /// same reason as [`Self::set_open_files_prompt_for_test`].
    pub(crate) fn set_save_file_prompt_for_test(&mut self, prompt: SaveFilePrompt) {
        self.save_file_prompt = prompt;
    }
}

mod open_flow;
mod save_flow;
mod startup;

#[cfg(test)]
mod tests;
