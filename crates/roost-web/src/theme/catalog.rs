//! Persisted palette metadata and immutable Libero theme pairs.
//!
//! Picker identifiers and order remain stable; color seeds were converted once
//! from the former OKLCH catalog and are assembled into static themes below.

use libero::theme::{HexColor, Theme, ThemeSet};

use super::choice::ThemeChoice;
use super::palettes::accent_a::ACCENT_THEMES;
use super::palettes::base::BASE_THEMES;

/// Semantic color inputs selected from a base palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteColors {
    pub surface: u32,
    pub ink: u32,
    pub primary: u32,
    pub secondary: u32,
    pub muted: u32,
}

/// Stable base palette metadata and its converted light/dark colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseThemeSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub swatch: &'static str,
    pub neutral: u32,
    pub light: PaletteColors,
    pub dark: PaletteColors,
}

/// Optional accent overrides, retaining base roles when absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccentColors {
    pub primary: Option<u32>,
    pub secondary: Option<u32>,
}

/// Stable accent metadata and converted light/dark overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccentThemeSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub swatch: &'static str,
    pub light: AccentColors,
    pub dark: AccentColors,
}

/// The base a fresh reader gets: Neutral.
pub const DEFAULT_BASE_ID: &str = "neutral";
/// The accent a fresh reader gets: no accent, the base's own primary.
pub const DEFAULT_ACCENT_ID: &str = "default";

pub fn base_themes() -> &'static [BaseThemeSpec] {
    &BASE_THEMES
}
pub fn accent_themes() -> impl Iterator<Item = &'static AccentThemeSpec> {
    ACCENT_THEMES.iter()
}
pub fn base_theme(id: &str) -> Option<&'static BaseThemeSpec> {
    BASE_THEMES.iter().find(|theme| theme.id == id)
}
pub fn accent_theme(id: &str) -> Option<&'static AccentThemeSpec> {
    ACCENT_THEMES.iter().find(|theme| theme.id == id)
}
pub fn default_base() -> &'static BaseThemeSpec {
    &BASE_THEMES[0]
}
pub fn default_accent() -> &'static AccentThemeSpec {
    &ACCENT_THEMES[0]
}

const ERROR_LIGHT: u32 = 0xE7000B;
const ERROR_DARK: u32 = 0xFF6467;
const WARNING_LIGHT: u32 = 0xD76F04;
const WARNING_DARK: u32 = 0xD76F04;
const INFO_LIGHT: u32 = 0x0072D5;
const INFO_DARK: u32 = 0x0F92F7;
const SUCCESS_LIGHT: u32 = 0x43A84C;
const SUCCESS_DARK: u32 = 0x43A84C;

const fn make_theme(base: PaletteColors, neutral: u32, accent: AccentColors, dark: bool) -> Theme {
    let (primary, secondary, error, warning, info, success) = if dark {
        (
            match accent.primary {
                Some(color) => color,
                None => base.primary,
            },
            match accent.secondary {
                Some(color) => color,
                None => base.secondary,
            },
            ERROR_DARK,
            WARNING_DARK,
            INFO_DARK,
            SUCCESS_DARK,
        )
    } else {
        (
            match accent.primary {
                Some(color) => color,
                None => base.primary,
            },
            match accent.secondary {
                Some(color) => color,
                None => base.secondary,
            },
            ERROR_LIGHT,
            WARNING_LIGHT,
            INFO_LIGHT,
            SUCCESS_LIGHT,
        )
    };
    Theme {
        primary: HexColor::new(primary),
        secondary: HexColor::new(secondary),
        error: HexColor::new(error),
        warning: HexColor::new(warning),
        info: HexColor::new(info),
        success: HexColor::new(success),
        neutral: HexColor::new(neutral),
        muted: HexColor::new(base.muted),
        ink: HexColor::new(base.ink),
        surface: HexColor::new(base.surface),
        ..if dark { Theme::DARK } else { Theme::DEFAULT }
    }
}

