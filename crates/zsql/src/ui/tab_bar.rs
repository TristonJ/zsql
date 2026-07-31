//! The workspace tab bar's render logic: one entry per open tab, each styled
//! per its kind, plus the trailing "+" affordance that opens a new script
//! tab. The tabs themselves scroll horizontally once they overflow the
//! bar's available width, via `zsql_ui`'s reusable `scrollable` module; the
//! "+" affordance stays fixed at the strip's trailing end, outside the
//! scrolling region.

use std::cell::{Cell, RefCell};

use gpui::{
    ClickEvent, Context, Entity, IntoElement, MouseButton, MouseDownEvent, Pixels, ScrollHandle,
    ScrollWheelEvent, Window, div, point, prelude::*, px, rgb,
};
use zsql_ui::context_menu::{ContextMenu, ContextMenuItem};
use zsql_ui::icon::{IconName, icon};
use zsql_ui::scrollable::{Axis, ScrollSource, ScrollableState, ScrollbarStyle, WithScrollbars};
use zsql_ui::theme::{ActiveTheme, Theme};

use super::tabs::{Tab, TabId, TabModel};
use super::theme as workspace_theme;
use super::workspace::WorkspaceView;
use crate::session_store::ScriptBacking;
use crate::session_store::TabKind;
use crate::session_store::backing::SaveAction;

/// A tab count no real tab strip ever reaches, used to seed
/// [`TabBarState::last_rendered_tab_count`] so its very first render is
/// always treated as reflecting a stale (never yet laid out) viewport.
const UNMEASURED_TAB_COUNT: usize = usize::MAX;

/// Frame-persistent state behind the tab strip's horizontal overflow: the
/// scroll handle backing it, the drag/wheel/thumb chrome
/// `zsql_ui::scrollable` renders over it, and which tab was active as of
/// the last render (so scrolling the active tab into view happens only on
/// an actual activation, never re-snapping over a scroll the user just made
/// by hand while that same tab stayed active).
pub(crate) struct TabBarState {
    scroll: Entity<ScrollableState>,
    handle: ScrollHandle,
    last_active: Cell<Option<TabId>>,
    /// The tab count as of the render whose layout `handle`'s bounds and
    /// max offset currently reflect. When this differs from the tab count
    /// a render sees, that render's own elements have not been through a
    /// layout pass yet, so `handle`'s geometry still describes the tab
    /// strip's previous content and cannot answer whether the active tab
    /// is on screen.
    last_rendered_tab_count: Cell<usize>,
    /// Whether a retry render, scheduled because the viewport's geometry
    /// was not yet trustworthy, is already pending.
    reveal_retry_pending: Cell<bool>,
    /// The currently open tab right-click context menu, if any.
    context_menu: RefCell<Option<TabContextMenuState>>,
}

/// A tab's open right-click context menu: which tab it targets and the
/// window position it should anchor to.
#[derive(Debug, Clone, Copy)]
struct TabContextMenuState {
    tab_id: TabId,
    position: gpui::Point<Pixels>,
}

impl TabBarState {
    #[must_use]
    pub(crate) fn new(cx: &mut Context<WorkspaceView>) -> Self {
        Self {
            scroll: cx.new(ScrollableState::new),
            handle: ScrollHandle::new(),
            last_active: Cell::new(None),
            last_rendered_tab_count: Cell::new(UNMEASURED_TAB_COUNT),
            reveal_retry_pending: Cell::new(false),
            context_menu: RefCell::new(None),
        }
    }

    /// Open the right-click context menu for `tab_id`, anchored at
    /// `position`, replacing any menu already open.
    pub(crate) fn open_context_menu(&self, tab_id: TabId, position: gpui::Point<Pixels>) {
        *self.context_menu.borrow_mut() = Some(TabContextMenuState { tab_id, position });
    }

    /// Close the open tab context menu, if any. Returns whether one was
    /// open, so a caller can decide whether a re-render is needed.
    fn close_context_menu(&self) -> bool {
        self.context_menu.borrow_mut().take().is_some()
    }

