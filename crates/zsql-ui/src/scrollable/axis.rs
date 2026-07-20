//! Per-render scroll configuration for one axis.

use super::source::ScrollSource;

/// One axis's scroll configuration
#[derive(Clone)]
pub struct Axis {
    pub source: ScrollSource,
    pub content_extent: f32,
    /// Distance from the wrapper's leading edge -- top for a vertical axis,
    /// left for a horizontal one -- to where this axis's track starts
    pub track_start: f32,
}

impl Axis {
    /// A track spanning the wrapper's full extent along this axis. Use
    /// [`Axis::track_start`] when the viewport starts partway along it.
    #[must_use]
    pub fn new(source: ScrollSource, content_extent: f32) -> Self {
        Self {
            source,
            content_extent,
            track_start: 0.0,
        }
    }

    /// Offset this axis's track from the wrapper's leading edge, for a
    /// viewport that does not start at it.
    #[must_use]
    pub fn track_start(mut self, track_start: f32) -> Self {
        self.track_start = track_start;
        self
    }
}
