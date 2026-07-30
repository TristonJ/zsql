//! The theme's color roles: the semantic palette every view paints with,
//! plus alpha-blend/mix derivations computed from those roles at read time
//! so a re-themed base color always carries its washes and outlines along
//! with it.

use gpui::Rgba;
use serde::Deserialize;

/// The active theme's semantic color roles, plus alpha-blend/mix
/// derivations computed from them.
///
/// Every field is a plain `0xRRGGBB` (opaque) or `0xRRGGBBAA` (carrying its
/// own alpha) literal, ready for `gpui::rgb`/`gpui::rgba`.
///
/// Deserializes from a JSON object whose keys are these field names and
/// whose values are `#`-prefixed hex color strings -- 6 digits for an
/// opaque role, 8 for a role that carries its own alpha. Every field is
/// optional and falls back to [`Colors::default`]'s value for that field
/// when absent, so a file overriding a single role is a valid theme. An
/// unrecognized key is a deserialization error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Colors {
    /// Window/page background.
    #[serde(default = "default_bg_app", deserialize_with = "parse_rgb_hex")]
    pub bg_app: u32,
    /// Bars (title bar, results header bar) background.
    #[serde(default = "default_bg_panel", deserialize_with = "parse_rgb_hex")]
    pub bg_panel: u32,
    /// Raised surfaces: the column-header row background.
    #[serde(default = "default_bg_raised", deserialize_with = "parse_rgb_hex")]
    pub bg_raised: u32,
    /// Overlay surfaces: context menus and other floating panels raised
    /// above [`Colors::bg_raised`].
    #[serde(default = "default_bg_overlay", deserialize_with = "parse_rgb_hex")]
    pub bg_overlay: u32,
    /// Text-input field background.
    #[serde(default = "default_bg_input", deserialize_with = "parse_rgb_hex")]
    pub bg_input: u32,
    /// Standard hairline border color.
    #[serde(default = "default_border", deserialize_with = "parse_rgb_hex")]
    pub border: u32,
    /// A softer hairline, used between body cells and header columns.
    #[serde(default = "default_border_soft", deserialize_with = "parse_rgb_hex")]
    pub border_soft: u32,
    /// Primary text color.
    #[serde(default = "default_text_primary", deserialize_with = "parse_rgb_hex")]
    pub text_primary: u32,
    /// Secondary/muted text (labels, timestamps).
    #[serde(default = "default_text_secondary", deserialize_with = "parse_rgb_hex")]
    pub text_secondary: u32,
    /// Faint text: NULLs, row numbers, disabled-ish labels.
    #[serde(default = "default_text_tertiary", deserialize_with = "parse_rgb_hex")]
    pub text_tertiary: u32,
    /// Accent color: row counts, active affordances, the Run button.
    #[serde(default = "default_accent", deserialize_with = "parse_rgb_hex")]
    pub accent: u32,
    /// Text color painted on top of a solid [`Colors::accent`] fill.
    #[serde(
        default = "default_accent_contrast",
        deserialize_with = "parse_rgb_hex"
    )]
    pub accent_contrast: u32,
    /// Boolean cell text.
    #[serde(default = "default_value_bool", deserialize_with = "parse_rgb_hex")]
    pub value_bool: u32,
    /// Raw-bytes cell text.
    #[serde(default = "default_value_bytes", deserialize_with = "parse_rgb_hex")]
    pub value_bytes: u32,
    /// JSON/JSONB cell text.
    #[serde(default = "default_value_json", deserialize_with = "parse_rgb_hex")]
    pub value_json: u32,
    /// NULL cell text.
    #[serde(default = "default_value_null", deserialize_with = "parse_rgb_hex")]
    pub value_null: u32,
    /// Numeric cell text.
    #[serde(default = "default_value_number", deserialize_with = "parse_rgb_hex")]
    pub value_number: u32,
    /// Plain text cell text.
    #[serde(default = "default_value_text", deserialize_with = "parse_rgb_hex")]
    pub value_text: u32,
    /// Timestamp cell text.
    #[serde(
        default = "default_value_timestamp",
        deserialize_with = "parse_rgb_hex"
    )]
    pub value_timestamp: u32,
    /// Fallback/attention color for values that do not map to a more
    /// specific kind (arrays, unmapped backend types).
    #[serde(default = "default_value_unknown", deserialize_with = "parse_rgb_hex")]
    pub value_unknown: u32,
    /// Syntax highlight color for a keyword span.
    #[serde(default = "default_syntax_keyword", deserialize_with = "parse_rgb_hex")]
    pub syntax_keyword: u32,
    /// Syntax highlight color for a string-literal span.
    #[serde(default = "default_syntax_string", deserialize_with = "parse_rgb_hex")]
    pub syntax_string: u32,
    /// Status color: a connection attempt in flight, or a partial/degraded
    /// state.
    #[serde(default = "default_status_warn", deserialize_with = "parse_rgb_hex")]
    pub status_warn: u32,
    /// Status color: a failed connection or query.
    #[serde(default = "default_status_error", deserialize_with = "parse_rgb_hex")]
    pub status_error: u32,
    /// Status color: a query result truncated at the configured row limit.
    #[serde(default = "default_status_limited", deserialize_with = "parse_rgb_hex")]
    pub status_limited: u32,
    /// View relation-kind tint.
    #[serde(default = "default_kind_view", deserialize_with = "parse_rgb_hex")]
    pub kind_view: u32,
    /// Materialized-view relation-kind tint.
    #[serde(default = "default_kind_matview", deserialize_with = "parse_rgb_hex")]
    pub kind_matview: u32,
    /// Partitioned-table relation-kind tint.
    #[serde(
        default = "default_kind_partitioned",
        deserialize_with = "parse_rgb_hex"
    )]
    pub kind_partitioned: u32,
    /// Foreign-key hue: the schema view's FK rail tick and link chip.
    #[serde(default = "default_key_fk", deserialize_with = "parse_rgb_hex")]
    pub key_fk: u32,
    /// Faint wash painted under a hovered row.
    #[serde(default = "default_hover_wash", deserialize_with = "parse_rgba_hex")]
    pub hover_wash: u32,
    /// Dimming scrim behind a modal.
    #[serde(default = "default_scrim", deserialize_with = "parse_rgba_hex")]
    pub scrim: u32,
    /// Shadow color cast by a dialog/modal panel.
    #[serde(default = "default_shadow_dialog", deserialize_with = "parse_rgba_hex")]
    pub shadow_dialog: u32,
    /// Shadow color cast by a floating overlay (e.g. a context menu).
    #[serde(
        default = "default_shadow_overlay",
        deserialize_with = "parse_rgba_hex"
    )]
    pub shadow_overlay: u32,
    /// Resting fill of a scrollbar thumb.
    #[serde(
        default = "default_scrollbar_thumb",
        deserialize_with = "parse_rgba_hex"
    )]
    pub scrollbar_thumb: u32,
    /// Hovered fill of a scrollbar thumb.
    #[serde(
        default = "default_scrollbar_thumb_hover",
        deserialize_with = "parse_rgba_hex"
    )]
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

