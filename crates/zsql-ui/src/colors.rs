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
/// Dimmer teal for a subtler accent than [`TEAL`] itself, e.g. a generated
/// tab's compact strip's left border -- the mockup's `--teal-dim` token.
pub const TEAL_DIM: u32 = 0x2b_85_79;
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
/// View relation-kind tint: a cool blue.
pub const VIEW: u32 = 0x4d_9c_e0;
/// Materialized-view relation-kind tint: a warm amber, distinct from
/// [`BOOL`]'s.
pub const MATVIEW: u32 = 0xe8_b1_3a;
/// Partitioned-table relation-kind tint: a violet.
pub const PARTITIONED: u32 = 0x8b_7f_d6;
/// Link/foreign-key hue: a periwinkle blue, distinct from [`VIEW`]'s cooler
/// blue. Used for the schema view's foreign-key rail tick and link chip.
pub const LINK: u32 = 0x7f_9c_ff;

#[cfg(test)]
mod tests {
    use super::{
        BOOL, BYTES, FAINT, INK, JSON, LINE, LINE_SOFT, LINK, MATVIEW, MUTED, NUMBER, PANEL,
        PARTITIONED, RAISE, TEAL, TEAL_DIM, TEXT, UNKNOWN, VIEW,
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
        assert_eq!(TEAL_DIM, 0x2b_85_79);
        assert_eq!(NUMBER, 0xcf_9b_e8);
        assert_eq!(JSON, 0x9f_b4_d8);
        assert_eq!(UNKNOWN, 0xe2_6d_78);
        assert_eq!(BOOL, 0xd9_a2_5a);
        assert_eq!(BYTES, 0x2b_85_79);
        assert_eq!(VIEW, 0x4d_9c_e0);
        assert_eq!(MATVIEW, 0xe8_b1_3a);
        assert_eq!(PARTITIONED, 0x8b_7f_d6);
        assert_eq!(LINK, 0x7f_9c_ff);
    }

    #[test]
    fn relation_kind_tints_are_pairwise_distinct() {
        let tints = [TEAL, VIEW, MATVIEW, PARTITIONED];
        for (i, a) in tints.iter().enumerate() {
            for b in &tints[i + 1..] {
                assert_ne!(a, b, "relation-kind tints must not collide");
            }
        }
        assert_ne!(MATVIEW, BOOL, "matview must not reuse BOOL's amber");
    }
}