    /// Scrolls the tab at `active_index` into view if `active_id` differs
    /// from the tab active as of the last render whose geometry `handle`
    /// reflects, and any part of that tab currently sits outside the
    /// strip's visible width, given every tab renders at `tab_width`. A
    /// no-op when `active_id` was already active last render (so a manual
    /// scroll or drag is never fought back into place), when the strip has
    /// nothing to scroll, or when `active_index` is `None`.
    ///
    /// While `handle`'s geometry still describes an earlier tab count (or
    /// has never been through a layout pass at all), this cannot tell
    /// whether `active_id` is really off screen: rather than risk a verdict
    /// computed from stale bounds, it leaves `active_id` unconsumed and
    /// schedules one retry render for once the pending layout has settled.
    fn scroll_active_into_view(
        &self,
        active_id: Option<TabId>,
        active_index: Option<usize>,
        tab_width: Pixels,
        tab_count: usize,
        cx: &mut Context<WorkspaceView>,
    ) {
        let viewport_reflects_current_content = self.last_rendered_tab_count.get() == tab_count
            && self.handle.bounds().size.width != px(0.0);
        self.last_rendered_tab_count.set(tab_count);

        if !viewport_reflects_current_content {
            self.schedule_reveal_retry(cx);
            return;
        }
        self.reveal_retry_pending.set(false);

        if self.last_active.replace(active_id) == active_id {
            return;
        }
        let Some(index) = active_index else {
            return;
        };
        let max_offset = f32::from(self.handle.max_offset().width);
        if max_offset <= 0.0 {
            return;
        }
        let viewport_extent = f32::from(self.handle.bounds().size.width);
        let current_offset = -f32::from(self.handle.offset().x);
        let Some(target) = scroll_offset_to_reveal(
            index,
            f32::from(tab_width),
            viewport_extent,
            max_offset,
            current_offset,
        ) else {
            return;
        };

        let offset_y = self.handle.offset().y;
        self.handle.set_offset(point(px(-target), offset_y));
    }

    /// Schedules a single re-render so a deferred reveal can retry once the
    /// tab strip's pending layout has settled. Coalesces with any retry
    /// already pending.
    fn schedule_reveal_retry(&self, cx: &mut Context<WorkspaceView>) {
        if self.reveal_retry_pending.replace(true) {
            return;
        }
        cx.spawn(async move |view, cx| {
            if let Err(err) = view.update(cx, |_, cx| cx.notify()) {
                tracing::debug!(%err, "tab bar reveal retry: view gone before re-render");
            }
        })
        .detach();
    }
}

/// The horizontal offset that brings the tab at `index` (every tab
/// `tab_width` pixels wide, laid out left to right from zero) fully within
/// a `viewport_extent`-wide viewport currently scrolled to `current_offset`,
/// clamped to `[0, max_offset]`. `None` when that tab is already fully
/// visible at `current_offset`.
#[allow(clippy::cast_precision_loss)] // a tab's index is always a small count
fn scroll_offset_to_reveal(
    index: usize,
    tab_width: f32,
    viewport_extent: f32,
    max_offset: f32,
    current_offset: f32,
) -> Option<f32> {
    let left = index as f32 * tab_width;
    let right = left + tab_width;
    let viewport_end = current_offset + viewport_extent;

    let target = if left < current_offset {
        left
    } else if right > viewport_end {
        right - viewport_extent
    } else {
        return None;
    };

    Some(target.clamp(0.0, max_offset))
}

/// The horizontal component of a wheel gesture's pixel delta over the tab
/// strip: the delta's own x component when populated (a horizontal
/// trackpad swipe, or a shift-held wheel), falling back to its y component
/// otherwise so a plain vertical wheel notch pans the strip too. The strip
/// has no vertical axis of its own to disambiguate against, so every wheel
/// gesture over it is read as horizontal, the way editors typically treat
/// wheel input over an overflowing tab bar.
fn wheel_delta_x(event: &ScrollWheelEvent, window: &Window) -> Pixels {
    let delta = event.delta.pixel_delta(window.line_height());
    if delta.x == px(0.0) { delta.y } else { delta.x }
}

