//! [`WithScrollbars`]: wraps a scroll viewport in the track+thumb overlays
//! its [`ScrollableState`] configures.

use gpui::{
    App, Context, Div, Entity, IntoElement, MouseButton, MouseDownEvent, Render, SharedString,
    Stateful, Window, div, prelude::*, px, rgba,
};

use super::source::Orientation;
use super::state::{AxisSnapshot, ScrollableState};
use super::style::ScrollbarStyle;

/// Wraps a scroll viewport in the track+thumb overlays [`ScrollableState`]'s
/// configured axes call for.
///
/// Implemented for both `Div` and `Stateful<Div>` because a horizontally
/// scrolling container must be `Stateful<Div>` to carry `.track_scroll()`,
/// while a vertically-scrolling wrapper around an inner `uniform_list` (the
/// list itself carries the scroll handle) is often a plain `Div`.
pub trait WithScrollbars: Sized {
    /// Wrap `self` -- the scroll viewport, already carrying whatever
    /// `.track_scroll()`/`uniform_list` wiring the caller needs -- in a
    /// `.relative()` container and hang track/thumb overlays beside it as
    /// siblings, for every axis `state` has configured this render. An axis
    /// whose content already fits its viewport renders nothing for that
    /// axis.
    ///
    /// The overlays are siblings of `self`, never descendants: gpui
    /// translates every descendant of a scroll container (including
    /// `.absolute()` ones) by its scroll offset during prepaint, so a
    /// scrollbar nested inside the scrolling element would be dragged off
    /// the viewport's edge as soon as the content scrolls.
    ///
    /// The returned wrapper fills a column-direction flex parent in both
    /// axes, the way `self` would have on its own; a caller embedding it in
    /// a row-direction or fixed-size layout must account for that.
    ///
    /// For a horizontal [`super::ScrollSource::Container`] axis, the caller
    /// must apply three things `with_scrollbars` cannot apply for them --
    /// by the time it receives `self`, the scrolled child is already sealed
    /// inside it, and `content_extent` is per-axis configuration on
    /// [`ScrollableState`], not a property `self` exposes:
    ///
    /// - `.min_w_0()` on `self`, or flexbox refuses to shrink the viewport
    ///   below its content's intrinsic width and there is nothing to scroll.
    /// - `.min_w(px(content_extent))` on the viewport's immediate scrolled
    ///   child -- the element whose true width that axis's `content_extent`
    ///   describes -- so it lays out at its full width inside the viewport.
    /// - `.overflow_x_hidden()` on `self`. gpui applies a content mask only
    ///   when at least one overflow axis is non-`Visible`, so without it the
    ///   overflowing content paints straight past the viewport's edge:
    ///   `.track_scroll()` alone moves content but never clips it.
    ///
    /// Only a thumb's own mouse-down (starting a drag) is attached here.
    /// The drag's move/up tracking is not -- see
    /// [`ScrollableState::drag_handlers`] for why, and where to attach it.
    ///
    /// Schedules at most one extra re-render of the caller's own view when a
    /// configured axis's viewport has not been through a layout pass yet
    /// (its bounds still read back as zero), so a scrollbar for content that
    /// overflows on the very first frame it appears still shows up on the
    /// next frame instead of staying hidden until unrelated input forces a
    /// repaint.
    fn with_scrollbars<V: Render>(
        self,
        state: &Entity<ScrollableState>,
        style: ScrollbarStyle,
        cx: &mut Context<V>,
    ) -> Div;
}

impl WithScrollbars for Div {
    fn with_scrollbars<V: Render>(
        self,
        state: &Entity<ScrollableState>,
        style: ScrollbarStyle,
        cx: &mut Context<V>,
    ) -> Div {
        build(self, state, style, cx)
    }
}

impl WithScrollbars for Stateful<Div> {
    fn with_scrollbars<V: Render>(
        self,
        state: &Entity<ScrollableState>,
        style: ScrollbarStyle,
        cx: &mut Context<V>,
    ) -> Div {
        build(self, state, style, cx)
    }
}

fn build<V: Render>(
    content: impl IntoElement,
    state: &Entity<ScrollableState>,
    style: ScrollbarStyle,
    cx: &mut Context<V>,
) -> Div {
    state.update(cx, |scrollable, _cx| scrollable.set_style(style));
    nudge_when_unmeasured(state, cx);

    let snapshot = state.read(cx).snapshot();
    let vertical_visible = snapshot.vertical.is_some_and(|axis| axis.geometry.visible);

    let mut wrapper = div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .h_full()
        .child(content.into_any_element());

    if let Some(axis) = snapshot.vertical.filter(|axis| axis.geometry.visible) {
        wrapper = wrapper.child(vertical_track(axis, &style, state));
    }
    if let Some(axis) = snapshot.horizontal.filter(|axis| axis.geometry.visible) {
        wrapper = wrapper.child(horizontal_track(axis, &style, vertical_visible, state));
    }

    wrapper
}

/// Schedule at most one outstanding re-render of the caller's own view while
/// a configured axis's viewport bounds still read back as zero: a scroll
/// container's bounds are only known after the render that first lays it
/// out, so a scrollbar for overflowing content would otherwise stay hidden
/// until something unrelated forces a repaint. `ScrollableState` tracks
/// whether a nudge is already pending so this never schedules a second one
/// on top of it, and clears that flag once every configured axis is
/// measured -- an axis that is legitimately, permanently zero-extent (a
/// collapsed pane) gets exactly one nudge and then settles, rather than
/// spinning notify -> render -> notify forever.
///
/// `request_animation_frame` cannot do this job -- it only queues a
/// callback without forcing a draw, so on an otherwise idle window it never
/// fires.
fn nudge_when_unmeasured<V: Render>(state: &Entity<ScrollableState>, cx: &mut Context<V>) {
    let should_schedule = state.update(cx, ScrollableState::should_schedule_nudge);
    if !should_schedule {
        return;
    }
    cx.spawn(async move |this, cx| {
        this.update(cx, |_, cx| cx.notify()).ok();
    })
    .detach();
}

