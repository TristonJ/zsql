//! The theme's color roles: the semantic palette every view paints with,
//! plus alpha-blend/mix derivations computed from those roles at read time
//! so a re-themed base color always carries its washes and outlines along
//! with it.

/// The active theme's semantic color roles, plus alpha-blend/mix
/// derivations computed from them.
///
/// Every field is a plain `0xRRGGBB` (opaque) or `0xRRGGBBAA` (carrying its
/// own alpha) literal, ready for `gpui::rgb`/`gpui::rgba`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colors {
    /// Window/page background.
    pub bg_app: u32,
    /// Bars (title bar, results header bar) background.
    pub bg_panel: u32,
    /// Raised surfaces: the column-header row background.
    pub bg_raised: u32,
    /// Overlay surfaces: context menus and other floating panels raised
    /// above [`Colors::bg_raised`].
    pub bg_overlay: u32,
    /// Text-input field background.
    pub bg_input: u32,
    /// Standard hairline border color.
    pub border: u32,
    /// A softer hairline, used between body cells and header columns.
    pub border_soft: u32,
    /// Primary text color.
    pub text_primary: u32,
    /// Secondary/muted text (labels, timestamps).
    pub text_secondary: u32,
    /// Faint text: NULLs, row numbers, disabled-ish labels.
    pub text_tertiary: u32,
    /// Accent color: row counts, active affordances, the Run button.
    pub accent: u32,
    /// Text color painted on top of a solid [`Colors::accent`] fill.
    pub accent_contrast: u32,
    /// Boolean cell text.
    pub value_bool: u32,
    /// Raw-bytes cell text.
    pub value_bytes: u32,
    /// JSON/JSONB cell text.
    pub value_json: u32,
    /// NULL cell text.
    pub value_null: u32,
    /// Numeric cell text.
    pub value_number: u32,
    /// Plain text cell text.
    pub value_text: u32,
    /// Timestamp cell text.
    pub value_timestamp: u32,
    /// Fallback/attention color for values that do not map to a more
    /// specific kind (arrays, unmapped backend types).
    pub value_unknown: u32,
    /// Syntax highlight color for a keyword span.
    pub syntax_keyword: u32,
    /// Syntax highlight color for a string-literal span.
    pub syntax_string: u32,
    /// Status color: a connection attempt in flight, or a partial/degraded
    /// state.
    pub status_warn: u32,
    /// Status color: a failed connection or query.
    pub status_error: u32,
    /// Status color: a query result truncated at the configured row limit.
    pub status_limited: u32,
    /// View relation-kind tint.
    pub kind_view: u32,
    /// Materialized-view relation-kind tint.
    pub kind_matview: u32,
    /// Partitioned-table relation-kind tint.
    pub kind_partitioned: u32,
    /// Foreign-key hue: the schema view's FK rail tick and link chip.
    pub key_fk: u32,
    /// Faint wash painted under a hovered row.
    pub hover_wash: u32,
    /// Dimming scrim behind a modal.
    pub scrim: u32,
    /// Shadow color cast by a dialog/modal panel.
    pub shadow_dialog: u32,
    /// Shadow color cast by a floating overlay (e.g. a context menu).
    pub shadow_overlay: u32,
    /// Resting fill of a scrollbar thumb.
    pub scrollbar_thumb: u32,
    /// Hovered fill of a scrollbar thumb.
    pub scrollbar_thumb_hover: u32,
}

