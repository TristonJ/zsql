//! Frame-persistent state behind a scrollable region: which handle backs
//! each axis, its content extent as of the last render, and any
//! in-progress thumb drag.

use gpui::{
    App, Context, Entity, IsZero, MouseMoveEvent, Pixels, ScrollWheelEvent, Styled, UniformList,
    Window, px,
};

use crate::scrollbar::ScrollbarGeometry;

use super::axis::Axis;
use super::source::Orientation;
use super::style::ScrollbarStyle;

/// Confine `list`'s own built-in wheel scrolling to its own axis.
#[must_use]
pub fn restrict_wheel_to_own_axis(mut list: UniformList) -> UniformList {
    list.style().restrict_scroll_to_axis = Some(true);
    list
}

/// A boxed scroll-wheel listener, as returned by [`ScrollableState::wheel_handler`].
type WheelListener = Box<dyn Fn(&ScrollWheelEvent, &mut Window, &mut App)>;

/// Where a scrollbar thumb-drag started
#[derive(Debug, Clone, Copy)]
struct DragState {
    pointer_start: f32,
    offset_start: f32,
}

struct AxisState {
    axis: Axis,
    drag: Option<DragState>,
}

/// One axis's scrollbar geometry, snapshotted once per `with_scrollbars`
/// call
#[derive(Debug, Clone, Copy)]
pub(crate) struct AxisSnapshot {
    pub geometry: ScrollbarGeometry,
    pub track_length: f32,
    /// Where the track starts along the wrapper
    pub track_start: f32,
}

/// Both axes' scrollbar geometry as of one `with_scrollbars` call.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Snapshot {
    pub vertical: Option<AxisSnapshot>,
    pub horizontal: Option<AxisSnapshot>,
}

/// Frame-persistent state behind a scrollable region built with
/// [`super::WithScrollbars::with_scrollbars`]
pub struct ScrollableState {
    vertical: Option<AxisState>,
    horizontal: Option<AxisState>,
    style: ScrollbarStyle,
    /// Whether a first-frame re-render nudge is already pending
    nudge_pending: bool,
}

