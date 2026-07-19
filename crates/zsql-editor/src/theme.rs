//! Layout/color tokens for the SQL editor pane, matching the app's locked
//! visual spec. Centralized here so no view code hardcodes a raw pixel or
//! color literal inline.

use gpui::{Pixels, px};
use zsql_ui::colors;

/// Font size of the editor's text and gutter line numbers.
pub const EDITOR_TEXT_SIZE: f32 = 13.0;
/// Height of each row of editor text, content plus line spacing.
pub const EDITOR_LINE_HEIGHT: f32 = 21.0;
/// Width of the line-number gutter.
pub const EDITOR_GUTTER_WIDTH: Pixels = px(44.0);
/// Horizontal padding inside a gutter line-number cell.
pub const EDITOR_GUTTER_PADDING_X: f32 = 12.0;
/// Horizontal padding inside the editor's text area.
pub const EDITOR_PADDING_X: f32 = 14.0;
/// Vertical padding above the first line and below the last line of the
/// editor's text area.
pub const EDITOR_PADDING_Y: f32 = 12.0;
/// Width of the text cursor.
pub const EDITOR_CURSOR_WIDTH: Pixels = px(2.0);
/// How far a selection highlight extends past the end of a line that isn't
/// the selection's last line, so a selected line break reads as selected
/// too instead of stopping abruptly at the last character.
pub const EDITOR_SELECTION_EOL_PAD: f32 = 8.0;
/// Background tint for the active selection highlight: teal at low opacity
/// (`0x33c2ac` at ~20% alpha).
pub const EDITOR_SELECTION_BG: u32 = 0x33_c2_ac_33;

// -- syntax highlighting -----------------------------------------------
//
// One color per `crate::HighlightKind`, each reused from the app's existing
// locked palette rather than a new hue: keywords get the app's single teal
// accent, a called function's name gets the dimmer teal already used for
// secondary accents, and literal/comment/operator/punctuation spans reuse
// the same tokens the results grid already colors matching value kinds
// with. `Identifier` deliberately matches the editor's own base text color,
// so a plain identifier reads as unstyled.

/// Color for a `HighlightKind::Keyword` span.
pub const SYNTAX_KEYWORD: u32 = colors::TEAL;
/// Color for a `HighlightKind::Function` span.
pub const SYNTAX_FUNCTION: u32 = colors::TEAL_DIM;
/// Color for a `HighlightKind::Number` span.
pub const SYNTAX_NUMBER: u32 = colors::NUMBER;
/// Color for a `HighlightKind::String` span.
pub const SYNTAX_STRING: u32 = colors::JSON;
/// Color for a `HighlightKind::Comment` span.
pub const SYNTAX_COMMENT: u32 = colors::FAINT;
/// Color for a `HighlightKind::Operator` span.
pub const SYNTAX_OPERATOR: u32 = colors::MUTED;
/// Color for a `HighlightKind::Punctuation` span.
pub const SYNTAX_PUNCTUATION: u32 = colors::FAINT;
/// Color for a `HighlightKind::Identifier` span: the editor's base text
/// color, so a plain identifier reads as unstyled.
pub const SYNTAX_IDENTIFIER: u32 = colors::TEXT;

/// The color a `HighlightKind` paints its spans with.
#[must_use]
pub fn syntax_color(kind: crate::HighlightKind) -> u32 {
    use crate::HighlightKind::{
        Comment, Function, Identifier, Keyword, Number, Operator, Punctuation, String,
    };
    match kind {
        Keyword => SYNTAX_KEYWORD,
        String => SYNTAX_STRING,
        Number => SYNTAX_NUMBER,
        Comment => SYNTAX_COMMENT,
        Function => SYNTAX_FUNCTION,
        Operator => SYNTAX_OPERATOR,
        Identifier => SYNTAX_IDENTIFIER,
        Punctuation => SYNTAX_PUNCTUATION,
    }
}