/// Pans `handle` by a wheel gesture's horizontal delta, clamped to
/// `[0, max_offset]`. A no-op once the strip has nothing left to scroll or
/// the gesture carries no horizontal component. Returns whether the offset
/// changed.
fn scroll_tab_strip_by_wheel(
    handle: &ScrollHandle,
    event: &ScrollWheelEvent,
    window: &Window,
) -> bool {
    let max_offset = f32::from(handle.max_offset().width);
    if max_offset <= 0.0 {
        return false;
    }

    let delta_x = f32::from(wheel_delta_x(event, window));
    if delta_x == 0.0 {
        return false;
    }

    let current_offset = -f32::from(handle.offset().x);
    let new_offset = (current_offset - delta_x).clamp(0.0, max_offset);
    if (new_offset - current_offset).abs() < f32::EPSILON {
        return false;
    }

    let offset_y = handle.offset().y;
    handle.set_offset(point(px(-new_offset), offset_y));
    true
}

/// The width the row of tabs must be forced to (via an explicit `min_w` on
/// the scrolled child) so real overflow occurs instead of flexbox shrinking
/// every tab to fit the strip's available width.
#[allow(clippy::cast_precision_loss)] // the open-tab count is always small
fn tab_row_min_width(tab_count: usize, tab_width: Pixels) -> Pixels {
    px(tab_count as f32 * f32::from(tab_width))
}

/// The tab bar: a horizontally scrollable strip holding one entry per open
/// tab, in order, plus the trailing "+" affordance that opens a new script
/// tab, fixed at the strip's end regardless of scroll position or tab count.
#[must_use]
pub fn render_tab_bar(
    tabs_entity: &Entity<TabModel>,
    tab_bar: &TabBarState,
    tab_width: Pixels,
    cx: &mut Context<WorkspaceView>,
) -> impl IntoElement {
    let theme = cx.theme().clone();

    let (tab_count, active_id, active_index, rendered_tabs) = {
        let tabs = tabs_entity.read(cx);
        let active_id = tabs.active_id();
        let active_index = active_id.and_then(|id| tabs.tabs().iter().position(|t| t.id() == id));
        let rendered = tabs
            .tabs()
            .iter()
            .map(|tab| {
                let active = active_id == Some(tab.id());
                render_tab(tab, active, tab_width, &theme, cx).into_any_element()
            })
            .collect::<Vec<_>>();
        (tabs.tabs().len(), active_id, active_index, rendered)
    };

    tab_bar.scroll_active_into_view(active_id, active_index, tab_width, tab_count, cx);

    let handle = tab_bar.handle.clone();
    tab_bar.scroll.update(cx, move |state, _cx| {
        state.horizontal(Axis::measured(ScrollSource::Container(handle)));
    });

    let row = div()
        .id("tab-bar-row")
        .flex()
        .flex_row()
        .h_full()
        .min_w(tab_row_min_width(tab_count, tab_width))
        .children(rendered_tabs);

    let wheel_handle = tab_bar.handle.clone();
    let wheel_scroll_state = tab_bar.scroll.clone();
    let viewport = div()
        .id("tab-bar-scroll-viewport")
        .debug_selector(|| "tab-bar-scroll-viewport".to_owned())
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .h_full()
        .overflow_x_hidden()
        .track_scroll(&tab_bar.handle)
        .on_scroll_wheel(move |event: &ScrollWheelEvent, window, cx| {
            if scroll_tab_strip_by_wheel(&wheel_handle, event, window) {
                // `handle` is a plain `ScrollHandle`, not an entity of its
                // own to notify through; piggyback on the scrollable state
                // entity this same strip already owns so its offset change
                // still triggers a repaint.
                wheel_scroll_state.update(cx, |_state, cx| cx.notify());
            }
        })
        .child(row);

    let scrolled = viewport.with_scrollbars(
        &tab_bar.scroll,
        ScrollbarStyle {
            track_width: workspace_theme::TAB_SCROLLBAR_TRACK_WIDTH,
            ..Default::default()
        },
        cx,
    );

    zsql_ui::tabs::tab_bar_shell(&theme)
        .child(scrolled)
        .child(
            zsql_ui::tabs::new_tab_glyph(&theme)
                .id("workspace-new-tab")
                .debug_selector(|| "workspace-new-tab".to_owned())
                .cursor_pointer()
                .on_click(cx.listener(|view, _event: &ClickEvent, window, cx| {
                    view.open_new_script_tab(window, cx);
                })),
        )
        .children(render_tab_context_menu(tabs_entity, tab_bar, cx))
}

