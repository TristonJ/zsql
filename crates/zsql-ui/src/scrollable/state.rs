//! Frame-persistent state behind a scrollable region: which handle backs
//! each axis, its content extent as of the last render, and any
//! in-progress thumb drag.

use gpui::{
    App, Context, Entity, IsZero, MouseMoveEvent, MouseUpEvent, Pixels, ScrollWheelEvent, Window,
    px,
};

use crate::scrollbar::ScrollbarGeometry;

use super::axis::Axis;
use super::source::Orientation;
use super::style::ScrollbarStyle;

/// A boxed mouse-move listener, as returned by [`ScrollableState::drag_handlers`].
type MoveListener = Box<dyn Fn(&MouseMoveEvent, &mut Window, &mut App)>;
/// A boxed mouse-up listener, as returned by [`ScrollableState::drag_handlers`].
type UpListener = Box<dyn Fn(&MouseUpEvent, &mut Window, &mut App)>;
/// A boxed scroll-wheel listener, as returned by [`ScrollableState::wheel_handler`].
type WheelListener = Box<dyn Fn(&ScrollWheelEvent, &mut Window, &mut App)>;

/// Mouse-move/mouse-up/mouse-up-out listeners for tracking a scrollbar
/// thumb-drag, meant to be attached at the caller's own view root -- see
/// [`ScrollableState::drag_handlers`].
pub struct DragHandlers {
    pub on_move: MoveListener,
    pub on_up: UpListener,
    pub on_up_out: UpListener,
}

fn end_drag_listener(state: &Entity<ScrollableState>) -> UpListener {
    let state = state.clone();
    Box::new(
        move |_event: &MouseUpEvent, _window: &mut Window, cx: &mut App| {
            state.update(cx, |state, cx| {
                if state.end_drag() {
                    cx.notify();
                }
            });
        },
    )
}

/// Where a scrollbar thumb-drag started: the pointer's position and the
/// axis's scroll offset at that moment, so later pointer movement can be
/// translated into a new absolute offset.
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
/// call so the wrapper and its track/thumb renderers agree on the same
/// numbers: the geometry itself, plus the track length (that axis's
/// measured viewport extent) it was computed against.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AxisSnapshot {
    pub geometry: ScrollbarGeometry,
    pub track_length: f32,
    /// Where the track starts along the wrapper, carried through from
    /// [`Axis::track_start`] so the painted track shares the thumb's origin.
    pub track_start: f32,
}

/// Both axes' scrollbar geometry as of one `with_scrollbars` call.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Snapshot {
    pub vertical: Option<AxisSnapshot>,
    pub horizontal: Option<AxisSnapshot>,
}

/// Frame-persistent state behind a scrollable region built with
/// [`super::WithScrollbars::with_scrollbars`]: which scroll handle backs
/// each axis, that axis's caller-supplied content extent as of the last
/// render, any in-progress thumb drag, and the [`ScrollbarStyle`] the drag
/// math was last computed against.
///
/// Held by the parent view as `Entity<ScrollableState>` and rebuilt fresh
/// each render via [`ScrollableState::vertical`]/[`ScrollableState::horizontal`]
/// -- everything here except the in-progress drag is caller-supplied, not
/// derived.
///
/// A thumb drag's move/up tracking cannot be attached to the small overlay
/// `with_scrollbars` builds: gpui's `on_mouse_move`/`on_mouse_up` listeners
/// only fire while the pointer is within the registering element's own
/// hit-tested bounds (`Interactivity::paint`'s dispatch gates on
/// `hitbox.is_hovered(window)`), so a drag that carries the pointer outside
/// a scrollbar's own small viewport -- easy to do, since the viewport is
/// usually much smaller than the window -- would stop updating mid-drag.
/// [`ScrollableState::drag_handlers`] returns listeners meant to be attached
/// at the largest ancestor element the drag should keep tracking within --
/// tracking still stops the moment the pointer leaves whatever element they
/// end up attached to, so a caller whose own root is itself a narrow column
/// (rather than the whole window) only gets drag tracking bounded by that
/// column's own width.
pub struct ScrollableState {
    vertical: Option<AxisState>,
    horizontal: Option<AxisState>,
    style: ScrollbarStyle,
    /// Whether a first-frame re-render nudge is already pending, so
    /// [`ScrollableState::should_schedule_nudge`] never schedules a second
    /// one on top of it.
    nudge_pending: bool,
}

