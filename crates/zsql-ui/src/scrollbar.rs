//! Pure scrollbar geometry, shared by any gpui view that overlays a
//! track+thumb affordance on a scrollable region. [`ScrollbarGeometry::compute`]
//! takes only extents and an offset -- no `Window`/`Context` -- so it is
//! unit-testable without a running event loop. The chrome constants here are
//! centralized so no call site hardcodes a raw pixel or hex literal.

/// Width of an overlaid vertical scrollbar's track and thumb.
pub const TRACK_WIDTH: f32 = 8.0;
/// Shortest a thumb is ever drawn, regardless of how large the scrolled
/// content is relative to the viewport, so it stays grabbable on very tall
/// result sets.
pub const MIN_THUMB_LENGTH: f32 = 24.0;
/// Track background: page ink at very low opacity, just enough to read as a
/// gutter without competing with the grid's row hairlines.
pub const TRACK_COLOR: u32 = 0x10_12_17_40;
/// Thumb fill: muted foreground at moderate opacity, distinct from the
/// track and from the grid's hairline borders.
pub const THUMB_COLOR: u32 = 0x87_8e_9f_66;

/// A track+thumb's geometry along one scroll axis, derived purely from the
/// scrolled content's extent, the viewport's extent, and the current scroll
/// offset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollbarGeometry {
    /// Length of the draggable thumb, in the same pixel units as
    /// `track_length`. Zero when `visible` is false.
    pub thumb_length: f32,
    /// The thumb's scroll progress as a fraction of the track it can travel,
    /// in `[0.0, 1.0]`: `0.0` at the top/left, `1.0` at the bottom/right.
    pub thumb_position: f32,
    /// Whether a track+thumb should be drawn at all: false once the content
    /// already fits inside the viewport, i.e. there is nothing to scroll.
    pub visible: bool,
}

impl ScrollbarGeometry {
    /// Geometry for a track of `track_length` pixels, given the scrolled
    /// content's total extent, the viewport's visible extent, and the
    /// current scroll offset (`0.0` at the top/left, growing positive
    /// toward the bottom/right).
    ///
    /// `scroll_offset` is clamped into `[0.0, content_extent -
    /// viewport_extent]` before use, so a stale offset from a frame taken
    /// before a resize or a row-count change cannot push the thumb past
    /// either end of the track. Returns a hidden, zero-length geometry
    /// (never dividing by zero) whenever `content_extent <=
    /// viewport_extent`, including a zero-row or single-row result set.
    #[must_use]
    pub fn compute(
        content_extent: f32,
        viewport_extent: f32,
        scroll_offset: f32,
        track_length: f32,
        min_thumb_length: f32,
    ) -> Self {
        if viewport_extent <= 0.0 || content_extent <= viewport_extent {
            return Self {
                thumb_length: 0.0,
                thumb_position: 0.0,
                visible: false,
            };
        }

        let visible_fraction = viewport_extent / content_extent;
        let thumb_floor = min_thumb_length.min(track_length).max(0.0);
        let thumb_length = (track_length * visible_fraction).clamp(thumb_floor, track_length);

        let max_offset = content_extent - viewport_extent;
        let clamped_offset = scroll_offset.clamp(0.0, max_offset);
        let thumb_position = (clamped_offset / max_offset).clamp(0.0, 1.0);

        Self {
            thumb_length,
            thumb_position,
            visible: true,
        }
    }

    /// The thumb's top/left offset from the track's start, in pixels, for a
    /// track of `track_length` pixels: `thumb_position` scaled by however
    /// far the thumb can travel within the track.
    #[must_use]
    pub fn thumb_offset(&self, track_length: f32) -> f32 {
        self.thumb_position * (track_length - self.thumb_length).max(0.0)
    }

