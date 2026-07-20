//! Pure column-width estimation, shared by any table's header/body sizing.
//! Every function here takes only primitives and a [`TableStyle`] -- no
//! caller domain type -- so a column's estimated width can be computed
//! before any of its cells are actually laid out.

use gpui::{Pixels, px};

use super::style::TableStyle;

/// Approximate advance width, in pixels, of one monospace glyph at a
/// default row-number gutter's text size, used to size [`super::Gutter::RowNumbers`]
/// from its widest digit count.
pub const DEFAULT_ROW_NUMBER_CHAR_WIDTH: f32 = 7.2;
/// Narrowest a default row-number gutter is ever allowed to shrink to.
pub const DEFAULT_ROW_NUMBER_MIN_WIDTH: f32 = 80.0;

/// Bounds for [`column_width`]'s estimate.
#[derive(Debug, Clone, Copy)]
pub struct ColumnWidthLimits {
    /// One glyph's advance width at the table's text size.
    pub char_width: f32,
    /// Extra fixed header-chrome width beyond the header text itself, e.g.
    /// a type-name badge.
    pub header_extra_width: f32,
    /// Narrowest a column is ever allowed to shrink to.
    pub min_width: f32,
    /// Widest a column is ever allowed to grow to.
    pub max_width: f32,
}

/// A column's pixel width estimated from its header's character count and
/// the longest formatted body value seen so far, clamped to
/// `[limits.min_width, limits.max_width]`.
///
/// # Panics
///
/// Panics if `limits.min_width` exceeds `limits.max_width`, in any build:
/// `f32::clamp` rejects an inverted range unconditionally. A debug build
/// reports it with the field names attached.
// Character counts here are always small (column names, formatted scalar
// values), so the `usize -> f32` conversions below cannot lose meaningful
// precision.
#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn column_width(
    header_chars: usize,
    max_body_chars: usize,
    style: &TableStyle,
    limits: ColumnWidthLimits,
) -> Pixels {
    debug_assert!(
        limits.min_width <= limits.max_width,
        "ColumnWidthLimits.min_width ({}) exceeds max_width ({})",
        limits.min_width,
        limits.max_width,
    );
    let padding = f32::from(style.cell_padding_x) * 2.0;
    let header_width =
        padding + header_chars as f32 * limits.char_width + limits.header_extra_width;
    let body_width = padding + max_body_chars as f32 * limits.char_width;
    px(header_width
        .max(body_width)
        .clamp(limits.min_width, limits.max_width))
}

/// A row-number gutter's pixel width, wide enough for the largest row
/// number in a result of `row_count` rows, clamped at `min_width`.
#[allow(clippy::cast_precision_loss)]
#[must_use]
pub fn row_number_column_width(
    row_count: usize,
    style: &TableStyle,
    char_width: f32,
    min_width: f32,
) -> Pixels {
    let digits = row_count.to_string().chars().count().max(1);
    let width = f32::from(style.cell_padding_x) * 2.0 + digits as f32 * char_width;
    px(width.max(min_width))
}

#[cfg(test)]
mod tests {
    use gpui::px;

    use super::{
        ColumnWidthLimits, DEFAULT_ROW_NUMBER_CHAR_WIDTH, DEFAULT_ROW_NUMBER_MIN_WIDTH, TableStyle,
        column_width, row_number_column_width,
    };

    const LIMITS: ColumnWidthLimits = ColumnWidthLimits {
        char_width: 7.2,
        header_extra_width: 34.0,
        min_width: 90.0,
        max_width: 320.0,
    };

    #[test]
    fn width_grows_with_longer_body_content() {
        let style = TableStyle::default();
        let narrow = column_width(2, 2, &style, LIMITS);
        let wide = column_width(2, 40, &style, LIMITS);
        assert!(f32::from(wide) > f32::from(narrow));
    }

    #[test]
    fn width_is_driven_by_a_long_header_even_with_no_body_content() {
        let style = TableStyle::default();
        let short_header = column_width(2, 0, &style, LIMITS);
        let long_header = column_width(30, 0, &style, LIMITS);
        assert!(
            f32::from(long_header) > f32::from(short_header),
            "a longer header must grow the column width even when the body contributes nothing"
        );
    }

