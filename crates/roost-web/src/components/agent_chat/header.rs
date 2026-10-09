//! Conversation identity and model controls, including stop and confirmed delete.
//! Model options are fetched once while this route is mounted.

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_protocol::wire::agent_chat::ModelRef;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog};

use crate::components::md::{
    Button, ButtonVariant, Dialog, IconButton, Select, SelectOption, StatusDot,
};
use crate::pump::{Pump, use_store};

#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::{
    AbortAgentChat, ConfigureAgentChat, DeleteAgentChat, ListAgentModels,
};

#[component]
pub fn AgentChatHeader(
    conversation: ConversationSummary,
    pump: Pump,
    navigate: EventHandler<String>,
) -> Element {
    #[allow(unused_mut)]
    let mut catalog = use_signal(|| None::<ModelsCatalog>);
    let mut confirm_delete = use_signal(|| false);
    let store_pump = use_store();
    let online = {
        let core = store_pump.core();
        let core = core.borrow();
        let store = core.store();
        store
            .workers
            .get(&conversation.worker_fp)
            .is_some_and(|worker| {
                roost_client_core::store::navigation::worker_online(
                    worker,
                    store.routable_worker_fps.as_ref(),
                    i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX),
                )
            })
    };
    #[cfg(target_arch = "wasm32")]
    let models_pump = pump.clone();
    #[cfg(target_arch = "wasm32")]
    use_effect(use_reactive((&conversation.id,), move |_| {
        let pump = models_pump.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match pump.rpc().call(&ListAgentModels).await {
                Ok(models) => catalog.set(Some(models)),
                Err(error) => {
                    tracing::warn!(target: "agent_chat", %error, "agent models unavailable")
                }
            }
        });
    }));
    let options = catalog()
        .map(|catalog| {
            catalog
                .models
                .into_iter()
                .filter(|model| model.available)
                .map(|model| {
                    SelectOption::new(format!("{}/{}", model.provider, model.model_id), model.name)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let current_model = conversation
        .model
        .as_ref()
        .map(|model| format!("{}/{}", model.provider, model.model_id))
        .unwrap_or_default();
    let reasoning = catalog()
        .and_then(|catalog| {
            conversation.model.as_ref().and_then(|model| {
                catalog
                    .models
                    .iter()
                    .find(|entry| {
                        entry.provider == model.provider && entry.model_id == model.model_id
                    })
                    .map(|entry| entry.reasoning)
            })
        })
        .unwrap_or(false);
    let thinking_options = catalog()
        .map(|catalog| {
            catalog
                .thinking_levels
                .into_iter()
                .map(|level| SelectOption::new(level.clone(), level))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    #[cfg(target_arch = "wasm32")]
    let conversation_id = conversation.id.clone();
    #[cfg(target_arch = "wasm32")]
    let pump_for_model = pump.clone();
    let change_model = move |value: String| {
        if let Some((provider, model_id)) = value.split_once('/') {
            #[cfg(target_arch = "wasm32")]
            {
                let pump = pump_for_model.clone();
                let conversation_id = conversation_id.clone();
                let model = ModelRef {
                    provider: provider.to_owned(),
                    model_id: model_id.to_owned(),
                };
                wasm_bindgen_futures::spawn_local(async move {
                    if let Err(error) = pump
                        .rpc()
                        .call(&ConfigureAgentChat {
                            conversation_id,
                            model_provider: Some(model.provider),
                            model_id: Some(model.model_id),
                            ..Default::default()
                        })
                        .await
                    {
                        tracing::warn!(target: "agent_chat", %error, "agent model change failed");
                    }
                });
            }
            #[cfg(not(target_arch = "wasm32"))]
            let _ = (provider, model_id);
        }
    };
    let conversation_id = conversation.id.clone();
    let pump_for_thinking = pump.clone();
    let change_thinking = move |level: String| {
        #[cfg(target_arch = "wasm32")]
        {
            let pump = pump_for_thinking.clone();
            let conversation_id = conversation_id.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Err(error) = pump
                    .rpc()
                    .call(&ConfigureAgentChat {
                        conversation_id,
                        thinking_level: Some(level),
                        ..Default::default()
                    })
                    .await
                {
                    tracing::warn!(target: "agent_chat", %error, "agent thinking level change failed");
                }
            });
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (&pump_for_thinking, &conversation_id, level);
    };
    #[cfg(target_arch = "wasm32")]
    let stop_id = conversation.id.clone();
    #[cfg(target_arch = "wasm32")]
    let stop_pump = pump.clone();
    let delete_id = conversation.id.clone();
    let delete_pump = pump.clone();
    rsx! {
        header { class: "agent-chat__header",
            div { class: "agent-chat__identity",
                h1 { {conversation.title.clone()} }
                p {
                    StatusDot {
                        status: if online { "ok".to_string() } else { "offline".to_string() },
                        title: Some(if online { "Machine online" } else { "Machine offline" }.to_string()),
                    }
                    "{conversation.worker_label} · {conversation.cwd}"
                    if !online { " · Offline" }
                }
            }
            div { class: "agent-chat__controls",
                Select { label: Some("Model".to_string()), value: current_model, options, on_change: change_model, placeholder: Some("Select model".to_string()) }
                if reasoning {
                    Select { label: Some("Thinking".to_string()), value: conversation.thinking_level.clone().unwrap_or_default(), options: thinking_options, on_change: change_thinking }
                }
                if conversation.run_state == roost_protocol::wire::agent_chat::AgentRunState::Running {
                    IconButton { icon: "stop", label: "Stop", onclick: move |_| {
                        #[cfg(target_arch = "wasm32")]
                        { let pump = stop_pump.clone(); let id = stop_id.clone(); wasm_bindgen_futures::spawn_local(async move { if let Err(error) = pump.rpc().call(&AbortAgentChat { conversation_id: id }).await { tracing::warn!(target: "agent_chat", %error, "agent abort failed"); } }); }
                    } }
                }
                IconButton { icon: "delete", label: "Delete conversation", onclick: move |_| confirm_delete.set(true) }
            }
            Dialog {
                open: confirm_delete(), on_close: move |_| confirm_delete.set(false),
                headline: Some("Delete conversation?".to_string()),
                description: Some(rsx! { p { "This conversation will be removed from Roost." } }),
                actions: Some(rsx! {
                    Button { variant: ButtonVariant::Outline, onclick: move |_| confirm_delete.set(false), "Cancel" }
                    Button { variant: ButtonVariant::Destructive, onclick: move |_| {
                        let pump = delete_pump.clone(); let id = delete_id.clone(); let navigate = navigate;
                        #[cfg(target_arch = "wasm32")]
                        wasm_bindgen_futures::spawn_local(async move {
                            match pump.rpc().call(&DeleteAgentChat { conversation_id: id }).await {
                                Ok(()) => navigate.call("/".to_string()),
                                Err(error) => tracing::warn!(target: "agent_chat", %error, "agent delete failed"),
                            }
                        });
                        #[cfg(not(target_arch = "wasm32"))]
                        let _ = (pump, id, navigate);
                    }, "Delete" }
                }),
                "Delete this conversation and its transcript?"
            }
        }
    }
}
