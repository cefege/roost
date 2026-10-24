//! Roost's persisted appearance preferences and Libero startup integration.
//! App owns the preference signal above Libero's portal outlet; settings mutate it.
//! Libero owns scheme following, stylesheet publication and root attributes.

pub mod catalog;
pub mod choice;
pub mod palettes;

use std::rc::Rc;

use dioxus::prelude::*;
use libero::use_color_scheme;

use crate::platform::storage::LocalStorageKeyValueStore;
use choice::{ThemeChoice, prime_libero_scheme};

#[derive(Debug, Clone)]
pub struct ThemePreferences {
    pub choice: Signal<ThemeChoice>,
    pub storage: Rc<LocalStorageKeyValueStore>,
}

impl ThemePreferences {
    pub fn load() -> Self {
        let storage = Rc::new(LocalStorageKeyValueStore::new());
        let choice = prime_libero_scheme(storage.as_ref());
        Self {
            choice: Signal::new(choice),
            storage,
        }
    }
}

/// Storage can refuse writes while the provider still has to start dark.
#[component]
pub fn AppearanceBootstrap(children: Element) -> Element {
    let preferences = use_context::<ThemePreferences>();
    let scheme = use_color_scheme();
    use_hook(move || scheme.set(preferences.choice.peek().mode.libero_setting()));
    children
}
