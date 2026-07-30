//! Layout, color, and limit tokens for the SQL editor pane, matching the
//! app's locked visual spec. Centralized here so no view or buffer code
//! hardcodes a raw pixel, color, or limit literal inline.

use gpui::{Pixels, px};
use zsql_ui::theme::Theme;

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
/// Maximum number of undo groups `TextBuffer` retains. The oldest group is
/// evicted once a new one would exceed this, bounding memory growth across
/// an arbitrarily long editing session.
pub const EDITOR_HISTORY_CAP: usize = 200;

/// The active selection highlight's background: the theme's accent wash.
#[must_use]
pub fn selection_bg(theme: &Theme) -> gpui::Rgba {
    theme.colors.accent_wash_hover()
}

// -- syntax highlighting -----------------------------------------------
//
// One color per `crate::HighlightKind`, matching the style guide's syntax
// roles: a keyword takes the blue link hue, a called function's name takes
// the accent, and literal/comment/operator/punctuation spans reuse the same
// theme roles the results grid colors matching value kinds with.
// `Identifier` deliberately matches the editor's own base text color, so a
// plain identifier reads as unstyled.

/// The color a `HighlightKind` paints its spans with.
#[must_use]
pub fn syntax_color(theme: &Theme, kind: crate::HighlightKind) -> u32 {
    use crate::HighlightKind::{
        Comment, Function, Identifier, Keyword, Number, Operator, Punctuation, String,
    };
    let colors = &theme.colors;
    match kind {
        Keyword => colors.syntax_keyword,
        String => colors.syntax_string,
        Number => colors.value_number,
        Comment | Punctuation => colors.text_tertiary,
        Function => colors.accent,
        Operator => colors.text_secondary,
        Identifier => colors.text_primary,
    }
}

#[cfg(test)]
mod tests {
    use super::syntax_color;
    use crate::HighlightKind;
    use zsql_ui::theme::Theme;

    /// Each `HighlightKind` maps to its own named role, not the role another
    /// kind also happens to read -- pinned against the role fields directly
    /// (not by re-deriving the same match this function itself contains) so
    /// a future edit that swaps two arms is caught here.
    #[test]
    fn every_highlight_kind_maps_to_its_documented_role() {
        let theme = Theme::default();
        let colors = &theme.colors;
        assert_eq!(
            syntax_color(&theme, HighlightKind::Keyword),
            colors.syntax_keyword
        );
        assert_eq!(
            colors.syntax_keyword, 0x7f_9c_ff,
            "the style guide's blue link role"
        );
        assert_eq!(syntax_color(&theme, HighlightKind::Function), colors.accent);
        assert_eq!(
            syntax_color(&theme, HighlightKind::String),
            colors.syntax_string
        );
        assert_eq!(
            syntax_color(&theme, HighlightKind::Number),
            colors.value_number
        );
        assert_eq!(
            syntax_color(&theme, HighlightKind::Comment),
            colors.text_tertiary
        );
        assert_eq!(
            syntax_color(&theme, HighlightKind::Punctuation),
            colors.text_tertiary
        );
        assert_eq!(
            syntax_color(&theme, HighlightKind::Operator),
            colors.text_secondary
        );
        assert_eq!(
            syntax_color(&theme, HighlightKind::Identifier),
            colors.text_primary
        );
    }
}
