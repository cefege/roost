//! The theme engine's behaviour, natively: what a stored choice loads as, what
//! it resolves to under each mode, what applying it persists and writes, and
//! when an OS flip re-applies. Exercises `roost_web::theme` and
//! `roost_web::theme::choice` over an in-memory store and a recording document.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::{Cell, RefCell};

use roost_client_core::{KeyValueStore, MemoryKeyValueStore};
use roost_web::theme::choice::{
    ACCENT_STORAGE_KEY, BASE_STORAGE_KEY, MODE_STORAGE_KEY, ThemeApplication, ThemeChoice,
    ThemeMode, effective_choice, load_theme, managed_properties, persist_theme, resolve_appearance,
    theme_application,
};
use roost_web::theme::tokens::ThemeAppearance;
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

fn choice(base: &str, accent: &str, mode: ThemeMode) -> ThemeChoice {
    ThemeChoice {
        base: base.to_owned(),
        accent: accent.to_owned(),
        mode,
    }
}

#[test]
fn an_empty_store_loads_the_default_tokyo_night_dark_choice() {
    let store = MemoryKeyValueStore::default();
    let loaded = load_theme(&store);
    assert_eq!(loaded, ThemeChoice::default());
    assert_eq!(loaded.base, "tokyo-night");
    assert_eq!(loaded.accent, "default");
    assert_eq!(loaded.mode, ThemeMode::Dark);
}

#[test]
fn an_unknown_stored_base_falls_back_to_the_default() {
    let store = MemoryKeyValueStore::default();
    store.set(BASE_STORAGE_KEY, "solarized-next");
    assert_eq!(load_theme(&store).base, "tokyo-night");
}

#[test]
fn the_legacy_single_key_is_ignored() {
    let store = MemoryKeyValueStore::default();
    store.set("roost.theme", "graphite");
    assert_eq!(load_theme(&store), ThemeChoice::default());
}

#[test]
fn a_saved_choice_round_trips_through_the_store() {
    let store = MemoryKeyValueStore::default();
    let saved = choice("zinc", "blue", ThemeMode::Light);
    persist_theme(&store, &saved);
    assert_eq!(store.get(BASE_STORAGE_KEY).as_deref(), Some("zinc"));
    assert_eq!(store.get(ACCENT_STORAGE_KEY).as_deref(), Some("blue"));
    assert_eq!(store.get(MODE_STORAGE_KEY).as_deref(), Some("light"));
    assert_eq!(load_theme(&store), saved);
}

#[test]
fn an_explicit_mode_ignores_the_os_preference() {
    assert_eq!(
        resolve_appearance(ThemeMode::Dark, Some(ThemeAppearance::Light)),
        ThemeAppearance::Dark
    );
    assert_eq!(
        resolve_appearance(ThemeMode::Light, Some(ThemeAppearance::Dark)),
        ThemeAppearance::Light
    );
}

#[test]
fn system_mode_follows_the_os_preference() {
    assert_eq!(
        resolve_appearance(ThemeMode::System, Some(ThemeAppearance::Light)),
        ThemeAppearance::Light
    );
}

#[test]
fn applying_a_choice_persists_it_and_writes_one_application() {
    let store = MemoryKeyValueStore::default();
    let document = RecordingDocument::new(Some(ThemeAppearance::Dark));
    let applied = apply_theme_with(&store, &document, &choice("stone", "rose", ThemeMode::Dark));

    assert_eq!(applied.base_id, "stone");
    assert_eq!(applied.accent_id, "rose");
    assert_eq!(applied.appearance, ThemeAppearance::Dark);
    assert_eq!(
        document.writes.borrow().as_slice(),
        std::slice::from_ref(&applied)
    );
    assert_eq!(load_theme(&store).base, "stone");
}

#[test]
fn every_written_property_is_one_the_engine_manages() {
    let application = theme_application(
        &choice("mauve", "violet", ThemeMode::Light),
        Some(ThemeAppearance::Light),
    );
    let managed = managed_properties();
    assert!(!application.properties.is_empty());
    for (key, _) in &application.properties {
        assert!(managed.contains(key), "{key} is written but not managed");
    }
}

#[test]
fn an_omarchy_base_fixes_its_appearance_and_ignores_the_stored_accent() {
    let application = theme_application(
        &choice("tokyo-night", "violet", ThemeMode::Light),
        Some(ThemeAppearance::Light),
    );
    assert_eq!(application.base_id, "tokyo-night");
    assert_eq!(application.accent_id, "default");
    assert_eq!(application.appearance, ThemeAppearance::Dark);
    assert!(
        application
            .properties
            .iter()
            .any(|(name, _)| *name == "--background")
    );
}

#[test]
fn the_settings_pane_shows_an_omarchy_base_with_its_fixed_accent_and_mode() {
    assert_eq!(
        effective_choice(&choice("nord", "violet", ThemeMode::Light)),
        choice("nord", "default", ThemeMode::Dark)
    );
    assert_eq!(
        effective_choice(&choice("neutral", "violet", ThemeMode::Light)),
        choice("neutral", "violet", ThemeMode::Light)
    );
}

#[test]
fn managed_properties_are_sorted_and_unique() {
    let managed = managed_properties();
    let mut sorted = managed.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(managed, sorted);
}

#[test]
fn an_os_flip_reapplies_only_under_system_mode() {
    let store = MemoryKeyValueStore::default();
    let document = RecordingDocument::new(Some(ThemeAppearance::Light));

    persist_theme(&store, &choice("neutral", "default", ThemeMode::Dark));
    assert!(reapply_for_system_flip(&store, &document).is_none());

    persist_theme(&store, &choice("neutral", "default", ThemeMode::System));
    let reapplied = reapply_for_system_flip(&store, &document).expect("system re-applies");
    assert_eq!(reapplied.appearance, ThemeAppearance::Light);
    assert_eq!(document.writes.borrow().len(), 1);
}
