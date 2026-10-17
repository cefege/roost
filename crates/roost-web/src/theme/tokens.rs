//! The light or dark appearance a theme resolves to. The colour roles themselves
//! live in `catalog` (rust-ui palettes) and `palettes::roost` (Roost-owned roles).

/// Whether a resolved theme is drawn light or dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeAppearance {
    Light,
    Dark,
}

impl ThemeAppearance {
    /// The CSS `color-scheme` value for this appearance.
    pub const fn color_scheme(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}
