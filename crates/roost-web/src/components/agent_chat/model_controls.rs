//! The composer's model and thinking pickers, and the setup card shown while
//! no model can run. The current model always shows, whether or not the
//! catalog has loaded or still offers it. Choosing sends `AgentChatConfigure`;
//! "Use folder default" sends empty ids, which clears the session override.
//! Mounted by `agent_chat::composer` and `agent_chat::surface`; the thinking
//! menu opens upward because the composer sits at the bottom of its pane.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog};

use super::model_picker::{ModelPicker, PickerOption};
use super::toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::md::{Button, ButtonSize, ButtonVariant, Card, CardVariant, Dialog};
use crate::pump::Pump;
use crate::router_state::use_navigate;

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::ConfigureAgentChat;

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
            Some(catalog)
                if catalog
                    .models
                    .iter()
                    .any(|model| model.available && !model.classifier) =>
            {
                Self::Ready
            }
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
    let mut picker_open = use_signal(|| false);
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
    let model_label = current_model_label(catalog.as_ref(), &conversation);
    let selected_selector = conversation
        .effective_model
        .as_ref()
        .or(conversation.model.as_ref())
        .map(|model| format!("{}/{}", model.provider, model.model_id));
    let picker_options = model_picker_options(catalog.as_ref(), &conversation);
    let reasoning = supports_reasoning(&conversation, catalog.as_ref());
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
    let choose_pump = pump.clone();
    let clear_pump = pump.clone();
    let thinking_pump = pump;
    let model_id = conversation.id.clone();
    let clear_id = conversation.id.clone();
    let thinking_id = conversation.id.clone();
    rsx! {
        div { class: "agent-chat__model-controls",
            Button {
                variant: ButtonVariant::Ghost,
                size: ButtonSize::Sm,
                class: "agent-chat__picker",
                id: "agent-chat-model-trigger",
                "aria-haspopup": "dialog",
                "data-testid": "agent-chat-model-trigger",
                title: format!("{model_label}. Choose a session-only model; folder and task defaults stay unchanged."),
                onclick: move |_| picker_open.set(true),
                span { class: "agent-chat__picker-label", "{model_label}" }
            }
            Dialog {
                open: picker_open(),
                on_close: move |_| picker_open.set(false),
                headline: "Model for this session".to_string(),
                description: rsx! { "This session only. Folder and global model roles stay unchanged." },
                test_id: "agent-chat-model-dialog".to_string(),
                actions: rsx! {
                    Button {
                        variant: ButtonVariant::Outline,
                        size: ButtonSize::Sm,
                        "data-testid": "agent-chat-model-use-folder-default",
                        onclick: move |_| {
                            configure(clear_pump.clone(), clear_id.clone(), Configure::Model(None));
                            picker_open.set(false);
                        },
                        if conversation.mode == "plan" { "Use planning default" } else { "Use folder default" }
                    }
                    Button {
                        variant: ButtonVariant::Link,
                        size: ButtonSize::Sm,
                        "data-testid": "agent-chat-model-manage",
                        onclick: move |_| open_settings(),
                        "Manage models…"
                    }
                },
                ModelPicker {
                    options: picker_options,
                    selected: selected_selector,
                    placeholder: "Search models",
                    test_id: "agent-chat-model-picker",
                    on_choose: move |selector: String| {
                        configure(choose_pump.clone(), model_id.clone(), Configure::Model(Some(selector)));
                        picker_open.set(false);
                    },
                }
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
    Model(Option<String>),
    Thinking(String),
}

fn thinking_label(level: &str) -> String {
    let mut characters = level.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
}

/// Every available model, grouped under its provider's name, plus the session's
/// own model when the catalog no longer lists it as available, so the picker
/// can always show what is in use.
fn model_picker_options(
    catalog: Option<&ModelsCatalog>,
    conversation: &ConversationSummary,
) -> Vec<PickerOption> {
    let mut options: Vec<PickerOption> = catalog
        .map(|catalog| {
            catalog
                .models
                .iter()
                .filter(|model| model.available && !model.classifier)
                .map(|model| PickerOption {
                    selector: format!("{}/{}", model.provider, model.model_id),
                    name: model.name.clone(),
                    group: provider_display_name(catalog, &model.provider),
                    detail: Some(model.model_id.clone()),
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(current) = conversation.model.as_ref()
        && !catalog.is_some_and(|catalog| {
            catalog.models.iter().any(|entry| {
                entry.classifier
                    && entry.provider == current.provider
                    && entry.model_id == current.model_id
            })
        })
    {
        let selector = format!("{}/{}", current.provider, current.model_id);
        if !options.iter().any(|option| option.selector == selector) {
            options.insert(
                0,
                PickerOption {
                    selector,
                    name: current.model_id.clone(),
                    group: "Current".to_string(),
                    detail: None,
                },
            );
        }
    }
    options
}

fn provider_display_name(catalog: &ModelsCatalog, provider_id: &str) -> String {
    catalog
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .map_or_else(|| provider_id.to_string(), |provider| provider.name.clone())
}

fn current_model_label(
    catalog: Option<&ModelsCatalog>,
    conversation: &ConversationSummary,
) -> String {
    let inherited = conversation.model.is_none();
    let Some(current) = conversation
        .effective_model
        .as_ref()
        .or(conversation.model.as_ref())
    else {
        return if conversation.mode == "plan" {
            "Planning default"
        } else {
            "Folder default"
        }
        .to_string();
    };
    let name = catalog
        .and_then(|catalog| {
            catalog.models.iter().find(|model| {
                model.provider == current.provider && model.model_id == current.model_id
            })
        })
        .map_or_else(|| current.model_id.clone(), |model| model.name.clone());
    if inherited {
        format!("{name} · Default")
    } else {
        name
    }
}

#[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
fn configure(pump: Pump, conversation_id: String, change: Configure) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let call = match change {
            Configure::Model(None) => ConfigureAgentChat {
                conversation_id,
                model_provider: Some(String::new()),
                model_id: Some(String::new()),
                ..Default::default()
            },
            Configure::Model(Some(selector)) => {
                let Some((provider, model_id)) = selector.split_once('/') else {
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

pub(super) fn supports_reasoning(
    conversation: &ConversationSummary,
    catalog: Option<&ModelsCatalog>,
) -> bool {
    catalog.is_some_and(|catalog| {
        conversation
            .effective_model
            .as_ref()
            .or(conversation.model.as_ref())
            .is_some_and(|model| {
                catalog.models.iter().any(|entry| {
                    entry.provider == model.provider
                        && entry.model_id == model.model_id
                        && entry.reasoning
                })
            })
    })
}

#[cfg(test)]
#[path = "model_controls_tests.rs"]
mod tests;