/// A thin track + draggable thumb pinned to the right edge of the viewport.
fn vertical_track(
    axis: AxisSnapshot,
    style: &ScrollbarStyle,
    state: &Entity<ScrollableState>,
) -> Div {
    let thumb_top = axis.geometry.thumb_offset(axis.track_length);

    let mut thumb = div()
        .id(thumb_id(state, "v"))
        .absolute()
        .top(px(thumb_top))
        .right(px(0.0))
        .w(px(style.track_width))
        .h(px(axis.geometry.thumb_length))
        .rounded(px(style.radius))
        .bg(rgba(style.thumb_color))
        .on_mouse_down(MouseButton::Left, start_drag(state, Orientation::Vertical));
    if let Some(hover_color) = style.thumb_hover_color {
        thumb = thumb.hover(move |el| el.bg(rgba(hover_color)));
    }
    thumb = debug_tag(thumb, state, "v");

    let mut track = div()
        .absolute()
        .top(px(axis.track_start))
        .right(px(style.inset))
        .bottom(px(0.0))
        .w(px(style.track_width));
    if let Some(track_color) = style.track_color {
        track = track.bg(rgba(track_color));
    }
    track.child(thumb)
}

/// A thin track + draggable thumb pinned to the bottom edge of the
/// viewport. `vertical_visible` reserves room at the track's far end --
/// `axis.track_length` already excludes it (see
/// [`ScrollableState::snapshot`]), so the thumb's own length and position
/// are scaled to the same shortened track the track div is painted at,
/// and neither ever runs underneath a simultaneously-visible vertical
/// scrollbar.
fn horizontal_track(
    axis: AxisSnapshot,
    style: &ScrollbarStyle,
    vertical_visible: bool,
    state: &Entity<ScrollableState>,
) -> Div {
    let thumb_left = axis.geometry.thumb_offset(axis.track_length);
    let right_reserve = style.horizontal_reserve(vertical_visible);

    let mut thumb = div()
        .id(thumb_id(state, "h"))
        .absolute()
        .left(px(thumb_left))
        .bottom(px(0.0))
        .w(px(axis.geometry.thumb_length))
        .h(px(style.track_width))
        .rounded(px(style.radius))
        .bg(rgba(style.thumb_color))
        .on_mouse_down(
            MouseButton::Left,
            start_drag(state, Orientation::Horizontal),
        );
    if let Some(hover_color) = style.thumb_hover_color {
        thumb = thumb.hover(move |el| el.bg(rgba(hover_color)));
    }
    thumb = debug_tag(thumb, state, "h");

    let mut track = div()
        .absolute()
        .left(px(axis.track_start))
        .right(px(right_reserve))
        .bottom(px(style.inset))
        .h(px(style.track_width));
    if let Some(track_color) = style.track_color {
        track = track.bg(rgba(track_color));
    }
    track.child(thumb)
}

fn thumb_id(state: &Entity<ScrollableState>, axis: &str) -> SharedString {
    SharedString::from(format!(
        "zsql-ui-scrollbar-thumb-{axis}-{}",
        state.entity_id()
    ))
}

/// Tags a thumb with a lookup key for `VisualTestContext::debug_bounds`, so
/// render tests can assert its painted position without reaching into
/// gpui's own layout internals.
#[cfg(any(test, feature = "test-support"))]
fn debug_tag(thumb: Stateful<Div>, state: &Entity<ScrollableState>, axis: &str) -> Stateful<Div> {
    let selector = thumb_id(state, axis).to_string();
    thumb.debug_selector(move || selector.clone())
}

/// A no-op outside test builds: gpui's own `debug_selector` already
/// discards its argument there, but the selector string built above would
/// still be allocated on every render without this split, since the
/// allocation happens before `debug_selector` ever sees it.
#[cfg(not(any(test, feature = "test-support")))]
fn debug_tag(thumb: Stateful<Div>, _state: &Entity<ScrollableState>, _axis: &str) -> Stateful<Div> {
    thumb
}

/// The `VisualTestContext::debug_bounds` lookup key for `state`'s vertical
/// scrollbar thumb, for a consumer crate's own render tests. Requires this
/// crate's `test-support` feature (or building this crate's own tests).
///
/// The returned `&'static str` is deliberately leaked:
/// `VisualTestContext::debug_bounds` takes `&'static str`, and the key is
/// per-entity so it cannot be a literal. Test-support builds only, and one
/// small leak per call.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn vertical_thumb_debug_selector(state: &Entity<ScrollableState>) -> &'static str {
    Box::leak(thumb_id(state, "v").to_string().into_boxed_str())
}

/// The `VisualTestContext::debug_bounds` lookup key for `state`'s horizontal
/// scrollbar thumb, for a consumer crate's own render tests. Requires this
/// crate's `test-support` feature (or building this crate's own tests).
///
/// The returned `&'static str` is deliberately leaked, for the same reason
/// as [`vertical_thumb_debug_selector`].
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn horizontal_thumb_debug_selector(state: &Entity<ScrollableState>) -> &'static str {
    Box::leak(thumb_id(state, "h").to_string().into_boxed_str())
}