    /// The scroll offset produced by dragging a thumb by `pointer_delta`
    /// pixels from a drag that started at `offset_start`, given the same
    /// extents used to size the thumb.
    ///
    /// Pointer movement is scaled by how far the content can scroll
    /// (`content_extent - viewport_extent`) versus how far the thumb can
    /// physically travel within the track (`track_length - thumb_length`),
    /// so a full-length drag across the track's travel maps to a full scroll
    /// from top to bottom. The result is clamped to `[0.0, content_extent -
    /// viewport_extent]`. Returns `offset_start` clamped into that range,
    /// unmoved, whenever there is nothing to scroll (`content_extent <=
    /// viewport_extent`) or the thumb has no room to travel (a thumb floored
    /// at `min_thumb_length` that already fills the whole track).
    #[must_use]
    pub fn scroll_offset_for_drag(
        offset_start: f32,
        pointer_delta: f32,
        content_extent: f32,
        viewport_extent: f32,
        track_length: f32,
        min_thumb_length: f32,
    ) -> f32 {
        if viewport_extent <= 0.0 || content_extent <= viewport_extent {
            return 0.0;
        }
        let max_offset = content_extent - viewport_extent;

        let thumb_length = Self::compute(
            content_extent,
            viewport_extent,
            0.0,
            track_length,
            min_thumb_length,
        )
        .thumb_length;
        let max_thumb_travel = track_length - thumb_length;
        if max_thumb_travel <= 0.0 {
            return offset_start.clamp(0.0, max_offset);
        }

        let offset_delta = pointer_delta * (max_offset / max_thumb_travel);
        (offset_start + offset_delta).clamp(0.0, max_offset)
    }
}

#[cfg(test)]
mod tests {
    use super::ScrollbarGeometry;

    const TRACK: f32 = 200.0;
    const MIN_THUMB: f32 = 24.0;

    #[test]
    // The hidden-geometry branch returns `0.0` verbatim (no arithmetic on
    // it), so an exact comparison here is intentional.
    #[allow(clippy::float_cmp)]
    fn hidden_when_content_exactly_fits_the_viewport() {
        let geometry = ScrollbarGeometry::compute(400.0, 400.0, 0.0, TRACK, MIN_THUMB);
        assert!(!geometry.visible);
        assert_eq!(geometry.thumb_length, 0.0);
    }

    #[test]
    fn hidden_when_content_is_smaller_than_the_viewport() {
        let geometry = ScrollbarGeometry::compute(100.0, 400.0, 0.0, TRACK, MIN_THUMB);
        assert!(!geometry.visible);
    }

    #[test]
    fn hidden_for_a_zero_row_or_single_row_result_set_without_panicking() {
        // A zero-row result: no content at all.
        let empty = ScrollbarGeometry::compute(0.0, 400.0, 0.0, TRACK, MIN_THUMB);
        assert!(!empty.visible);

        // A single row, far shorter than the viewport.
        let single_row = ScrollbarGeometry::compute(24.0, 400.0, 0.0, TRACK, MIN_THUMB);
        assert!(!single_row.visible);
    }

    #[test]
    fn visible_once_content_overflows_the_viewport() {
        let geometry = ScrollbarGeometry::compute(1_000.0, 400.0, 0.0, TRACK, MIN_THUMB);
        assert!(geometry.visible);
    }

    #[test]
    fn thumb_shrinks_as_content_grows_relative_to_the_viewport() {
        let moderate = ScrollbarGeometry::compute(800.0, 400.0, 0.0, TRACK, MIN_THUMB);
        let huge = ScrollbarGeometry::compute(80_000.0, 400.0, 0.0, TRACK, MIN_THUMB);
        assert!(huge.thumb_length < moderate.thumb_length);
    }

    #[test]
    // `clamp`'s floor arm returns `MIN_THUMB` verbatim (no further
    // arithmetic on it), so an exact comparison here is intentional.
    #[allow(clippy::float_cmp)]
    fn thumb_length_floors_at_the_minimum_constant_for_very_large_content() {
        let geometry = ScrollbarGeometry::compute(1_000_000.0, 400.0, 0.0, TRACK, MIN_THUMB);
        assert_eq!(geometry.thumb_length, MIN_THUMB);
    }

