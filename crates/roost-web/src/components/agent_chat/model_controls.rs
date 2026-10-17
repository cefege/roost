//! The composer's model button and thinking picker, and the setup card shown
//! while no model can run. The button names the model this chat runs and opens
//! `role_switcher`, where every model role is changed in place. Mounted by
//! `agent_chat::composer` and `agent_chat::surface`; the thinking menu opens
//! upward because the composer sits at the bottom of its pane.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog};

use super::composer_commands::model_picker_trigger_id;
use super::role_switcher::RoleSwitcher;
use super::toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::ui::{Button, ButtonSize, ButtonVariant, Card, CardContent, CardHeader, CardTitle, CardVariant, IconType};
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
    let trigger_id = model_picker_trigger_id(&conversation.id);
    // Nothing to pick from: a button that says why, never an empty menu. The
    // id is the picker's, so `/model` lands here too.
    let placeholder = match (conversation.model.is_some(), catalog.is_some(), setup) {
        (true, _, _) | (false, true, ModelSetup::Ready) => None,
        (false, _, ModelSetup::NoProvider) => Some(("Connect a model", IconType::Link, false)),
        (false, _, ModelSetup::CatalogFailed) => Some(("Models unavailable", IconType::CircleAlert, false)),
        (false, false, ModelSetup::Ready) => Some(("Loading models…", IconType::Hourglass, true)),
    };
    if let Some((label, icon, waiting)) = placeholder {
        return rsx! {
            div { class: "flex flex-wrap items-center gap-1",
                Button {
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::Sm,
                class: "min-w-0 max-w-full justify-start",
                    id: "{trigger_id}",
                icon: Some(icon),
                    disabled: waiting,
                    "aria-busy": waiting.then_some("true"),
                    "data-testid": "agent-chat-model-setup",
                    title: "Open Settings → Agents → Models",
                    onclick: move |_| open_settings(),
                    span { class: "truncate", "{label}" }
                }
            }
        };
    }
    let model_label = current_model_label(catalog.as_ref(), &conversation);
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
    let thinking_pump = pump.clone();
    let thinking_id = conversation.id.clone();
    let expanded = if picker_open() { "true" } else { "false" };
    rsx! {
        div { class: "flex flex-wrap items-center gap-1",
            Button {
                variant: ButtonVariant::Ghost,
                size: ButtonSize::Sm,
                class: "min-w-0 max-w-full justify-start",
                id: "{trigger_id}",
                "aria-haspopup": "true",
                "aria-expanded": expanded,
                "data-testid": "agent-chat-model-trigger",
                title: format!("{model_label}. Change the models for this chat and its subagents."),
                onclick: move |_| picker_open.toggle(),
                span { class: "truncate", "{model_label}" }
            }
            RoleSwitcher {
                open: picker_open,
                trigger_id: trigger_id.clone(),
                conversation: conversation.clone(),
                catalog: catalog.clone(),
                pump,
            }
            if reasoning && !thinking_items.is_empty() {
                ToolbarMenu {
                    menu_key: "thinking",
                    trigger: ToolbarTrigger::Picker { label: thinking_label(&thinking) },
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
            variant: CardVariant::Outlined,
            class: Some("gap-3".to_string()),
            "data-testid": "agent-chat-model-setup-card",
            CardHeader {
                CardTitle { "{title}" }
            }
            CardContent {
                p { class: "text-sm text-muted-foreground", "{body}" }
                div { class: "flex flex-wrap gap-2",
                    if setup == ModelSetup::CatalogFailed {
                        Button {
                            variant: ButtonVariant::Outline,
                            icon: Some(IconType::RefreshCw),
                            onclick: move |_| on_retry.call(()),
                            "Try again"
                        }
                    }
                    Button {
                        icon: Some(IconType::Link),
                        onclick: open_settings,
                        "data-testid": "agent-chat-model-setup-open",
                        if setup == ModelSetup::NoProvider { "Connect a model" } else { "Open model settings" }
                    }
                }
            }
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub(super) enum Configure {
    Model(Option<String>),
    Thinking(String),
}

/// `xhigh` → `XHigh`, `auto` → `Auto`: a thinking level as a label.
pub(super) fn thinking_label(level: &str) -> String {
    if level == "xhigh" {
        return "XHigh".to_owned();
    }
    let mut characters = level.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
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
            "Architect default"
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
pub(super) fn configure(pump: Pump, conversation_id: String, change: Configure) {
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
