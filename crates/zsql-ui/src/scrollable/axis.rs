//! Per-render scroll configuration for one axis.

use super::source::ScrollSource;

/// One axis's scroll configuration, supplied fresh by the caller every
/// render: which handle backs it, how far its content actually extends, and
/// where along the wrapper its track begins.
///
/// `content_extent` is domain knowledge the abstraction cannot derive on its
/// own -- a virtualized list's true height is `row_count * row_height`,
/// which no gpui handle reports.
#[derive(Clone)]
pub struct Axis {
    pub source: ScrollSource,
    pub content_extent: f32,
    /// Distance from the wrapper's leading edge -- top for a vertical axis,
    /// left for a horizontal one -- to where this axis's track starts.
    ///
    /// Zero when the scroll handle's viewport fills the wrapper along this
    /// axis. A viewport that starts partway down or across its wrapper needs
    /// the matching offset here, or the track paints over the intervening
    /// space while the thumb -- positioned within `track_length`, which is
    /// measured from the handle -- rides at a different origin than the
    /// track it sits in. A grid whose column-header row shares the wrapper
    /// with its scrolling body is the usual case: the body's viewport starts
    /// one header-height down, so its vertical axis wants that height here.
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
