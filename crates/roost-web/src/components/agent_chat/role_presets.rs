//! The presets row at the top of the composer's model panel: one chip per
//! preset in name order (starters and saved ones together), the one in use
//! checked; a tap applies it to all folders. "Save as" stores the current
//! roles under a name, replacing a preset of that name. Mounted by
//! `role_switcher`; shares its pure rules with the models hub (`role_switch`).

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AgentSettingsView, ConversationSummary, active_preset, presets_with_builtins,
};

use super::model_controls::configure_model_pin;
use super::role_switch::{pin_hides, with_applied_preset, with_saved_preset};
use crate::components::settings::agent_roles::save_settings;
use crate::components::ui::{Button, ButtonSize, ButtonVariant, Chip, IconType, TextField};
use crate::pump::Pump;

#[component]
pub(super) fn RolePresets(
    conversation: ConversationSummary,
    settings: Signal<AgentSettingsView>,
    error: Signal<String>,
    pump: Pump,
) -> Element {
    let mut error = error;
    let mut naming = use_signal(|| false);
    let mut name = use_signal(String::new);
    let current = settings();
    let presets = presets_with_builtins(&current.model_presets);
    let active = active_preset(&presets, &current.model_roles).map(str::to_owned);
    // Applying moves Default and plan; a pin on the role this chat runs
    // would keep the chat where it was.
    let clear_pin = conversation.model.is_some()
        && ["default", "plan"]
            .iter()
            .any(|role| pin_hides(role, conversation.running_role()));
    let typed = name().trim().to_owned();
    let replaces = presets.contains_key(&typed);
    let save_pump = pump.clone();
    let mut save = move || {
        let typed = name.peek().trim().to_owned();
        match with_saved_preset(&settings.peek(), &typed) {
            Ok(updated) => {
                save_settings(save_pump.clone(), updated, settings, error);
                name.set(String::new());
                naming.set(false);
            }
            Err(message) => error.set(message),
        }
    };
    let mut save_on_enter = save.clone();
    rsx! {
        div { class: "flex flex-wrap items-center gap-2", role: "group", "aria-label": "Presets",
            "data-testid": "agent-roles-presets",
            for (preset_name, roles) in presets {
                {
                    let is_active = active.as_deref() == Some(preset_name.as_str());
                    let apply_pump = pump.clone();
                    let conversation_id = conversation.id.clone();
                    rsx! {
                        Chip {
                            key: "{preset_name}",
                            label: preset_name.clone(),
                            icon: is_active.then_some(IconType::Check),
                            selected: Some(is_active),
                            test_id: Some(format!("agent-roles-preset-{preset_name}")),
                            title: Some(if is_active {
                                "These are the all-folders roles now".to_owned()
                            } else {
                                format!("Use {preset_name} for every role, in all folders")
                            }),
                            onclick: Some(EventHandler::new(move |_| {
                                if is_active {
                                    return;
                                }
                                let updated = with_applied_preset(&settings.peek(), &roles);
                                save_settings(apply_pump.clone(), updated, settings, error);
                                if clear_pin {
                                    configure_model_pin(apply_pump.clone(), conversation_id.clone(), None);
                                }
                            })),
                        }
                    }
                }
            }
            if !naming() {
                Chip {
                    label: "Save as…".to_owned(),
                    icon: Some(IconType::Plus),
                    test_id: Some("agent-roles-preset-save".to_owned()),
                    title: Some("Save these roles as a preset".to_owned()),
                    onclick: Some(EventHandler::new(move |_| naming.set(true))),
                }
            }
        }
        if naming() {
            div { class: "flex flex-wrap items-center gap-2",
                TextField {
                    value: name(),
                    on_input: move |text: String| name.set(text),
                    placeholder: "Preset name".to_owned(),
                    aria_label: "Preset name".to_owned(),
                    autocomplete: "off".to_owned(),
                    max_length: 64,
                    test_id: "agent-roles-preset-name".to_owned(),
                    onkeydown: move |event: KeyboardEvent| {
                        if event.key() == Key::Named(NamedKey::Enter) && !event.is_composing() {
                            event.prevent_default();
                            save_on_enter();
                        }
                    },
                }
                Button {
                    variant: ButtonVariant::Default,
                    size: ButtonSize::Sm,
                    disabled: typed.is_empty(),
                    "data-testid": "agent-roles-preset-save-confirm",
                    onclick: move |_| save(),
                    if replaces { "Replace {typed}" } else { "Save" }
                }
                Button {
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::Sm,
                    onclick: move |_| {
                        naming.set(false);
                        name.set(String::new());
                    },
                    "Cancel"
                }
            }
        }
    }
}
