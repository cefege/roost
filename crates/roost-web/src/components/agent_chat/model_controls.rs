//! The composer's model and thinking pickers, and the setup card shown while
//! no model can run. The current model always shows, whether or not the
//! catalog has loaded or still offers it; choosing sends `AgentChatConfigure`.
//! Mounted by `agent_chat::composer` and `agent_chat::surface`; the menus
//! open upward because the composer sits at the bottom of its pane.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog};

use super::toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::md::{Button, ButtonSize, ButtonVariant, Card, CardVariant};
use crate::pump::Pump;
use crate::router_state::use_navigate;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::ConfigureAgentChat;

/// The model menu's last row, which opens Settings → Agents → Models.
const MANAGE_MODELS: &str = "manage-models";

/// Where the model catalog stands, as the chat surface fetched it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSetup {
    /// At least one model can run, or the catalog is still loading.
    Ready,
    /// The catalog loaded and no connected provider offers a model.
    NoProvider,
    /// The catalog request failed.
    CatalogFailed,
}

impl ModelSetup {
    pub fn from_catalog(catalog: Option<&ModelsCatalog>, failed: bool) -> Self {
        match catalog {
            Some(catalog) if catalog.models.iter().any(|model| model.available) => Self::Ready,
            Some(_) => Self::NoProvider,
            None if failed => Self::CatalogFailed,
            None => Self::Ready,
        }
    }
}

#[component]
pub fn AgentModelControls(
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    catalog_failed: bool,
    pump: Pump,
) -> Element {
    let navigate = use_navigate();
    let open_settings = move || navigate.call(crate::routes::settings_pane_href("models"));
    let setup = ModelSetup::from_catalog(catalog.as_ref(), catalog_failed);
    // Nothing to pick from: a button that says why, never an empty menu. The
    // id is the picker's, so `/model` lands here too.
    let placeholder = match (conversation.model.is_some(), catalog.is_some(), setup) {
        (true, _, _) | (false, true, ModelSetup::Ready) => None,
        (false, _, ModelSetup::NoProvider) => Some(("Connect a model", "add_link", false)),
        (false, _, ModelSetup::CatalogFailed) => Some(("Models unavailable", "error", false)),
        (false, false, ModelSetup::Ready) => Some(("Loading models…", "hourglass_empty", true)),
    };
    if let Some((label, icon, waiting)) = placeholder {
        return rsx! {
            div { class: "agent-chat__model-controls",
                Button {
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::Sm,
                    class: "agent-chat__picker",
                    id: "agent-chat-model-trigger",
                    icon: icon.to_string(),
                    disabled: waiting,
                    "aria-busy": waiting.then_some("true"),
                    "data-testid": "agent-chat-model-setup",
                    title: "Open Settings → Agents → Models",
                    onclick: move |_| open_settings(),
                    span { class: "agent-chat__picker-label", "{label}" }
                }
            }
        };
    }
    let current = conversation
        .model
        .as_ref()
        .map(|model| format!("{}/{}", model.provider, model.model_id));
    let mut model_items = model_menu_items(catalog.as_ref(), &conversation);
    model_items.push(ToolbarMenuItem::choice(
        MANAGE_MODELS,
        if setup == ModelSetup::Ready {
            "Manage models…"
        } else {
            "Connect a model provider…"
        },
        false,
    ));
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
                    if value == MANAGE_MODELS {
                        open_settings();
                    } else if Some(&value) != current.as_ref() {
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

/// Shown above the composer while no model can run: what is wrong and the
/// one button that fixes it.
#[component]
pub fn ModelSetupCard(setup: ModelSetup, on_retry: EventHandler<()>) -> Element {
    let navigate = use_navigate();
    let open_settings = move |_| navigate.call(crate::routes::settings_pane_href("models"));
    let (title, body) = match setup {
        ModelSetup::Ready => return rsx! {},
        ModelSetup::NoProvider => (
            "Connect a model to get started",
            "The agent needs a model provider. Sign in with a subscription or add an API key, then pick a model here.",
        ),
        ModelSetup::CatalogFailed => (
            "Couldn't load the model list",
            "Roost didn't answer with the models it can use. Try again, or check your providers in settings.",
        ),
    };
    rsx! {
        Card {
            title: Some(title.to_string()),
            variant: CardVariant::Outlined,
            class: "agent-chat__notice-card agent-chat__notice-card--info agent-chat__model-setup",
            test_id: Some("agent-chat-model-setup-card".to_string()),
            children: rsx! {
                p { class: "agent-chat__model-setup-body", "{body}" }
                div { class: "agent-chat__model-setup-actions",
                    if setup == ModelSetup::CatalogFailed {
                        Button {
                            variant: ButtonVariant::Outline,
                            icon: "refresh".to_string(),
                            onclick: move |_| on_retry.call(()),
                            "Try again"
                        }
                    }
                    Button {
                        icon: "add_link".to_string(),
                        onclick: open_settings,
                        "data-testid": "agent-chat-model-setup-open",
                        if setup == ModelSetup::NoProvider { "Connect a model" } else { "Open model settings" }
                    }
                }
            },
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