impl ScrollableState {
    /// An empty state with neither axis configured
    #[must_use]
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            vertical: None,
            horizontal: None,
            style: ScrollbarStyle::default(),
            nudge_pending: false,
        }
    }

    /// Configure the vertical axis
    pub fn vertical(&mut self, axis: Axis) -> &mut Self {
        let drag = self.vertical.take().and_then(|state| state.drag);
        self.vertical = Some(AxisState { axis, drag });
        self
    }

    /// Configure the horizontal axis
    pub fn horizontal(&mut self, axis: Axis) -> &mut Self {
        let drag = self.horizontal.take().and_then(|state| state.drag);
        self.horizontal = Some(AxisState { axis, drag });
        self
    }

    /// Drop the vertical axis
    pub fn clear_vertical(&mut self) -> &mut Self {
        self.vertical = None;
        self
    }

    /// Drop the horizontal axis
    pub fn clear_horizontal(&mut self) -> &mut Self {
        self.horizontal = None;
        self
    }

    pub(crate) fn set_style(&mut self, style: ScrollbarStyle) {
        self.style = style;
    }

    /// Whether any configured axis's viewport has not yet been through a
    /// layout pass
    pub(crate) fn any_axis_unmeasured(&self) -> bool {
        let vertical_unmeasured = self
            .vertical
            .as_ref()
            .is_some_and(|state| !state.axis.source.viewport_measured(Orientation::Vertical));
        let horizontal_unmeasured = self
            .horizontal
            .as_ref()
            .is_some_and(|state| !state.axis.source.viewport_measured(Orientation::Horizontal));
        vertical_unmeasured || horizontal_unmeasured
    }

    /// Whether the scrollbars are currently being dragged
    pub(crate) fn is_dragging(&self) -> bool {
        self.vertical
            .as_ref()
            .is_some_and(|state| state.drag.is_some())
            || self
                .horizontal
                .as_ref()
                .is_some_and(|state| state.drag.is_some())
    }

    /// Whether `with_scrollbars` should schedule a first-frame re-render
    /// nudge this call
    pub(crate) fn should_schedule_nudge(&mut self, _cx: &mut Context<Self>) -> bool {
        if !self.any_axis_unmeasured() {
            self.nudge_pending = false;
            return false;
        }
        if self.nudge_pending {
            return false;
        }
        self.nudge_pending = true;
        true
    }

    /// Whether the vertical axis currently has anything to scroll
    #[must_use]
    pub fn vertical_visible(&self) -> bool {
        self.vertical.as_ref().is_some_and(|state| {
            axis_snapshot(
                &state.axis,
                Orientation::Vertical,
                self.style.min_thumb_length,
                0.0,
            )
            .geometry
            .visible
        })
    }

    /// Whether the horizontal axis currently has anything to scroll
    #[must_use]
    pub fn horizontal_visible(&self) -> bool {
        self.horizontal.as_ref().is_some_and(|state| {
            axis_snapshot(
                &state.axis,
                Orientation::Horizontal,
                self.style.min_thumb_length,
                0.0,
            )
            .geometry
            .visible
        })
    }

    /// Both axes' scrollbar geometry
    pub(crate) fn snapshot(&self) -> Snapshot {
        let vertical = self.vertical.as_ref().map(|state| {
            axis_snapshot(
                &state.axis,
                Orientation::Vertical,
                self.style.min_thumb_length,
                0.0,
            )
        });
        let vertical_visible = vertical.is_some_and(|axis| axis.geometry.visible);
        let horizontal_reserve = self.style.horizontal_reserve(vertical_visible);
        let horizontal = self.horizontal.as_ref().map(|state| {
            axis_snapshot(
                &state.axis,
                Orientation::Horizontal,
                self.style.min_thumb_length,
                horizontal_reserve,
            )
        });
        Snapshot {
            vertical,
            horizontal,
        }
    }

    /// Begin a thumb-drag on `orientation`'s axis at `pointer`, capturing
    /// that axis's current scroll offset. A no-op if that axis is not
    /// currently configured.
    pub(crate) fn start_drag(&mut self, orientation: Orientation, pointer: f32) {
        let Some(axis_state) = self.axis_state_mut(orientation) else {
            return;
        };
        let offset_start = axis_state.axis.source.scroll_offset(orientation);
        axis_state.drag = Some(DragState {
            pointer_start: pointer,
            offset_start,
        });
    }

    /// Translate `event`'s pointer position into a new scroll offset for
    /// every axis with an in-progress drag. Returns whether any axis's drag
    /// state changed
    pub(crate) fn handle_drag_move(&mut self, event: &MouseMoveEvent) -> bool {
        let dragging = event.dragging();
        let min_thumb_length = self.style.min_thumb_length;
        let horizontal_reserve = self.style.horizontal_reserve(self.vertical_visible());
        let vertical_changed = Self::drag_move_axis(
            self.vertical.as_mut(),
            Orientation::Vertical,
            f32::from(event.position.y),
            dragging,
            min_thumb_length,
            0.0,
        );
        let horizontal_changed = Self::drag_move_axis(
            self.horizontal.as_mut(),
            Orientation::Horizontal,
            f32::from(event.position.x),
            dragging,
            min_thumb_length,
            horizontal_reserve,
        );
        vertical_changed || horizontal_changed
    }

    /// `track_reserve` shortens the effective track length the drag's
    /// geometry is computed against (see [`ScrollbarStyle::horizontal_reserve`]),
    /// so a horizontal drag agrees with what `with_scrollbars` actually
    /// painted when a vertical scrollbar shares the same viewport.
    fn drag_move_axis(
        axis_state: Option<&mut AxisState>,
        orientation: Orientation,
        pointer: f32,
        dragging: bool,
        min_thumb_length: f32,
        track_reserve: f32,
    ) -> bool {
        let Some(axis_state) = axis_state else {
            return false;
        };

        if !dragging {
            return axis_state.drag.take().is_some();
        }

        let Some(drag) = axis_state.drag else {
            return false;
        };

        let content_extent = axis_state.axis.content_extent(orientation);
        let viewport_extent = axis_state.axis.source.viewport_extent(orientation);
        let current_offset = axis_state.axis.source.scroll_offset(orientation);
        let track_length = (viewport_extent - track_reserve).max(0.0);

        // If the content has shrunk to fit the viewport mid-drag, there is
        // nothing left to drag
        let geometry = ScrollbarGeometry::compute(
            content_extent,
            viewport_extent,
            current_offset,
            track_length,
            min_thumb_length,
        );
        if !geometry.visible {
            return false;
        }

        let pointer_delta = pointer - drag.pointer_start;
        let new_offset = ScrollbarGeometry::scroll_offset_for_drag(
            drag.offset_start,
            pointer_delta,
            content_extent,
            viewport_extent,
            track_length,
            min_thumb_length,
        );
        if (new_offset - current_offset).abs() < f32::EPSILON {
            return false;
        }
        axis_state
            .axis
            .source
            .set_offset_along(orientation, px(-new_offset));
        true
    }

    /// End every axis's in-progress drag, if any. Returns whether a drag was
    /// actually in progress.
    pub(crate) fn end_drag(&mut self) -> bool {
        let vertical_ended = self
            .vertical
            .as_mut()
            .is_some_and(|state| state.drag.take().is_some());
        let horizontal_ended = self
            .horizontal
            .as_mut()
            .is_some_and(|state| state.drag.take().is_some());
        vertical_ended || horizontal_ended
    }

    fn axis_state_mut(&mut self, orientation: Orientation) -> Option<&mut AxisState> {
        match orientation {
            Orientation::Vertical => self.vertical.as_mut(),
            Orientation::Horizontal => self.horizontal.as_mut(),
        }
    }

    /// A shift-held wheel event moves only the horizontal axis's scroll
    /// offset, by the wheel's magnitude; a plain wheel event, or a state
    /// with no horizontal axis configured, is left untouched. Returns
    /// whether the offset changed.
    pub fn on_scroll_wheel(&mut self, event: &ScrollWheelEvent, window: &Window) -> bool {
        if !event.modifiers.shift {
            return false;
        }
        let Some(axis_state) = self.horizontal.as_ref() else {
            return false;
        };

        let delta = event.delta.pixel_delta(window.line_height());
        let wheel_delta_x = horizontal_wheel_delta(delta.x, delta.y);

        let content_extent = axis_state.axis.content_extent(Orientation::Horizontal);
        let viewport_extent = axis_state
            .axis
            .source
            .viewport_extent(Orientation::Horizontal);
        let current_offset = axis_state
            .axis
            .source
            .scroll_offset(Orientation::Horizontal);
        let new_offset = ScrollbarGeometry::clamp_offset(
            current_offset - f32::from(wheel_delta_x),
            content_extent,
            viewport_extent,
        );

        if (new_offset - current_offset).abs() < f32::EPSILON {
            return false;
        }

        axis_state
            .axis
            .source
            .set_offset_along(Orientation::Horizontal, px(-new_offset));
        true
    }

    /// A listener for the caller to attach with `.on_scroll_wheel(...)` on
    /// the horizontally-scrolling element, wiring [`ScrollableState::on_scroll_wheel`]
    /// to the entity.
    ///
    /// Any `uniform_list` nested inside that element must also be passed
    /// through [`restrict_wheel_to_own_axis`], or it scrolls vertically off
    /// the same shift-held gesture this listener scrolls horizontally.
    #[must_use = "the returned listener does nothing unless attached with .on_scroll_wheel(...)"]
    pub fn wheel_handler(state: &Entity<Self>) -> WheelListener {
        let state = state.clone();
        Box::new(
            move |event: &ScrollWheelEvent, window: &mut Window, cx: &mut App| {
                state.update(cx, |state, cx| {
                    if state.on_scroll_wheel(event, window) {
                        cx.notify();
                    }
                });
            },
        )
    }
}

