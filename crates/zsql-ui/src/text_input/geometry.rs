//! Pure caret/selection paint-quad geometry: takes pre-resolved pixel
//! positions and line metrics, returns `gpui` paint quads. No `Window`,
//! `App`, or `Element` coupling -- a consumer's `Element::prepaint` computes
//! the inputs (shaped-line x-offsets, which lines a selection spans) from
//! its own buffer/model and shaped lines, then calls into this module to
//! turn that geometry into quads.

use gpui::{Background, Bounds, PaintQuad, Pixels, fill, point, size};

/// The pixel y-offset of `line_index`'s top edge, within an element whose
/// content starts at `origin`.
// Line indices are always small (an editor pane or a single-line field, not
// a huge document), so this `usize -> f32` conversion cannot lose
// meaningful precision.
#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn line_top(origin: Pixels, line_height: Pixels, line_index: usize) -> Pixels {
    origin + line_height * line_index as f32
}

/// One line's horizontal selection extent to paint: the pixel x-span to
/// fill on `line_index`, in element-local coordinates (i.e. relative to the
/// content element's left edge).
pub struct SelectionLineSpan {
    pub line_index: usize,
    pub start_x: Pixels,
    pub end_x: Pixels,
}

/// One selection span's fill quad at element-local coordinates.
pub fn selection_quad(
    span: &SelectionLineSpan,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    color: impl Into<Background>,
) -> PaintQuad {
    let top = line_top(bounds.top(), line_height, span.line_index);
    fill(
        Bounds::from_corners(
            point(bounds.left() + span.start_x, top),
            point(bounds.left() + span.end_x, top + line_height),
        ),
        color,
    )
}

/// Fill quads for a selection's per-line spans, one quad per span.
pub fn selection_quads(
    spans: &[SelectionLineSpan],
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    color: impl Into<Background> + Copy,
) -> Vec<PaintQuad> {
    spans
        .iter()
        .map(|span| selection_quad(span, bounds, line_height, color))
        .collect()
}

/// A caret's fill quad at element-local x-offset `x` on `line_index`.
pub fn caret_quad(
    bounds: Bounds<Pixels>,
    x: Pixels,
    line_index: usize,
    line_height: Pixels,
    width: Pixels,
    color: impl Into<Background>,
) -> PaintQuad {
    let top = line_top(bounds.top(), line_height, line_index);
    fill(
        Bounds::new(point(bounds.left() + x, top), size(width, line_height)),
        color,
    )
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, Pixels, Point, Rgba, point, px, size};

    use super::{SelectionLineSpan, caret_quad, line_top, selection_quad, selection_quads};

    fn bounds() -> Bounds<Pixels> {
        Bounds::new(point(px(10.0), px(20.0)), size(px(300.0), px(200.0)))
    }

    #[test]
    fn line_top_offsets_by_line_index_times_line_height() {
        let origin = px(20.0);
        let line_height = px(21.0);
        assert_eq!(line_top(origin, line_height, 0), origin);
        assert_eq!(line_top(origin, line_height, 3), origin + px(63.0));
    }

    #[test]
    fn selection_quads_builds_one_quad_per_span_at_the_right_bounds() {
        let color: Rgba = gpui::rgb(0x0033_6699);
        let spans = [
            SelectionLineSpan {
                line_index: 0,
                start_x: px(5.0),
                end_x: px(40.0),
            },
            SelectionLineSpan {
                line_index: 1,
                start_x: px(0.0),
                end_x: px(100.0),
            },
        ];
        let quads = selection_quads(&spans, bounds(), px(21.0), color);
        assert_eq!(quads.len(), 2);
        assert_eq!(
            quads[0].bounds,
            Bounds::from_corners(point(px(15.0), px(20.0)), point(px(50.0), px(41.0)),)
        );
        assert_eq!(
            quads[1].bounds,
            Bounds::from_corners(point(px(10.0), px(41.0)), point(px(110.0), px(62.0)),)
        );
    }

    #[test]
    fn selection_quad_builds_a_single_span_quad_at_the_right_bounds() {
        let color: Rgba = gpui::rgb(0x0033_6699);
        let span = SelectionLineSpan {
            line_index: 1,
            start_x: px(0.0),
            end_x: px(100.0),
        };
        let quad = selection_quad(&span, bounds(), px(21.0), color);
        assert_eq!(
            quad.bounds,
            Bounds::from_corners(point(px(10.0), px(41.0)), point(px(110.0), px(62.0)),)
        );
    }

    #[test]
    fn caret_quad_is_a_thin_column_at_the_given_x_and_line() {
        let color: Rgba = gpui::rgb(0x00ff_0000);
        let quad = caret_quad(bounds(), px(30.0), 2, px(21.0), px(2.0), color);
        let expected_top: Point<Pixels> = point(px(40.0), px(62.0));
        assert_eq!(quad.bounds.origin, expected_top);
        assert_eq!(quad.bounds.size, size(px(2.0), px(21.0)));
    }
}