fn start_drag(
    state: &Entity<ScrollableState>,
    orientation: Orientation,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    let state = state.clone();
    move |event: &MouseDownEvent, _window: &mut Window, cx: &mut App| {
        let pointer = match orientation {
            Orientation::Vertical => f32::from(event.position.y),
            Orientation::Horizontal => f32::from(event.position.x),
        };
        state.update(cx, |scrollable, cx| {
            scrollable.start_drag(orientation, pointer);
            cx.notify();
        });
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        Context, Div, Entity, IntoElement, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
        MouseUpEvent, Render, ScrollDelta, ScrollHandle, ScrollWheelEvent, TestAppContext,
        TouchPhase, UniformListScrollHandle, Window, div, point, prelude::*, px, uniform_list,
    };

    use super::{ScrollableState, WithScrollbars};
    use crate::scrollable::{
        Axis, ScrollSource, ScrollbarStyle, horizontal_thumb_debug_selector,
        vertical_thumb_debug_selector,
    };

    const ROW_HEIGHT: f32 = 20.0;
    /// The fixed footprint the harness constrains its scrollable pane to,
    /// deliberately much smaller than the test window (1920x1080): large
    /// enough to leave room for a draggable thumb, small enough that a drag
    /// dispatched well outside it exercises root- vs wrapper-level
    /// attachment.
    const PANE_SIZE: f32 = 220.0;

    /// A minimal view exercising `with_scrollbars` with a configurable
    /// vertical row count and/or horizontal content width, for tests that
    /// only care about the abstraction's own behavior.
    struct Harness {
        scroll: Entity<ScrollableState>,
        list_handle: UniformListScrollHandle,
        container_handle: ScrollHandle,
        vertical_enabled: bool,
        horizontal_enabled: bool,
        row_count: usize,
        content_width: f32,
        style: ScrollbarStyle,
        /// When set, overrides the vertical axis's `content_extent` in place
        /// of `row_count * ROW_HEIGHT`, without changing `row_count` itself
        /// -- so a test can shrink what the axis reports as scrollable
        /// while leaving the real `uniform_list`'s own row count (and thus
        /// its own internally-clamped offset) untouched.
        vertical_content_extent_override: Option<f32>,
        /// Offsets the vertical axis's track, standing in for a caller whose
        /// scrolling viewport starts below a header row.
        vertical_track_start: f32,
        /// Backs the vertical axis with the container handle instead of the
        /// uniform list's, so the abstraction can be exercised against a
        /// `ScrollSource::Container` on the axis it does not usually take.
        vertical_uses_container: bool,
        /// How many times `render` has run, so a test can tell whether the
        /// first-frame nudge settles after one extra render or keeps
        /// rescheduling itself.
        render_count: usize,
    }

    impl Harness {
        fn new(
            vertical_enabled: bool,
            horizontal_enabled: bool,
            row_count: usize,
            content_width: f32,
            cx: &mut Context<Self>,
        ) -> Self {
            Self::with_style(
                vertical_enabled,
                horizontal_enabled,
                row_count,
                content_width,
                ScrollbarStyle::default(),
                cx,
            )
        }

        fn with_style(
            vertical_enabled: bool,
            horizontal_enabled: bool,
            row_count: usize,
            content_width: f32,
            style: ScrollbarStyle,
            cx: &mut Context<Self>,
        ) -> Self {
            Self {
                scroll: cx.new(ScrollableState::new),
                list_handle: UniformListScrollHandle::new(),
                container_handle: ScrollHandle::new(),
                vertical_enabled,
                horizontal_enabled,
                row_count,
                content_width,
                style,
                vertical_content_extent_override: None,
                vertical_track_start: 0.0,
                vertical_uses_container: false,
                render_count: 0,
            }
        }

        /// Point `scroll`'s axes at this render's handles and extents,
        /// clearing whichever axis this harness has disabled.
        fn configure_axes(&self, vertical_content_extent: f32, cx: &mut Context<Self>) {
            let vertical_enabled = self.vertical_enabled;
            let horizontal_enabled = self.horizontal_enabled;
            let content_width = self.content_width;
            let container_handle = self.container_handle.clone();
            let vertical_track_start = self.vertical_track_start;
            let vertical_source = if self.vertical_uses_container {
                ScrollSource::Container(container_handle.clone())
            } else {
                ScrollSource::UniformList(self.list_handle.clone())
            };

            self.scroll.update(cx, |state, _cx| {
                if vertical_enabled {
                    state.vertical(
                        Axis::new(vertical_source, vertical_content_extent)
                            .track_start(vertical_track_start),
                    );
                } else {
                    state.clear_vertical();
                }
                if horizontal_enabled {
                    state.horizontal(Axis::new(
                        ScrollSource::Container(container_handle),
                        content_width,
                    ));
                } else {
                    state.clear_horizontal();
                }
            });
        }
    }

    impl Render for Harness {
        // Row counts in these tests are always small (at most a few
        // hundred), far below where an f32 conversion could lose precision.
        #[allow(clippy::cast_precision_loss)]
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.render_count += 1;
            let horizontal_enabled = self.horizontal_enabled;
            let row_count = self.row_count;
            let content_width = self.content_width;
            let style = self.style;
            let vertical_content_extent = self
                .vertical_content_extent_override
                .unwrap_or(row_count as f32 * ROW_HEIGHT);

            self.configure_axes(vertical_content_extent, cx);

            let list = uniform_list(
                "harness-rows",
                row_count,
                cx.processor(|_this, range: std::ops::Range<usize>, _window, _cx| {
                    range
                        .map(|ix| div().h(px(ROW_HEIGHT)).child(ix.to_string()))
                        .collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .track_scroll(self.list_handle.clone());

            let wrapped: Div = if self.vertical_uses_container {
                // A container-backed vertical axis: the scrolled child needs
                // a definite height, since a flex child's min-height:auto
                // resolves to zero on the cross axis and the container would
                // never see content taller than itself.
                let inner = div()
                    .id("harness-v-container")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .h_full()
                    .overflow_y_hidden()
                    .track_scroll(&self.container_handle)
                    // flex_shrink_0 as well as a definite height: a flex
                    // child shrinks on the main axis by default, so without
                    // it the child collapses to the viewport and the
                    // container never sees anything to scroll.
                    .child(
                        div()
                            .h(px(vertical_content_extent))
                            .w_full()
                            .flex_shrink_0(),
                    );
                inner.with_scrollbars(&self.scroll, style, cx)
            } else if horizontal_enabled {
                let inner = div()
                    .id("harness-h-scroll")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .h_full()
                    .overflow_x_hidden()
                    .track_scroll(&self.container_handle)
                    .on_scroll_wheel(ScrollableState::wheel_handler(&self.scroll))
                    .child(
                        div()
                            .min_w(px(content_width))
                            .flex()
                            .flex_col()
                            .flex_1()
                            .child(list),
                    );
                inner.with_scrollbars(&self.scroll, style, cx)
            } else {
                let inner = div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .h_full()
                    .child(list);
                inner.with_scrollbars(&self.scroll, style, cx)
            };

            let handlers = ScrollableState::drag_handlers(&self.scroll);

            div()
                .size_full()
                .on_mouse_move(handlers.on_move)
                .on_mouse_up(MouseButton::Left, handlers.on_up)
                .on_mouse_up_out(MouseButton::Left, handlers.on_up_out)
                .child(
                    div()
                        .w(px(PANE_SIZE))
                        .h(px(PANE_SIZE))
                        .flex()
                        .flex_col()
                        .child(wrapped),
                )
        }
    }

    // -- Spike: drag tracking must survive the pointer leaving the wrapper --

    #[gpui::test]
    fn drag_started_on_the_thumb_keeps_tracking_after_the_pointer_leaves_the_wrapper(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, false, 400, 0.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        let selector = vertical_thumb_debug_selector(&scroll);
        let thumb_bounds = vcx
            .debug_bounds(selector)
            .expect("the vertical thumb must be painted once the viewport is measured");
        let thumb_center = thumb_bounds.center();

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: thumb_center,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });

        // Far outside both the thumb's own hit region and the harness's
        // 220x220 wrapper, but still inside the (1920x1080) test window:
        // root-level attachment must still see this as part of the drag.
        let far_outside = point(px(1500.0), px(900.0));
        vcx.simulate_event(MouseMoveEvent {
            position: far_outside,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        vcx.run_until_parked();

        let offset_after_move = harness.read_with(vcx, |h, _app| {
            h.list_handle.0.borrow().base_handle.offset().y
        });
        assert!(
            offset_after_move < px(0.0),
            "a drag started on the thumb must keep tracking pointer movement that leaves the \
             with_scrollbars wrapper's own (much smaller) bounds -- move/up handlers are \
             attached at the caller's own view root via ScrollableState::drag_handlers, not on \
             the wrapper itself, exactly because gpui's on_mouse_move only fires while the \
             pointer is within the registering element's own hit-tested bounds"
        );

        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: far_outside,
            modifiers: Modifiers::default(),
            click_count: 1,
        });
    }

    #[gpui::test]
    fn a_thumb_drag_ends_on_mouse_up_and_stops_tracking_further_pointer_movement(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, false, 400, 0.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        let selector = vertical_thumb_debug_selector(&scroll);
        let thumb_center = vcx
            .debug_bounds(selector)
            .expect("the vertical thumb must be painted once the viewport is measured")
            .center();

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: thumb_center,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        let dragged_to = point(px(1500.0), px(900.0));
        vcx.simulate_event(MouseMoveEvent {
            position: dragged_to,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        vcx.run_until_parked();

        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: dragged_to,
            modifiers: Modifiers::default(),
            click_count: 1,
        });
        vcx.run_until_parked();
        let offset_after_release = harness.read_with(vcx, |h, _app| {
            h.list_handle.0.borrow().base_handle.offset().y
        });

        // A further move with no button held must not be mistaken for a
        // continuing drag: if `end_drag` (or the up/up-out listeners
        // invoking it) failed to clear the drag state, this would keep
        // scrolling the list even though the button was released.
        vcx.simulate_event(MouseMoveEvent {
            position: point(px(10.0), px(10.0)),
            pressed_button: None,
            modifiers: Modifiers::default(),
        });
        vcx.run_until_parked();
        let offset_after_extra_move = harness.read_with(vcx, |h, _app| {
            h.list_handle.0.borrow().base_handle.offset().y
        });

        assert_eq!(
            offset_after_extra_move, offset_after_release,
            "a drag must stop tracking the pointer once it has ended on mouse-up"
        );
    }

    #[gpui::test]
    fn dragging_the_horizontal_thumb_moves_the_horizontal_offset_when_both_axes_are_configured(
        cx: &mut TestAppContext,
    ) {
        // Both axes configured together is the one case that exercises the
        // horizontal track's `right_reserve` arithmetic, which shortens the
        // track to leave room for the simultaneously-visible vertical one.
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, true, 400, 2_000.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        let selector = horizontal_thumb_debug_selector(&scroll);
        let thumb_center = vcx
            .debug_bounds(selector)
            .expect("the horizontal thumb must be painted once the viewport is measured")
            .center();

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: thumb_center,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        let dragged_to = point(thumb_center.x + px(150.0), thumb_center.y);
        vcx.simulate_event(MouseMoveEvent {
            position: dragged_to,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        vcx.run_until_parked();

        let container_offset_x = harness.read_with(vcx, |h, _app| h.container_handle.offset().x);
        assert!(
            container_offset_x < px(0.0),
            "dragging the horizontal thumb to the right must move the horizontal offset"
        );

        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: dragged_to,
            modifiers: Modifiers::default(),
            click_count: 1,
        });
    }

    // -- Scrollbars are siblings of the scrolled content, not descendants --

    #[gpui::test]
    fn the_scrollbar_thumb_is_a_sibling_of_the_scrolled_content_not_a_descendant(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, false, 400, 0.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        let selector = vertical_thumb_debug_selector(&scroll);
        let thumb_bounds_before = vcx
            .debug_bounds(selector)
            .expect("the thumb must be painted once measured");

        // A scroll deep enough that a mistakenly-descendant thumb (dragged
        // along by gpui's scroll-offset translation of every descendant)
        // would be painted thousands of pixels outside the pane.
        harness.update(vcx, |h, cx| {
            h.list_handle
                .0
                .borrow()
                .base_handle
                .set_offset(point(px(0.0), px(-3000.0)));
            // Setting a handle's offset does not itself mark the view dirty,
            // so without this the window never repaints and the assertions
            // below would compare the first frame against itself.
            cx.notify();
        });
        vcx.run_until_parked();

        let thumb_bounds_after = vcx
            .debug_bounds(selector)
            .expect("the thumb must still be painted after scrolling");
        let shift = (thumb_bounds_after.origin.y - thumb_bounds_before.origin.y).abs();
        assert!(
            shift > px(0.0),
            "the thumb's painted position must track the scroll offset (got no shift at all), \
             or the overlay is not reading the handle's live offset"
        );
        assert!(
            shift < px(PANE_SIZE),
            "the thumb's painted position must only move by its own bounded travel within the \
             {PANE_SIZE}px pane (got a {shift:?} shift), not by the raw 3000px scroll offset -- \
             a shift anywhere near the scroll delta would mean the thumb is a descendant of the \
             scrolled content and got dragged along with it during prepaint"
        );
    }

    // -- Independent axes --

    #[gpui::test]
    fn a_vertical_only_axis_shows_only_the_vertical_scrollbar(cx: &mut TestAppContext) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, false, 400, 0.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        assert!(
            vcx.debug_bounds(vertical_thumb_debug_selector(&scroll))
                .is_some(),
            "overflowing rows must show the vertical scrollbar"
        );
        assert!(
            vcx.debug_bounds(horizontal_thumb_debug_selector(&scroll))
                .is_none(),
            "a vertical-only axis must never render a horizontal scrollbar"
        );
    }

    #[gpui::test]
    fn a_horizontal_only_axis_shows_only_the_horizontal_scrollbar_and_actually_scrolls(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(false, true, 0, 2_000.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        assert!(
            vcx.debug_bounds(horizontal_thumb_debug_selector(&scroll))
                .is_some(),
            "content wider than the pane must show the horizontal scrollbar"
        );
        assert!(
            vcx.debug_bounds(vertical_thumb_debug_selector(&scroll))
                .is_none(),
            "a horizontal-only axis must never render a vertical scrollbar"
        );

        let container_handle = harness.read_with(vcx, |h, _app| h.container_handle.clone());
        assert!(
            container_handle.max_offset().width > px(0.0),
            "content wider than the viewport must leave the pane actually scrollable, not just \
             visually overflowing"
        );
    }

    #[gpui::test]
    fn both_axes_configured_together_scroll_independently(cx: &mut TestAppContext) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, true, 400, 2_000.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        assert!(
            vcx.debug_bounds(vertical_thumb_debug_selector(&scroll))
                .is_some(),
            "both-axis overflow must show the vertical scrollbar"
        );
        assert!(
            vcx.debug_bounds(horizontal_thumb_debug_selector(&scroll))
                .is_some(),
            "both-axis overflow must show the horizontal scrollbar"
        );

        harness.update(vcx, |h, cx| {
            h.list_handle
                .0
                .borrow()
                .base_handle
                .set_offset(point(px(0.0), px(-500.0)));
            // Forces the repaint that would expose a snapshot or
            // set_scroll_offset path clobbering the other axis.
            cx.notify();
        });
        vcx.run_until_parked();

        let horizontal_offset_x = harness.read_with(vcx, |h, _app| h.container_handle.offset().x);
        assert_eq!(
            horizontal_offset_x,
            px(0.0),
            "driving the vertical axis must leave the horizontal axis's offset untouched"
        );
    }

    #[gpui::test]
    fn content_that_fits_the_pane_renders_no_scrollbar(cx: &mut TestAppContext) {
        // 3 rows at ROW_HEIGHT easily fit inside the PANE_SIZE viewport.
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, false, 3, 0.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        scroll.read_with(vcx, |state, _app| {
            let snapshot = state.snapshot();
            assert!(
                !snapshot
                    .vertical
                    .expect("the vertical axis is configured")
                    .geometry
                    .visible,
                "content that fits the viewport must compute a hidden scrollbar geometry"
            );
        });
        assert!(
            vcx.debug_bounds(vertical_thumb_debug_selector(&scroll))
                .is_none(),
            "content that fits the viewport must render no scrollbar element at all"
        );
    }

    // -- First-frame nudge --

    /// `TestAppContext::add_window_view` already drains the scheduler
    /// (`run_until_parked`) once before returning, so the nudge's one extra
    /// re-render has already resolved by the time a test observes the
    /// view -- mirroring `results.rs`'s/`sidebar.rs`'s own equivalent
    /// regression tests, which likewise only assert presence after parking
    /// rather than absence beforehand (not externally observable through
    /// this harness). A hung `run_until_parked` here would be the signature
    /// of a nudge that keeps rescheduling itself instead of settling.
    #[gpui::test]
    fn the_scrollbar_appears_after_the_first_frame_once_the_viewport_is_measured(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, false, 400, 0.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        let selector = vertical_thumb_debug_selector(&scroll);
        assert!(
            vcx.debug_bounds(selector).is_some(),
            "once the viewport is measured, the scrollbar must appear without any further user \
             input"
        );

        // Parking again must be a no-op: the condition self-clears once
        // every configured axis is measured, so this must not schedule
        // another nudge or otherwise change anything.
        vcx.run_until_parked();
        assert!(
            vcx.debug_bounds(selector).is_some(),
            "the scrollbar must remain visible after a second, unrelated parking pass"
        );
    }

    #[gpui::test]
    fn the_first_frame_nudge_settles_instead_of_rescheduling_itself(cx: &mut TestAppContext) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, false, 400, 0.0, cx));
        vcx.run_until_parked();

        let render_count_once_measured = harness.read_with(vcx, |h, _app| h.render_count);

        // Parking again with nothing new to measure must not trigger another
        // render: a nudge that kept rescheduling itself instead of settling
        // would grow this count on every extra park.
        vcx.run_until_parked();
        vcx.run_until_parked();
        let render_count_after_idle_parking = harness.read_with(vcx, |h, _app| h.render_count);

        assert_eq!(
            render_count_after_idle_parking, render_count_once_measured,
            "once every configured axis is measured, idle parking must not trigger any further \
             renders -- the first-frame nudge must schedule at most one, not keep rescheduling"
        );
    }

    // -- Independent axes: horizontal_visible mirrors vertical_visible --

    #[gpui::test]
    fn horizontal_visible_is_true_once_content_overflows_the_pane(cx: &mut TestAppContext) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(false, true, 0, 2_000.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        assert!(
            scroll.read_with(vcx, |state, _app| state.horizontal_visible()),
            "content wider than the pane must report the horizontal axis as visible"
        );
    }

    #[gpui::test]
    fn horizontal_visible_is_false_once_content_fits_the_pane(cx: &mut TestAppContext) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(false, true, 0, 10.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        assert!(
            !scroll.read_with(vcx, |state, _app| state.horizontal_visible()),
            "content narrower than the pane must report the horizontal axis as not visible"
        );
    }

    // -- Wheel handling --

    #[gpui::test]
    fn shift_held_wheel_scrolls_the_horizontal_axis(cx: &mut TestAppContext) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(false, true, 0, 2_000.0, cx));
        vcx.run_until_parked();

        vcx.simulate_event(ScrollWheelEvent {
            position: point(px(50.0), px(50.0)),
            delta: ScrollDelta::Pixels(point(px(-40.0), px(0.0))),
            modifiers: Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            touch_phase: TouchPhase::Moved,
        });
        vcx.run_until_parked();

        let container_handle = harness.read_with(vcx, |h, _app| h.container_handle.clone());
        assert!(
            container_handle.offset().x < px(0.0),
            "a shift-held wheel event must move the horizontal axis's offset"
        );
    }

    #[gpui::test]
    fn a_plain_wheel_event_does_not_move_the_horizontal_axis(cx: &mut TestAppContext) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(false, true, 0, 2_000.0, cx));
        vcx.run_until_parked();

        vcx.simulate_event(ScrollWheelEvent {
            position: point(px(50.0), px(50.0)),
            delta: ScrollDelta::Pixels(point(px(-40.0), px(0.0))),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        });
        vcx.run_until_parked();

        let container_handle = harness.read_with(vcx, |h, _app| h.container_handle.clone());
        assert_eq!(
            container_handle.offset().x,
            px(0.0),
            "a plain (non-shift) wheel event over a horizontal axis must not move it -- it is \
             left entirely to the element's own native vertical scroll handling"
        );
    }

    #[gpui::test]
    fn on_scroll_wheel_is_a_noop_when_no_horizontal_axis_is_configured(cx: &mut TestAppContext) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, false, 400, 0.0, cx));
        vcx.run_until_parked();

        let event = ScrollWheelEvent {
            position: point(px(50.0), px(50.0)),
            delta: ScrollDelta::Pixels(point(px(-40.0), px(0.0))),
            modifiers: Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            touch_phase: TouchPhase::Moved,
        };

        let changed = harness.update_in(vcx, |h, window, cx| {
            h.scroll
                .update(cx, |state, _cx| state.on_scroll_wheel(&event, window))
        });

        assert!(
            !changed,
            "a shift-held wheel event must be a no-op when the state has no horizontal axis \
             configured"
        );
    }

    #[gpui::test]
    fn shift_held_wheel_falls_back_to_the_vertical_delta_component_when_horizontal_is_zero(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(false, true, 0, 2_000.0, cx));
        vcx.run_until_parked();

        vcx.simulate_event(ScrollWheelEvent {
            position: point(px(50.0), px(50.0)),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-40.0))),
            modifiers: Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            touch_phase: TouchPhase::Moved,
        });
        vcx.run_until_parked();

        let container_handle = harness.read_with(vcx, |h, _app| h.container_handle.clone());
        assert!(
            container_handle.offset().x < px(0.0),
            "a shift-held wheel event whose horizontal delta component is zero must fall back \
             to the vertical component, through the real ScrollableState::on_scroll_wheel entry \
             point rather than just the pure horizontal_wheel_delta helper"
        );
    }

    #[gpui::test]
    fn repeated_shift_wheel_events_pin_the_horizontal_offset_at_the_content_end(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(false, true, 0, 2_000.0, cx));
        vcx.run_until_parked();

        for _ in 0..50 {
            vcx.simulate_event(ScrollWheelEvent {
                position: point(px(50.0), px(50.0)),
                delta: ScrollDelta::Pixels(point(px(-400.0), px(0.0))),
                modifiers: Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
                touch_phase: TouchPhase::Moved,
            });
        }
        vcx.run_until_parked();

        let container_handle = harness.read_with(vcx, |h, _app| h.container_handle.clone());
        let max_offset = container_handle.max_offset().width;
        assert_eq!(
            -container_handle.offset().x,
            max_offset,
            "scrolling far past the content's end must pin the offset at content_extent - \
             viewport_extent, never overshoot it"
        );
    }

    // -- Style: a non-default ScrollbarStyle actually reaches the painted thumb --

    #[gpui::test]
    fn a_style_with_a_hover_color_and_a_nonzero_inset_paints_the_thumb_shifted_by_the_inset(
        cx: &mut TestAppContext,
    ) {
        let (default_harness, default_vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, false, 400, 0.0, cx));
        default_vcx.run_until_parked();
        let default_scroll = default_harness.read_with(default_vcx, |h, _app| h.scroll.clone());
        let default_x = default_vcx
            .debug_bounds(vertical_thumb_debug_selector(&default_scroll))
            .expect("the default-style thumb must be painted")
            .origin
            .x;

        let inset = 6.0;
        let custom_style = ScrollbarStyle {
            inset,
            thumb_hover_color: Some(0x11_22_33_ff),
            ..ScrollbarStyle::default()
        };
        let (custom_harness, custom_vcx) = cx.add_window_view(|_window, cx| {
            Harness::with_style(true, false, 400, 0.0, custom_style, cx)
        });
        custom_vcx.run_until_parked();
        let custom_scroll = custom_harness.read_with(custom_vcx, |h, _app| h.scroll.clone());
        let custom_x = custom_vcx
            .debug_bounds(vertical_thumb_debug_selector(&custom_scroll))
            .expect(
                "the custom-style thumb (with a hover color also configured) must still be \
                     painted",
            )
            .origin
            .x;

        assert_eq!(
            default_x - custom_x,
            px(inset),
            "a non-zero ScrollbarStyle::inset must shift the thumb left by exactly that many \
             pixels from where the default (zero-inset) style paints it"
        );
    }

    #[gpui::test]
    fn a_style_with_a_custom_track_width_paints_the_thumb_at_that_width(cx: &mut TestAppContext) {
        let track_width = 17.0;
        let style = ScrollbarStyle {
            track_width,
            ..ScrollbarStyle::default()
        };
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::with_style(true, false, 400, 0.0, style, cx));
        vcx.run_until_parked();
        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());

        let painted = vcx
            .debug_bounds(vertical_thumb_debug_selector(&scroll))
            .expect("the custom-width thumb must be painted");
        assert_eq!(
            painted.size.width,
            px(track_width),
            "ScrollbarStyle::track_width must reach the painted thumb, not just the struct"
        );
    }

    #[gpui::test]
    fn a_style_with_a_custom_min_thumb_length_floors_the_painted_thumb(cx: &mut TestAppContext) {
        let min_thumb_length = 64.0;
        let style = ScrollbarStyle {
            min_thumb_length,
            ..ScrollbarStyle::default()
        };
        // A row count this far past the pane's height drives the
        // proportional thumb length well below the floor, so the painted
        // height can only come from `min_thumb_length`.
        let (harness, vcx) = cx
            .add_window_view(|_window, cx| Harness::with_style(true, false, 5000, 0.0, style, cx));
        vcx.run_until_parked();
        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());

        let painted = vcx
            .debug_bounds(vertical_thumb_debug_selector(&scroll))
            .expect("the floored thumb must be painted");
        assert_eq!(
            painted.size.height,
            px(min_thumb_length),
            "ScrollbarStyle::min_thumb_length must floor the painted thumb's length so it stays \
             grabbable on very large content"
        );
    }

    #[gpui::test]
    fn a_vertical_axis_track_start_offsets_the_painted_track_without_moving_the_thumb_within_it(
        cx: &mut TestAppContext,
    ) {
        let (flush_harness, flush_vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, false, 400, 0.0, cx));
        flush_vcx.run_until_parked();
        let flush_scroll = flush_harness.read_with(flush_vcx, |h, _app| h.scroll.clone());
        let flush_top = flush_vcx
            .debug_bounds(vertical_thumb_debug_selector(&flush_scroll))
            .expect("the zero-track_start thumb must be painted")
            .origin
            .y;

        let track_start = 30.0;
        let (offset_harness, offset_vcx) = cx.add_window_view(|_window, cx| {
            let mut harness = Harness::new(true, false, 400, 0.0, cx);
            harness.vertical_track_start = track_start;
            harness
        });
        offset_vcx.run_until_parked();
        let offset_scroll = offset_harness.read_with(offset_vcx, |h, _app| h.scroll.clone());
        let offset_top = offset_vcx
            .debug_bounds(vertical_thumb_debug_selector(&offset_scroll))
            .expect("the offset-track_start thumb must be painted")
            .origin
            .y;

        assert_eq!(
            offset_top - flush_top,
            px(track_start),
            "Axis::track_start must push the whole track (and the thumb sitting at its origin) \
             down by exactly that many pixels, so a viewport that starts below a header row \
             keeps its thumb aligned with the content it scrolls"
        );
    }

    #[gpui::test]
    fn a_container_backed_vertical_axis_paints_and_drags_like_a_uniform_list_one(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) = cx.add_window_view(|_window, cx| {
            let mut harness = Harness::new(true, false, 400, 0.0, cx);
            harness.vertical_uses_container = true;
            harness
        });
        vcx.run_until_parked();
        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());

        let selector = vertical_thumb_debug_selector(&scroll);
        let thumb_before = vcx
            .debug_bounds(selector)
            .expect("a container-backed vertical axis must paint a thumb when content overflows");

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: thumb_before.center(),
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(MouseMoveEvent {
            position: thumb_before.center() + point(px(0.0), px(40.0)),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        vcx.run_until_parked();

        let container_handle = harness.read_with(vcx, |h, _app| h.container_handle.clone());
        assert!(
            -container_handle.offset().y > px(0.0),
            "dragging the thumb of a container-backed vertical axis must scroll its handle, the \
             same as a uniform-list-backed one"
        );
    }

    // -- Drag edge cases --

    #[gpui::test]
    fn a_pointer_move_with_no_button_held_ends_an_in_progress_drag(cx: &mut TestAppContext) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, false, 400, 0.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        let selector = vertical_thumb_debug_selector(&scroll);
        let thumb_center = vcx
            .debug_bounds(selector)
            .expect("the vertical thumb must be painted once the viewport is measured")
            .center();

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: thumb_center,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });

        // No button held, e.g. the button was released outside the window
        // and no mouse-up ever reached gpui: this must still end the drag.
        vcx.simulate_event(MouseMoveEvent {
            position: point(px(10.0), px(10.0)),
            pressed_button: None,
            modifiers: Modifiers::default(),
        });
        vcx.run_until_parked();
        let offset_after_release_move = harness.read_with(vcx, |h, _app| {
            h.list_handle.0.borrow().base_handle.offset().y
        });

        // A further move, still with no button, must not move the offset
        // any further: the drag already ended.
        vcx.simulate_event(MouseMoveEvent {
            position: point(px(200.0), px(200.0)),
            pressed_button: None,
            modifiers: Modifiers::default(),
        });
        vcx.run_until_parked();
        let offset_after_second_move = harness.read_with(vcx, |h, _app| {
            h.list_handle.0.borrow().base_handle.offset().y
        });

        assert_eq!(
            offset_after_release_move, offset_after_second_move,
            "a mouse-move reporting no button held must end an in-progress drag rather than \
             leaving it stuck open"
        );
    }

    #[gpui::test]
    fn a_mouse_up_outside_the_view_root_still_ends_the_drag_via_on_mouse_up_out(
        cx: &mut TestAppContext,
    ) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, false, 400, 0.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        let selector = vertical_thumb_debug_selector(&scroll);
        let thumb_center = vcx
            .debug_bounds(selector)
            .expect("the vertical thumb must be painted once the viewport is measured")
            .center();

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: thumb_center,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(MouseMoveEvent {
            position: point(thumb_center.x, thumb_center.y + px(30.0)),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        vcx.run_until_parked();
        let offset_mid_drag = harness.read_with(vcx, |h, _app| {
            h.list_handle.0.borrow().base_handle.offset().y
        });

        // Outside the harness's own size_full() view root (negative
        // coordinates fall outside every hitbox in the 1920x1080 test
        // window): gpui fires on_mouse_up_out rather than on_mouse_up for a
        // mouse-up whose position lands outside the registering element's
        // own hit-tested bounds.
        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: point(px(-50.0), px(-50.0)),
            modifiers: Modifiers::default(),
            click_count: 1,
        });
        vcx.run_until_parked();

        // The drag must have ended: further pointer movement, even while
        // still reporting the button held, must not move the offset again.
        vcx.simulate_event(MouseMoveEvent {
            position: point(thumb_center.x, thumb_center.y + px(90.0)),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        vcx.run_until_parked();
        let offset_after_up_out = harness.read_with(vcx, |h, _app| {
            h.list_handle.0.borrow().base_handle.offset().y
        });

        assert_eq!(
            offset_after_up_out, offset_mid_drag,
            "a mouse-up outside the view root must end the drag through on_mouse_up_out, so \
             further pointer movement afterward does not keep scrolling"
        );
    }

    #[gpui::test]
    fn content_shrinking_to_fit_mid_drag_leaves_the_offset_untouched(cx: &mut TestAppContext) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, false, 400, 0.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        let selector = vertical_thumb_debug_selector(&scroll);
        let thumb_center = vcx
            .debug_bounds(selector)
            .expect("the vertical thumb must be painted once the viewport is measured")
            .center();

        vcx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: thumb_center,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        });
        vcx.simulate_event(MouseMoveEvent {
            position: point(thumb_center.x, thumb_center.y + px(30.0)),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        vcx.run_until_parked();
        let offset_mid_drag = harness.read_with(vcx, |h, _app| {
            h.list_handle.0.borrow().base_handle.offset().y
        });
        assert!(
            offset_mid_drag < px(0.0),
            "the drag so far must have moved the offset"
        );

        // Report the axis's content as fitting the viewport mid-drag,
        // without changing the real uniform_list's own row count -- gpui's
        // own scroll-position clamping (independent of this crate) reacts
        // to the list's real row count, not to what `Axis::content_extent`
        // claims, so leaving `row_count` untouched isolates this guard from
        // that unrelated clamping.
        harness.update(vcx, |h, cx| {
            h.vertical_content_extent_override = Some(10.0);
            cx.notify();
        });
        vcx.run_until_parked();

        vcx.simulate_event(MouseMoveEvent {
            position: point(thumb_center.x, thumb_center.y + px(60.0)),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        });
        vcx.run_until_parked();
        let offset_after_shrink = harness.read_with(vcx, |h, _app| {
            h.list_handle.0.borrow().base_handle.offset().y
        });

        assert_eq!(
            offset_after_shrink, offset_mid_drag,
            "once the content shrinks to fit the viewport mid-drag, further pointer movement \
             must leave the offset exactly where it was rather than snapping it to zero"
        );

        vcx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: point(thumb_center.x, thumb_center.y + px(60.0)),
            modifiers: Modifiers::default(),
            click_count: 1,
        });
    }

    // -- First-frame nudge: re-arms after a remount --

    #[gpui::test]
    fn a_remounted_scroll_handle_re_arms_the_first_frame_nudge(cx: &mut TestAppContext) {
        let (harness, vcx) =
            cx.add_window_view(|_window, cx| Harness::new(true, false, 400, 0.0, cx));
        vcx.run_until_parked();

        let scroll = harness.read_with(vcx, |h, _app| h.scroll.clone());
        let selector = vertical_thumb_debug_selector(&scroll);
        assert!(
            vcx.debug_bounds(selector).is_some(),
            "the scrollbar must be visible once the original handle is measured"
        );

        // Swap in a fresh, never-laid-out handle -- the same "first frame"
        // state a scroll container is in the moment its content first
        // appears -- and confirm the nudge re-arms rather than staying
        // permanently settled from the first measurement.
        harness.update(vcx, |h, cx| {
            h.list_handle = UniformListScrollHandle::new();
            cx.notify();
        });
        vcx.run_until_parked();

        assert!(
            vcx.debug_bounds(selector).is_some(),
            "swapping in a fresh, unmeasured scroll handle must re-arm the first-frame nudge so \
             the scrollbar reappears without any further input"
        );
    }
}
