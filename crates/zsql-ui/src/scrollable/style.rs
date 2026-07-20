//! Scrollbar chrome: track/thumb thickness, colors, radius, and inset.

use crate::scrollbar;

/// Visual chrome for the track+thumb overlays [`super::WithScrollbars`]
/// builds: thickness, minimum thumb length, colors, corner radius, and the
/// gap between a track and the viewport edge it hugs.
///
/// [`ScrollbarStyle::default`] reproduces `zsql_ui::scrollbar`'s own
/// constants, so a caller that never overrides it renders identically to a
/// track+thumb pinned straight to the viewport edge.
#[derive(Debug, Clone, Copy)]
pub struct ScrollbarStyle {
    /// Thickness of the track and thumb: width for a vertical scrollbar,
    /// height for a horizontal one.
    pub track_width: f32,
    /// Shortest a thumb is ever drawn, regardless of how large the scrolled
    /// content is relative to the viewport.
    pub min_thumb_length: f32,
    /// Track fill; `None` paints no track background at all, leaving only
    /// the thumb visible.
    pub track_color: Option<u32>,
    pub thumb_color: u32,
    /// Thumb fill while hovered; `None` leaves the thumb's color unchanged
    /// on hover.
    pub thumb_hover_color: Option<u32>,
    pub radius: f32,
    /// Gap between a track and the near edge of the viewport it hugs: the
    /// right edge for a vertical track, the bottom edge for a horizontal
    /// one.
    pub inset: f32,
}

impl Default for ScrollbarStyle {
    fn default() -> Self {
        Self {
            track_width: scrollbar::TRACK_WIDTH,
            min_thumb_length: scrollbar::MIN_THUMB_LENGTH,
            track_color: Some(scrollbar::TRACK_COLOR),
            thumb_color: scrollbar::THUMB_COLOR,
            thumb_hover_color: None,
            radius: scrollbar::TRACK_WIDTH / 2.0,
            inset: 0.0,
        }
    }
}

impl ScrollbarStyle {
    /// This style's inset, plus the vertical track's thickness when a
    /// vertical scrollbar is visible: how much of a horizontal track's far
    /// end stays clear so it never runs underneath that vertical track.
    #[must_use]
    pub(crate) fn horizontal_reserve(&self, vertical_visible: bool) -> f32 {
        self.inset
            + if vertical_visible {
                self.track_width
            } else {
                0.0
            }
    }
}

#[cfg(test)]
mod tests {
    use super::ScrollbarStyle;
    use crate::scrollbar;

    #[test]
    fn default_style_reproduces_the_scrollbar_modules_own_constants() {
        let style = ScrollbarStyle::default();
        assert!((style.track_width - scrollbar::TRACK_WIDTH).abs() < f32::EPSILON);
        assert!((style.min_thumb_length - scrollbar::MIN_THUMB_LENGTH).abs() < f32::EPSILON);
        assert_eq!(style.track_color, Some(scrollbar::TRACK_COLOR));
        assert_eq!(style.thumb_color, scrollbar::THUMB_COLOR);
        assert!(style.thumb_hover_color.is_none());
        assert!((style.radius - scrollbar::TRACK_WIDTH / 2.0).abs() < f32::EPSILON);
        assert!((style.inset - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn horizontal_reserve_adds_the_track_width_only_when_a_vertical_scrollbar_is_visible() {
        let style = ScrollbarStyle::default();
        assert!((style.horizontal_reserve(false) - style.inset).abs() < f32::EPSILON);
        assert!(
            (style.horizontal_reserve(true) - (style.inset + style.track_width)).abs()
                < f32::EPSILON
        );
    }
}
