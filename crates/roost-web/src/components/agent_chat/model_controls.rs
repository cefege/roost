//! The composer's model selector and catalog-error notice. The model menu owns this-chat
//! model pins and thinking level; the inspector's role editor owns shared role
//! settings. Mounted by `agent_chat::composer` and `agent_chat::surface`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog};

use super::composer_commands::model_picker_trigger_id;
use super::model_reference_menu::ModelReferenceMenu;
use super::provider_usage::ProviderUsage;
use crate::components::ui::{Button, ButtonSize, ButtonVariant, FieldError, IconType};
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
    let trigger_id = model_picker_trigger_id(&conversation.id);
    // Nothing to pick from: a button that says why, never an empty menu. The
    // id is the picker's, so `/model` lands here too.
    let placeholder = match (conversation.model.is_some(), catalog.is_some(), setup) {
        (true, _, _) | (false, true, ModelSetup::Ready) => None,
        (false, _, ModelSetup::NoProvider) => Some(("Connect a model", IconType::Link, false)),
        (false, _, ModelSetup::CatalogFailed) => {
            Some(("Models unavailable", IconType::CircleAlert, false))
        }
        (false, false, ModelSetup::Ready) => Some(("Loading models…", IconType::Hourglass, true)),
    };
    if let Some((label, icon, waiting)) = placeholder {
        return rsx! {
            div { class: "flex min-w-0 max-w-full items-center gap-1",
                Button {
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::Sm,
                    class: "min-w-0 max-w-full shrink justify-start",
                    id: "{trigger_id}",
                    icon: Some(icon),
                    disabled: waiting,
                    "aria-busy": waiting.then_some("true"),
                    "data-testid": "agent-chat-model-setup",
                    title: "Open Settings → Agents → Models",
                    onclick: move |_| open_settings(),
                    span { class: "truncate", "{label}" }
                }
                ProviderUsage {
                    conversation: conversation.clone(),
                    catalog: catalog.clone(),
                    pump: pump.clone(),
                }
            }
        };
    }
    rsx! {
        div { class: "flex min-w-0 max-w-full items-center gap-1",
            ModelReferenceMenu {
                trigger_id,
                conversation: conversation.clone(),
                catalog: catalog.clone(),
                catalog_failed,
                pump: pump.clone(),
            }
            ProviderUsage {
                conversation,
                catalog,
                pump,
            }
        }
    }
}

/// Catalog errors retain an explicit retry. Provider setup is already available
/// from the model selector, so it does not need a second call to action.
#[component]
pub fn ModelSetupNotice(setup: ModelSetup, on_retry: EventHandler<()>) -> Element {
    if setup != ModelSetup::CatalogFailed {
        return rsx! {};
    }
    rsx! {
        div { class: "flex flex-wrap items-center gap-2",
            FieldError { "Couldn't load the model list." }
            Button {
                variant: ButtonVariant::Ghost,
                size: ButtonSize::Sm,
                icon: Some(IconType::RefreshCw),
                onclick: move |_| on_retry.call(()),
                "Try again"
            }
        }
    }
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

pub(super) fn current_model_label(
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
            "Plan mode default"
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
pub(super) fn configure_model_pin(pump: Pump, conversation_id: String, selector: Option<String>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let call = match selector {
            None => ConfigureAgentChat {
                conversation_id,
                model_provider: Some(String::new()),
                model_id: Some(String::new()),
                ..Default::default()
            },
            Some(selector) => {
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
