//! The concrete gpui scroll handle backing one axis of a
//! [`super::ScrollableState`].

use gpui::{Bounds, Pixels, Point, ScrollHandle, UniformListScrollHandle, point};

/// Which axis a scroll computation operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Orientation {
    Vertical,
    Horizontal,
}

/// The concrete gpui handle backing one axis: a virtualized `uniform_list`'s
/// handle, or a plain overflow container's handle. Both track a full 2D
/// offset; every method here reads or mutates only the requested axis's
/// component, leaving the other axis's offset numerically untouched.
#[derive(Clone, Debug)]
pub enum ScrollSource {
    /// A virtualized `uniform_list`'s scroll handle.
    UniformList(UniformListScrollHandle),
    /// An overflow container's scroll handle.
    Container(ScrollHandle),
}

impl ScrollSource {
    pub(crate) fn offset(&self) -> Point<Pixels> {
        match self {
            Self::UniformList(handle) => handle.0.borrow().base_handle.offset(),
            Self::Container(handle) => handle.offset(),
        }
    }

    pub(crate) fn bounds(&self) -> Bounds<Pixels> {
        match self {
            Self::UniformList(handle) => handle.0.borrow().base_handle.bounds(),
            Self::Container(handle) => handle.bounds(),
        }
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        match self {
            Self::UniformList(handle) => handle.0.borrow().base_handle.set_offset(offset),
            Self::Container(handle) => handle.set_offset(offset),
        }
    }

    /// Move this axis's offset to `value` (gpui's own convention: negative
    /// in the scrolled direction) along `orientation`, leaving the other
    /// axis's offset component numerically unchanged.
    pub(crate) fn set_offset_along(&self, orientation: Orientation, value: Pixels) {
        let current = self.offset();
        let next = match orientation {
            Orientation::Vertical => point(current.x, value),
            Orientation::Horizontal => point(value, current.y),
        };
        self.set_offset(next);
    }

    /// This axis's live scroll offset (positive-down/right), the convention
    /// [`crate::scrollbar::ScrollbarGeometry`] expects -- the inverse of a
    /// gpui scroll handle's own negative-in-the-scrolled-direction offset.
    pub(crate) fn scroll_offset(&self, orientation: Orientation) -> f32 {
        let offset = self.offset();
        -f32::from(match orientation {
            Orientation::Vertical => offset.y,
            Orientation::Horizontal => offset.x,
        })
    }

    /// This axis's live measured viewport extent (height for vertical,
    /// width for horizontal): zero before the scroll container's first
    /// layout pass.
    pub(crate) fn viewport_extent(&self, orientation: Orientation) -> f32 {
        let size = self.bounds().size;
        f32::from(match orientation {
            Orientation::Vertical => size.height,
            Orientation::Horizontal => size.width,
        })
    }

    /// Whether this axis's viewport has been laid out at least once.
    pub(crate) fn viewport_measured(&self, orientation: Orientation) -> bool {
        let size = self.bounds().size;
        match orientation {
            Orientation::Vertical => size.height != Pixels::ZERO,
            Orientation::Horizontal => size.width != Pixels::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{ScrollHandle, UniformListScrollHandle, point, px};

    use super::{Orientation, ScrollSource};

    #[test]
    fn setting_the_vertical_offset_leaves_the_horizontal_component_untouched() {
        let handle = ScrollHandle::new();
        handle.set_offset(point(px(-40.0), px(-10.0)));
        let source = ScrollSource::Container(handle.clone());

        source.set_offset_along(Orientation::Vertical, px(-99.0));

        assert_eq!(handle.offset().x, px(-40.0));
        assert_eq!(handle.offset().y, px(-99.0));
    }

    #[test]
    fn setting_the_horizontal_offset_leaves_the_vertical_component_untouched() {
        let handle = ScrollHandle::new();
        handle.set_offset(point(px(-40.0), px(-10.0)));
        let source = ScrollSource::Container(handle.clone());

        source.set_offset_along(Orientation::Horizontal, px(-5.0));

        assert_eq!(handle.offset().x, px(-5.0));
        assert_eq!(handle.offset().y, px(-10.0));
    }

    #[test]
    fn a_uniform_list_source_also_preserves_the_other_axis() {
        let handle = UniformListScrollHandle::new();
        handle
            .0
            .borrow()
            .base_handle
            .set_offset(point(px(-40.0), px(-10.0)));
        let source = ScrollSource::UniformList(handle.clone());

        source.set_offset_along(Orientation::Horizontal, px(-5.0));

        let offset = handle.0.borrow().base_handle.offset();
        assert_eq!(offset.x, px(-5.0));
        assert_eq!(offset.y, px(-10.0));
    }

    #[test]
    fn scroll_offset_negates_gpuis_negative_down_convention() {
        let handle = ScrollHandle::new();
        handle.set_offset(point(px(0.0), px(-25.0)));
        let source = ScrollSource::Container(handle);

        assert!((source.scroll_offset(Orientation::Vertical) - 25.0).abs() < f32::EPSILON);
    }

    #[test]
    fn viewport_measured_is_false_before_any_layout_pass() {
        let source = ScrollSource::Container(ScrollHandle::new());
        assert!(!source.viewport_measured(Orientation::Vertical));
        assert!(!source.viewport_measured(Orientation::Horizontal));
    }

    #[test]
    fn a_uniform_list_source_is_also_unmeasured_before_any_layout_pass() {
        let source = ScrollSource::UniformList(UniformListScrollHandle::new());
        assert!(!source.viewport_measured(Orientation::Vertical));
        assert!(!source.viewport_measured(Orientation::Horizontal));
        assert!(source.viewport_extent(Orientation::Vertical).abs() < f32::EPSILON);
        assert!(source.viewport_extent(Orientation::Horizontal).abs() < f32::EPSILON);
    }

    #[test]
    fn a_uniform_list_sources_scroll_offset_negates_gpuis_negative_down_convention() {
        let handle = UniformListScrollHandle::new();
        handle
            .0
            .borrow()
            .base_handle
            .set_offset(point(px(0.0), px(-25.0)));
        let source = ScrollSource::UniformList(handle);

        assert!((source.scroll_offset(Orientation::Vertical) - 25.0).abs() < f32::EPSILON);
    }
}