    #[test]
    fn thumb_length_never_exceeds_the_track_length() {
        // Content only barely overflows: the naive proportional thumb length
        // would sit just under the track length, never over it.
        let geometry = ScrollbarGeometry::compute(401.0, 400.0, 0.0, TRACK, MIN_THUMB);
        assert!(geometry.thumb_length <= TRACK);
    }

    #[test]
    // A `0.0` offset and an offset equal to `max_offset` both divide out to
    // an exact `0.0`/`1.0` fraction, so an exact comparison here is
    // intentional.
    #[allow(clippy::float_cmp)]
    fn position_is_zero_at_the_top_and_one_at_the_max_offset() {
        let content = 1_000.0;
        let viewport = 400.0;
        let max_offset = content - viewport;

        let top = ScrollbarGeometry::compute(content, viewport, 0.0, TRACK, MIN_THUMB);
        assert_eq!(top.thumb_position, 0.0);

        let bottom = ScrollbarGeometry::compute(content, viewport, max_offset, TRACK, MIN_THUMB);
        assert_eq!(bottom.thumb_position, 1.0);
    }

    #[test]
    // `clamp`'s floor/ceiling arms return `0.0`/`1.0` verbatim (no further
    // arithmetic on them), so an exact comparison here is intentional.
    #[allow(clippy::float_cmp)]
    fn position_clamps_to_zero_one_for_offsets_outside_the_valid_range() {
        let content = 1_000.0;
        let viewport = 400.0;
        let max_offset = content - viewport;

        // A stale offset beyond max_offset, e.g. from a frame before rows
        // shrank or the viewport was resized.
        let beyond_max =
            ScrollbarGeometry::compute(content, viewport, max_offset + 500.0, TRACK, MIN_THUMB);
        assert_eq!(beyond_max.thumb_position, 1.0);

        // A negative offset should never occur in practice, but must still
        // clamp rather than produce a position outside [0.0, 1.0].
        let negative = ScrollbarGeometry::compute(content, viewport, -50.0, TRACK, MIN_THUMB);
        assert_eq!(negative.thumb_position, 0.0);
    }

    #[test]
    fn thumb_offset_keeps_the_thumb_within_the_track_at_the_extremes() {
        let geometry = ScrollbarGeometry::compute(1_000.0, 400.0, 300.0, TRACK, MIN_THUMB);
        let offset = geometry.thumb_offset(TRACK);
        assert!(offset >= 0.0);
        assert!(offset + geometry.thumb_length <= TRACK + f32::EPSILON);
    }

    #[test]
    fn drag_offset_is_monotonic_in_pointer_delta() {
        let content = 2_000.0;
        let viewport = 400.0;
        let small_drag = ScrollbarGeometry::scroll_offset_for_drag(
            0.0, 10.0, content, viewport, viewport, MIN_THUMB,
        );
        let large_drag = ScrollbarGeometry::scroll_offset_for_drag(
            0.0, 50.0, content, viewport, viewport, MIN_THUMB,
        );
        assert!(large_drag > small_drag);
    }

    #[test]
    // Dragging past the top edge clamps to `0.0` verbatim (the clamp's floor
    // arm), so an exact comparison here is intentional.
    #[allow(clippy::float_cmp)]
    fn drag_offset_clamps_at_zero_when_dragged_up_past_the_top() {
        let content = 2_000.0;
        let viewport = 400.0;
        // Already at the top; a large upward (negative) pointer delta must
        // not push the offset negative.
        let offset = ScrollbarGeometry::scroll_offset_for_drag(
            0.0, -10_000.0, content, viewport, viewport, MIN_THUMB,
        );
        assert_eq!(offset, 0.0);
    }

