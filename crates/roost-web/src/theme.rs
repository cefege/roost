//! The theme engine: the single source of truth for the active theme. `App`
//! calls [`apply_stored_theme`] once at boot, before the first paint, and
//! follows OS flips through [`use_follow_system_appearance`]; the Settings
//! theme picker calls [`apply_theme`].
//!
//! A choice is a base palette, an accent palette and a mode. Applying it
//! persists the three parts (`KeyValueStore`), resolves the mode against the OS
//! preference, clears every managed custom property, and writes the resolved
//! base, accent and Roost roles inline on `document.documentElement`, together
//! with the `dark` class rust-ui's `dark:` variant selects on. The sequence is
//! [`apply_theme_with`] over a [`ThemeDocument`], so it runs natively in tests;
//! the browser document is `document::BrowserThemeDocument`.

pub mod catalog;
pub mod choice;
pub mod omarchy;
pub mod palettes;
pub mod tokens;

#[cfg(target_arch = "wasm32")]
mod document;
pub(crate) mod terminal_palette;

use roost_client_core::KeyValueStore;

use choice::{
    ThemeApplication, ThemeChoice, ThemeMode, load_theme, persist_theme, theme_application,
};
use tokens::ThemeAppearance;

/// Where a theme is written, and where the OS preference is read.
pub trait ThemeDocument {
    /// The OS light/dark preference, or `None` when it cannot be read.
    fn system_appearance(&self) -> Option<ThemeAppearance>;
    /// Perform every write of an application on the document root.
    fn write(&self, application: &ThemeApplication);
}

/// Persist `choice` and apply the theme it resolves to. Returns what was
/// written.
pub fn apply_theme_with(
    store: &dyn KeyValueStore,
    document: &dyn ThemeDocument,
    choice: &ThemeChoice,
) -> ThemeApplication {
    persist_theme(store, choice);
    let application = theme_application(choice, document.system_appearance());
    document.write(&application);
    tracing::info!(
        target: "theme",
        base = application.base_id,
        accent = application.accent_id,
        mode = choice.mode.as_stored(),
        appearance = application.appearance.color_scheme(),
        "theme applied"
    );
    application
}

/// The OS preference flipped: re-apply when, and only when, the persisted mode
/// is System. An explicit Light or Dark does not change under the reader.
pub fn reapply_for_system_flip(
    store: &dyn KeyValueStore,
    document: &dyn ThemeDocument,
) -> Option<ThemeApplication> {
    let stored = load_theme(store);
    (stored.mode == ThemeMode::System).then(|| apply_theme_with(store, document, &stored))
}

/// Apply the persisted theme. Called once, by `App`, before the first
/// component renders.
#[cfg(target_arch = "wasm32")]
pub fn apply_stored_theme() {
    use crate::platform::LocalStorageKeyValueStore;

    let store = LocalStorageKeyValueStore::new();
    apply_theme_with(&store, &document::BrowserThemeDocument, &load_theme(&store));
}

/// Apply the persisted theme. A native build has no document to theme.
#[cfg(not(target_arch = "wasm32"))]
pub fn apply_stored_theme() {}

/// Follow the OS light/dark preference from now on: a flip re-applies the
/// persisted theme, which only a stored System mode changes under. Called once,
/// by `App`.
#[cfg(target_arch = "wasm32")]
pub fn use_follow_system_appearance() {
    use dioxus::prelude::use_effect;

    use crate::platform::LocalStorageKeyValueStore;

    let prefers_dark =
        crate::components::hooks::use_media_query::use_media_query(document::PREFERS_DARK_QUERY);
    use_effect(move || {
        // Reading the signal subscribes; the first run, after mount, re-applies
        // the theme `apply_stored_theme` just wrote.
        let _ = prefers_dark();
        reapply_for_system_flip(
            &LocalStorageKeyValueStore::new(),
            &document::BrowserThemeDocument,
        );
    });
}

/// A native build has no OS preference to follow.
#[cfg(not(target_arch = "wasm32"))]
pub fn use_follow_system_appearance() {}

/// Persist `choice` and apply it to this document, instantly.
#[cfg(target_arch = "wasm32")]
pub fn apply_theme(choice: &ThemeChoice) {
    let store = crate::platform::LocalStorageKeyValueStore::new();
    apply_theme_with(&store, &document::BrowserThemeDocument, choice);
}

/// Persist and apply `choice`. A native build has no document to theme.
#[cfg(not(target_arch = "wasm32"))]
pub fn apply_theme(_choice: &ThemeChoice) {}

/// The OS light/dark preference, for the picker's "System" preview.
#[cfg(target_arch = "wasm32")]
pub fn system_appearance() -> Option<ThemeAppearance> {
    document::BrowserThemeDocument.system_appearance()
}

/// The OS light/dark preference; a native build has none.
#[cfg(not(target_arch = "wasm32"))]
pub fn system_appearance() -> Option<ThemeAppearance> {
    None
}
