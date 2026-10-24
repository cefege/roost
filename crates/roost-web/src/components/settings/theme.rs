//! Interface preferences: persisted appearance plus TV and controller policy.
//! ThemePane reads the root-owned choice; Libero owns schemes and option navigation.
//! Modality persistence remains in input_nav and never remounts the workbench.

use dioxus::prelude::*;
use libero::components::{
    Button, Combobox, ComboboxOption, ComboboxOptionArgs, Flex, Text, use_combobox,
};
use libero::sx::sx;
use libero::{ColorSchemeHandle, ThemeSetHandle, use_color_scheme, use_theme_set};
use roost_client_core::store::shell_intent::ShellIntent;
use roost_client_core::{ClientEvent, KeyValueStore};

use crate::input_nav::modality::{ModeChoice, NavModality};
#[cfg(target_arch = "wasm32")]
use crate::input_nav::{set_pad_mode_choice, set_tv_mode_choice};
use crate::pump::{Pump, use_store};
use crate::theme::ThemePreferences;
use crate::theme::catalog::{accent_themes, base_themes, theme_set_for_choice};
use crate::theme::choice::{ThemeChoice, ThemeMode, persist_theme};

fn change_theme(
    mut preferences: ThemePreferences,
    pump: &Pump,
    themes: &ThemeSetHandle,
    scheme: &ColorSchemeHandle,
    change: impl FnOnce(&mut ThemeChoice),
) {
    let previous = preferences.choice.peek().clone();
    let mut choice = previous.clone();
    change(&mut choice);
    if choice.base != previous.base || choice.accent != previous.accent {
        themes.set(theme_set_for_choice(&choice).clone());
    }
    if choice.mode != previous.mode {
        scheme.set(choice.mode.libero_setting());
    }
    persist_theme(preferences.storage.as_ref(), &choice);
    tracing::info!(target: "theme", base = choice.base, accent = choice.accent,
        mode = choice.mode.as_stored(), "appearance preference changed");
    preferences.choice.set(choice);
    pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
        message: "Theme saved".to_owned(),
    }));
}

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

#[component]
pub fn ThemePane() -> Element {
    let preferences = use_context::<ThemePreferences>();
    let pump = use_store();
    let modality = use_context::<Signal<NavModality>>();
    let stored = (preferences.choice)();
    let themes = use_theme_set();
    let scheme = use_color_scheme();
    let states = [
        use_combobox(),
        use_combobox(),
        use_combobox(),
        use_combobox(),
        use_combobox(),
    ];
    let layout = use_hook(|| {
        sx().flex_direction("column")
            .gap("md")
            .max_width("36rem")
            .min_width("0")
    });
    let row_layout = use_hook(|| sx().flex_direction("column").gap("xs"));

    let tv_change = use_callback({
        let pump = pump.clone();
        let storage = preferences.storage.clone();
        move |value: String| {
            if let Some(choice) = ModeChoice::parse_stored(&value) {
                persist_tv_choice(modality, storage.as_ref(), choice);
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
                    message: "TV mode saved".to_owned(),
                }));
            }
        }
    });
    let pad_change = use_callback({
        let pump = pump.clone();
        let storage = preferences.storage.clone();
        move |value: String| {
            if let Some(choice) = ModeChoice::parse_stored(&value) {
                persist_pad_choice(modality, storage.as_ref(), choice);
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
                    message: "Controller mode saved".to_owned(),
                }));
            }
        }
    });
    let base_change = use_callback({
        let preferences = preferences.clone();
        let pump = pump.clone();
        let themes = themes.clone();
        let scheme = scheme.clone();
        move |value: String| {
            change_theme(preferences.clone(), &pump, &themes, &scheme, |choice| {
                choice.base = value
            })
        }
    });
    let accent_change = use_callback({
        let preferences = preferences.clone();
        let pump = pump.clone();
        let themes = themes.clone();
        let scheme = scheme.clone();
        move |value: String| {
            change_theme(preferences.clone(), &pump, &themes, &scheme, |choice| {
                choice.accent = value
            })
        }
    });
    let mode_change = use_callback({
        let preferences = preferences.clone();
        move |value: String| {
            if let Some(mode) = ThemeMode::from_stored(&value) {
                change_theme(preferences.clone(), &pump, &themes, &scheme, |choice| {
                    choice.mode = mode
                });
            }
        }
    });
    let rows = [
        (
            "tv-mode-select",
            "TV mode",
            modality.read().tv_choice().as_str().to_owned(),
            vec![
                ("auto", "Auto (detect TV browser)"),
                ("on", "On"),
                ("off", "Off"),
            ],
            tv_change,
        ),
        (
            "pad-mode-select",
            "Game controller",
            modality.read().pad_choice().as_str().to_owned(),
            vec![
                ("auto", "Auto (when a controller is used)"),
                ("on", "On"),
                ("off", "Off"),
            ],
            pad_change,
        ),
        (
            "theme-base-select",
            "Base",
            stored.base,
            base_themes()
                .iter()
                .map(|theme| (theme.id, theme.label))
                .collect(),
            base_change,
        ),
        (
            "theme-accent-select",
            "Accent",
            stored.accent,
            accent_themes()
                .map(|theme| (theme.id, theme.label))
                .collect(),
            accent_change,
        ),
        (
            "theme-mode-select",
            "Mode",
            stored.mode.as_stored().to_owned(),
            ThemeMode::ALL
                .into_iter()
                .map(|mode| (mode.as_stored(), mode.label()))
                .collect(),
            mode_change,
        ),
    ];
    rsx! {
        Flex { sx: layout, "data-testid": "theme-pane",
            Text { size: "sm", color: "text-dimmed", "Pick a base, an accent and a mode. Applies to this browser profile." }
            for ((test_id, label, selected, options, on_change), state) in rows.into_iter().zip(states) {
                Flex { key: "{test_id}", sx: row_layout.clone(),
                    Text { size: "sm", "{label}" }
                    Combobox {
                        state, select_only: true, "aria-label": label,
                        selected_index: options.iter().position(|(id, _)| *id == selected),
                        option_label: move |row: (&'static str, &'static str)| row.1.to_owned(),
                        options: options.clone(),
                        option: {
                            let selected = selected.clone();
                            move |row: ComboboxOptionArgs<(&'static str, &'static str)>| rsx! {
                                ComboboxOption {
                                    selected: selected == row.value.0,
                                    onpick: move |_| { on_change.call(row.value.0.to_owned()); state.close(); },
                                    "{row.value.1}"
                                }
                            }
                        },
                        Button {
                            variant: "outlined", attributes: state.a11y_attributes(),
                            "data-testid": test_id, "aria-label": label,
                            onclick: move |_| state.toggle(), onblur: move |_| state.close(),
                            "{options.iter().find(|(id, _)| *id == selected).map(|(_, label)| *label).unwrap_or_default()}"
                        }
                    }
                }
            }
        }
    }
}