/// Declares a zero-argument function `$fn_name` returning
/// `Colors::default().$field`, used as that field's serde `default = "..."`
/// so a theme file omitting the key falls back to just that one role's
/// built-in value rather than the whole struct's.
macro_rules! default_color_fn {
    ($fn_name:ident, $field:ident) => {
        fn $fn_name() -> u32 {
            Colors::default().$field
        }
    };
}

default_color_fn!(default_bg_app, bg_app);
default_color_fn!(default_bg_panel, bg_panel);
default_color_fn!(default_bg_raised, bg_raised);
default_color_fn!(default_bg_overlay, bg_overlay);
default_color_fn!(default_bg_input, bg_input);
default_color_fn!(default_border, border);
default_color_fn!(default_border_soft, border_soft);
default_color_fn!(default_text_primary, text_primary);
default_color_fn!(default_text_secondary, text_secondary);
default_color_fn!(default_text_tertiary, text_tertiary);
default_color_fn!(default_accent, accent);
default_color_fn!(default_accent_contrast, accent_contrast);
default_color_fn!(default_value_bool, value_bool);
default_color_fn!(default_value_bytes, value_bytes);
default_color_fn!(default_value_json, value_json);
default_color_fn!(default_value_null, value_null);
default_color_fn!(default_value_number, value_number);
default_color_fn!(default_value_text, value_text);
default_color_fn!(default_value_timestamp, value_timestamp);
default_color_fn!(default_value_unknown, value_unknown);
default_color_fn!(default_syntax_keyword, syntax_keyword);
default_color_fn!(default_syntax_string, syntax_string);
default_color_fn!(default_status_warn, status_warn);
default_color_fn!(default_status_error, status_error);
default_color_fn!(default_status_limited, status_limited);
default_color_fn!(default_kind_view, kind_view);
default_color_fn!(default_kind_matview, kind_matview);
default_color_fn!(default_kind_partitioned, kind_partitioned);
default_color_fn!(default_key_fk, key_fk);
default_color_fn!(default_hover_wash, hover_wash);
default_color_fn!(default_scrim, scrim);
default_color_fn!(default_shadow_dialog, shadow_dialog);
default_color_fn!(default_shadow_overlay, shadow_overlay);
default_color_fn!(default_scrollbar_thumb, scrollbar_thumb);
default_color_fn!(default_scrollbar_thumb_hover, scrollbar_thumb_hover);

