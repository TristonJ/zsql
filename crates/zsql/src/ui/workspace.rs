//! The root workspace view

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, ClickEvent, Context, CursorStyle, Entity, FocusHandle, Focusable, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Render, Window, canvas, div, prelude::*,
    px, rgb,
};
use zsql_ui::colors;

use super::connections::{ActiveConnection, ConnectionManagerView};
use super::footer::ConnectionFooterView;
use super::results::ResultsView;
use super::sidebar::SidebarView;
use super::tabs::{Tab, TabId, TabKind, TabModel};
use super::theme;
use crate::config::LayoutConfig;
use crate::connections::ConnectionStore;
use crate::session::Session;

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
}

impl WorkspaceView {
    /// Build a workspace over `session`, with pane sizes seeded from `layout`
    /// and `connection_store` backing the connection-manager modal.
    #[must_use]
    pub fn new(
        session: Entity<Session>,
        layout: LayoutConfig,
        connection_store: ConnectionStore,
        cx: &mut Context<Self>,
    ) -> Self {
        let results = cx.new(|cx| ResultsView::new(session.clone(), "", cx));
        let tabs = cx.new(|cx| TabModel::new(session.clone(), results.clone(), cx));
        // Every workspace opens with one empty script tab so the editor
        // pane is never blank; `TabModel::new` itself stays tab-less so its
        // own constructor never has to call back into an entity that has
        // not finished being built yet.
        tabs.update(cx, |tabs, cx| {
            tabs.new_script_tab(cx);
        });
        let sidebar = cx.new(|cx| SidebarView::new(session.clone(), tabs.clone(), cx));
        let connections =
            cx.new(|cx| ConnectionManagerView::new(session.clone(), connection_store, cx));
        let footer = cx.new(|cx| ConnectionFooterView::new(session, connections.clone(), cx));
        let sidebar_width = layout.sidebar_default_width;
        let editor_height = layout.editor_default_height;

        // Opening/closing the modal (or switching its list/add-form panel)
        // lives entirely inside `connections`' own state; this workspace
        // must still re-render to mount or unmount that entity as the modal
        // overlay child below.
        cx.observe(&connections, |_this, _connections, cx| cx.notify())
            .detach();
        // Opening, reusing, converting, closing, or switching a tab lives
        // entirely inside `tabs`' own state; this workspace must still
        // re-render the tab bar and the active tab's body whenever any of
        // that changes.
        cx.observe(&tabs, |_this, _tabs, cx| cx.notify()).detach();

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
        }
    }

    /// The active tab's editor focus handle, so the app can focus it on
    /// startup. `None` only when every tab has been closed.
    #[must_use]
    pub fn editor_focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        self.tabs
            .read(cx)
            .active_tab()
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

    /// Set the tracked active connection, e.g. once the startup connect
    /// (`main.rs`, via [`Session::connect`]'s resolved DSN) succeeds. Threads
    /// straight through to the connection-manager modal, which is the single
    /// owner of this state; the footer observes that entity and updates
    /// itself.
    pub fn set_active_connection(&mut self, active: ActiveConnection, cx: &mut Context<Self>) {
        self.connections
            .update(cx, |view, cx| view.set_active(Some(active), cx));
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
                // The tab bar sits above the editor pane inside the same
                // measured column, so its fixed height is not itself
                // resizable and must be carved out of the container height
                // before splitting the rest between the editor and results.
                let available_height = self.column_height.get() - zsql_ui::tabs::TAB_BAR_HEIGHT;
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

    /// The tab bar: one entry per open tab, in order, plus the trailing "+"
    /// affordance that opens a new script tab.
    fn render_tab_bar(&self, cx: &Context<Self>) -> impl IntoElement {
        let active_id = self.tabs.read(cx).active_id();
        let mut bar = zsql_ui::tabs::tab_bar_shell();
        for tab in self.tabs.read(cx).tabs() {
            let active = active_id == Some(tab.id());
            bar = bar.child(Self::render_tab(tab, active, cx));
        }
        bar.child(
            zsql_ui::tabs::new_tab_glyph()
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
        let id = tab.id();
        let mut shell = zsql_ui::tabs::tab_shell(active).id(("workspace-tab", id));

        shell = match tab.kind() {
            TabKind::Generated { .. } => {
                shell = shell
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(theme::TAB_ICON_TEXT_SIZE))
                            .text_color(rgb(colors::TEAL))
                            .child("#"),
                    )
                    .child(div().italic().child(tab.title().to_owned()));
                if active {
                    shell = shell.child(zsql_ui::tabs::active_underline_solid());
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
                    shell = shell.child(zsql_ui::tabs::active_underline_solid());
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
                zsql_ui::tabs::close_glyph()
                    .id(("workspace-tab-close", id))
                    .cursor_pointer()
                    .on_click(cx.listener(move |view, _event: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        view.close_tab(id, window, cx);
                    })),
            )
    }

    /// The active tab's editor body: a compact, teal-tinted strip for a
    /// live `Generated` tab, or the full-height editor pane for a `Script`
    /// tab (including a converted-from-generated one). Renders nothing when
    /// every tab has been closed.
    fn render_active_body(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let Some(active) = self.tabs.read(cx).active_tab() else {
            return div().flex_shrink_0().into_any_element();
        };

        if active.is_generated() {
            Self::render_generated_strip(active).into_any_element()
        } else {
            div()
                .flex_shrink_0()
                .w_full()
                .h(self.editor_height)
                .child(active.editor().clone())
                .into_any_element()
        }
    }

    /// The compact, single-line SQL strip a live `Generated` tab renders
    /// instead of the full editor: a teal-tinted, left-accented row holding
    /// the (compact-mode) editor plus a trailing "generated" tag and hint.
    fn render_generated_strip(tab: &Tab) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .w_full()
            .h(theme::GENERATED_STRIP_HEIGHT)
            .bg(gpui::rgba(theme::GENERATED_STRIP_BG))
            .border_color(rgb(theme::GENERATED_STRIP_ACCENT))
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
                    .child(zsql_ui::grid::type_tag("generated"))
                    .child(
                        div()
                            .text_size(px(theme::GENERATED_HINT_TEXT_SIZE))
                            .text_color(rgb(colors::FAINT))
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

        div()
            .id("workspace-root")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(colors::INK))
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
                            .bg(rgb(colors::LINE))
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
                            .child(self.render_active_body(cx))
                            .child(
                                div()
                                    .id("editor-results-divider")
                                    .flex_shrink_0()
                                    .w_full()
                                    .h(divider_thickness)
                                    .cursor(CursorStyle::ResizeUpDown)
                                    .bg(rgb(colors::LINE))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(Self::start_editor_results_drag),
                                    ),
                            )
                            .child(div().flex_1().min_h_0().child(self.results.clone())),
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
    use gpui::AppContext as _;
    use zsql_core::{Catalog, Relation, RelationKind, SchemaNs, SchemaTree};

    use super::WorkspaceView;
    use crate::config::LayoutConfig;
    use crate::connections::ConnectionStore;
    use crate::session::{SchemaState, Session};

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
            WorkspaceView::new(session, LayoutConfig::default(), empty_store_for_test(), cx)
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
            WorkspaceView::new(session, LayoutConfig::default(), empty_store_for_test(), cx)
        });

        let generated_id = workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                let generated_id = tabs.open_or_reuse_generated("public", "orders", 200, cx);
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
            WorkspaceView::new(session, LayoutConfig::default(), empty_store_for_test(), cx)
        });

        let (converted_id, editor) = workspace.update(vcx, |workspace, cx| {
            workspace.tabs.update(cx, |tabs, cx| {
                let id = tabs.open_or_reuse_generated("public", "orders", 200, cx);
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
            WorkspaceView::new(session, layout, empty_store_for_test(), cx)
        });
        workspace.read_with(vcx, |workspace, _cx| {
            assert_eq!(workspace.sidebar_width, expected_sidebar_width);
            assert_eq!(workspace.editor_height, expected_editor_height);
        });
    }
}
