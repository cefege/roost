//! The composer's model and thinking pickers. The current model always shows,
//! whether or not the catalog has loaded or still offers it; choosing sends
//! `AgentChatConfigure`. Mounted by `agent_chat::composer`; the menus open
//! upward because the composer sits at the bottom of its pane.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog};

use super::toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::pump::Pump;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::ConfigureAgentChat;

#[component]
pub fn AgentModelControls(
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    pump: Pump,
) -> Element {
    let current = conversation
        .model
        .as_ref()
        .map(|model| format!("{}/{}", model.provider, model.model_id));
    let model_items = model_menu_items(catalog.as_ref(), &conversation);
    let model_label = current_model_label(catalog.as_ref(), &conversation);
    let reasoning = catalog.as_ref().is_some_and(|catalog| {
        conversation.model.as_ref().is_some_and(|model| {
            catalog.models.iter().any(|entry| {
                entry.provider == model.provider
                    && entry.model_id == model.model_id
                    && entry.reasoning
            })
        })
    });
    let thinking = conversation
        .thinking_level
        .clone()
        .unwrap_or_else(|| "off".to_string());
    let thinking_items = catalog
        .as_ref()
        .map(|catalog| {
            catalog
                .thinking_levels
                .iter()
                .map(|level| {
                    ToolbarMenuItem::choice(
                        level.clone(),
                        thinking_label(level),
                        *level == thinking,
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let model_pump = pump.clone();
    let model_id = conversation.id.clone();
    let thinking_pump = pump;
    let thinking_id = conversation.id.clone();
    rsx! {
        div { class: "agent-chat__model-controls",
            ToolbarMenu {
                menu_key: "model",
                trigger: ToolbarTrigger::Picker { label: model_label },
                aria_label: "Model",
                items: model_items,
                opens_up: true,
                on_choose: move |value: String| {
                    if Some(&value) != current.as_ref() {
                        configure(model_pump.clone(), model_id.clone(), Configure::Model(value));
                    }
                },
            }
            if reasoning && !thinking_items.is_empty() {
                ToolbarMenu {
                    menu_key: "thinking",
                    trigger: ToolbarTrigger::Picker { label: format!("Thinking · {}", thinking_label(&thinking)) },
                    aria_label: "Thinking effort",
                    items: thinking_items,
                    opens_up: true,
                    on_choose: move |level: String| {
                        configure(thinking_pump.clone(), thinking_id.clone(), Configure::Thinking(level))
                    },
                }
            }
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
enum Configure {
    Model(String),
    Thinking(String),
}

fn thinking_label(level: &str) -> String {
    let mut characters = level.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
}

/// Available models, plus the conversation's own when the catalog no longer
/// lists it as available, so the picker can always show what is in use.
fn model_menu_items(
    catalog: Option<&ModelsCatalog>,
    conversation: &ConversationSummary,
) -> Vec<ToolbarMenuItem> {
    let current = conversation.model.as_ref();
    let mut items: Vec<ToolbarMenuItem> = catalog
        .map(|catalog| {
            catalog
                .models
                .iter()
                .filter(|model| model.available)
                .map(|model| ToolbarMenuItem {
                    detail: Some(model.provider.clone()),
                    ..ToolbarMenuItem::choice(
                        format!("{}/{}", model.provider, model.model_id),
                        model.name.clone(),
                        current.is_some_and(|current| {
                            current.provider == model.provider && current.model_id == model.model_id
                        }),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(current) = current
        && !items.iter().any(|item| item.selected)
    {
        items.insert(
            0,
            ToolbarMenuItem {
                detail: Some(current.provider.clone()),
                ..ToolbarMenuItem::choice(
                    format!("{}/{}", current.provider, current.model_id),
                    current.model_id.clone(),
                    true,
                )
            },
        );
    }
    items
}

fn current_model_label(
    catalog: Option<&ModelsCatalog>,
    conversation: &ConversationSummary,
) -> String {
    let Some(current) = conversation.model.as_ref() else {
        return "Choose model".to_string();
    };
    catalog
        .and_then(|catalog| {
            catalog.models.iter().find(|model| {
                model.provider == current.provider && model.model_id == current.model_id
            })
        })
        .map_or_else(|| current.model_id.clone(), |model| model.name.clone())
}

#[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
fn configure(pump: Pump, conversation_id: String, change: Configure) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let call = match change {
            Configure::Model(value) => {
                let Some((provider, model_id)) = value.split_once('/') else {
                    return;
                };
                ConfigureAgentChat {
                    conversation_id,
                    model_provider: Some(provider.to_owned()),
                    model_id: Some(model_id.to_owned()),
                    ..Default::default()
                }
            }
            Configure::Thinking(level) => ConfigureAgentChat {
                conversation_id,
                thinking_level: Some(level),
                ..Default::default()
            },
        };
        if let Err(error) = pump.rpc().call(&call).await {
            tracing::warn!(target: "agent_chat", %error, "agent configure failed");
        }
    });
}
