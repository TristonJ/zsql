//! Layout/color tokens for the single-line [`super::TextFieldState`],
//! matching the app's locked visual spec: a bordered field, a teal focus
//! ring and caret, and muted placeholder text. Centralized here so no view
//! code hardcodes a raw pixel or color literal inline.

use gpui::{Pixels, px};

/// Height of the field's outer chrome (border box).
pub const FIELD_HEIGHT: Pixels = px(34.0);
/// Horizontal padding inside the field, between the border and the text.
pub const FIELD_PADDING_X: f32 = 11.0;
/// Default vertical padding inside the field, between the border and the text
pub const FIELD_PADDING_Y: f32 = 11.0;
/// Corner radius of the field's border.
pub const FIELD_RADIUS: f32 = 7.0;
/// Height of the text line the field shapes and paints: the cursor and
/// selection quads use this as their height, and the outer chrome centers
/// this within [`FIELD_HEIGHT`].
pub const FIELD_LINE_HEIGHT: Pixels = px(16.0);
/// Font size of the field's text, placeholder, and IME marked-text.
pub const FIELD_TEXT_SIZE: f32 = 13.0;
/// Width of the blinking text cursor.
pub const FIELD_CURSOR_WIDTH: Pixels = px(2.0);
/// Minimum distance kept between the caret and the field's viewport edge
/// when scrolled content is caret-followed: the caret never paints glued to
/// the field's inner border.
pub const FIELD_CARET_SCROLL_MARGIN: Pixels = px(6.0);
