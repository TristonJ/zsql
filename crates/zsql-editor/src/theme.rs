//! Layout/color tokens for the SQL editor pane, matching the app's locked
//! visual spec. Centralized here so no view code hardcodes a raw pixel or
//! color literal inline.

use gpui::{Pixels, px};

/// Height of the editor toolbar that holds the Run button.
pub const EDITOR_TOOLBAR_HEIGHT: Pixels = px(38.0);
/// Horizontal padding inside the editor toolbar.
pub const EDITOR_TOOLBAR_PADDING_X: f32 = 10.0;
/// Text size of the toolbar's left-hand pane label.
pub const EDITOR_TOOLBAR_LABEL_TEXT_SIZE: f32 = 10.5;
/// Height of the Run button.
pub const RUN_BUTTON_HEIGHT: Pixels = px(25.0);
/// Horizontal padding inside the Run button.
pub const RUN_BUTTON_PADDING_X: f32 = 11.0;
/// Corner radius of the Run button.
pub const RUN_BUTTON_RADIUS: f32 = 5.0;
/// Text size of the Run button's label.
pub const RUN_BUTTON_TEXT_SIZE: f32 = 11.5;
/// Size of the Run button's play icon.
pub const RUN_BUTTON_ICON_SIZE: Pixels = px(12.0);
/// Text size of the Run button's keyboard-shortcut hint.
pub const RUN_BUTTON_HINT_TEXT_SIZE: f32 = 10.0;
/// Run button background when hovered: a lighter teal than the resting accent.
pub const RUN_BUTTON_HOVER_BG: u32 = 0x46_cf_ba;
/// Run button shortcut-hint color: the page ink at reduced opacity, so it
/// reads as secondary against the teal fill.
pub const RUN_BUTTON_HINT: u32 = 0x10_12_17_b3;
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
