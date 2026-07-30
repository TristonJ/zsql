//! Pure horizontal scroll-offset arithmetic for the single-line text
//! field's content. Window- and paint-independent: a consumer supplies the
//! content and viewport widths it already measured during shaping/layout,
//! and gets back a valid, caret-aware offset.

use gpui::Pixels;

use super::theme::FIELD_CARET_SCROLL_MARGIN;

/// The largest valid scroll offset for `content_width` of text within a
/// `viewport_width`-wide field: zero once the content already fits,
/// otherwise the overflow past the viewport.
#[must_use]
pub fn max_scroll_offset(content_width: Pixels, viewport_width: Pixels) -> Pixels {
    (content_width - viewport_width).max(Pixels::ZERO)
}

/// `offset` constrained to a valid scroll position for `content_width` of
/// text within a `viewport_width`-wide field.
#[must_use]
pub fn clamp_scroll_offset(
    offset: Pixels,
    content_width: Pixels,
    viewport_width: Pixels,
) -> Pixels {
    offset.clamp(
        Pixels::ZERO,
        max_scroll_offset(content_width, viewport_width),
    )
}

/// The scroll offset that keeps a caret painted at unshifted x-position
/// `caret_x` within `[FIELD_CARET_SCROLL_MARGIN, viewport_width -
/// FIELD_CARET_SCROLL_MARGIN]`, moving `offset` the minimal distance needed
/// -- or leaving it untouched if the caret already sits in that band. Also
/// re-clamps `offset` against the current `content_width`/`viewport_width`,
/// so it never returns a stale offset left over from before an edit or a
/// resize. Always zero once the content already fits the viewport.
#[must_use]
pub fn follow_caret(
    offset: Pixels,
    caret_x: Pixels,
    content_width: Pixels,
    viewport_width: Pixels,
) -> Pixels {
    let max_offset = max_scroll_offset(content_width, viewport_width);
    if max_offset <= Pixels::ZERO {
        return Pixels::ZERO;
    }

    let margin = FIELD_CARET_SCROLL_MARGIN;
    let visible_x = caret_x - offset;
    let followed = if visible_x < margin {
        caret_x - margin
    } else if visible_x > viewport_width - margin {
        caret_x - (viewport_width - margin)
    } else {
        offset
    };
    followed.clamp(Pixels::ZERO, max_offset)
}

#[cfg(test)]
mod tests {
    use gpui::px;

    use super::{FIELD_CARET_SCROLL_MARGIN, clamp_scroll_offset, follow_caret, max_scroll_offset};

    // -- max_scroll_offset / clamp_scroll_offset --------------------------

    #[test]
    fn max_scroll_offset_is_zero_when_content_fits_the_viewport() {
        assert_eq!(max_scroll_offset(px(50.0), px(200.0)), px(0.0));
        assert_eq!(max_scroll_offset(px(200.0), px(200.0)), px(0.0));
    }

    #[test]
    fn max_scroll_offset_is_the_overflow_past_the_viewport() {
        assert_eq!(max_scroll_offset(px(500.0), px(200.0)), px(300.0));
    }

    #[test]
    fn clamp_scroll_offset_pulls_a_negative_offset_up_to_zero() {
        assert_eq!(
            clamp_scroll_offset(px(-10.0), px(500.0), px(200.0)),
            px(0.0)
        );
    }

    #[test]
    fn clamp_scroll_offset_pulls_an_overlarge_offset_down_to_the_max() {
        assert_eq!(
            clamp_scroll_offset(px(900.0), px(500.0), px(200.0)),
            px(300.0)
        );
    }

    #[test]
    fn clamp_scroll_offset_leaves_a_valid_offset_untouched() {
        assert_eq!(
            clamp_scroll_offset(px(120.0), px(500.0), px(200.0)),
            px(120.0)
        );
    }

    // -- follow_caret: content fits, always zero --------------------------

