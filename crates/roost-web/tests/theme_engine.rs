//! The theme engine's behaviour, natively: what a stored choice loads as, what
//! it resolves to under each OS preference, what applying it persists and
//! writes, and when an OS flip re-applies. Exercises `roost_web::theme` and
//! `roost_web::theme::choice`, the port of `apps/web/src/lib/theme.ts`, over an
//! in-memory store and a recording document.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::{Cell, RefCell};

use roost_client_core::{KeyValueStore, MemoryKeyValueStore};
use roost_web::theme::choice::{
    THEME_STORAGE_KEY, ThemeApplication, ThemeChoice, load_theme, resolve_theme_id,
};
use roost_web::theme::themes::{DEFAULT_THEME_ID, SYSTEM_DARK_ID, SYSTEM_LIGHT_ID, theme_by_id};
use roost_web::theme::tokens::{CanonicalToken, ThemeAppearance};
use roost_web::theme::{ThemeDocument, apply_theme_with, reapply_for_system_flip};

/// A document that reports a fixed OS preference and records every write.
struct RecordingDocument {
    system: Cell<Option<ThemeAppearance>>,
    writes: RefCell<Vec<ThemeApplication>>,
}

impl RecordingDocument {
    fn new(system: Option<ThemeAppearance>) -> Self {
        Self {
            system: Cell::new(system),
            writes: RefCell::new(Vec::new()),
        }
    }
}

impl ThemeDocument for RecordingDocument {
    fn system_appearance(&self) -> Option<ThemeAppearance> {
        self.system.get()
    }

    fn write(&self, application: &ThemeApplication) {
        self.writes.borrow_mut().push(application.clone());
    }
}

fn named(id: &str) -> ThemeChoice {
    ThemeChoice::Named(id.to_string())
}

#[test]
fn nothing_stored_loads_as_auto() {
    let store = MemoryKeyValueStore::default();
    assert_eq!(load_theme(&store), ThemeChoice::Auto);
    store.set(THEME_STORAGE_KEY, "");
    assert_eq!(load_theme(&store), ThemeChoice::Auto);
}

#[test]
fn a_stored_id_loads_as_itself_even_when_unregistered() {
    // A newer client's theme id must survive a visit from this build: the
    // choice is kept verbatim and only its RESOLUTION falls back.
    let store = MemoryKeyValueStore::default();
    store.set(THEME_STORAGE_KEY, "light");
    assert_eq!(load_theme(&store), named("light"));
    store.set(THEME_STORAGE_KEY, "solarized-next");
    assert_eq!(load_theme(&store), named("solarized-next"));
    store.set(THEME_STORAGE_KEY, "auto");
    assert_eq!(load_theme(&store), ThemeChoice::Auto);
}

#[test]
fn auto_follows_the_os_and_falls_back_when_it_cannot_be_read() {
    assert_eq!(resolve_theme_id(&ThemeChoice::Auto, Some(ThemeAppearance::Dark)), SYSTEM_DARK_ID);
    assert_eq!(resolve_theme_id(&ThemeChoice::Auto, Some(ThemeAppearance::Light)), SYSTEM_LIGHT_ID);
    assert_eq!(resolve_theme_id(&ThemeChoice::Auto, None), DEFAULT_THEME_ID);
}

#[test]
fn an_explicit_choice_ignores_the_os() {
    assert_eq!(resolve_theme_id(&named("light"), Some(ThemeAppearance::Dark)), "light");
    assert_eq!(resolve_theme_id(&named(SYSTEM_DARK_ID), Some(ThemeAppearance::Light)), SYSTEM_DARK_ID);
}

#[test]
fn an_unregistered_choice_resolves_to_the_default() {
    assert_eq!(resolve_theme_id(&named("solarized-next"), Some(ThemeAppearance::Light)), DEFAULT_THEME_ID);
}

#[test]
fn applying_persists_the_choice_not_the_resolution() {
    // Persisting the resolved id would turn "follow the OS" into a pinned theme
    // the first time it was applied.
    let store = MemoryKeyValueStore::default();
    let document = RecordingDocument::new(Some(ThemeAppearance::Light));
    apply_theme_with(&store, &document, &ThemeChoice::Auto);
    assert_eq!(store.get(THEME_STORAGE_KEY).as_deref(), Some("auto"));
    apply_theme_with(&store, &document, &named("solarized-next"));
    assert_eq!(store.get(THEME_STORAGE_KEY).as_deref(), Some("solarized-next"));
}

#[test]
fn applying_writes_every_canonical_token_of_the_resolved_theme() {
    let store = MemoryKeyValueStore::default();
    let document = RecordingDocument::new(Some(ThemeAppearance::Light));
    let application = apply_theme_with(&store, &document, &ThemeChoice::Auto);
    let light = theme_by_id(SYSTEM_LIGHT_ID).unwrap();

    assert_eq!(application.theme_id, SYSTEM_LIGHT_ID);
    assert_eq!(application.color_scheme, "light");
    assert_eq!(application.properties.len(), CanonicalToken::ALL.len());
    for token in CanonicalToken::ALL {
        let written = application
            .properties
            .iter()
            .find(|(property, _)| *property == token.custom_property())
            .map(|(_, value)| *value);
        assert_eq!(written, Some(light.token(token)), "{}", token.name());
    }
    assert_eq!(document.writes.borrow().as_slice(), &[application]);
}

#[test]
fn a_dark_theme_writes_the_dark_colour_scheme() {
    let store = MemoryKeyValueStore::default();
    let document = RecordingDocument::new(None);
    let application = apply_theme_with(&store, &document, &named(SYSTEM_DARK_ID));
    assert_eq!(application.theme_id, SYSTEM_DARK_ID);
    assert_eq!(application.color_scheme, "dark");
}

#[test]
fn an_os_flip_reapplies_only_while_the_choice_is_auto() {
    let store = MemoryKeyValueStore::default();
    let document = RecordingDocument::new(Some(ThemeAppearance::Dark));
    apply_theme_with(&store, &document, &ThemeChoice::Auto);

    document.system.set(Some(ThemeAppearance::Light));
    let reapplied = reapply_for_system_flip(&store, &document).expect("auto follows the OS");
    assert_eq!(reapplied.theme_id, SYSTEM_LIGHT_ID);

    apply_theme_with(&store, &document, &named(SYSTEM_DARK_ID));
    let writes_before = document.writes.borrow().len();
    document.system.set(Some(ThemeAppearance::Dark));
    assert_eq!(reapply_for_system_flip(&store, &document), None);
    assert_eq!(document.writes.borrow().len(), writes_before);
}
