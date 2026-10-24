//! The theme catalog: the rust-ui base palettes, the accent palettes, and the
//! lookups the engine resolves a stored choice against. The palette data lives
//! in `palettes/`, copied verbatim from rust-ui.

use super::palettes::accent_a::ACCENT_THEMES_A;
use super::palettes::accent_b::ACCENT_THEMES_B;
use super::palettes::base::BASE_THEMES;

/// One selectable palette: a base (surface and text roles) or an accent (the
/// primary, ring and sidebar roles layered over a base).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeSpec {
    pub id: &'static str,
    pub label: &'static str,
    /// The hex colour the picker shows for this palette.
    pub swatch: &'static str,
    pub light: &'static [(&'static str, &'static str)],
    pub dark: &'static [(&'static str, &'static str)],
}

/// The base a fresh reader gets: rust-ui's Neutral.
pub const DEFAULT_BASE_ID: &str = "neutral";
/// The accent a fresh reader gets: no accent, the base's own primary.
pub const DEFAULT_ACCENT_ID: &str = "default";

/// Every base palette, in picker order.
pub fn base_themes() -> &'static [ThemeSpec] {
    &BASE_THEMES
}

/// Every accent palette, in picker order.
pub fn accent_themes() -> impl Iterator<Item = &'static ThemeSpec> {
    ACCENT_THEMES_A.iter().chain(ACCENT_THEMES_B.iter())
}

/// The base palette with `id`, when one exists.
pub fn base_theme(id: &str) -> Option<&'static ThemeSpec> {
    BASE_THEMES.iter().find(|theme| theme.id == id)
}

/// The accent palette with `id`, when one exists.
pub fn accent_theme(id: &str) -> Option<&'static ThemeSpec> {
    accent_themes().find(|theme| theme.id == id)
}

/// The fallback base, used when a stored id is unknown. Neutral is entry 0 of
/// the generated table.
pub fn default_base() -> &'static ThemeSpec {
    &BASE_THEMES[0]
}

/// The fallback accent, used when a stored id is unknown. "Default" is entry 0
/// of the generated accent table.
pub fn default_accent() -> &'static ThemeSpec {
    &ACCENT_THEMES_A[0]
}
