//! A reusable scrollable-region abstraction: per-axis scroll configuration
//! ([`Axis`]/[`ScrollSource`]), frame-persistent drag/geometry state
//! ([`ScrollableState`]), overlay chrome ([`ScrollbarStyle`]), and the
//! [`WithScrollbars`] trait that composes a scroll viewport with its
//! track+thumb overlays.
//!
//! Builds on [`crate::scrollbar::ScrollbarGeometry`] for the actual
//! clamp/thumb-length/thumb-position/drag-offset math -- this module only
//! adapts that pure geometry to gpui's scroll handles, mouse events, and
//! element tree.
//!
//! Two gpui layout quirks a caller must accommodate, documented in full on
//! [`WithScrollbars::with_scrollbars`]: the overlays it builds must land as
//! siblings of the scroll viewport, never descendants; and a horizontally
//! scrolling [`ScrollSource::Container`] needs `.min_w_0()` and
//! `.overflow_x_hidden()` on the viewport plus
//! `.min_w(px(content_extent))` on its scrolled child, all of which the
//! caller must apply -- the trait cannot reach into an already-built
//! element tree to add any of them.
//!
//! A third obligation applies to wheel input: any `uniform_list` nested in
//! a scrollable that also has a horizontal axis must be passed through
//! [`restrict_wheel_to_own_axis`], or a shift-held gesture scrolls both
//! axes at once.

mod axis;
mod source;
mod state;
mod style;
mod view;
mod wrapper;

pub use axis::Axis;
pub use source::ScrollSource;
pub use state::{ScrollableState, restrict_wheel_to_own_axis};
pub use style::ScrollbarStyle;
pub use view::{ScrollView, vertical_scroll};
pub use wrapper::WithScrollbars;
#[cfg(any(test, feature = "test-support"))]
pub use wrapper::{horizontal_thumb_debug_selector, vertical_thumb_debug_selector};
