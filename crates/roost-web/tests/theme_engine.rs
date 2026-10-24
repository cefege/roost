//! Persistence migration and palette selection contracts.
//! Exercises validated legacy choices and Libero's static light/dark pairs.
//! Browser scheme and pane lifetime checks exercise the rendered provider separately.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use libero::theme::{COLOR_SCHEME_STORAGE_KEY, HexColor};
use roost_client_core::{KeyValueStore, MemoryKeyValueStore};
use roost_web::theme::catalog::theme_set_for_choice;
use roost_web::theme::choice::{
    ACCENT_STORAGE_KEY, BASE_STORAGE_KEY, MODE_STORAGE_KEY, ThemeChoice, ThemeMode, load_theme,
    persist_theme, prime_libero_scheme,
};

fn choice(base: &str, accent: &str, mode: ThemeMode) -> ThemeChoice {
    ThemeChoice {
        base: base.to_owned(),
        accent: accent.to_owned(),
        mode,
    }
}

#[test]
fn missing_legacy_mode_primes_dark_without_adopting_libero_system_default() {
    let store = MemoryKeyValueStore::default();
    store.set(COLOR_SCHEME_STORAGE_KEY, "system");
    let loaded = prime_libero_scheme(&store);
    assert_eq!(loaded, choice("neutral", "default", ThemeMode::Dark));
    assert_eq!(store.get(COLOR_SCHEME_STORAGE_KEY).as_deref(), Some("dark"));
}

#[test]
fn invalid_legacy_mode_primes_dark_and_invalid_palette_ids_are_not_reset_to_stock_libero() {
    let store = MemoryKeyValueStore::default();
    store.set(MODE_STORAGE_KEY, "sepia");
    store.set(BASE_STORAGE_KEY, "solarized-next");
    store.set(ACCENT_STORAGE_KEY, "retired-accent");
    assert_eq!(
        prime_libero_scheme(&store),
        choice("neutral", "default", ThemeMode::Dark)
    );
    assert_eq!(store.get(COLOR_SCHEME_STORAGE_KEY).as_deref(), Some("dark"));
}

#[test]
fn every_valid_legacy_mode_primes_the_same_libero_setting() {
    for mode in [ThemeMode::Light, ThemeMode::Dark, ThemeMode::System] {
        let store = MemoryKeyValueStore::default();
        persist_theme(&store, &choice("stone", "rose", mode));
        store.set(COLOR_SCHEME_STORAGE_KEY, "outdated");
        assert_eq!(prime_libero_scheme(&store), choice("stone", "rose", mode));
        assert_eq!(
            store.get(COLOR_SCHEME_STORAGE_KEY).as_deref(),
            Some(mode.as_stored())
        );
    }
}

#[test]
fn a_saved_choice_round_trips_all_three_roost_keys() {
    let store = MemoryKeyValueStore::default();
    persist_theme(&store, &choice("zinc", "blue", ThemeMode::Light));
    assert_eq!(store.get(BASE_STORAGE_KEY).as_deref(), Some("zinc"));
    assert_eq!(store.get(ACCENT_STORAGE_KEY).as_deref(), Some("blue"));
    assert_eq!(store.get(MODE_STORAGE_KEY).as_deref(), Some("light"));
    assert_eq!(load_theme(&store), choice("zinc", "blue", ThemeMode::Light));
}

#[test]
fn a_refused_store_still_returns_in_memory_dark_defaults() {
    struct RefusedStore;
    impl KeyValueStore for RefusedStore {
        fn get(&self, _key: &str) -> Option<String> {
            None
        }
        fn set(&self, _key: &str, _value: &str) {}
        fn remove(&self, _key: &str) {}
    }
    assert_eq!(
        prime_libero_scheme(&RefusedStore),
        choice("neutral", "default", ThemeMode::Dark)
    );
}

#[test]
fn neutral_and_accented_palettes_keep_surface_ink_and_accent_identity_in_both_schemes() {
    let neutral = theme_set_for_choice(&choice("neutral", "default", ThemeMode::Dark));
    assert_eq!(neutral.light_theme().surface, HexColor::new(0xFFFFFF));
    assert_eq!(neutral.light_theme().ink, HexColor::new(0x0A0A0A));
    assert_eq!(
        neutral.dark_theme().unwrap().surface,
        HexColor::new(0x0A0A0A)
    );
    let blue = theme_set_for_choice(&choice("neutral", "blue", ThemeMode::Dark));
    assert_eq!(blue.light_theme().surface, neutral.light_theme().surface);
    assert_eq!(
        blue.dark_theme().unwrap().ink,
        neutral.dark_theme().unwrap().ink
    );
    assert_ne!(blue.light_theme().primary, neutral.light_theme().primary);
    assert_ne!(
        blue.dark_theme().unwrap().primary,
        neutral.dark_theme().unwrap().primary
    );
}

#[test]
fn invalid_palette_lookup_uses_roost_defaults_not_libero_stock() {
    let fallback = theme_set_for_choice(&choice("unknown", "unknown", ThemeMode::Light));
    let default = theme_set_for_choice(&choice("neutral", "default", ThemeMode::Light));
    assert!(std::ptr::eq(fallback, default));
}

mod provider_lifetime {
    use std::cell::{Cell, RefCell};

    use dioxus::prelude::*;
    use libero::theme::HexColor;
    use libero::{LiberoProvider, ThemeSetHandle, use_theme, use_theme_set};
    use roost_web::theme::{AppearanceBootstrap, ThemePreferences};

    use super::{ThemeMode, choice, theme_set_for_choice};

    thread_local! {
        static THEMES: RefCell<Option<ThemeSetHandle>> = const { RefCell::new(None) };
        static MOUNTS: Cell<usize> = const { Cell::new(0) };
        static PRIMARY: Cell<Option<HexColor>> = const { Cell::new(None) };
    }

    fn retained_pane() -> Element {
        use_hook(|| MOUNTS.set(MOUNTS.get() + 1));
        PRIMARY.set(Some(use_theme().primary));
        rsx! { "Retained pane" }
    }

    fn palette_controller() -> Element {
        let themes = use_theme_set();
        use_hook(move || THEMES.with(|slot| slot.replace(Some(themes))));
        rsx! {}
    }

    fn root() -> Element {
        let preferences = use_context_provider(ThemePreferences::load);
        let themes = use_hook(|| theme_set_for_choice(&preferences.choice.peek()).clone());
        rsx! {
            LiberoProvider { themes,
                AppearanceBootstrap {
                    palette_controller {}
                    retained_pane {}
                }
            }
        }
    }

    fn settle(dom: &mut VirtualDom) {
        for _ in 0..8 {
            dom.render_immediate(&mut dioxus::core::NoOpMutations);
            dom.process_events();
        }
    }

    #[test]
    fn switching_base_and_accent_restyles_a_mounted_pane_without_remounting_it() {
        let mut dom = VirtualDom::new(root);
        dom.rebuild_in_place();
        settle(&mut dom);
        assert_eq!(MOUNTS.get(), 1);
        let initial = PRIMARY.get();
        let themes = THEMES.with(|slot| slot.borrow().clone()).unwrap();
        for selected in [
            choice("stone", "rose", ThemeMode::Dark),
            choice("mauve", "blue", ThemeMode::Dark),
        ] {
            let pair = theme_set_for_choice(&selected);
            dom.in_runtime(|| themes.set(pair.clone()));
            settle(&mut dom);
            assert_eq!(PRIMARY.get(), Some(pair.dark_theme().unwrap().primary));
            assert_ne!(PRIMARY.get(), initial);
            assert_eq!(MOUNTS.get(), 1);
        }
    }
}
