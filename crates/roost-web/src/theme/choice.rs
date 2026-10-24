//! Persisted base, accent and mode identifiers, validated against Roost's catalog.
//! App primes Libero's scheme storage before mounting its provider.
//! Settings persist the same three Roost keys; Libero owns their presentation.

use libero::theme::ColorSchemeSetting;
use roost_client_core::KeyValueStore;

use super::catalog::{DEFAULT_ACCENT_ID, DEFAULT_BASE_ID, accent_theme, base_theme};

pub const BASE_STORAGE_KEY: &str = "roost.theme.base";
pub const ACCENT_STORAGE_KEY: &str = "roost.theme.accent";
pub const MODE_STORAGE_KEY: &str = "roost.theme.mode";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    System,
    Light,
    #[default]
    Dark,
}

impl ThemeMode {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub const fn as_stored(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_stored() == value)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    pub const fn libero_setting(self) -> ColorSchemeSetting {
        match self {
            Self::System => ColorSchemeSetting::System,
            Self::Light => ColorSchemeSetting::Light,
            Self::Dark => ColorSchemeSetting::Dark,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeChoice {
    pub base: String,
    pub accent: String,
    pub mode: ThemeMode,
}

impl Default for ThemeChoice {
    fn default() -> Self {
        Self {
            base: DEFAULT_BASE_ID.to_owned(),
            accent: DEFAULT_ACCENT_ID.to_owned(),
            mode: ThemeMode::default(),
        }
    }
}

pub fn load_theme(store: &dyn KeyValueStore) -> ThemeChoice {
    let default = ThemeChoice::default();
    ThemeChoice {
        base: store
            .get(BASE_STORAGE_KEY)
            .filter(|id| base_theme(id).is_some())
            .unwrap_or(default.base),
        accent: store
            .get(ACCENT_STORAGE_KEY)
            .filter(|id| accent_theme(id).is_some())
            .unwrap_or(default.accent),
        mode: store
            .get(MODE_STORAGE_KEY)
            .and_then(|value| ThemeMode::from_stored(&value))
            .unwrap_or(default.mode),
    }
}

pub fn persist_theme(store: &dyn KeyValueStore, choice: &ThemeChoice) {
    store.set(BASE_STORAGE_KEY, &choice.base);
    store.set(ACCENT_STORAGE_KEY, &choice.accent);
    store.set(MODE_STORAGE_KEY, choice.mode.as_stored());
}

pub fn prime_libero_scheme(store: &dyn KeyValueStore) -> ThemeChoice {
    let choice = load_theme(store);
    store.set(
        libero::theme::COLOR_SCHEME_STORAGE_KEY,
        choice.mode.as_stored(),
    );
    choice
}