macro_rules! define_theme_pair {
    ($pair:ident, $light:ident, $dark:ident, $base:literal, $accent:literal) => {
        static $light: Theme = make_theme(
            BASE_THEMES[$base].light,
            BASE_THEMES[$base].neutral,
            ACCENT_THEMES[$accent].light,
            false,
        );
        static $dark: Theme = make_theme(
            BASE_THEMES[$base].dark,
            BASE_THEMES[$base].neutral,
            ACCENT_THEMES[$accent].dark,
            true,
        );
        static $pair: ThemeSet = ThemeSet::pair("Roost", &$light, &$dark);
    };
}
define_theme_pair!(PAIR_0, LIGHT_0, DARK_0, 0, 0);
define_theme_pair!(PAIR_1, LIGHT_1, DARK_1, 0, 1);
define_theme_pair!(PAIR_2, LIGHT_2, DARK_2, 0, 2);
define_theme_pair!(PAIR_3, LIGHT_3, DARK_3, 0, 3);
define_theme_pair!(PAIR_4, LIGHT_4, DARK_4, 0, 4);
define_theme_pair!(PAIR_5, LIGHT_5, DARK_5, 0, 5);
define_theme_pair!(PAIR_6, LIGHT_6, DARK_6, 0, 6);
define_theme_pair!(PAIR_7, LIGHT_7, DARK_7, 0, 7);
define_theme_pair!(PAIR_8, LIGHT_8, DARK_8, 0, 8);
define_theme_pair!(PAIR_9, LIGHT_9, DARK_9, 0, 9);
define_theme_pair!(PAIR_10, LIGHT_10, DARK_10, 0, 10);
define_theme_pair!(PAIR_11, LIGHT_11, DARK_11, 0, 11);
define_theme_pair!(PAIR_12, LIGHT_12, DARK_12, 0, 12);
define_theme_pair!(PAIR_13, LIGHT_13, DARK_13, 0, 13);
define_theme_pair!(PAIR_14, LIGHT_14, DARK_14, 0, 14);
define_theme_pair!(PAIR_15, LIGHT_15, DARK_15, 0, 15);
define_theme_pair!(PAIR_16, LIGHT_16, DARK_16, 0, 16);
define_theme_pair!(PAIR_17, LIGHT_17, DARK_17, 0, 17);
define_theme_pair!(PAIR_18, LIGHT_18, DARK_18, 1, 0);
define_theme_pair!(PAIR_19, LIGHT_19, DARK_19, 1, 1);
define_theme_pair!(PAIR_20, LIGHT_20, DARK_20, 1, 2);
define_theme_pair!(PAIR_21, LIGHT_21, DARK_21, 1, 3);
define_theme_pair!(PAIR_22, LIGHT_22, DARK_22, 1, 4);
define_theme_pair!(PAIR_23, LIGHT_23, DARK_23, 1, 5);
define_theme_pair!(PAIR_24, LIGHT_24, DARK_24, 1, 6);
define_theme_pair!(PAIR_25, LIGHT_25, DARK_25, 1, 7);
define_theme_pair!(PAIR_26, LIGHT_26, DARK_26, 1, 8);
define_theme_pair!(PAIR_27, LIGHT_27, DARK_27, 1, 9);
define_theme_pair!(PAIR_28, LIGHT_28, DARK_28, 1, 10);
define_theme_pair!(PAIR_29, LIGHT_29, DARK_29, 1, 11);
define_theme_pair!(PAIR_30, LIGHT_30, DARK_30, 1, 12);
define_theme_pair!(PAIR_31, LIGHT_31, DARK_31, 1, 13);
define_theme_pair!(PAIR_32, LIGHT_32, DARK_32, 1, 14);
define_theme_pair!(PAIR_33, LIGHT_33, DARK_33, 1, 15);
define_theme_pair!(PAIR_34, LIGHT_34, DARK_34, 1, 16);
define_theme_pair!(PAIR_35, LIGHT_35, DARK_35, 1, 17);
define_theme_pair!(PAIR_36, LIGHT_36, DARK_36, 2, 0);
define_theme_pair!(PAIR_37, LIGHT_37, DARK_37, 2, 1);
define_theme_pair!(PAIR_38, LIGHT_38, DARK_38, 2, 2);
define_theme_pair!(PAIR_39, LIGHT_39, DARK_39, 2, 3);
define_theme_pair!(PAIR_40, LIGHT_40, DARK_40, 2, 4);
define_theme_pair!(PAIR_41, LIGHT_41, DARK_41, 2, 5);
define_theme_pair!(PAIR_42, LIGHT_42, DARK_42, 2, 6);
define_theme_pair!(PAIR_43, LIGHT_43, DARK_43, 2, 7);
define_theme_pair!(PAIR_44, LIGHT_44, DARK_44, 2, 8);
define_theme_pair!(PAIR_45, LIGHT_45, DARK_45, 2, 9);
define_theme_pair!(PAIR_46, LIGHT_46, DARK_46, 2, 10);
define_theme_pair!(PAIR_47, LIGHT_47, DARK_47, 2, 11);
define_theme_pair!(PAIR_48, LIGHT_48, DARK_48, 2, 12);
define_theme_pair!(PAIR_49, LIGHT_49, DARK_49, 2, 13);
define_theme_pair!(PAIR_50, LIGHT_50, DARK_50, 2, 14);
define_theme_pair!(PAIR_51, LIGHT_51, DARK_51, 2, 15);
define_theme_pair!(PAIR_52, LIGHT_52, DARK_52, 2, 16);
define_theme_pair!(PAIR_53, LIGHT_53, DARK_53, 2, 17);
define_theme_pair!(PAIR_54, LIGHT_54, DARK_54, 3, 0);
define_theme_pair!(PAIR_55, LIGHT_55, DARK_55, 3, 1);
define_theme_pair!(PAIR_56, LIGHT_56, DARK_56, 3, 2);
define_theme_pair!(PAIR_57, LIGHT_57, DARK_57, 3, 3);
define_theme_pair!(PAIR_58, LIGHT_58, DARK_58, 3, 4);
define_theme_pair!(PAIR_59, LIGHT_59, DARK_59, 3, 5);
define_theme_pair!(PAIR_60, LIGHT_60, DARK_60, 3, 6);
define_theme_pair!(PAIR_61, LIGHT_61, DARK_61, 3, 7);
define_theme_pair!(PAIR_62, LIGHT_62, DARK_62, 3, 8);
define_theme_pair!(PAIR_63, LIGHT_63, DARK_63, 3, 9);
define_theme_pair!(PAIR_64, LIGHT_64, DARK_64, 3, 10);
define_theme_pair!(PAIR_65, LIGHT_65, DARK_65, 3, 11);
define_theme_pair!(PAIR_66, LIGHT_66, DARK_66, 3, 12);
define_theme_pair!(PAIR_67, LIGHT_67, DARK_67, 3, 13);
define_theme_pair!(PAIR_68, LIGHT_68, DARK_68, 3, 14);
define_theme_pair!(PAIR_69, LIGHT_69, DARK_69, 3, 15);
define_theme_pair!(PAIR_70, LIGHT_70, DARK_70, 3, 16);
define_theme_pair!(PAIR_71, LIGHT_71, DARK_71, 3, 17);
define_theme_pair!(PAIR_72, LIGHT_72, DARK_72, 4, 0);
define_theme_pair!(PAIR_73, LIGHT_73, DARK_73, 4, 1);
define_theme_pair!(PAIR_74, LIGHT_74, DARK_74, 4, 2);
define_theme_pair!(PAIR_75, LIGHT_75, DARK_75, 4, 3);
define_theme_pair!(PAIR_76, LIGHT_76, DARK_76, 4, 4);
define_theme_pair!(PAIR_77, LIGHT_77, DARK_77, 4, 5);
define_theme_pair!(PAIR_78, LIGHT_78, DARK_78, 4, 6);
define_theme_pair!(PAIR_79, LIGHT_79, DARK_79, 4, 7);
define_theme_pair!(PAIR_80, LIGHT_80, DARK_80, 4, 8);
define_theme_pair!(PAIR_81, LIGHT_81, DARK_81, 4, 9);
define_theme_pair!(PAIR_82, LIGHT_82, DARK_82, 4, 10);
define_theme_pair!(PAIR_83, LIGHT_83, DARK_83, 4, 11);
define_theme_pair!(PAIR_84, LIGHT_84, DARK_84, 4, 12);
define_theme_pair!(PAIR_85, LIGHT_85, DARK_85, 4, 13);
define_theme_pair!(PAIR_86, LIGHT_86, DARK_86, 4, 14);
define_theme_pair!(PAIR_87, LIGHT_87, DARK_87, 4, 15);
define_theme_pair!(PAIR_88, LIGHT_88, DARK_88, 4, 16);
define_theme_pair!(PAIR_89, LIGHT_89, DARK_89, 4, 17);
define_theme_pair!(PAIR_90, LIGHT_90, DARK_90, 5, 0);
define_theme_pair!(PAIR_91, LIGHT_91, DARK_91, 5, 1);
define_theme_pair!(PAIR_92, LIGHT_92, DARK_92, 5, 2);
define_theme_pair!(PAIR_93, LIGHT_93, DARK_93, 5, 3);
define_theme_pair!(PAIR_94, LIGHT_94, DARK_94, 5, 4);
define_theme_pair!(PAIR_95, LIGHT_95, DARK_95, 5, 5);
define_theme_pair!(PAIR_96, LIGHT_96, DARK_96, 5, 6);
define_theme_pair!(PAIR_97, LIGHT_97, DARK_97, 5, 7);
define_theme_pair!(PAIR_98, LIGHT_98, DARK_98, 5, 8);
define_theme_pair!(PAIR_99, LIGHT_99, DARK_99, 5, 9);
define_theme_pair!(PAIR_100, LIGHT_100, DARK_100, 5, 10);
define_theme_pair!(PAIR_101, LIGHT_101, DARK_101, 5, 11);
define_theme_pair!(PAIR_102, LIGHT_102, DARK_102, 5, 12);
define_theme_pair!(PAIR_103, LIGHT_103, DARK_103, 5, 13);
define_theme_pair!(PAIR_104, LIGHT_104, DARK_104, 5, 14);
define_theme_pair!(PAIR_105, LIGHT_105, DARK_105, 5, 15);
define_theme_pair!(PAIR_106, LIGHT_106, DARK_106, 5, 16);
define_theme_pair!(PAIR_107, LIGHT_107, DARK_107, 5, 17);
define_theme_pair!(PAIR_108, LIGHT_108, DARK_108, 6, 0);
define_theme_pair!(PAIR_109, LIGHT_109, DARK_109, 6, 1);
define_theme_pair!(PAIR_110, LIGHT_110, DARK_110, 6, 2);
define_theme_pair!(PAIR_111, LIGHT_111, DARK_111, 6, 3);
define_theme_pair!(PAIR_112, LIGHT_112, DARK_112, 6, 4);
define_theme_pair!(PAIR_113, LIGHT_113, DARK_113, 6, 5);
define_theme_pair!(PAIR_114, LIGHT_114, DARK_114, 6, 6);
define_theme_pair!(PAIR_115, LIGHT_115, DARK_115, 6, 7);
define_theme_pair!(PAIR_116, LIGHT_116, DARK_116, 6, 8);
define_theme_pair!(PAIR_117, LIGHT_117, DARK_117, 6, 9);
define_theme_pair!(PAIR_118, LIGHT_118, DARK_118, 6, 10);
define_theme_pair!(PAIR_119, LIGHT_119, DARK_119, 6, 11);
define_theme_pair!(PAIR_120, LIGHT_120, DARK_120, 6, 12);
define_theme_pair!(PAIR_121, LIGHT_121, DARK_121, 6, 13);
define_theme_pair!(PAIR_122, LIGHT_122, DARK_122, 6, 14);
define_theme_pair!(PAIR_123, LIGHT_123, DARK_123, 6, 15);
define_theme_pair!(PAIR_124, LIGHT_124, DARK_124, 6, 16);
define_theme_pair!(PAIR_125, LIGHT_125, DARK_125, 6, 17);