/// Digit count of a `#`-prefixed opaque `RRGGBB` hex color string, the
/// width every [`gpui::rgb`]-consumed [`Colors`] role parses from.
const RGB_HEX_DIGITS: usize = 6;
/// Digit count of a `#`-prefixed `RRGGBBAA` hex color string carrying its
/// own alpha byte, the width every [`gpui::rgba`]-consumed [`Colors`] role
/// parses from.
const RGBA_HEX_DIGITS: usize = 8;

/// Parses `text` as a `#`-prefixed hex color of exactly `expected_digits`
/// hex digits, producing the literal `u32` `gpui::rgb`/`gpui::rgba` expects.
/// Rejects a missing `#`, a wrong digit count, and any non-hex-digit
/// character rather than substituting a default -- a malformed theme value
/// is a loud parse error, not a silently wrong color.
fn parse_hex_color(text: &str, expected_digits: usize) -> Result<u32, String> {
    let Some(digits) = text.strip_prefix('#') else {
        return Err(format!("color {text:?} must start with '#'"));
    };
    if digits.len() != expected_digits {
        return Err(format!(
            "color {text:?} must have exactly {expected_digits} hex digits after '#', found {}",
            digits.len()
        ));
    }
    if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("color {text:?} contains a non-hex-digit character"));
    }
    u32::from_str_radix(digits, 16)
        .map_err(|_err| format!("color {text:?} contains a non-hex-digit character"))
}

/// Deserializes a `#RRGGBB` string into the opaque `0xRRGGBB` a
/// [`gpui::rgb`]-consumed role stores.
fn parse_rgb_hex<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let text = String::deserialize(deserializer)?;
    parse_hex_color(&text, RGB_HEX_DIGITS).map_err(serde::de::Error::custom)
}

/// Deserializes a `#RRGGBBAA` string into the `0xRRGGBBAA` a
/// [`gpui::rgba`]-consumed role stores.
fn parse_rgba_hex<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let text = String::deserialize(deserializer)?;
    parse_hex_color(&text, RGBA_HEX_DIGITS).map_err(serde::de::Error::custom)
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
const fn wash(base: u32, alpha: u8) -> Rgba {
    rgba(wash_hex(base, alpha))
}

