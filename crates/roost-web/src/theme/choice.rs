//! The theme engine's rules, target-independent: the persisted choice, its
//! resolution against the OS preference, and the exact set of writes a resolved
//! theme makes on the document root. Ported from `apps/web/src/lib/theme.ts`
//! (`loadTheme`, `resolveThemeId`, the body of `applyTheme`, and the OS-flip
//! listener's condition); `theme.rs` persists through `KeyValueStore` and
//! `document.rs` performs the writes.
//!
//! The choice is `auto` (follow the OS) or a theme id. An id this build does not
//! register is KEPT as the choice and resolves to the default, so a newer
//! client's theme survives a round trip through an older one.

use roost_client_core::KeyValueStore;

use super::themes::{
    DEFAULT_THEME_ID, SYSTEM_DARK_ID, SYSTEM_LIGHT_ID, default_theme, theme_by_id,
};
use super::tokens::{CanonicalToken, Theme, ThemeAppearance};

/// The local-storage key the choice is persisted under.
pub const THEME_STORAGE_KEY: &str = "roost.theme";

/// The persisted spelling of "follow the OS".
pub const AUTO_THEME_CHOICE: &str = "auto";

/// The reader's theme choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeChoice {
    /// Follow the OS light/dark preference.
    Auto,
    /// A theme by id — registered or not.
    Named(String),
}

impl ThemeChoice {
    /// The choice a stored value names. Nothing stored, or an empty value, is
    /// `Auto`, as v2's `loadTheme` read it.
    pub fn from_stored(stored: Option<&str>) -> Self {
        match stored {
            None | Some("") | Some(AUTO_THEME_CHOICE) => Self::Auto,
            Some(id) => Self::Named(id.to_string()),
        }
    }

    /// The value persisted for this choice.
    pub fn as_stored(&self) -> &str {
        match self {
            Self::Auto => AUTO_THEME_CHOICE,
            Self::Named(id) => id,
        }
    }

    /// Whether this choice re-resolves when the OS preference flips.
    pub fn follows_system(&self) -> bool {
        matches!(self, Self::Auto)
    }
}

/// The persisted choice, or `Auto` when nothing is persisted.
pub fn load_theme(store: &dyn KeyValueStore) -> ThemeChoice {
    ThemeChoice::from_stored(store.get(THEME_STORAGE_KEY).as_deref())
}

/// Persist a choice.
pub fn persist_theme(store: &dyn KeyValueStore, choice: &ThemeChoice) {
    store.set(THEME_STORAGE_KEY, choice.as_stored());
}

/// The theme id a choice resolves to.
///
/// `system` is the OS preference, or `None` when it cannot be read (no
/// `matchMedia`), which resolves `auto` to the default rather than guessing.
pub fn resolve_theme_id(choice: &ThemeChoice, system: Option<ThemeAppearance>) -> &'static str {
    match choice {
        ThemeChoice::Auto => match system {
            Some(ThemeAppearance::Dark) => SYSTEM_DARK_ID,
            Some(ThemeAppearance::Light) => SYSTEM_LIGHT_ID,
            None => DEFAULT_THEME_ID,
        },
        ThemeChoice::Named(id) => theme_by_id(id).map_or(DEFAULT_THEME_ID, |theme| theme.id),
    }
}

/// The theme a choice resolves to.
pub fn resolve_theme(choice: &ThemeChoice, system: Option<ThemeAppearance>) -> &'static Theme {
    theme_by_id(resolve_theme_id(choice, system)).unwrap_or_else(default_theme)
}

/// Everything applying a theme writes on `document.documentElement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeApplication {
    /// The `data-theme` attribute: the resolved id.
    pub theme_id: &'static str,
    /// The inline `color-scheme`.
    pub color_scheme: &'static str,
    /// Every canonical custom property and its value, inline on the root so it
    /// wins over the `:root` fallback in `theme-vars.css`.
    pub properties: Vec<(String, &'static str)>,
}

/// The writes for a choice under an OS preference.
pub fn theme_application(
    choice: &ThemeChoice,
    system: Option<ThemeAppearance>,
) -> ThemeApplication {
    let theme = resolve_theme(choice, system);
    ThemeApplication {
        theme_id: theme.id,
        color_scheme: theme.appearance.color_scheme(),
        properties: CanonicalToken::ALL
            .iter()
            .map(|token| (token.custom_property(), theme.token(*token)))
            .collect(),
    }
}
