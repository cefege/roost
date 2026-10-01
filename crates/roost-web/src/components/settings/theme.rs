//! Settings → Interface → Theme: the appearance picker, plus the two device
//! modality switches that sit above it.
//!
//! Ports `apps/web/src/components/Settings/ThemePane.tsx`. Depends on
//! `crate::theme` for the engine and the registry, and on
//! `input_nav::modality_dom` for the TV and controller choices the same pane
//! offered.
//!
//! Each row previews itself: the swatch strip is read straight out of the
//! theme's canonical tokens, so the list cannot advertise colours the engine
//! does not write.

use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::KeyValueStore;
use roost_client_core::store::shell_intent::ShellIntent;

use crate::components::md::{Button, ButtonVariant, Icon, Select, SelectOption};
use crate::input_nav::modality::{ModeChoice, NavModality};
#[cfg(target_arch = "wasm32")]
use crate::input_nav::{set_pad_mode_choice, set_tv_mode_choice};
use crate::platform::storage::LocalStorageKeyValueStore;
use crate::pump::{Pump, use_store};
use crate::theme::choice::{AUTO_THEME_CHOICE, ThemeChoice, load_theme, resolve_theme_id};
use crate::theme::themes::{THEMES, theme_by_id};
use crate::theme::tokens::{CanonicalToken, ThemeAppearance, ThemeGroup};
use crate::theme::{apply_theme, system_appearance};

/// The five tokens that read as a recognisable preview of a theme.
const SWATCH_TOKENS: [CanonicalToken; 5] = [
    CanonicalToken::BgBase,
    CanonicalToken::Surface2,
    CanonicalToken::Accent,
    CanonicalToken::TextHi,
    CanonicalToken::StatusOk,
];

/// One pickable choice: the stored spelling, its wording, and its swatches.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ThemeEntry {
    choice: String,
    label: &'static str,
    support: &'static str,
    swatches: Vec<&'static str>,
}

/// One swatch cell's inline style: the token's canonical value, written
/// straight from the theme so a row cannot advertise a colour the engine does
/// not write.
fn swatch_style(swatch: &str) -> String {
    format!("width: 16px; height: 32px; background: {swatch};")
}

fn swatches_for(theme_id: &str) -> Vec<&'static str> {
    theme_by_id(theme_id)
        .map(|theme| {
            SWATCH_TOKENS
                .iter()
                .map(|token| theme.token(*token))
                .collect()
        })
        .unwrap_or_default()
}

/// Every choice, grouped the way the picker draws them.
fn entries() -> Vec<(ThemeGroup, Vec<ThemeEntry>)> {
    let mut groups: Vec<(ThemeGroup, Vec<ThemeEntry>)> = Vec::new();
    for group in ThemeGroup::ORDER {
        let mut rows: Vec<ThemeEntry> = Vec::new();
        if group == ThemeGroup::System {
            let resolved = resolve_theme_id(&ThemeChoice::Auto, system_appearance());
            rows.push(ThemeEntry {
                choice: AUTO_THEME_CHOICE.to_owned(),
                label: "System",
                support: "Follow the operating-system preference",
                swatches: swatches_for(resolved),
            });
        }
        for theme in THEMES {
            if theme.group != group {
                continue;
            }
            let support = match theme.appearance {
                ThemeAppearance::Dark => "Dark",
                ThemeAppearance::Light => "Light",
            };
            rows.push(ThemeEntry {
                choice: theme.id.to_owned(),
                label: theme.label,
                support,
                swatches: swatches_for(theme.id),
            });
        }
        if !rows.is_empty() {
            groups.push((group, rows));
        }
    }
    groups
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
    let stored = load_theme(storage.as_ref()).as_stored().to_owned();
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
    rsx! {
        div {
            class: "settings-pane",
            style: "max-width: 560px;",
            "data-testid": "theme-pane",
            div { style: "margin-block-end: var(--md-space-5);",
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
            }
            div { style: "margin-block-end: var(--md-space-5);",
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
            }
            p { class: "md-body-s", style: "color: var(--md-sys-color-on-surface-variant); margin: 0 0 var(--md-space-4);",
                "Pick an appearance. The swatches preview each theme's colors. Applies to this browser profile."
            }
            for (group, rows) in entries() {
                div { style: "margin-block-end: var(--md-space-5);",
                    h3 { class: "md-label-s",
                        style: "color: var(--md-sys-color-on-surface-variant); text-transform: uppercase; margin: 0 0 var(--md-space-2);",
                        {group.label()}
                    }
                    div { style: "display: flex; flex-direction: column; gap: var(--md-space-2);",
                        for row in rows {
                            ThemeRow { entry: row.clone(), selected: row.choice == stored, pump: pump.clone() }
                        }
                    }
                }
            }
        }
    }
}

/// One pickable appearance, with its swatch strip.
#[component]
fn ThemeRow(entry: ThemeEntry, selected: bool, pump: Pump) -> Element {
    let choice = entry.choice.clone();
    let label = entry.label;
    let support = entry.support;
    let swatch_styles: Vec<String> = entry
        .swatches
        .iter()
        .map(|swatch| swatch_style(swatch))
        .collect();
    let icon_name = if selected {
        "check_circle"
    } else {
        "radio_button_unchecked"
    }
    .to_owned();
    rsx! {
        Button {
            variant: ButtonVariant::Ghost,
            "data-testid": format!("theme-row-{choice}"),
            "data-selected": if selected { "true" } else { "false" },
            style: "display: flex; align-items: center; gap: var(--md-space-3); width: 100%; text-align: left;",
            onclick: move |_| {
                apply_theme(&ThemeChoice::from_stored(Some(choice.as_str())));
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionSucceeded {
                    message: "Theme saved".to_owned(),
                }));
            },
            span {
                style: "display: flex; flex-shrink: 0; border-radius: var(--md-shape-sm); overflow: hidden; border: 1px solid var(--md-sys-color-outline-variant);",
                for (index, style) in swatch_styles.iter().enumerate() {
                    span { style: style, key: "{index}" }
                }
            }
            span { style: "flex: 1; min-width: 0;",
                span { class: "md-body-m", style: "font-weight: 600; display: block;", {label} }
                span { class: "md-body-s",
                    style: "color: var(--md-sys-color-on-surface-variant); display: block;",
                    {support}
                }
            }
            Icon {
                name: icon_name,
                filled: selected,
            }
        }
    }
}
