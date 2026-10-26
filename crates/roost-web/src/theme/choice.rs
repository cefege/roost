//! The theme choice a reader makes (base, accent, mode), its persistence through
//! `KeyValueStore`, and its resolution to the concrete custom properties to
//! write. Every rule the browser applies is decided here, natively testable.

use roost_client_core::KeyValueStore;

use super::catalog::{
    DEFAULT_ACCENT_ID, DEFAULT_BASE_ID, ThemeSpec, accent_theme, accent_themes, base_theme,
    base_themes, default_accent, default_base,
};
use super::omarchy::derive_roles;
use super::palettes::roost::{ROOST_DARK, ROOST_LIGHT};
use super::tokens::ThemeAppearance;

pub const BASE_STORAGE_KEY: &str = "roost.theme.base";
pub const ACCENT_STORAGE_KEY: &str = "roost.theme.accent";
pub const MODE_STORAGE_KEY: &str = "roost.theme.mode";

/// The mode the reader picked: follow the OS, or pin light or dark.
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
}

/// The reader's full theme choice. Ids are stored verbatim; an id no longer in
/// the catalog resolves to the default.
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

/// What a choice writes onto the document root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeApplication {
    pub base_id: &'static str,
    pub accent_id: &'static str,
    pub appearance: ThemeAppearance,
    pub properties: Vec<(&'static str, String)>,
}

/// Read the persisted choice. A missing or unrecognised value is the default.
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

/// Persist every part of the choice.
pub fn persist_theme(store: &dyn KeyValueStore, choice: &ThemeChoice) {
    store.set(BASE_STORAGE_KEY, &choice.base);
    store.set(ACCENT_STORAGE_KEY, &choice.accent);
    store.set(MODE_STORAGE_KEY, choice.mode.as_stored());
}

/// Resolve a mode against the OS preference. An unreadable preference draws dark.
pub fn resolve_appearance(mode: ThemeMode, system: Option<ThemeAppearance>) -> ThemeAppearance {
    match mode {
        ThemeMode::Light => ThemeAppearance::Light,
        ThemeMode::Dark => ThemeAppearance::Dark,
        ThemeMode::System => system.unwrap_or(ThemeAppearance::Dark),
    }
}

/// Resolve `choice` against the OS preference. An Omarchy base ignores the
/// stored accent and mode: its palette fixes both and derives every role.
/// Otherwise base values come first, then accent values (which override base
/// keys they share), then Roost's own roles.
pub fn theme_application(
    choice: &ThemeChoice,
    system: Option<ThemeAppearance>,
) -> ThemeApplication {
    let base = base_theme(&choice.base).unwrap_or_else(default_base);
    if let Some(palette) = base.omarchy {
        return ThemeApplication {
            base_id: base.id,
            accent_id: default_accent().id,
            appearance: palette.appearance,
            properties: derive_roles(palette),
        };
    }
    let appearance = resolve_appearance(choice.mode, system);
    let accent = accent_theme(&choice.accent).unwrap_or_else(default_accent);
    let (base_vars, accent_vars, roost_vars) = match appearance {
        ThemeAppearance::Light => (base.light, accent.light, ROOST_LIGHT),
        ThemeAppearance::Dark => (base.dark, accent.dark, ROOST_DARK),
    };
    ThemeApplication {
        base_id: base.id,
        accent_id: accent.id,
        appearance,
        properties: base_vars
            .iter()
            .chain(accent_vars)
            .chain(roost_vars)
            .map(|(name, value)| (*name, (*value).to_owned()))
            .collect(),
    }
}

/// The choice the settings pane shows. An Omarchy base fixes its accent and
/// mode, so the controls display those rather than the stored values, which
/// stay persisted for when the reader returns to a rust-ui base.
pub fn effective_choice(choice: &ThemeChoice) -> ThemeChoice {
    let fixed_palette = base_theme(&choice.base).and_then(|base: &ThemeSpec| base.omarchy);
    match fixed_palette {
        Some(palette) => ThemeChoice {
            base: choice.base.clone(),
            accent: DEFAULT_ACCENT_ID.to_owned(),
            mode: match palette.appearance {
                ThemeAppearance::Light => ThemeMode::Light,
                ThemeAppearance::Dark => ThemeMode::Dark,
            },
        },
        None => choice.clone(),
    }
}

/// Every custom property any palette may write. The document clears these
/// before writing a choice, so a value left by a previous palette cannot leak.
pub fn managed_properties() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = base_themes()
        .chain(accent_themes())
        .flat_map(|theme| theme.light.iter().chain(theme.dark))
        .chain(ROOST_LIGHT.iter().chain(ROOST_DARK))
        .map(|(name, _)| *name)
        .collect();
    if let Some(palette) = base_themes().find_map(|theme| theme.omarchy) {
        names.extend(derive_roles(palette).into_iter().map(|(name, _)| name));
    }
    names.sort_unstable();
    names.dedup();
    names
}
