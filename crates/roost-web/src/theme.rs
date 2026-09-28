//! The theme engine: the single source of truth for the active theme. Ported
//! from `apps/web/src/lib/theme.ts` (with `themeTokens.ts` in `tokens` and
//! `themes.ts` in `themes`). `App` calls [`apply_stored_theme`] once at boot,
//! before the first paint; the Settings theme picker calls [`apply_theme`].
//!
//! Applying a theme persists the CHOICE (`auto` or an id, through
//! `KeyValueStore`), resolves it against the OS preference, and writes every
//! canonical token inline on `document.documentElement` with `data-theme` and
//! `color-scheme` — inline custom properties win over the `:root` fallback in
//! `theme-vars.css`, where every other token aliases to them. The sequence is
//! [`apply_theme_with`] over a [`ThemeDocument`], so it runs natively in tests;
//! the browser document is `document::BrowserThemeDocument`.
//!
//! The picker's cross-fade is v2's `withViewTransition(() => applyTheme(choice))`
//! (`apps/web/src/browser/viewTransition.ts`), which the SHELL slice owns; the
//! picker composes that wrapper around [`apply_theme`].

pub mod choice;
pub mod themes;
pub mod tokens;

#[cfg(target_arch = "wasm32")]
mod document;

use roost_client_core::KeyValueStore;

use choice::{ThemeApplication, ThemeChoice, load_theme, persist_theme, theme_application};
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
        choice = choice.as_stored(),
        theme = application.theme_id,
        "theme applied"
    );
    application
}

/// The OS preference flipped: re-apply when, and only when, the persisted
/// choice follows it. An explicit Light or Dark does not change under the reader.
pub fn reapply_for_system_flip(
    store: &dyn KeyValueStore,
    document: &dyn ThemeDocument,
) -> Option<ThemeApplication> {
    let stored = load_theme(store);
    stored
        .follows_system()
        .then(|| apply_theme_with(store, document, &stored))
}

/// Apply the persisted theme and follow OS flips from now on. Called once, by
/// `App`, before the first component renders.
#[cfg(target_arch = "wasm32")]
pub fn apply_stored_theme() {
    use crate::platform::LocalStorageKeyValueStore;

    let store = LocalStorageKeyValueStore::new();
    apply_theme_with(&store, &document::BrowserThemeDocument, &load_theme(&store));
    document::follow_system_appearance(|| {
        let store = LocalStorageKeyValueStore::new();
        reapply_for_system_flip(&store, &document::BrowserThemeDocument);
    });
}

/// Apply the persisted theme. A native build has no document to theme.
#[cfg(not(target_arch = "wasm32"))]
pub fn apply_stored_theme() {}

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