    #[test]
    fn follow_caret_stays_zero_when_content_fits_regardless_of_caret_position() {
        assert_eq!(
            follow_caret(px(0.0), px(190.0), px(200.0), px(200.0)),
            px(0.0)
        );
        assert_eq!(follow_caret(px(0.0), px(0.0), px(50.0), px(200.0)), px(0.0));
    }

    // -- follow_caret: overflowing content ---------------------------------

    #[test]
    fn follow_caret_leaves_the_offset_untouched_when_the_caret_is_already_visible() {
        // content 500px, viewport 200px, offset 100 -> visible band is
        // [100+margin, 300-margin]; a caret at 200 sits well inside it.
        let offset = follow_caret(px(100.0), px(200.0), px(500.0), px(200.0));
        assert_eq!(offset, px(100.0));
    }

    #[test]
    fn follow_caret_scrolls_right_the_minimal_distance_when_the_caret_passes_the_right_margin() {
        // Typing to the right: caret at 320, viewport 200, current offset 100
        // puts the caret at unshifted-visible x 220, past the right margin.
        let offset = follow_caret(px(100.0), px(320.0), px(500.0), px(200.0));
        let expected = px(320.0) - (px(200.0) - FIELD_CARET_SCROLL_MARGIN);
        assert_eq!(offset, expected);
        // The caret now paints exactly at the right margin.
        assert_eq!(px(320.0) - offset, px(200.0) - FIELD_CARET_SCROLL_MARGIN);
    }

    #[test]
    fn follow_caret_scrolls_left_the_minimal_distance_when_the_caret_passes_the_left_margin() {
        // Navigating left: caret at 80, current offset 150 puts the caret at
        // unshifted-visible x -70, well past the left margin.
        let offset = follow_caret(px(150.0), px(80.0), px(500.0), px(200.0));
        let expected = px(80.0) - FIELD_CARET_SCROLL_MARGIN;
        assert_eq!(offset, expected);
        assert_eq!(px(80.0) - offset, FIELD_CARET_SCROLL_MARGIN);
    }

    #[test]
    fn follow_caret_at_the_content_start_clamps_to_zero() {
        // Home: caret at 0 always resolves to offset 0, regardless of the
        // offset it started from.
        let offset = follow_caret(px(250.0), px(0.0), px(500.0), px(200.0));
        assert_eq!(offset, px(0.0));
    }

    #[test]
    fn follow_caret_at_the_content_end_clamps_to_the_max_offset() {
        // End: caret at content_width always resolves to the max offset, so
        // the content's right edge sits at the viewport's right edge.
        let content_width = px(500.0);
        let viewport_width = px(200.0);
        let offset = follow_caret(px(0.0), content_width, content_width, viewport_width);
        assert_eq!(offset, max_scroll_offset(content_width, viewport_width));
    }

    #[test]
    fn follow_caret_re_clamps_a_stale_offset_that_still_overflows_but_less_than_before() {
        // The caret sits comfortably inside the visible band under the
        // stale offset, so only the trailing clamp -- not the caret-follow
        // branch -- pulls the offset back down to the new, smaller max.
        let stale_offset = px(280.0); // valid before the content shrank
        let offset = follow_caret(stale_offset, px(300.0), px(300.0), px(200.0));
        assert_eq!(offset, max_scroll_offset(px(300.0), px(200.0)));
    }

    #[test]
    fn follow_caret_re_clamps_a_stale_offset_after_content_shrinks() {
        // The caret sits well inside the (now much narrower) content, so the
        // caret-follow branch is not what fixes this -- the final clamp is.
        let stale_offset = px(280.0); // valid for a since-deleted wider value
        let offset = follow_caret(stale_offset, px(20.0), px(60.0), px(200.0));
        assert_eq!(
            offset,
            px(0.0),
            "content now fits entirely, so the offset must snap back to zero"
        );
    }
}