impl ScrollableState {
    /// An empty state with neither axis configured, ready for the first
    /// render's [`ScrollableState::vertical`]/[`ScrollableState::horizontal`]
    /// calls to populate.
    ///
    /// Takes `&mut Context<Self>` only so this can be passed directly to
    /// `cx.new(ScrollableState::new)`; nothing here reads it.
    #[must_use]
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            vertical: None,
            horizontal: None,
            style: ScrollbarStyle::default(),
            nudge_pending: false,
        }
    }

    /// Configure the vertical axis for this render, preserving any
    /// in-progress drag from a previous render.
    pub fn vertical(&mut self, axis: Axis) -> &mut Self {
        let drag = self.vertical.take().and_then(|state| state.drag);
        self.vertical = Some(AxisState { axis, drag });
        self
    }

    /// Configure the horizontal axis for this render, preserving any
    /// in-progress drag from a previous render.
    pub fn horizontal(&mut self, axis: Axis) -> &mut Self {
        let drag = self.horizontal.take().and_then(|state| state.drag);
        self.horizontal = Some(AxisState { axis, drag });
        self
    }

    /// Drop the vertical axis: nothing renders or drags on it until the
    /// next [`ScrollableState::vertical`] call.
    pub fn clear_vertical(&mut self) -> &mut Self {
        self.vertical = None;
        self
    }

    /// Drop the horizontal axis: nothing renders or drags on it until the
    /// next [`ScrollableState::horizontal`] call.
    pub fn clear_horizontal(&mut self) -> &mut Self {
        self.horizontal = None;
        self
    }

    pub(crate) fn set_style(&mut self, style: ScrollbarStyle) {
        self.style = style;
    }

    /// Whether any configured axis's viewport has not yet been through a
    /// layout pass -- the first-frame state a scroll container's bounds
    /// read back in before it is measured.
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

    /// Whether `with_scrollbars` should schedule a first-frame re-render
    /// nudge this call: true at most once per unmeasured stretch, so a
    /// configured axis whose viewport never measures (a collapsed pane)
    /// gets exactly one nudge rather than one every render. Clears the
    /// pending flag once every configured axis is measured, ready for the
    /// next time an axis becomes unmeasured (e.g. a remounted scroll
    /// handle).
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

    /// Whether the vertical axis currently has anything to scroll: its
    /// content overflows its measured viewport, so `with_scrollbars` would
    /// draw a track+thumb for it.
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

    /// Whether the horizontal axis currently has anything to scroll: its
    /// content overflows its measured viewport, so `with_scrollbars` would
    /// draw a track+thumb for it.
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

    /// Both axes' scrollbar geometry as of this call, the horizontal axis's
    /// track shortened to leave room for a simultaneously-visible vertical
    /// scrollbar (see [`ScrollbarStyle::horizontal_reserve`]) so the two
    /// never paint on top of each other.
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
    /// state changed: a new offset, or a drag that ended because the mouse
    /// button is no longer held.
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

        let content_extent = axis_state.axis.content_extent;
        let viewport_extent = axis_state.axis.source.viewport_extent(orientation);
        let current_offset = axis_state.axis.source.scroll_offset(orientation);
        let track_length = (viewport_extent - track_reserve).max(0.0);

        // If the content has shrunk to fit the viewport mid-drag (a schema
        // collapses, rows get filtered out), there is nothing left to drag:
        // leave the offset exactly as it is rather than letting
        // `scroll_offset_for_drag`'s "nothing to scroll" case snap it to 0.
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

        let content_extent = axis_state.axis.content_extent;
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

    /// Mouse-move/mouse-up/mouse-up-out listeners for the caller to attach
    /// at their own view root, so a thumb drag keeps tracking the pointer
    /// once it leaves the small overlay `with_scrollbars` built. See this
    /// type's own docs for why wrapper-level attachment is not enough.
    #[must_use = "the returned listeners do nothing unless attached to the caller's view root"]
    pub fn drag_handlers(state: &Entity<Self>) -> DragHandlers {
        let move_state = state.clone();
        let on_move: MoveListener = Box::new(
            move |event: &MouseMoveEvent, _window: &mut Window, cx: &mut App| {
                move_state.update(cx, |state, cx| {
                    if state.handle_drag_move(event) {
                        cx.notify();
                    }
                });
            },
        );

        DragHandlers {
            on_move,
            on_up: end_drag_listener(state),
            on_up_out: end_drag_listener(state),
        }
    }

    /// A listener for the caller to attach with `.on_scroll_wheel(...)` on
    /// the horizontally-scrolling element, wiring [`ScrollableState::on_scroll_wheel`]
    /// to the entity.
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
/// its raw measured viewport extent -- `0.0` for an axis that need not make
/// room for anything else, [`ScrollbarStyle::horizontal_reserve`]'s result
/// for a horizontal axis sharing its viewport with a vertical scrollbar.
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
        axis.content_extent,
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

/// Select the horizontal component of a shift-held wheel delta, falling
/// back to the vertical component when the horizontal component is zero
/// (some platforms do not swap the components before dispatch).
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
