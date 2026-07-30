//! The four built-in Catppuccin flavors, each generated from one
//! role-to-hue mapping applied to that flavor's raw palette -- adding a
//! fifth flavor is a new palette constant, not a new hand-written role
//! table.

use super::Colors;

/// Config-facing name for the Catppuccin Latte flavor.
pub const LATTE_NAME: &str = "catppuccin-latte";
/// Config-facing name for the Catppuccin Frappe flavor.
pub const FRAPPE_NAME: &str = "catppuccin-frappe";
/// Config-facing name for the Catppuccin Macchiato flavor.
pub const MACCHIATO_NAME: &str = "catppuccin-macchiato";
/// Config-facing name for the Catppuccin Mocha flavor.
pub const MOCHA_NAME: &str = "catppuccin-mocha";

/// A Catppuccin flavor's raw named hues, plus its six bespoke depth-role
/// (base color, alpha byte) pairs -- the alpha and, for the lighter Latte
/// flavor, the base color itself differ per flavor rather than following
/// the named-hue table, so they are carried as data alongside it.
struct CatppuccinPalette {
    base: u32,
    mantle: u32,
    crust: u32,
    surface0: u32,
    surface1: u32,
    surface2: u32,
    overlay0: u32,
    overlay1: u32,
    text: u32,
    subtext0: u32,
    teal: u32,
    mauve: u32,
    peach: u32,
    sapphire: u32,
    red: u32,
    yellow: u32,
    green: u32,
    blue: u32,
    lavender: u32,
    hover_wash: (u32, u8),
    scrim: (u32, u8),
    shadow_dialog: (u32, u8),
    shadow_overlay: (u32, u8),
    scrollbar_thumb: (u32, u8),
    scrollbar_thumb_hover: (u32, u8),
}

const CATPPUCCIN_LATTE: CatppuccinPalette = CatppuccinPalette {
    base: 0xef_f1_f5,
    mantle: 0xe6_e9_ef,
    crust: 0xdc_e0_e8,
    surface0: 0xcc_d0_da,
    surface1: 0xbc_c0_cc,
    surface2: 0xac_b0_be,
    overlay0: 0x9c_a0_b0,
    overlay1: 0x8c_8f_a1,
    text: 0x4c_4f_69,
    subtext0: 0x6c_6f_85,
    teal: 0x17_92_99,
    mauve: 0x88_39_ef,
    peach: 0xfe_64_0b,
    sapphire: 0x20_9f_b5,
    red: 0xd2_0f_39,
    yellow: 0xdf_8e_1d,
    green: 0x40_a0_2b,
    blue: 0x1e_66_f5,
    lavender: 0x72_87_fd,
    hover_wash: (0x4c_4f_69, 0x0b),
    scrim: (0x4c_4f_69, 0x59),
    shadow_dialog: (0x4c_4f_69, 0x38),
    shadow_overlay: (0x4c_4f_69, 0x2e),
    scrollbar_thumb: (0x9c_a0_b0, 0xb3),
    scrollbar_thumb_hover: (0x7c_7f_93, 0xd9),
};

const CATPPUCCIN_FRAPPE: CatppuccinPalette = CatppuccinPalette {
    base: 0x30_34_46,
    mantle: 0x29_2c_3c,
    crust: 0x23_26_34,
    surface0: 0x41_45_59,
    surface1: 0x51_57_6d,
    surface2: 0x62_68_80,
    overlay0: 0x73_79_94,
    overlay1: 0x83_8b_a7,
    text: 0xc6_d0_f5,
    subtext0: 0xa5_ad_ce,
    teal: 0x81_c8_be,
    mauve: 0xca_9e_e6,
    peach: 0xef_9f_76,
    sapphire: 0x85_c1_dc,
    red: 0xe7_82_84,
    yellow: 0xe5_c8_90,
    green: 0xa6_d1_89,
    blue: 0x8c_aa_ee,
    lavender: 0xba_bb_f1,
    hover_wash: (0xc6_d0_f5, 0x08),
    scrim: (0x23_26_34, 0xa3),
    shadow_dialog: (0x00_00_00, 0x80),
    shadow_overlay: (0x00_00_00, 0x80),
    scrollbar_thumb: (0x73_79_94, 0x8c),
    scrollbar_thumb_hover: (0x73_79_94, 0xcc),
};

