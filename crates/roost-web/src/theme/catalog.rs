//! The theme catalog: Roost's paired light/dark default, rust-ui base palettes,
//! Omarchy bases and accent palettes. Stored ids resolve here without migration;
//! palette data lives in `palettes/`, derived Omarchy roles in `omarchy.rs`.

use super::omarchy::OmarchyPalette;
use super::palettes::accent_a::ACCENT_THEMES_A;
use super::palettes::accent_b::ACCENT_THEMES_B;
use super::palettes::base::BASE_THEMES;
use super::palettes::brand::ROOST_THEME;
use super::palettes::omarchy_a::OMARCHY_THEMES_A;
use super::palettes::omarchy_b::OMARCHY_THEMES_B;

/// One selectable palette: a base (surface and text roles) or an accent (the
/// primary, ring and sidebar roles layered over a base).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeSpec {
    pub id: &'static str,
    pub label: &'static str,
    /// The hex colour the picker shows for this palette.
    pub swatch: &'static str,
    /// The Omarchy palette a base derives all its roles from. `None` for the
    /// rust-ui bases and for accents, which carry their own role lists.
    pub omarchy: Option<&'static OmarchyPalette>,
    pub light: &'static [(&'static str, &'static str)],
    pub dark: &'static [(&'static str, &'static str)],
}

/// The base a fresh reader gets.
pub const DEFAULT_BASE_ID: &str = "roost";
/// The accent a fresh reader gets: no accent, the base's own primary.
pub const DEFAULT_ACCENT_ID: &str = "default";

/// Every base palette, in picker order: Roost, rust-ui, then Omarchy.
pub fn base_themes() -> impl Iterator<Item = &'static ThemeSpec> {
    std::iter::once(&ROOST_THEME)
        .chain(BASE_THEMES.iter())
        .chain(OMARCHY_THEMES_A.iter())
        .chain(OMARCHY_THEMES_B.iter())
}

/// Every accent palette, in picker order.
pub fn accent_themes() -> impl Iterator<Item = &'static ThemeSpec> {
    ACCENT_THEMES_A.iter().chain(ACCENT_THEMES_B.iter())
}

/// The base palette with `id`, when one exists.
pub fn base_theme(id: &str) -> Option<&'static ThemeSpec> {
    base_themes().find(|theme| theme.id == id)
}

/// The accent palette with `id`, when one exists.
pub fn accent_theme(id: &str) -> Option<&'static ThemeSpec> {
    accent_themes().find(|theme| theme.id == id)
}

/// The fallback base, used when a stored id is unknown.
pub fn default_base() -> &'static ThemeSpec {
    &ROOST_THEME
}

/// The fallback accent, used when a stored id is unknown. "Default" is entry 0
/// of the generated accent table.
pub fn default_accent() -> &'static ThemeSpec {
    &ACCENT_THEMES_A[0]
}