/// One action offered by a `Script` tab's right-click context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabMenuAction {
    Save,
    SaveAs,
    Rename,
    CopyToLibrary,
    RevealInFiles,
    Close,
}

impl TabMenuAction {
    fn item_id(self) -> &'static str {
        match self {
            Self::Save => "tab-context-menu-save",
            Self::SaveAs => "tab-context-menu-save-as",
            Self::Rename => "tab-context-menu-rename",
            Self::CopyToLibrary => "tab-context-menu-copy-to-library",
            Self::RevealInFiles => "tab-context-menu-reveal-in-files",
            Self::Close => "tab-context-menu-close",
        }
    }

    /// The menu item's label, including its keyboard shortcut hint where one
    /// actually exists
    fn label(self) -> String {
        match self {
            Self::Save => format!("Save  {}", workspace_theme::save_shortcut_label()),
            Self::SaveAs => format!("Save as...  {}", workspace_theme::save_as_shortcut_label()),
            Self::Rename => "Rename...".to_owned(),
            Self::CopyToLibrary => "Copy to library".to_owned(),
            Self::RevealInFiles => "Reveal in files".to_owned(),
            Self::Close => "Close".to_owned(),
        }
    }
}

/// The `Script` tab context menu's items, in display order
const TAB_CONTEXT_MENU_ITEMS: [TabMenuAction; 6] = [
    TabMenuAction::Save,
    TabMenuAction::SaveAs,
    TabMenuAction::Rename,
    TabMenuAction::CopyToLibrary,
    TabMenuAction::RevealInFiles,
    TabMenuAction::Close,
];

/// The indices within [`TAB_CONTEXT_MENU_ITEMS`] a separator renders before:
/// one before the file-verb group (Copy to library / Reveal in files), one
/// before Close.
const TAB_CONTEXT_MENU_SEPARATORS_BEFORE: [usize; 2] = [3, 5];

/// Whether `action` must render disabled for a tab with `backing`
fn action_disabled_for_backing(action: TabMenuAction, backing: Option<&ScriptBacking>) -> bool {
    match action {
        TabMenuAction::Save => backing.is_some_and(|b| b.save_action() == SaveAction::NoOp),
        TabMenuAction::Rename => backing.is_some_and(|b| !b.supports_rename()),
        TabMenuAction::SaveAs
        | TabMenuAction::CopyToLibrary
        | TabMenuAction::RevealInFiles
        | TabMenuAction::Close => false,
    }
}