const CATPPUCCIN_MACCHIATO: CatppuccinPalette = CatppuccinPalette {
    base: 0x24_27_3a,
    mantle: 0x1e_20_30,
    crust: 0x18_19_26,
    surface0: 0x36_3a_4f,
    // Grouped as 4/4 rather than the usual 2/2/2 byte grouping: a trailing
    // `_64` group reads to clippy's mistyped_literal_suffixes lint as a
    // typo'd `i64`/`u64` suffix and is denied workspace-wide.
    surface1: 0x49_4d64,
    surface2: 0x5b_60_78,
    overlay0: 0x6e_73_8d,
    overlay1: 0x80_87_a2,
    text: 0xca_d3_f5,
    subtext0: 0xa5_ad_cb,
    teal: 0x8b_d5_ca,
    mauve: 0xc6_a0_f6,
    peach: 0xf5_a9_7f,
    sapphire: 0x7d_c4_e4,
    red: 0xed_87_96,
    yellow: 0xee_d4_9f,
    green: 0xa6_da_95,
    blue: 0x8a_ad_f4,
    lavender: 0xb7_bd_f8,
    hover_wash: (0xca_d3_f5, 0x08),
    scrim: (0x18_19_26, 0xa6),
    shadow_dialog: (0x00_00_00, 0x85),
    shadow_overlay: (0x00_00_00, 0x85),
    scrollbar_thumb: (0x6e_73_8d, 0x8c),
    scrollbar_thumb_hover: (0x6e_73_8d, 0xcc),
};

const CATPPUCCIN_MOCHA: CatppuccinPalette = CatppuccinPalette {
    base: 0x1e_1e_2e,
    mantle: 0x18_18_25,
    crust: 0x11_11_1b,
    surface0: 0x31_32_44,
    surface1: 0x45_47_5a,
    surface2: 0x58_5b_70,
    overlay0: 0x6c_70_86,
    overlay1: 0x7f_84_9c,
    text: 0xcd_d6_f4,
    subtext0: 0xa6_ad_c8,
    teal: 0x94_e2_d5,
    mauve: 0xcb_a6_f7,
    peach: 0xfa_b3_87,
    sapphire: 0x74_c7_ec,
    red: 0xf3_8b_a8,
    yellow: 0xf9_e2_af,
    green: 0xa6_e3_a1,
    blue: 0x89_b4_fa,
    lavender: 0xb4_be_fe,
    hover_wash: (0xcd_d6_f4, 0x08),
    scrim: (0x11_11_1b, 0xa8),
    shadow_dialog: (0x00_00_00, 0x8c),
    shadow_overlay: (0x00_00_00, 0x8c),
    scrollbar_thumb: (0x6c_70_86, 0x8c),
    scrollbar_thumb_hover: (0x6c_70_86, 0xcc),
};

