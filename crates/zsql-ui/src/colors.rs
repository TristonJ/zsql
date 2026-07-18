//! The base dark color palette, as `0xRRGGBB` (or `0xRRGGBBAA`) literals for
//! `gpui::rgb`/`gpui::rgba`. Centralized here so no view hardcodes a raw hex
//! literal inline.

/// Window/page background.
pub const INK: u32 = 0x10_12_17;
/// Bars (title bar, results header bar) background.
pub const PANEL: u32 = 0x16_19_22;
/// Raised surfaces: the column-header row background.
pub const RAISE: u32 = 0x1c_20_29;
/// Standard hairline border color.
pub const LINE: u32 = 0x2a_2f_3b;
/// A softer hairline, used between body cells and header columns.
pub const LINE_SOFT: u32 = 0x22_26_2f;
/// Primary text color.
pub const TEXT: u32 = 0xdb_e0_ea;
/// Secondary/muted text (labels, timestamps).
pub const MUTED: u32 = 0x87_8e_9f;
/// Faint text: NULLs, row numbers, disabled-ish labels.
pub const FAINT: u32 = 0x59_60_6f;
/// Accent color: row counts, active affordances.
pub const TEAL: u32 = 0x33_c2_ac;
/// Numeric cell text.
pub const NUMBER: u32 = 0xcf_9b_e8;
/// JSON/JSONB cell text.
pub const JSON: u32 = 0x9f_b4_d8;
/// Fallback/attention color for values that do not map to a more specific
/// kind (arrays, unmapped backend types).
pub const UNKNOWN: u32 = 0xe2_6d_78;
/// Boolean cell text.
pub const BOOL: u32 = 0xd9_a2_5a;
/// Raw-bytes cell text.
pub const BYTES: u32 = 0x2b_85_79;

#[cfg(test)]
mod tests {
    use super::{
        BOOL, BYTES, FAINT, INK, JSON, LINE, LINE_SOFT, MUTED, NUMBER, PANEL, RAISE, TEAL, TEXT,
        UNKNOWN,
    };

    #[test]
    fn palette_tokens_keep_their_locked_hex_values() {
        assert_eq!(INK, 0x10_12_17);
        assert_eq!(PANEL, 0x16_19_22);
        assert_eq!(RAISE, 0x1c_20_29);
        assert_eq!(LINE, 0x2a_2f_3b);
        assert_eq!(LINE_SOFT, 0x22_26_2f);
        assert_eq!(TEXT, 0xdb_e0_ea);
        assert_eq!(MUTED, 0x87_8e_9f);
        assert_eq!(FAINT, 0x59_60_6f);
        assert_eq!(TEAL, 0x33_c2_ac);
        assert_eq!(NUMBER, 0xcf_9b_e8);
        assert_eq!(JSON, 0x9f_b4_d8);
        assert_eq!(UNKNOWN, 0xe2_6d_78);
        assert_eq!(BOOL, 0xd9_a2_5a);
        assert_eq!(BYTES, 0x2b_85_79);
    }
}
