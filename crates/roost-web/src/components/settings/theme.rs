//! Settings → Interface → Theme: the base, accent and mode pickers, plus the two
//! device modality switches that sit above them.
//!
//! The pickers read their options from `crate::theme::catalog` (rust-ui's base
//! and accent palettes) and persist through `crate::theme::apply_theme`, which
//! writes the resolved palette onto the document root. The modality switches
//! depend on `input_nav::modality` for the TV and controller choices.

use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::KeyValueStore;
use roost_client_core::store::shell_intent::ShellIntent;

use crate::components::ui::{Select, SelectOption};
use crate::input_nav::modality::{ModeChoice, NavModality};
#[cfg(target_arch = "wasm32")]
use crate::input_nav::{set_pad_mode_choice, set_tv_mode_choice};
use crate::platform::storage::LocalStorageKeyValueStore;
use crate::pump::{Pump, use_store};
use crate::theme::apply_theme;
use crate::theme::catalog::{accent_themes, base_theme, base_themes};
use crate::theme::choice::{ThemeChoice, ThemeMode, effective_choice, load_theme};

/// Persist a changed theme choice and apply it. `change` edits the stored
/// choice; the result is written through the engine, which persists it too.
fn change_theme(storage: &dyn KeyValueStore, pump: &Pump, change: impl FnOnce(&mut ThemeChoice)) {
    let mut choice = load_theme(storage);
    change(&mut choice);
    apply_theme(&choice);
    pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
        message: "Theme saved".to_owned(),
    }));
}

/// Persist the TV choice and re-apply the root attribute the stylesheets key
/// off. The host build has no document, so it persists and nothing else.
#[cfg(target_arch = "wasm32")]
fn persist_tv_choice(
    modality: Signal<NavModality>,
    storage: &dyn KeyValueStore,
    choice: ModeChoice,
) {
    set_tv_mode_choice(modality, storage, choice);
}

#[cfg(not(target_arch = "wasm32"))]
fn persist_tv_choice(
    mut modality: Signal<NavModality>,
    storage: &dyn KeyValueStore,
    choice: ModeChoice,
) {
    modality.with_mut(|modality| modality.set_tv_choice(storage, choice));
}

/// The controller half of [`persist_tv_choice`].
#[cfg(target_arch = "wasm32")]
fn persist_pad_choice(
    modality: Signal<NavModality>,
    storage: &dyn KeyValueStore,
    choice: ModeChoice,
) {
    set_pad_mode_choice(modality, storage, choice);
}

#[cfg(not(target_arch = "wasm32"))]
fn persist_pad_choice(
    mut modality: Signal<NavModality>,
    storage: &dyn KeyValueStore,
    choice: ModeChoice,
) {
    modality.with_mut(|modality| modality.set_pad_choice(storage, choice));
}

/// The pane.
#[component]
pub fn ThemePane() -> Element {
    let storage = use_hook(|| Rc::new(LocalStorageKeyValueStore::new()));
    let pump = use_store();
    let modality = use_context::<Signal<NavModality>>();
    let stored = load_theme(storage.as_ref());
    let tv_choice = modality.peek().tv_choice();
    let pad_choice = modality.peek().pad_choice();

    let on_tv_change = {
        let tv_pump = pump.clone();
        let tv_modality = modality;
        let tv_storage = storage.clone();
        move |value: String| {
            let Some(choice) = ModeChoice::parse_stored(&value) else {
                return;
            };
            persist_tv_choice(tv_modality, tv_storage.as_ref(), choice);
            tv_pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
                message: "TV mode saved".to_owned(),
            }));
        }
    };
    let on_pad_change = {
        let pad_pump = pump.clone();
        let pad_modality = modality;
        let pad_storage = storage.clone();
        move |value: String| {
            let Some(choice) = ModeChoice::parse_stored(&value) else {
                return;
            };
            persist_pad_choice(pad_modality, pad_storage.as_ref(), choice);
            pad_pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
                message: "Controller mode saved".to_owned(),
            }));
        }
    };
    let on_base_change = {
        let base_pump = pump.clone();
        let base_storage = storage.clone();
        move |value: String| {
            change_theme(base_storage.as_ref(), &base_pump, |choice| {
                choice.base = value
            });
        }
    };
    let on_accent_change = {
        let accent_pump = pump.clone();
        let accent_storage = storage.clone();
        move |value: String| {
            change_theme(accent_storage.as_ref(), &accent_pump, |choice| {
                choice.accent = value;
            });
        }
    };
    let on_mode_change = {
        let mode_pump = pump.clone();
        let mode_storage = storage.clone();
        move |value: String| {
            let Some(mode) = ThemeMode::from_stored(&value) else {
                return;
            };
            change_theme(mode_storage.as_ref(), &mode_pump, |choice| {
                choice.mode = mode
            });
        }
    };
    let shown = effective_choice(&stored);
    let fixed = base_theme(&stored.base).is_some_and(|base| base.omarchy.is_some());
    rsx! {
        div {
            class: "flex max-w-xl flex-col gap-5",
            "data-testid": "theme-pane",
            Select {
                test_id: "tv-mode-select",
                label: "TV mode",
                value: tv_choice.as_str().to_owned(),
                options: vec![
                    SelectOption::new("auto", "Auto (detect TV browser)"),
                    SelectOption::new("on", "On"),
                    SelectOption::new("off", "Off"),
                ],
                on_change: on_tv_change,
            }
            Select {
                test_id: "pad-mode-select",
                label: "Game controller",
                value: pad_choice.as_str().to_owned(),
                options: vec![
                    SelectOption::new("auto", "Auto (when a controller is used)"),
                    SelectOption::new("on", "On"),
                    SelectOption::new("off", "Off"),
                ],
                on_change: on_pad_change,
            }
            p { class: "text-sm text-muted-foreground",
                "Pick a base, an accent and a mode. Applies to this browser profile."
            }
            Select {
                test_id: "theme-base-select",
                label: "Base",
                value: stored.base.clone(),
                options: base_themes()
                    .map(|theme| SelectOption::new(theme.id, theme.label))
                    .collect::<Vec<_>>(),
                on_change: on_base_change,
            }
            Select {
                test_id: "theme-accent-select",
                label: "Accent",
                value: shown.accent.clone(),
                disabled: fixed,
                options: accent_themes()
                    .map(|theme| SelectOption::new(theme.id, theme.label))
                    .collect::<Vec<_>>(),
                on_change: on_accent_change,
            }
            Select {
                test_id: "theme-mode-select",
                label: "Mode",
                value: shown.mode.as_stored().to_owned(),
                disabled: fixed,
                options: ThemeMode::ALL
                    .into_iter()
                    .map(|mode| SelectOption::new(mode.as_stored(), mode.label()))
                    .collect::<Vec<_>>(),
                on_change: on_mode_change,
            }
        }
    }
}