/// Applies the Catppuccin role-to-hue mapping (from Catppuccin's own style
/// guide) to `palette`, producing every [`Colors`] role. The single place
/// that mapping is written down: a new flavor is a new [`CatppuccinPalette`]
/// constant run through this same function, never a hand-copied [`Colors`]
/// literal.
const fn catppuccin(palette: &CatppuccinPalette) -> Colors {
    Colors {
        bg_app: palette.base,
        bg_panel: palette.mantle,
        bg_raised: palette.surface0,
        bg_overlay: palette.surface1,
        bg_input: palette.crust,
        border: palette.surface2,
        border_soft: palette.surface0,
        text_primary: palette.text,
        text_secondary: palette.subtext0,
        text_tertiary: palette.overlay1,
        accent: palette.teal,
        accent_contrast: palette.base,
        value_bool: palette.peach,
        value_bytes: palette.teal,
        value_json: palette.sapphire,
        value_null: palette.overlay0,
        value_number: palette.mauve,
        value_text: palette.text,
        value_timestamp: palette.subtext0,
        value_unknown: palette.red,
        syntax_keyword: palette.blue,
        syntax_string: palette.green,
        status_warn: palette.yellow,
        status_error: palette.red,
        status_limited: palette.peach,
        kind_view: palette.sapphire,
        kind_matview: palette.yellow,
        kind_partitioned: palette.lavender,
        key_fk: palette.blue,
        hover_wash: super::colors::wash_hex(palette.hover_wash.0, palette.hover_wash.1),
        scrim: super::colors::wash_hex(palette.scrim.0, palette.scrim.1),
        shadow_dialog: super::colors::wash_hex(palette.shadow_dialog.0, palette.shadow_dialog.1),
        shadow_overlay: super::colors::wash_hex(palette.shadow_overlay.0, palette.shadow_overlay.1),
        scrollbar_thumb: super::colors::wash_hex(
            palette.scrollbar_thumb.0,
            palette.scrollbar_thumb.1,
        ),
        scrollbar_thumb_hover: super::colors::wash_hex(
            palette.scrollbar_thumb_hover.0,
            palette.scrollbar_thumb_hover.1,
        ),
    }
}

/// The Catppuccin Latte flavor's colors: Catppuccin's light theme.
#[must_use]
pub const fn latte() -> Colors {
    catppuccin(&CATPPUCCIN_LATTE)
}

/// The Catppuccin Frappe flavor's colors.
#[must_use]
pub const fn frappe() -> Colors {
    catppuccin(&CATPPUCCIN_FRAPPE)
}

/// The Catppuccin Macchiato flavor's colors.
#[must_use]
pub const fn macchiato() -> Colors {
    catppuccin(&CATPPUCCIN_MACCHIATO)
}

/// The Catppuccin Mocha flavor's colors.
#[must_use]
pub const fn mocha() -> Colors {
    catppuccin(&CATPPUCCIN_MOCHA)
}

