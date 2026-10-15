//! The hub's presets: named snapshots of the all-folders role chains, saved,
//! applied and deleted here or with `/modelpreset`. Both edit the one
//! `model_presets` map, and the coordinator re-validates it on save. Delete
//! asks once more in place, since a preset cannot be restored.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AgentSettingsView, PRESET_NAME_HINT, active_preset, valid_preset_name,
};

use crate::components::file_viewer::dom::focus_by_id;
use crate::components::md::{
    Button, ButtonSize, ButtonVariant, Chip, List, ListRow, SectionTitle, TextField,
};
use crate::components::settings::agent_roles::save_settings;
use crate::pump::Pump;

/// The presets section's element id. A delete moves focus here: the row is
/// gone, and a text field would raise a phone's keyboard unasked.
const SECTION_ID: &str = "agent-model-presets-section";

fn keep_id(preset: &str) -> String {
    format!("agent-model-preset-keep-{preset}")
}

fn delete_id(preset: &str) -> String {
    format!("agent-model-preset-delete-button-{preset}")
}

#[component]
pub(super) fn HubPresets(
    settings: Signal<AgentSettingsView>,
    error: Signal<String>,
    pump: Pump,
    compact: bool,
) -> Element {
    let mut error = error;
    let mut name = use_signal(String::new);
    // The preset whose Delete was pressed once and now asks to confirm.
    let mut confirming = use_signal(|| None::<String>);
    // Where focus goes once the row has re-rendered: the control that replaced
    // the one just pressed, so keyboard and screen-reader focus never falls
    // back to the page.
    let mut focus_after = use_signal(|| None::<String>);
    use_effect(move || {
        if let Some(id) = focus_after() {
            focus_by_id(&id);
            focus_after.set(None);
        }
    });
    let current = settings();
    let active = active_preset(&current.model_presets, &current.model_roles).map(str::to_owned);
    let size = if compact {
        ButtonSize::Default
    } else {
        ButtonSize::Sm
    };
    let save_pump = pump.clone();
    let mut save = move || {
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
    let mut save_on_enter = save.clone();
    rsx! {
        section {
            id: SECTION_ID,
            class: "model-hub__presets",
            tabindex: "-1",
            "aria-label": "Presets",
            "data-testid": "agent-model-presets",
            SectionTitle { "Presets" }
            if current.model_presets.is_empty() {
                p { class: "agent-settings__support",
                    "No presets yet. A preset saves every all-folders role chain under one name."
                }
            } else {
                List {
                    contained: true,
                    for (preset_name, roles) in current.model_presets.clone() {
                        {
                            let summary = roles
                                .iter()
                                .map(|(role, chain)| format!("{role}: {chain}"))
                                .collect::<Vec<_>>()
                                .join("; ");
                            let is_active = active.as_deref() == Some(preset_name.as_str());
                            let asking = confirming().as_deref() == Some(preset_name.as_str());
                            let apply_pump = pump.clone();
                            let delete_pump = pump.clone();
                            let apply_roles = roles.clone();
                            let delete_name = preset_name.clone();
                            let ask_name = preset_name.clone();
                            let keep_name = preset_name.clone();
                            rsx! {
                                ListRow {
                                    key: "{preset_name}",
                                    class: "model-hub__preset",
                                    headline: rsx! { "{preset_name}" },
                                    support: rsx! { "{summary}" },
                                    test_id: format!("agent-model-preset-{preset_name}"),
                                    trailing: rsx! {
                                        div { class: "model-hub__preset-actions",
                                            if asking {
                                                span { class: "model-hub__confirm", role: "status", "Delete {preset_name}?" }
                                                // Delete before Keep: a double tap on the first Delete
                                                // lands on Keep, never on the confirmation.
                                                Button {
                                                    variant: ButtonVariant::Destructive,
                                                    size,
                                                    "data-testid": "agent-model-preset-confirm-delete-{preset_name}",
                                                    onclick: move |_| {
                                                        let mut updated = settings.peek().clone();
                                                        updated.model_presets.remove(&delete_name);
                                                        save_settings(delete_pump.clone(), updated, settings, error);
                                                        confirming.set(None);
                                                        focus_after.set(Some(SECTION_ID.to_owned()));
                                                    },
                                                    "Delete"
                                                }
                                                Button {
                                                    id: keep_id(&preset_name),
                                                    variant: ButtonVariant::Ghost,
                                                    size,
                                                    "data-testid": "agent-model-preset-cancel-{preset_name}",
                                                    onclick: move |_| {
                                                        confirming.set(None);
                                                        focus_after.set(Some(delete_id(&keep_name)));
                                                    },
                                                    "Keep"
                                                }
                                            } else {
                                                if is_active {
                                                    Chip { label: "Active".to_owned(), small: true }
                                                }
                                                Button {
                                                    variant: ButtonVariant::Outline,
                                                    size,
                                                    disabled: is_active,
                                                    title: if is_active { "These roles are already in use" } else { "Make these the all-folders roles" },
                                                    "data-testid": "agent-model-preset-apply-{preset_name}",
                                                    onclick: move |_| {
                                                        let mut updated = settings.peek().clone();
                                                        updated.model_roles = apply_roles.clone();
                                                        save_settings(apply_pump.clone(), updated, settings, error);
                                                    },
                                                    "Apply"
                                                }
                                                Button {
                                                    id: delete_id(&preset_name),
                                                    variant: ButtonVariant::Ghost,
                                                    size,
                                                    "data-testid": "agent-model-preset-delete-{preset_name}",
                                                    onclick: move |_| {
                                                        focus_after.set(Some(keep_id(&ask_name)));
                                                        confirming.set(Some(ask_name.clone()));
                                                    },
                                                    "Delete"
                                                }
                                            }
                                        }
                                    },
                                }
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
                    autocomplete: "off".to_owned(),
                    max_length: 64,
                    test_id: "agent-model-preset-name".to_owned(),
                    onkeydown: move |event: KeyboardEvent| {
                        if event.key() == Key::Enter && !event.is_composing() {
                            event.prevent_default();
                            save_on_enter();
                        }
                    },
                }
                Button {
                    variant: ButtonVariant::Default,
                    disabled: name().trim().is_empty(),
                    "data-testid": "agent-model-preset-save",
                    onclick: move |_| save(),
                    "Save current roles"
                }
            }
        }
    }
}