/// Right-clicking a `Script` tab's context menu: [`TAB_CONTEXT_MENU_ITEMS`]
/// in order. `None` when no tab's menu is currently open.
fn render_tab_context_menu(
    tabs_entity: &Entity<TabModel>,
    tab_bar: &TabBarState,
    cx: &mut Context<WorkspaceView>,
) -> Option<gpui::AnyElement> {
    let state = *tab_bar.context_menu.borrow().as_ref()?;
    let id = state.tab_id;
    let backing = tabs_entity.read(cx).script_backing_of(id);

    let mut menu = ContextMenu::new("tab-context-menu")
        .position(state.position)
        .on_close(cx.listener(|view, (), _window, cx| {
            if view.tab_bar.close_context_menu() {
                cx.notify();
            }
        }));

    for (index, action) in TAB_CONTEXT_MENU_ITEMS.into_iter().enumerate() {
        if TAB_CONTEXT_MENU_SEPARATORS_BEFORE.contains(&index) {
            menu = menu.add_separator();
        }
        let disabled = action_disabled_for_backing(action, backing.as_ref());
        menu = menu.add_item(
            ContextMenuItem::with_id(action.item_id(), action.label())
                .disabled(disabled)
                .on_click(cx.listener(move |view, _event, window, cx| {
                    match action {
                        TabMenuAction::Save => {
                            view.tabs.update(cx, |tabs, cx| tabs.trigger_save(id, cx));
                        }
                        TabMenuAction::SaveAs => {
                            view.tabs
                                .update(cx, |tabs, cx| tabs.trigger_save_as(id, cx));
                        }
                        TabMenuAction::Rename => view.open_rename_modal(id, cx),
                        TabMenuAction::CopyToLibrary => view.copy_tab_to_library(id, cx),
                        TabMenuAction::RevealInFiles => view.reveal_tab_in_files(id, cx),
                        TabMenuAction::Close => {
                            view.tab_bar.close_context_menu();
                            view.close_tab(id, window, cx);
                            return;
                        }
                    }
                    view.tab_bar.close_context_menu();
                    cx.notify();
                })),
        );
    }

    Some(menu.into_any_element())
}

/// The `VisualTestContext::debug_bounds` lookup key for the tab-bar entry
/// rendered for `id`, shared between [`render_tab`] (which sets it) and this
/// crate's own render tests (which look it up).
fn tab_debug_selector(id: TabId) -> String {
    format!("workspace-tab-{id}")
}

/// The `VisualTestContext::debug_bounds` lookup key for `id`'s tab-bar entry,
/// for this crate's own render tests. Leaked since `debug_bounds` takes
/// `&'static str` and the key is per-tab, so it cannot be a literal.
#[cfg(test)]
#[must_use]
pub(crate) fn tab_debug_selector_for_test(id: TabId) -> &'static str {
    Box::leak(tab_debug_selector(id).into_boxed_str())
}