    #[test]
    // Dragging past the bottom edge clamps to `max_offset` verbatim (the
    // clamp's ceiling arm), so an exact comparison here is intentional.
    #[allow(clippy::float_cmp)]
    fn drag_offset_clamps_at_max_offset_when_dragged_down_past_the_bottom() {
        let content = 2_000.0;
        let viewport = 400.0;
        let max_offset = content - viewport;
        // Already at the bottom; a large downward pointer delta must not
        // push the offset past `max_offset`.
        let offset = ScrollbarGeometry::scroll_offset_for_drag(
            max_offset, 10_000.0, content, viewport, viewport, MIN_THUMB,
        );
        assert_eq!(offset, max_offset);
    }

    #[test]
    fn a_full_travel_drag_maps_to_the_full_max_offset() {
        let content = 2_000.0;
        let viewport = 400.0;
        let max_offset = content - viewport;
        let thumb_length =
            ScrollbarGeometry::compute(content, viewport, 0.0, viewport, MIN_THUMB).thumb_length;
        let max_thumb_travel = viewport - thumb_length;

        let offset = ScrollbarGeometry::scroll_offset_for_drag(
            0.0,
            max_thumb_travel,
            content,
            viewport,
            viewport,
            MIN_THUMB,
        );
        assert!((offset - max_offset).abs() < 0.01);
    }

    #[test]
    // Nothing to scroll returns `0.0` verbatim (the early-return branch), so
    // an exact comparison here is intentional.
    #[allow(clippy::float_cmp)]
    fn drag_offset_is_zero_when_content_fits_the_viewport() {
        let offset =
            ScrollbarGeometry::scroll_offset_for_drag(0.0, 100.0, 100.0, 400.0, 400.0, MIN_THUMB);
        assert_eq!(offset, 0.0);
    }

    #[test]
    // Pre-layout: viewport has not yet been measured, so viewport_extent is
    // still zero. This guards against division by zero in visible fraction
    // calculation and matches the documented "hidden, zero-length" contract.
    #[allow(clippy::float_cmp)]
    fn compute_returns_hidden_geometry_for_zero_height_viewport() {
        let geometry = ScrollbarGeometry::compute(100.0, 0.0, 0.0, TRACK, MIN_THUMB);
        assert!(!geometry.visible);
        assert_eq!(geometry.thumb_length, 0.0);
    }

    #[test]
    // Thumb floored at min_thumb_length fills the whole track, leaving
    // max_thumb_travel <= 0.0. This guard branch confirms the function
    // returns offset_start clamped to [0, max_offset] without dividing by
    // zero or scaling the pointer movement.
    #[allow(clippy::float_cmp)]
    fn scroll_offset_for_drag_returns_clamped_start_when_thumb_fills_track() {
        // Thumb floored at min_thumb_length fills the whole track:
        // track_length = viewport_extent = 20.0, min_thumb = 24.0, content = 200.0
        let track = 20.0;
        let viewport = 20.0;
        let min_thumb = 24.0;
        let content = 200.0;
        let max_offset = content - viewport;

        // Verify that thumb_length equals track_length in this scenario.
        let geom = ScrollbarGeometry::compute(content, viewport, 0.0, track, min_thumb);
        assert_eq!(geom.thumb_length, track);

        // Dragging with no thumb travel room returns offset_start unchanged
        // (clamped to [0, max_offset]).
        let offset_start = 50.0;
        let offset = ScrollbarGeometry::scroll_offset_for_drag(
            offset_start,
            100.0,
            content,
            viewport,
            track,
            min_thumb,
        );
        assert_eq!(offset, offset_start);

        // Negative offset_start gets clamped up to 0.
        let offset = ScrollbarGeometry::scroll_offset_for_drag(
            -10.0, 100.0, content, viewport, track, min_thumb,
        );
        assert_eq!(offset, 0.0);

        // offset_start > max_offset gets clamped down.
        let offset = ScrollbarGeometry::scroll_offset_for_drag(
            max_offset + 50.0,
            100.0,
            content,
            viewport,
            track,
            min_thumb,
        );
        assert_eq!(offset, max_offset);
    }
}
