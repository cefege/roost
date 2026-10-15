//! The hub's presets: named snapshots of the all-folders role chains, saved,
//! applied and deleted here or with `/modelpreset`. Both edit the one
//! `model_presets` map, and the coordinator re-validates it on save.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AgentSettingsView, PRESET_NAME_HINT, active_preset, valid_preset_name,
};

use crate::components::md::{
    Button, ButtonSize, ButtonVariant, Chip, List, ListRow, SectionTitle, TextField,
};
use crate::components::settings::agent_roles::save_settings;
use crate::pump::Pump;

#[component]
pub(super) fn HubPresets(
    settings: Signal<AgentSettingsView>,
    error: Signal<String>,
    pump: Pump,
) -> Element {
    let mut error = error;
    let mut name = use_signal(String::new);
    let current = settings();
    let active = active_preset(&current.model_presets, &current.model_roles).map(str::to_owned);
    let save_pump = pump.clone();
    let save = move |_: MouseEvent| {
        let typed = name.peek().trim().to_owned();
        if !valid_preset_name(&typed) {
            error.set(format!(
                "Invalid preset name \"{typed}\": {PRESET_NAME_HINT}"
            ));
            return;
        }
        let mut updated = settings.peek().clone();
        let roles: BTreeMap<String, String> = updated
            .model_roles
            .iter()
            .filter(|(_, chain)| !chain.trim().is_empty())
            .map(|(role, chain)| (role.clone(), chain.clone()))
            .collect();
        if roles.is_empty() {
            error.set("Set at least one all-folders role before saving a preset.".to_owned());
            return;
        }
        updated.model_presets.insert(typed, roles);
        save_settings(save_pump.clone(), updated, settings, error);
        name.set(String::new());
    };
    rsx! {
        section { class: "model-hub__presets", "data-testid": "agent-model-presets",
            SectionTitle { "Presets" }
            if current.model_presets.is_empty() {
                p { class: "model-hub__empty",
                    "No presets yet. A preset saves every all-folders role chain under one name."
                }
            }
            List {
                for (preset_name, roles) in current.model_presets.clone() {
                    {
                        let summary = roles
                            .iter()
                            .map(|(role, chain)| format!("{role}: {chain}"))
                            .collect::<Vec<_>>()
                            .join("; ");
                        let is_active = active.as_deref() == Some(preset_name.as_str());
                        let apply_pump = pump.clone();
                        let delete_pump = pump.clone();
                        let apply_roles = roles.clone();
                        let delete_name = preset_name.clone();
                        rsx! {
                            ListRow {
                                key: "{preset_name}",
                                headline: rsx! { "{preset_name}" },
                                support: rsx! { "{summary}" },
                                test_id: format!("agent-model-preset-{preset_name}"),
                                trailing: rsx! {
                                    div { class: "model-hub__preset-actions",
                                        if is_active {
                                            Chip { label: "Active".to_owned(), small: true }
                                        }
                                        Button {
                                            variant: ButtonVariant::Outline,
                                            size: ButtonSize::Sm,
                                            "data-testid": "agent-model-preset-apply-{preset_name}",
                                            onclick: move |_| {
                                                let mut updated = settings.peek().clone();
                                                updated.model_roles = apply_roles.clone();
                                                save_settings(apply_pump.clone(), updated, settings, error);
                                            },
                                            "Apply"
                                        }
                                        Button {
                                            variant: ButtonVariant::Ghost,
                                            size: ButtonSize::Sm,
                                            "data-testid": "agent-model-preset-delete-{preset_name}",
                                            onclick: move |_| {
                                                let mut updated = settings.peek().clone();
                                                updated.model_presets.remove(&delete_name);
                                                save_settings(delete_pump.clone(), updated, settings, error);
                                            },
                                            "Delete"
                                        }
                                    }
                                },
                            }
                        }
                    }
                }
            }
            div { class: "model-hub__preset-save",
                TextField {
                    value: name(),
                    on_input: move |text: String| name.set(text),
                    placeholder: "Preset name".to_owned(),
                    aria_label: "Preset name".to_owned(),
                    test_id: "agent-model-preset-name".to_owned(),
                }
                Button {
                    variant: ButtonVariant::Default,
                    size: ButtonSize::Sm,
                    "data-testid": "agent-model-preset-save",
                    onclick: save,
                    "Save current roles"
                }
            }
        }
    }
}