/// Look up a built-in flavor's colors by its config-facing name, or `None`
/// if `name` does not match one.
#[must_use]
pub fn built_in_by_name(name: &str) -> Option<Colors> {
    match name {
        LATTE_NAME => Some(latte()),
        FRAPPE_NAME => Some(frappe()),
        MACCHIATO_NAME => Some(macchiato()),
        MOCHA_NAME => Some(mocha()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{frappe, latte, macchiato, mocha};

    #[test]
    fn mocha_bg_app_is_the_base_hex_and_differs_from_bg_raised() {
        let colors = mocha();
        assert_eq!(colors.bg_app, 0x1e_1e_2e);
        assert_ne!(colors.bg_app, colors.bg_raised);
    }

    #[test]
    fn mocha_pins_a_handful_of_style_guide_roles() {
        let colors = mocha();
        assert_eq!(colors.accent, 0x94_e2_d5);
        assert_eq!(colors.text_primary, 0xcd_d6_f4);
        assert_eq!(colors.syntax_keyword, 0x89_b4_fa);
        assert_eq!(colors.status_error, 0xf3_8b_a8);
    }

    #[test]
    fn latte_bg_app_is_lighter_than_its_own_text_primary() {
        let colors = latte();
        // A crude but sufficient lightness proxy: sum of channels. A light
        // theme's background must outshine its own text, or the mapping
        // produced an inverted (broken) palette.
        let channel_sum = |rgb: u32| ((rgb >> 16) & 0xff) + ((rgb >> 8) & 0xff) + (rgb & 0xff);
        assert!(channel_sum(colors.bg_app) > channel_sum(colors.text_primary));
    }

    #[test]
    fn latte_pins_a_handful_of_style_guide_roles() {
        let colors = latte();
        assert_eq!(colors.bg_app, 0xef_f1_f5);
        assert_eq!(colors.accent, 0x17_92_99);
        assert_eq!(colors.value_number, 0x88_39_ef);
        assert_eq!(colors.status_error, 0xd2_0f_39);
    }

    #[test]
    fn frappe_pins_a_handful_of_style_guide_roles() {
        let colors = frappe();
        assert_eq!(colors.bg_app, 0x30_34_46);
        assert_eq!(colors.accent, 0x81_c8_be);
        assert_eq!(colors.syntax_string, 0xa6_d1_89);
        assert_eq!(colors.kind_partitioned, 0xba_bb_f1);
    }

    #[test]
    fn macchiato_pins_a_handful_of_style_guide_roles() {
        let colors = macchiato();
        assert_eq!(colors.bg_app, 0x24_27_3a);
        assert_eq!(colors.accent, 0x8b_d5_ca);
        assert_eq!(colors.value_bool, 0xf5_a9_7f);
        assert_eq!(colors.key_fk, 0x8a_ad_f4);
    }

    #[test]
    fn every_flavor_gives_hover_wash_and_scrollbar_thumb_a_nonzero_alpha() {
        for colors in [latte(), frappe(), macchiato(), mocha()] {
            assert_ne!(colors.hover_wash & 0xff, 0);
            assert_ne!(colors.scrollbar_thumb & 0xff, 0);
        }
    }

    #[test]
    fn latte_pins_its_depth_roles_to_the_style_guide_rgba_values() {
        let colors = latte();
        assert_eq!(colors.scrim, 0x4c_4f_69_59);
        assert_eq!(colors.shadow_dialog, 0x4c_4f_69_38);
        assert_eq!(colors.shadow_overlay, 0x4c_4f_69_2e);
        assert_eq!(colors.scrollbar_thumb, 0x9c_a0_b0_b3);
    }

    #[test]
    fn frappe_pins_its_depth_roles_to_the_style_guide_rgba_values() {
        let colors = frappe();
        assert_eq!(colors.scrim, 0x23_26_34_a3);
        assert_eq!(colors.shadow_dialog, 0x00_00_00_80);
        assert_eq!(colors.shadow_overlay, 0x00_00_00_80);
        assert_eq!(colors.scrollbar_thumb, 0x73_79_94_8c);
    }

    #[test]
    fn macchiato_pins_its_depth_roles_to_the_style_guide_rgba_values() {
        let colors = macchiato();
        assert_eq!(colors.scrim, 0x18_19_26_a6);
        assert_eq!(colors.shadow_dialog, 0x00_00_00_85);
        assert_eq!(colors.shadow_overlay, 0x00_00_00_85);
        assert_eq!(colors.scrollbar_thumb, 0x6e_73_8d_8c);
    }

    #[test]
    fn mocha_pins_its_depth_roles_to_the_style_guide_rgba_values() {
        let colors = mocha();
        assert_eq!(colors.scrim, 0x11_11_1b_a8);
        assert_eq!(colors.shadow_dialog, 0x00_00_00_8c);
        assert_eq!(colors.shadow_overlay, 0x00_00_00_8c);
        assert_eq!(colors.scrollbar_thumb, 0x6c_70_86_8c);
    }

    #[test]
    fn built_in_by_name_matches_exactly_the_four_flavor_names_and_nothing_else() {
        assert_eq!(super::built_in_by_name("catppuccin-latte"), Some(latte()));
        assert_eq!(super::built_in_by_name("catppuccin-frappe"), Some(frappe()));
        assert_eq!(
            super::built_in_by_name("catppuccin-macchiato"),
            Some(macchiato())
        );
        assert_eq!(super::built_in_by_name("catppuccin-mocha"), Some(mocha()));
        assert_eq!(super::built_in_by_name("dark"), None);
        assert_eq!(super::built_in_by_name("catppuccin"), None);
        assert_eq!(super::built_in_by_name(""), None);
    }
}
