//! The workspace tab bar's render logic: one entry per open tab, each styled
//! per its kind, plus the trailing "+" affordance that opens a new script
//! tab. The tabs themselves scroll horizontally once they overflow the
//! bar's available width, via `zsql_ui`'s reusable `scrollable` module; the
//! "+" affordance stays fixed at the strip's trailing end, outside the
//! scrolling region.

use std::cell::Cell;

use gpui::{
    ClickEvent, Context, Entity, IntoElement, Pixels, ScrollHandle, div, point, prelude::*, px, rgb,
};
use zsql_ui::icon::{IconName, icon};
use zsql_ui::scrollable::{Axis, ScrollSource, ScrollableState, ScrollbarStyle, WithScrollbars};
use zsql_ui::theme::{ActiveTheme, Theme};

use super::tabs::{Tab, TabId, TabKind, TabModel};
use super::theme as workspace_theme;
use super::workspace::WorkspaceView;

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
        }
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
        .on_scroll_wheel(ScrollableState::wheel_handler(&tab_bar.scroll))
        .child(row);

    let scrolled = viewport.with_scrollbars(&tab_bar.scroll, ScrollbarStyle::default(), cx);

    zsql_ui::tabs::tab_bar_shell(&theme).child(scrolled).child(
        zsql_ui::tabs::new_tab_glyph(&theme)
            .id("workspace-new-tab")
            .cursor_pointer()
            .on_click(cx.listener(|view, _event: &ClickEvent, window, cx| {
                view.open_new_script_tab(window, cx);
            })),
    )
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
        TabKind::Script => {
            let mut label = tab.title().to_owned();
            if tab.dirty() {
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

    shell
        .cursor_pointer()
        .on_click(cx.listener(move |view, _event: &ClickEvent, window, cx| {
            view.activate_tab(id, window, cx);
        }))
        .child(
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
    use super::{scroll_offset_to_reveal, tab_row_min_width};

    const TAB_WIDTH: f32 = 160.0;
    const VIEWPORT: f32 = 640.0;

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
