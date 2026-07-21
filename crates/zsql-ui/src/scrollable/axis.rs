//! Per-render scroll configuration for one axis.

use super::source::{Orientation, ScrollSource};

/// Where an axis's content extent comes from
#[derive(Clone, Copy)]
enum ContentExtent {
    /// A value the caller supplies each render, e.g. `row_count * row_height`
    /// for a virtualized list whose true height no handle can report.
    Fixed(f32),
    /// Measured back from the scroll handle each render (viewport extent plus
    /// its max scroll offset), for a container that clips its own content.
    Measured,
}

/// One axis's scroll configuration
#[derive(Clone)]
pub struct Axis {
    pub source: ScrollSource,
    extent: ContentExtent,
    /// Distance from the wrapper's leading edge -- top for a vertical axis,
    /// left for a horizontal one -- to where this axis's track starts
    pub track_start: f32,
}

impl Axis {
    /// An axis whose content runs `content_extent` pixels along its
    /// orientation. Use this when the caller knows that extent and the
    /// handle cannot report it -- most importantly a
    /// [`ScrollSource::UniformList`], whose true length is
    /// `row_count * row_height`. Use [`Axis::track_start`] when the viewport
    /// starts partway along the wrapper.
    #[must_use]
    pub fn new(source: ScrollSource, content_extent: f32) -> Self {
        Self {
            source,
            extent: ContentExtent::Fixed(content_extent),
            track_start: 0.0,
        }
    }

    /// An axis that measures its own content extent from its
    /// [`ScrollSource::Container`] handle each render, so the caller need not
    /// compute it. Only meaningful for a container that actually clips its
    /// content (an `overflow`-scrolling div with `.track_scroll(..)`): the
    /// extent is read back from the handle's max scroll offset, which is that
    /// container's `content - viewport`. A `UniformList` source virtualizes
    /// its rows and never lays all of them out, so its max offset is not the
    /// true content extent -- pass the extent explicitly via [`Axis::new`]
    /// there instead.
    #[must_use]
    pub fn measured(source: ScrollSource) -> Self {
        Self {
            source,
            extent: ContentExtent::Measured,
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

    /// This axis's content extent for the current render: the caller's fixed
    /// value, or a fresh measurement off the handle.
    pub(crate) fn content_extent(&self, orientation: Orientation) -> f32 {
        match self.extent {
            ContentExtent::Fixed(value) => value,
            ContentExtent::Measured => {
                self.source.viewport_extent(orientation) + self.source.max_offset(orientation)
            }
        }
    }
}