/// One axis's snapshot, its track shortened by `track_reserve` pixels from
/// its raw measured viewport extent
fn axis_snapshot(
    axis: &Axis,
    orientation: Orientation,
    min_thumb_length: f32,
    track_reserve: f32,
) -> AxisSnapshot {
    let viewport_extent = axis.source.viewport_extent(orientation);
    let scroll_offset = axis.source.scroll_offset(orientation);
    let track_length = (viewport_extent - track_reserve).max(0.0);
    let geometry = ScrollbarGeometry::compute(
        axis.content_extent(orientation),
        viewport_extent,
        scroll_offset,
        track_length,
        min_thumb_length,
    );
    AxisSnapshot {
        geometry,
        track_length,
        track_start: axis.track_start,
    }
}

/// Select the horizontal component of a shift-held wheel delta
fn horizontal_wheel_delta(delta_x: Pixels, delta_y: Pixels) -> Pixels {
    if delta_x.is_zero() { delta_y } else { delta_x }
}

#[cfg(test)]
mod tests {
    use gpui::px;

    use super::horizontal_wheel_delta;

    #[test]
    fn horizontal_wheel_delta_uses_x_when_nonzero() {
        assert_eq!(horizontal_wheel_delta(px(10.0), px(5.0)), px(10.0));
    }

    #[test]
    fn horizontal_wheel_delta_falls_back_to_y_when_x_is_zero() {
        assert_eq!(horizontal_wheel_delta(px(0.0), px(15.0)), px(15.0));
    }

    #[test]
    fn horizontal_wheel_delta_returns_zero_when_both_are_zero() {
        assert_eq!(horizontal_wheel_delta(px(0.0), px(0.0)), px(0.0));
    }

    #[test]
    fn horizontal_wheel_delta_returns_negative_y_when_x_is_zero() {
        assert_eq!(horizontal_wheel_delta(px(0.0), px(-8.0)), px(-8.0));
    }
}
