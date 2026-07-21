use std::borrow::Cow;

pub const DEFAULT_FONT_DATA: &str = "JetBrains Mono";
pub const DEFAULT_FONT_UI: &str = "IBM Plex Sans";

/// The fonts used by the UI - organized by role
#[derive(Debug, Clone, PartialEq)]
pub struct Fonts {
    /// Font used for data-like elements (code, table cells, etc.)
    pub data: String,
    /// Font used for UI-elements (buttons, labels, etc.)
    pub ui: String,
}

impl Default for Fonts {
    fn default() -> Self {
        Self {
            data: DEFAULT_FONT_DATA.into(),
            ui: DEFAULT_FONT_UI.into(),
        }
    }
}

/// Return the bytes to enable the built-in fonts
#[must_use]
pub fn get_builtin_fonts() -> Vec<Cow<'static, [u8]>> {
    vec![
        Cow::Borrowed(include_bytes!(
            "../../assets/fonts/IBMPlexSans-VariableFont_wdth,wght.ttf"
        )),
        Cow::Borrowed(include_bytes!(
            "../../assets/fonts/IBMPlexSans-Italic-VariableFont_wdth,wght.ttf"
        )),
        Cow::Borrowed(include_bytes!(
            "../../assets/fonts/JetBrainsMono-VariableFont_wght.ttf"
        )),
        Cow::Borrowed(include_bytes!(
            "../../assets/fonts/JetBrainsMono-Italic-VariableFont_wght.ttf"
        )),
    ]
}