/// [`wash`], but as the `0xRRGGBBAA` hex form the alpha-carrying [`Colors`]
/// fields (and their theme-file deserialization) store.
pub(super) const fn wash_hex(base: u32, alpha: u8) -> u32 {
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

/// A const version of [`gpui::rgba`]
const fn rgba(color: u32) -> Rgba {
    let r = ((color >> 24) & 0xff) as u8 as f32 / 255.0;
    let g = ((color >> 16) & 0xff) as u8 as f32 / 255.0;
    let b = ((color >> 8) & 0xff) as u8 as f32 / 255.0;
    let a = (color & 0xff) as u8 as f32 / 255.0;
    Rgba { r, g, b, a }
}

impl Colors {
    /// [`Colors::accent`] at 13% opacity: a faint fill for a hovered or
    /// selected row.
    #[must_use]
    pub const fn accent_wash(&self) -> Rgba {
        wash(self.accent, 33)
    }

    /// [`Colors::accent`] at 20% opacity: a stronger fill than
    /// [`Colors::accent_wash`], e.g. a text selection highlight.
    #[must_use]
    pub const fn accent_wash_hover(&self) -> Rgba {
        wash(self.accent, 51)
    }

    /// [`Colors::accent`] at 9% opacity: a fill softer than
    /// [`Colors::accent_wash`], e.g. an active-row highlight in a dense
    /// list.
    #[must_use]
    pub const fn accent_wash_soft(&self) -> Rgba {
        wash(self.accent, 23)
    }

    /// [`Colors::accent`] at 14% opacity: a focus/selection ring.
    #[must_use]
    pub const fn accent_ring(&self) -> Rgba {
        wash(self.accent, 36)
    }

    /// [`Colors::accent`] at 30% opacity: a pill/badge outline.
    #[must_use]
    pub const fn accent_outline(&self) -> Rgba {
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
    pub const fn error_outline(&self) -> Rgba {
        wash(self.status_error, 82)
    }

    /// [`Colors::status_error`] at 10% opacity: a faint error-state fill.
    #[must_use]
    pub const fn error_wash(&self) -> Rgba {
        wash(self.status_error, 26)
    }

    /// [`Colors::status_warn`] at 32% opacity: a warn-state outline.
    #[must_use]
    pub const fn warn_outline(&self) -> Rgba {
        wash(self.status_warn, 82)
    }

    /// [`Colors::key_fk`] at 30% opacity: a foreign-key chip's outline.
    #[must_use]
    pub const fn fk_outline(&self) -> Rgba {
        wash(self.key_fk, 77)
    }

    /// [`Colors::key_fk`] at 7% opacity: a foreign-key chip's fill.
    #[must_use]
    pub const fn fk_wash(&self) -> Rgba {
        wash(self.key_fk, 18)
    }

    /// An arbitrary alpha wash of `base` at `alpha`/255 opacity -- the
    /// general form [`Colors::accent_wash`] and its siblings are built from,
    /// exposed for a caller whose wash does not fit one of those named
    /// percentages.
    #[must_use]
    pub const fn wash(base: u32, alpha: u8) -> Rgba {
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

    #[test]
    fn const_rgba_equals_non_const_rgba() {
        let colors = Colors::default();
        let const_rgba = super::rgba(colors.scrim);
        let non_const_rgba = gpui::rgba(colors.scrim);
        assert_eq!(const_rgba, non_const_rgba);
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
        assert_eq!(colors.accent_wash_soft(), gpui::rgba(0x33_c2_ac_17));
        // was zsql_editor::theme::EDITOR_SELECTION_BG and
        // zsql_ui::text_field::theme::FIELD_SELECTION_BG.
        assert_eq!(colors.accent_wash_hover(), gpui::rgba(0x33_c2_ac_33));
        // was zsql::ui::theme::SCHEMA_KIND_PILL_BORDER.
        assert_eq!(colors.accent_outline(), gpui::rgba(0x33_c2_ac_4d));
        // was zsql::ui::theme::SCHEMA_BADGE_UNIQUE_BORDER.
        assert_eq!(colors.warn_outline(), gpui::rgba(0xd9_a2_5a_52));
        // was zsql::ui::theme::SCHEMA_BADGE_LINK_BORDER.
        assert_eq!(colors.fk_outline(), gpui::rgba(0x7f_9c_ff_4d));
        // was zsql::ui::theme::SCHEMA_BADGE_LINK_BG.
        assert_eq!(colors.fk_wash(), gpui::rgba(0x7f_9c_ff_12));
        // was zsql::ui::theme::SIDEBAR_SELECTED_BG.
        assert_eq!(Colors::wash(colors.accent, 0x1a), gpui::rgba(0x33_c2_ac_1a));
        // was zsql::ui::theme::GENERATED_STRIP_BG.
        assert_eq!(Colors::wash(colors.accent, 0x0b), gpui::rgba(0x33_c2_ac_0b));
        // was zsql_ui::grid::TYPE_TAG_BORDER.
        assert_eq!(Colors::wash(colors.accent, 0x47), gpui::rgba(0x33_c2_ac_47));
        // was zsql::ui::theme::SCHEMA_BADGE_PK_BORDER.
        assert_eq!(Colors::wash(colors.accent, 0x52), gpui::rgba(0x33_c2_ac_52));
        // was zsql::ui::theme::RUN_BUTTON_HINT: the page ink at reduced
        // opacity.
        assert_eq!(Colors::wash(colors.bg_app, 0xb3), gpui::rgba(0x10_12_17_b3));
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

        assert_eq!(colors.accent_wash(), gpui::rgba(0x33_c2_ac_21));
        assert_eq!(colors.accent_ring(), gpui::rgba(0x33_c2_ac_24));
        assert_eq!(colors.error_outline(), gpui::rgba(0xe2_6d_78_52));
        assert_eq!(colors.error_wash(), gpui::rgba(0xe2_6d_78_1a));
        // accent mixed 62% toward bg_panel.
        assert_eq!(colors.accent_dim(), 0x28_82_78);
        // accent mixed 78% toward text_primary.
        assert_eq!(colors.accent_strong(), 0x58_c9_ba);
    }

    #[test]
    fn six_digit_hex_parses_as_an_opaque_rrggbb() {
        assert_eq!(super::parse_hex_color("#33c2ac", 6), Ok(0x33_c2_ac));
    }

    #[test]
    fn eight_digit_hex_parses_as_rrggbbaa_with_its_own_alpha_byte() {
        assert_eq!(super::parse_hex_color("#59606f66", 8), Ok(0x59_60_6f_66));
    }

    #[test]
    fn hex_missing_the_leading_hash_is_a_parse_error() {
        assert!(super::parse_hex_color("33c2ac", 6).is_err());
    }

    #[test]
    fn hex_with_the_wrong_digit_count_is_a_parse_error() {
        assert!(super::parse_hex_color("#33c2a", 6).is_err());
        assert!(super::parse_hex_color("#33c2acff", 6).is_err());
        assert!(super::parse_hex_color("#33c2ac", 8).is_err());
    }

    #[test]
    fn hex_with_a_non_hex_character_is_a_parse_error() {
        assert!(super::parse_hex_color("#33c2ag", 6).is_err());
        assert!(super::parse_hex_color("#zzzzzz", 6).is_err());
    }

    #[test]
    fn hex_with_a_leading_sign_character_is_a_parse_error() {
        // u32::from_str_radix alone accepts a leading '+' or '-', which
        // would otherwise let a malformed digit slip through as a shifted
        // color instead of failing.
        assert!(super::parse_hex_color("#+fffff", 6).is_err());
        assert!(super::parse_hex_color("#-fffff", 6).is_err());
    }

    #[test]
    fn a_theme_file_setting_only_one_field_matches_the_default_everywhere_else() {
        let colors: Colors = serde_json::from_str("{\"accent\": \"#e0a33f\"}")
            .expect("a single-field theme must parse");

        let expected = Colors {
            accent: 0xe0_a3_3f,
            ..Colors::default()
        };
        assert_eq!(colors, expected);
    }

    #[test]
    fn an_empty_theme_file_deserializes_to_the_default_palette() {
        let colors: Colors = serde_json::from_str("{}").expect("an empty theme must parse");
        assert_eq!(colors, Colors::default());
    }

    #[test]
    fn a_theme_file_can_override_an_rgba_role_with_eight_hex_digits() {
        let colors: Colors = serde_json::from_str("{\"scrim\": \"#11223344\"}")
            .expect("an rgba-role override must parse");
        assert_eq!(colors.scrim, 0x11_22_33_44);
        assert_eq!(colors.bg_app, Colors::default().bg_app);
    }

    #[test]
    fn an_rgb_role_given_eight_hex_digits_is_rejected() {
        let err = serde_json::from_str::<Colors>("{\"accent\": \"#33c2acff\"}")
            .expect_err("accent is an rgb-consumed role and must reject 8 digits");
        assert!(
            err.to_string().contains("hex digits"),
            "error should explain the digit-count mismatch: {err}"
        );
    }

    #[test]
    fn an_unknown_key_fails_to_parse_and_names_the_offending_key() {
        let err = serde_json::from_str::<Colors>("{\"accentt\": \"#e0a33f\"}")
            .expect_err("a misspelled role name must be rejected, not ignored");
        assert!(
            err.to_string().contains("accentt"),
            "error should name the unknown key: {err}"
        );
    }
}