/// One tab-bar entry for `tab`, `tab_width` pixels wide, marked active when
/// `active` and closable.
fn render_tab(
    tab: &Tab,
    active: bool,
    tab_width: Pixels,
    theme: &Theme,
    cx: &Context<WorkspaceView>,
) -> impl IntoElement {
    let id = tab.id();
    let is_script = matches!(tab.kind(), TabKind::Script { .. });
    let mut shell = zsql_ui::tabs::tab_shell(active, theme)
        .id(("workspace-tab", id))
        .debug_selector(move || tab_debug_selector(id))
        .w(tab_width);

    shell = match tab.kind() {
        TabKind::Generated { .. } => {
            shell = shell
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(workspace_theme::TAB_ICON_TEXT_SIZE))
                        .text_color(rgb(theme.colors.accent))
                        .child("#"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .italic()
                        .truncate()
                        .child(tab.title().to_owned()),
                );
            if active {
                shell = shell.child(zsql_ui::tabs::active_underline_solid(theme));
            }
            shell
        }
        TabKind::Script { .. } => {
            let mut label = tab.title().to_owned();
            // Only a diverged library- or external-backed tab ever shows
            // the marker: a session-owned tab autosaves continuously, so it
            // is never meaningfully "unsaved" regardless of `tab.dirty()`.
            if tab.diverged(cx) {
                label.push('*');
            }
            shell = shell.child(div().flex_1().min_w_0().truncate().child(label));
            if active {
                shell = shell.child(zsql_ui::tabs::active_underline_solid(theme));
            }
            shell
        }
        TabKind::Schema { .. } => {
            shell = shell
                .child(icon(
                    IconName::Table,
                    px(workspace_theme::TAB_ICON_TEXT_SIZE),
                    theme.colors.accent,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(tab.title().to_owned()),
                );
            if active {
                shell = shell.child(zsql_ui::tabs::active_underline_solid(theme));
            }
            shell
        }
    };

    let mut shell = shell.cursor_pointer().on_click(cx.listener(
        move |view, _event: &ClickEvent, window, cx| {
            view.activate_tab(id, window, cx);
        },
    ));

    if is_script {
        shell = shell.on_mouse_down(
            MouseButton::Right,
            cx.listener(move |view, event: &MouseDownEvent, _window, cx| {
                view.tab_bar.open_context_menu(id, event.position);
                cx.notify();
            }),
        );
    }

    shell.child(
        zsql_ui::tabs::close_glyph(format!("close-icon-{id}"), theme)
            .id(("workspace-tab-close", id))
            .cursor_pointer()
            .on_click(cx.listener(move |view, _event: &ClickEvent, window, cx| {
                cx.stop_propagation();
                view.close_tab(id, window, cx);
            })),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        TAB_CONTEXT_MENU_ITEMS, TAB_CONTEXT_MENU_SEPARATORS_BEFORE, TabMenuAction,
        action_disabled_for_backing, scroll_offset_to_reveal, tab_row_min_width, workspace_theme,
    };
    use crate::session_store::{LibraryName, ScriptBacking, ScriptFileName};

    const TAB_WIDTH: f32 = 160.0;
    const VIEWPORT: f32 = 640.0;

    #[test]
    fn tab_context_menu_items_are_in_the_expected_order() {
        assert_eq!(
            TAB_CONTEXT_MENU_ITEMS,
            [
                TabMenuAction::Save,
                TabMenuAction::SaveAs,
                TabMenuAction::Rename,
                TabMenuAction::CopyToLibrary,
                TabMenuAction::RevealInFiles,
                TabMenuAction::Close,
            ]
        );
    }

    #[test]
    fn tab_context_menu_item_ids_and_labels_carry_their_key_hints() {
        let expected = [
            (
                "tab-context-menu-save",
                format!("Save  {}", workspace_theme::save_shortcut_label()),
            ),
            (
                "tab-context-menu-save-as",
                format!("Save as...  {}", workspace_theme::save_as_shortcut_label()),
            ),
            ("tab-context-menu-rename", "Rename...".to_owned()),
            (
                "tab-context-menu-copy-to-library",
                "Copy to library".to_owned(),
            ),
            (
                "tab-context-menu-reveal-in-files",
                "Reveal in files".to_owned(),
            ),
            ("tab-context-menu-close", "Close".to_owned()),
        ];
        let actual: Vec<(&str, String)> = TAB_CONTEXT_MENU_ITEMS
            .map(|action| (action.item_id(), action.label()))
            .to_vec();
        assert_eq!(actual, expected);
        assert!(
            !actual.iter().any(|(_, label)| label.contains("Ctrl+W")),
            "Close has no bound shortcut and must never show a phantom one"
        );
    }

    #[test]
    fn separators_render_before_the_file_verb_group_and_before_close() {
        assert_eq!(
            TAB_CONTEXT_MENU_ITEMS[TAB_CONTEXT_MENU_SEPARATORS_BEFORE[0]],
            TabMenuAction::CopyToLibrary
        );
        assert_eq!(
            TAB_CONTEXT_MENU_ITEMS[TAB_CONTEXT_MENU_SEPARATORS_BEFORE[1]],
            TabMenuAction::Close
        );
    }

    #[test]
    fn save_is_disabled_only_for_an_already_autosaved_named_session_tab() {
        assert!(
            action_disabled_for_backing(
                TabMenuAction::Save,
                Some(&ScriptBacking::SessionNamed {
                    file: ScriptFileName::new("orders.sql").unwrap()
                })
            ),
            "a named session tab autosaves continuously, so Save is already a no-op"
        );
        assert!(!action_disabled_for_backing(
            TabMenuAction::Save,
            Some(&ScriptBacking::SessionScratch {
                file: ScriptFileName::new("query-1.sql").unwrap()
            })
        ));
        assert!(!action_disabled_for_backing(
            TabMenuAction::Save,
            Some(&ScriptBacking::Library {
                name: LibraryName::new("orders").unwrap(),
                saved_text: None,
            })
        ));
        assert!(!action_disabled_for_backing(
            TabMenuAction::Save,
            Some(&ScriptBacking::External {
                path: std::path::PathBuf::from("/tmp/migrate.sql"),
                saved_text: None,
            })
        ));
    }

    #[test]
    fn rename_is_disabled_only_for_an_external_tab() {
        assert!(action_disabled_for_backing(
            TabMenuAction::Rename,
            Some(&ScriptBacking::External {
                path: std::path::PathBuf::from("/tmp/migrate.sql"),
                saved_text: None,
            })
        ));
        assert!(!action_disabled_for_backing(
            TabMenuAction::Rename,
            Some(&ScriptBacking::SessionNamed {
                file: ScriptFileName::new("orders.sql").unwrap()
            })
        ));
        assert!(!action_disabled_for_backing(
            TabMenuAction::Rename,
            Some(&ScriptBacking::SessionScratch {
                file: ScriptFileName::new("query-1.sql").unwrap()
            })
        ));
        assert!(!action_disabled_for_backing(
            TabMenuAction::Rename,
            Some(&ScriptBacking::Library {
                name: LibraryName::new("orders").unwrap(),
                saved_text: None,
            })
        ));
    }

    #[test]
    fn every_other_action_is_always_enabled_regardless_of_backing() {
        for action in [
            TabMenuAction::SaveAs,
            TabMenuAction::CopyToLibrary,
            TabMenuAction::RevealInFiles,
            TabMenuAction::Close,
        ] {
            assert!(!action_disabled_for_backing(action, None));
            assert!(!action_disabled_for_backing(
                action,
                Some(&ScriptBacking::SessionNamed {
                    file: ScriptFileName::new("orders.sql").unwrap()
                })
            ));
        }
    }

    #[test]
    fn a_tab_already_within_the_viewport_needs_no_scroll() {
        // Index 2 spans [320, 480), comfortably inside a [0, 640) viewport.
        assert_eq!(
            scroll_offset_to_reveal(2, TAB_WIDTH, VIEWPORT, 2_000.0, 0.0),
            None
        );
    }

    #[test]
    fn a_tab_past_the_trailing_edge_scrolls_just_far_enough_to_reveal_its_end() {
        // Index 10 spans [1600, 1760); with a 640-wide viewport starting at
        // 0, only scrolling to 1760 - 640 = 1120 brings its trailing edge
        // into view.
        assert_eq!(
            scroll_offset_to_reveal(10, TAB_WIDTH, VIEWPORT, 2_000.0, 0.0),
            Some(1_120.0)
        );
    }

    #[test]
    fn a_tab_before_the_leading_edge_scrolls_back_to_its_own_start() {
        // Starting scrolled past index 10's tab, index 2 (at x=320) sits
        // before the viewport's current left edge: reveal it by scrolling
        // exactly to its own left edge, not any further.
        assert_eq!(
            scroll_offset_to_reveal(2, TAB_WIDTH, VIEWPORT, 2_000.0, 1_120.0),
            Some(320.0)
        );
    }

    #[test]
    fn the_first_tab_at_a_zero_offset_needs_no_scroll() {
        assert_eq!(
            scroll_offset_to_reveal(0, TAB_WIDTH, VIEWPORT, 2_000.0, 0.0),
            None
        );
    }

    #[test]
    fn a_target_past_the_content_end_clamps_to_max_offset() {
        // A trailing tab whose naive target would overshoot the real
        // scrollable range must clamp to it rather than requesting an
        // offset gpui itself would have to re-clamp.
        assert_eq!(
            scroll_offset_to_reveal(10, TAB_WIDTH, VIEWPORT, 1_000.0, 0.0),
            Some(1_000.0)
        );
    }

    #[test]
    fn tab_row_min_width_sums_every_tabs_width() {
        assert_eq!(
            tab_row_min_width(5, gpui::px(TAB_WIDTH)),
            gpui::px(5.0 * TAB_WIDTH)
        );
    }

    #[test]
    fn tab_row_min_width_is_zero_with_no_tabs() {
        assert_eq!(tab_row_min_width(0, gpui::px(TAB_WIDTH)), gpui::px(0.0));
    }
}