static THEME_PAIRS: [&ThemeSet; 126] = [
    &PAIR_0, &PAIR_1, &PAIR_2, &PAIR_3, &PAIR_4, &PAIR_5, &PAIR_6, &PAIR_7, &PAIR_8, &PAIR_9,
    &PAIR_10, &PAIR_11, &PAIR_12, &PAIR_13, &PAIR_14, &PAIR_15, &PAIR_16, &PAIR_17, &PAIR_18,
    &PAIR_19, &PAIR_20, &PAIR_21, &PAIR_22, &PAIR_23, &PAIR_24, &PAIR_25, &PAIR_26, &PAIR_27,
    &PAIR_28, &PAIR_29, &PAIR_30, &PAIR_31, &PAIR_32, &PAIR_33, &PAIR_34, &PAIR_35, &PAIR_36,
    &PAIR_37, &PAIR_38, &PAIR_39, &PAIR_40, &PAIR_41, &PAIR_42, &PAIR_43, &PAIR_44, &PAIR_45,
    &PAIR_46, &PAIR_47, &PAIR_48, &PAIR_49, &PAIR_50, &PAIR_51, &PAIR_52, &PAIR_53, &PAIR_54,
    &PAIR_55, &PAIR_56, &PAIR_57, &PAIR_58, &PAIR_59, &PAIR_60, &PAIR_61, &PAIR_62, &PAIR_63,
    &PAIR_64, &PAIR_65, &PAIR_66, &PAIR_67, &PAIR_68, &PAIR_69, &PAIR_70, &PAIR_71, &PAIR_72,
    &PAIR_73, &PAIR_74, &PAIR_75, &PAIR_76, &PAIR_77, &PAIR_78, &PAIR_79, &PAIR_80, &PAIR_81,
    &PAIR_82, &PAIR_83, &PAIR_84, &PAIR_85, &PAIR_86, &PAIR_87, &PAIR_88, &PAIR_89, &PAIR_90,
    &PAIR_91, &PAIR_92, &PAIR_93, &PAIR_94, &PAIR_95, &PAIR_96, &PAIR_97, &PAIR_98, &PAIR_99,
    &PAIR_100, &PAIR_101, &PAIR_102, &PAIR_103, &PAIR_104, &PAIR_105, &PAIR_106, &PAIR_107,
    &PAIR_108, &PAIR_109, &PAIR_110, &PAIR_111, &PAIR_112, &PAIR_113, &PAIR_114, &PAIR_115,
    &PAIR_116, &PAIR_117, &PAIR_118, &PAIR_119, &PAIR_120, &PAIR_121, &PAIR_122, &PAIR_123,
    &PAIR_124, &PAIR_125,
];

/// Static Libero light/dark pair for the requested persisted palette choice.
pub fn theme_set_for_choice(choice: &ThemeChoice) -> &'static ThemeSet {
    let base_idx = BASE_THEMES
        .iter()
        .position(|theme| theme.id == choice.base)
        .unwrap_or(0);
    let accent_idx = ACCENT_THEMES
        .iter()
        .position(|theme| theme.id == choice.accent)
        .unwrap_or(0);
    THEME_PAIRS[base_idx * ACCENT_THEMES.len() + accent_idx]
}