impl Default for Colors {
    /// The `zsql Dark` palette: the app's original, only color set.
    fn default() -> Self {
        Self {
            bg_app: 0x10_12_17,
            bg_panel: 0x16_19_22,
            bg_raised: 0x1c_20_29,
            bg_overlay: 0x20_24_2e,
            bg_input: 0x10_12_17,
            border: 0x2a_2f_3b,
            border_soft: 0x22_26_2f,
            text_primary: 0xdb_e0_ea,
            text_secondary: 0x87_8e_9f,
            text_tertiary: 0x59_60_6f,
            accent: 0x33_c2_ac,
            accent_contrast: 0xd7_f5_ef,
            value_bool: 0xd9_a2_5a,
            value_bytes: 0x2b_85_79,
            value_json: 0x9f_b4_d8,
            value_null: 0x59_60_6f,
            value_number: 0xcf_9b_e8,
            value_text: 0xdb_e0_ea,
            value_timestamp: 0x87_8e_9f,
            value_unknown: 0xe2_6d_78,
            syntax_keyword: 0x7f_9c_ff,
            syntax_string: 0xd9_a2_5a,
            status_warn: 0xd9_a2_5a,
            status_error: 0xe2_6d_78,
            status_limited: 0xe8_a1_3a,
            kind_view: 0x4d_9c_e0,
            kind_matview: 0xe8_b1_3a,
            kind_partitioned: 0x8b_7f_d6,
            key_fk: 0x7f_9c_ff,
            hover_wash: 0xff_ff_ff_05,
            scrim: 0x08_09_0c_9e,
            shadow_dialog: 0x00_00_00_99,
            shadow_overlay: 0x00_00_00_99,
            scrollbar_thumb: 0x59_60_6f_66,
            scrollbar_thumb_hover: 0x59_60_6f_99,
        }
    }
}

/// An opaque `0xRRGGBB` color's individual channels.
const fn channels(rgb: u32) -> (u8, u8, u8) {
    (
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    )
}

/// Blend two opaque `0xRRGGBB` colors, weighting `a` at `a_pct` percent and
/// `b` at the remainder, per channel, rounded to the nearest integer. A
/// weight above 100 is clamped to 100 (all `a`), so the complementary weight
/// can never underflow.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a weighted average of two u8 channels by a 0..=100 percent always stays in 0..=255"
)]
const fn mix(a: u32, b: u32, a_pct: u32) -> u32 {
    let a_pct = if a_pct > 100 { 100 } else { a_pct };
    let (ar, ag, ab) = channels(a);
    let (br, bg, bb) = channels(b);
    let b_pct = 100 - a_pct;
    // `+ 50) / 100` rounds the weighted average to the nearest integer
    // rather than truncating it, matching `color-mix`'s rounding.
    let r = (ar as u32 * a_pct + br as u32 * b_pct + 50) / 100;
    let g = (ag as u32 * a_pct + bg as u32 * b_pct + 50) / 100;
    let bl = (ab as u32 * a_pct + bb as u32 * b_pct + 50) / 100;
    ((r as u8 as u32) << 16) | ((g as u8 as u32) << 8) | (bl as u8 as u32)
}