    #[test]
    #[should_panic(expected = "exceeds max_width")]
    fn inverted_limits_are_rejected() {
        let style = TableStyle::default();
        let inverted = ColumnWidthLimits {
            min_width: 200.0,
            max_width: 100.0,
            ..LIMITS
        };
        let _ = column_width(2, 2, &style, inverted);
    }

    #[test]
    fn wider_cell_padding_widens_a_column() {
        let padded = TableStyle {
            cell_padding_x: px(20.0),
            ..TableStyle::default()
        };
        let narrow = column_width(6, 6, &TableStyle::default(), LIMITS);
        let wide = column_width(6, 6, &padded, LIMITS);
        assert!(
            f32::from(wide) > f32::from(narrow),
            "cell_padding_x contributes twice (both edges) to a column's estimate, so raising \
             it must widen the column rather than being read from a literal"
        );
    }

    #[test]
    fn wider_cell_padding_widens_the_row_number_column() {
        let padded = TableStyle {
            cell_padding_x: px(20.0),
            ..TableStyle::default()
        };
        // A zero floor, so the assertion is about the padding rather than
        // both sides being clamped up to the same minimum.
        let narrow = row_number_column_width(1_000, &TableStyle::default(), 7.2, 0.0);
        let wide = row_number_column_width(1_000, &padded, 7.2, 0.0);
        assert!(
            f32::from(wide) > f32::from(narrow),
            "the gutter's width is padding plus digits, so raising cell_padding_x must widen it"
        );
    }

    #[test]
    fn width_clamps_at_the_configured_minimum() {
        let style = TableStyle::default();
        let width = column_width(1, 0, &style, LIMITS);
        assert!(f32::from(width) >= LIMITS.min_width);
    }

    #[test]
    // `clamp`'s ceiling arm returns `LIMITS.max_width` verbatim (no further
    // arithmetic on it), so an exact comparison here is intentional.
    #[allow(clippy::float_cmp)]
    fn width_clamps_at_the_configured_maximum() {
        let style = TableStyle::default();
        let width = column_width(2, 5_000, &style, LIMITS);
        assert_eq!(f32::from(width), LIMITS.max_width);
    }

    #[test]
    // `.max()` returns `DEFAULT_ROW_NUMBER_MIN_WIDTH` verbatim when it wins
    // (no further arithmetic on it), so an exact comparison here is
    // intentional.
    #[allow(clippy::float_cmp)]
    fn row_number_width_for_zero_rows_clamps_at_the_minimum() {
        let style = TableStyle::default();
        let width = row_number_column_width(
            0,
            &style,
            DEFAULT_ROW_NUMBER_CHAR_WIDTH,
            DEFAULT_ROW_NUMBER_MIN_WIDTH,
        );
        assert_eq!(f32::from(width), DEFAULT_ROW_NUMBER_MIN_WIDTH);
    }

    #[test]
    fn row_number_width_grows_with_digit_count() {
        let style = TableStyle::default();
        let small = row_number_column_width(
            9,
            &style,
            DEFAULT_ROW_NUMBER_CHAR_WIDTH,
            DEFAULT_ROW_NUMBER_MIN_WIDTH,
        );
        // The smallest row count whose digit count actually clears
        // `DEFAULT_ROW_NUMBER_MIN_WIDTH`'s floor, so this is the smallest
        // count that demonstrably grows past `small`.
        let large = row_number_column_width(
            100_000_000,
            &style,
            DEFAULT_ROW_NUMBER_CHAR_WIDTH,
            DEFAULT_ROW_NUMBER_MIN_WIDTH,
        );
        assert!(f32::from(large) > f32::from(small));
    }

    #[test]
    // `.max()` returns `DEFAULT_ROW_NUMBER_MIN_WIDTH` verbatim when it wins
    // (no further arithmetic on it), so an exact comparison here is
    // intentional.
    #[allow(clippy::float_cmp)]
    fn row_number_width_clamps_at_the_minimum_for_small_row_counts() {
        let style = TableStyle::default();
        let width = row_number_column_width(
            9,
            &style,
            DEFAULT_ROW_NUMBER_CHAR_WIDTH,
            DEFAULT_ROW_NUMBER_MIN_WIDTH,
        );
        assert_eq!(f32::from(width), DEFAULT_ROW_NUMBER_MIN_WIDTH);
    }
}