/// `base`'s own color at `alpha` opacity, carried as an explicit alpha
/// byte rather than a blend against a surface -- the alpha channel does the
/// blending wherever this paints.
const fn wash(base: u32, alpha: u8) -> u32 {
    let (r, g, b) = channels(base);
    ((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | alpha as u32
}

/// `base`, its channels each nudged by a fixed amount (saturating at
/// `0xff`) -- an additive lighten distinct from [`mix`]'s blend toward a
/// second color, used for a hover fill that must stay recognizably the same
/// hue rather than drift toward whatever it is mixed with.
const fn lighten(base: u32, dr: u8, dg: u8, db: u8) -> u32 {
    let (r, g, b) = channels(base);
    let r = r.saturating_add(dr);
    let g = g.saturating_add(dg);
    let b = b.saturating_add(db);
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

impl Colors {
    /// [`Colors::accent`] at 13% opacity: a faint fill for a hovered or
    /// selected row.
    #[must_use]
    pub const fn accent_wash(&self) -> u32 {
        wash(self.accent, 33)
    }

    /// [`Colors::accent`] at 20% opacity: a stronger fill than
    /// [`Colors::accent_wash`], e.g. a text selection highlight.
    #[must_use]
    pub const fn accent_wash_hover(&self) -> u32 {
        wash(self.accent, 51)
    }

    /// [`Colors::accent`] at 9% opacity: a fill softer than
    /// [`Colors::accent_wash`], e.g. an active-row highlight in a dense
    /// list.
    #[must_use]
    pub const fn accent_wash_soft(&self) -> u32 {
        wash(self.accent, 23)
    }

    /// [`Colors::accent`] at 14% opacity: a focus/selection ring.
    #[must_use]
    pub const fn accent_ring(&self) -> u32 {
        wash(self.accent, 36)
    }

    /// [`Colors::accent`] at 30% opacity: a pill/badge outline.
    #[must_use]
    pub const fn accent_outline(&self) -> u32 {
        wash(self.accent, 77)
    }

    /// [`Colors::accent`] mixed 62% toward [`Colors::bg_panel`]: a dimmer
    /// accent for a subtler affordance than the accent itself.
    #[must_use]
    pub const fn accent_dim(&self) -> u32 {
        mix(self.accent, self.bg_panel, 62)
    }

    /// [`Colors::accent`] mixed 78% toward [`Colors::text_primary`]: a
    /// brighter accent for a filled control's hover state.
    #[must_use]
    pub const fn accent_strong(&self) -> u32 {
        mix(self.accent, self.text_primary, 78)
    }

    /// [`Colors::status_error`] at 32% opacity: an error-state outline.
    #[must_use]
    pub const fn error_outline(&self) -> u32 {
        wash(self.status_error, 82)
    }

    /// [`Colors::status_error`] at 10% opacity: a faint error-state fill.
    #[must_use]
    pub const fn error_wash(&self) -> u32 {
        wash(self.status_error, 26)
    }

    /// [`Colors::status_warn`] at 32% opacity: a warn-state outline.
    #[must_use]
    pub const fn warn_outline(&self) -> u32 {
        wash(self.status_warn, 82)
    }

    /// [`Colors::key_fk`] at 30% opacity: a foreign-key chip's outline.
    #[must_use]
    pub const fn fk_outline(&self) -> u32 {
        wash(self.key_fk, 77)
    }

    /// [`Colors::key_fk`] at 7% opacity: a foreign-key chip's fill.
    #[must_use]
    pub const fn fk_wash(&self) -> u32 {
        wash(self.key_fk, 18)
    }

    /// An arbitrary alpha wash of `base` at `alpha`/255 opacity -- the
    /// general form [`Colors::accent_wash`] and its siblings are built from,
    /// exposed for a caller whose wash does not fit one of those named
    /// percentages.
    #[must_use]
    pub const fn wash(base: u32, alpha: u8) -> u32 {
        wash(base, alpha)
    }

    /// An arbitrary blend of two opaque colors, weighting `a` at `a_pct`
    /// percent -- the general form [`Colors::accent_dim`] and
    /// [`Colors::accent_strong`] are built from. `a_pct` is clamped to
    /// `0..=100`, so a weight past 100 reads as all `a`.
    #[must_use]
    pub const fn mix(a: u32, b: u32, a_pct: u32) -> u32 {
        mix(a, b, a_pct)
    }

    /// `base`, additively lightened by a fixed per-channel amount
    /// (saturating), for a hover fill that must stay recognizably the same
    /// hue.
    #[must_use]
    pub const fn lighten(base: u32, dr: u8, dg: u8, db: u8) -> u32 {
        lighten(base, dr, dg, db)
    }
}

#[cfg(test)]
mod tests {
    use super::Colors;

    /// Every base role's default value must match the palette this theme
    /// system replaced, so introducing it changes nothing on screen.
    #[test]
    fn default_palette_matches_the_original_dark_theme() {
        let colors = Colors::default();
        assert_eq!(colors.bg_app, 0x10_12_17, "was colors::INK");
        assert_eq!(colors.bg_panel, 0x16_19_22, "was colors::PANEL");
        assert_eq!(colors.bg_raised, 0x1c_20_29, "was colors::RAISE");
        assert_eq!(
            colors.bg_overlay, 0x20_24_2e,
            "the style guide's raise-2, absent from the original palette"
        );
        assert_eq!(colors.border, 0x2a_2f_3b, "was colors::LINE");
        assert_eq!(colors.border_soft, 0x22_26_2f, "was colors::LINE_SOFT");
        assert_eq!(colors.text_primary, 0xdb_e0_ea, "was colors::TEXT");
        assert_eq!(colors.text_secondary, 0x87_8e_9f, "was colors::MUTED");
        assert_eq!(colors.text_tertiary, 0x59_60_6f, "was colors::FAINT");
        assert_eq!(colors.accent, 0x33_c2_ac, "was colors::TEAL");
        assert_eq!(colors.value_bool, 0xd9_a2_5a, "was colors::BOOL");
        assert_eq!(colors.value_bytes, 0x2b_85_79, "was colors::BYTES");
        assert_eq!(colors.value_json, 0x9f_b4_d8, "was colors::JSON");
        assert_eq!(
            colors.value_null, 0x59_60_6f,
            "was colors::FAINT (Null cells)"
        );
        assert_eq!(colors.value_number, 0xcf_9b_e8, "was colors::NUMBER");
        assert_eq!(
            colors.value_text, 0xdb_e0_ea,
            "was colors::TEXT (Text cells)"
        );
        assert_eq!(
            colors.value_timestamp, 0x87_8e_9f,
            "was colors::MUTED (Timestamp cells)"
        );
        assert_eq!(colors.value_unknown, 0xe2_6d_78, "was colors::UNKNOWN");
        assert_eq!(
            colors.syntax_keyword, 0x7f_9c_ff,
            "the style guide's blue keyword hue"
        );
        assert_eq!(
            colors.syntax_string, 0xd9_a2_5a,
            "the style guide's amber string hue"
        );
        assert_eq!(
            colors.status_warn, 0xd9_a2_5a,
            "was theme::STATUS_CONNECTING"
        );
        assert_eq!(colors.status_error, 0xe2_6d_78, "was theme::STATUS_ERROR");
        assert_eq!(
            colors.status_limited, 0xe8_a1_3a,
            "was theme::STATUS_LIMITED"
        );
        assert_eq!(colors.kind_view, 0x4d_9c_e0, "was colors::VIEW");
        assert_eq!(colors.kind_matview, 0xe8_b1_3a, "was colors::MATVIEW");
        assert_eq!(
            colors.kind_partitioned, 0x8b_7f_d6,
            "was colors::PARTITIONED"
        );
        assert_eq!(colors.key_fk, 0x7f_9c_ff, "was colors::LINK");
        assert_eq!(
            colors.scrollbar_thumb, 0x59_60_6f_66,
            "was theme::SIDEBAR_SCROLLBAR_THUMB"
        );
        assert_eq!(
            colors.scrollbar_thumb_hover, 0x59_60_6f_99,
            "was theme::SIDEBAR_SCROLLBAR_THUMB_HOVER"
        );
        assert_eq!(colors.scrim, 0x08_09_0c_9e, "was theme::MODAL_BACKDROP");
    }

    /// [`Colors::value_unknown`] and [`Colors::status_error`] are separate
    /// roles that happen to share a default hue, not one role doing double
    /// duty.
    #[test]
    fn value_unknown_and_status_error_default_to_the_same_hex_as_distinct_fields() {
        let colors = Colors::default();
        assert_eq!(colors.value_unknown, colors.status_error);
    }

    /// [`Colors::status_warn`], [`Colors::status_limited`], and
    /// [`Colors::kind_matview`] are three distinct near-identical ambers,
    /// not one constant reused.
    #[test]
    fn status_warn_status_limited_and_kind_matview_are_pairwise_distinct() {
        let colors = Colors::default();
        assert_ne!(colors.status_warn, colors.status_limited);
        assert_ne!(colors.status_warn, colors.kind_matview);
        assert_ne!(colors.status_limited, colors.kind_matview);
    }

    /// Every derived-color method must reproduce the exact ARGB value of
    /// the pre-refactor baked constant it replaces.
    #[test]
    fn derived_colors_reproduce_every_pre_refactor_baked_constant() {
        let colors = Colors::default();

        // was zsql::ui::theme::SIDEBAR_SELECTED_BG-adjacent but exact via
        // accent_wash_soft: zsql::ui::theme::MODAL_ROW_ACTIVE_BG.
        assert_eq!(colors.accent_wash_soft(), 0x33_c2_ac_17);
        // was zsql_editor::theme::EDITOR_SELECTION_BG and
        // zsql_ui::text_field::theme::FIELD_SELECTION_BG.
        assert_eq!(colors.accent_wash_hover(), 0x33_c2_ac_33);
        // was zsql::ui::theme::SCHEMA_KIND_PILL_BORDER.
        assert_eq!(colors.accent_outline(), 0x33_c2_ac_4d);
        // was zsql::ui::theme::SCHEMA_BADGE_UNIQUE_BORDER.
        assert_eq!(colors.warn_outline(), 0xd9_a2_5a_52);
        // was zsql::ui::theme::SCHEMA_BADGE_LINK_BORDER.
        assert_eq!(colors.fk_outline(), 0x7f_9c_ff_4d);
        // was zsql::ui::theme::SCHEMA_BADGE_LINK_BG.
        assert_eq!(colors.fk_wash(), 0x7f_9c_ff_12);
        // was zsql::ui::theme::SIDEBAR_SELECTED_BG.
        assert_eq!(Colors::wash(colors.accent, 0x1a), 0x33_c2_ac_1a);
        // was zsql::ui::theme::GENERATED_STRIP_BG.
        assert_eq!(Colors::wash(colors.accent, 0x0b), 0x33_c2_ac_0b);
        // was zsql_ui::grid::TYPE_TAG_BORDER.
        assert_eq!(Colors::wash(colors.accent, 0x47), 0x33_c2_ac_47);
        // was zsql::ui::theme::SCHEMA_BADGE_PK_BORDER.
        assert_eq!(Colors::wash(colors.accent, 0x52), 0x33_c2_ac_52);
        // was zsql::ui::theme::RUN_BUTTON_HINT: the page ink at reduced
        // opacity.
        assert_eq!(Colors::wash(colors.bg_app, 0xb3), 0x10_12_17_b3);
        // was zsql::ui::theme::RUN_BUTTON_HOVER_BG: a lighter teal than the
        // resting accent.
        assert_eq!(Colors::lighten(colors.accent, 19, 13, 14), 0x46_cf_ba);
        // was zsql::ui::theme::GENERATED_STRIP_ACCENT (colors::TEAL_DIM),
        // which shared BYTES' hex.
        assert_eq!(colors.value_bytes, 0x2b_85_79);
    }

    /// The derived-color methods with no call site yet (reserved for a
    /// future theme consumer) must still resolve to the style guide's
    /// documented alpha/mix values, so they are correct on the day something
    /// finally reads them.
    #[test]
    fn unused_derived_colors_match_the_style_guides_alpha_and_mix_values() {
        let colors = Colors::default();

        assert_eq!(colors.accent_wash(), 0x33_c2_ac_21);
        assert_eq!(colors.accent_ring(), 0x33_c2_ac_24);
        assert_eq!(colors.error_outline(), 0xe2_6d_78_52);
        assert_eq!(colors.error_wash(), 0xe2_6d_78_1a);
        // accent mixed 62% toward bg_panel.
        assert_eq!(colors.accent_dim(), 0x28_82_78);
        // accent mixed 78% toward text_primary.
        assert_eq!(colors.accent_strong(), 0x58_c9_ba);
    }
}
