//! Conversation-only OMP model and thinking pins with inherited source labels.
//! Mounted instead of native model controls; settings come from the shared RPC.
//! The same protocol resolver supplies the settings pane's inheritance text.

use super::{agent_alert::AgentAlert, omp_setting_menu::OmpSettingMenu};
use crate::pump::Pump;
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::*;

#[component]
pub fn OmpModelControls(conversation: ConversationSummary, pump: Pump) -> Element {
    let fetch_pump = pump.clone();
    let settings = use_resource(use_reactive((&conversation.id,), move |_| {
        let pump = fetch_pump.clone();
        async move { fetch_settings(pump).await }
    }));
    let error = use_signal(|| None::<String>);
    let loaded = settings.read();
    let view = loaded.as_ref().and_then(|result| result.as_ref().ok());
    let scopes = view.map_or(&[][..], |view| view.scopes.as_slice());
    let overrides = OmpSettingValues {
        model: conversation.model.as_ref().map(join_omp_model),
        thinking_level: conversation.thinking_level.clone(),
    };
    let effective = resolve_omp_settings(
        scopes,
        &conversation.worker_fp,
        &conversation.cwd,
        &overrides,
    );
    let inherited = resolve_omp_settings(
        scopes,
        &conversation.worker_fp,
        &conversation.cwd,
        &OmpSettingValues::default(),
    );
    let catalog = view.and_then(|view| {
        view.catalogs
            .iter()
            .find(|catalog| catalog.worker_fp == conversation.worker_fp)
    });
    let choices = |model| {
        let mut choices = catalog
            .map(|catalog| {
                if model {
                    catalog.models.clone()
                } else {
                    catalog.thinking_levels.clone()
                }
            })
            .unwrap_or_default();
        let current = if model {
            &overrides.model
        } else {
            &overrides.thinking_level
        };
        if view.is_some()
            && let Some(value) = current
            && !choices.iter().any(|choice| &choice.value == value)
        {
            choices.push(OmpChoice {
                value: value.clone(),
                name: value.clone(),
                description: None,
            });
        }
        choices
    };
    let model_pump = pump.clone();
    let model_id = conversation.id.clone();
    let thinking_pump = pump;
    let thinking_id = conversation.id;
    let fetch_error = loaded
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .cloned();
    rsx! {
        div {class:"flex min-w-0 max-w-full flex-col gap-1", "data-testid":"omp-model-controls",
            div {class:"flex min-w-0 flex-wrap items-center gap-1",
                OmpSettingMenu {label:"Model",value:effective.model,inherited:inherited.model,choices:choices(true),
                    on_choose:move |value| configure(model_pump.clone(),model_id.clone(),true,value,error)}
                OmpSettingMenu {label:"Thinking",value:effective.thinking_level,inherited:inherited.thinking_level,choices:choices(false),
                    on_choose:move |value| configure(thinking_pump.clone(),thinking_id.clone(),false,value,error)}
            }
            p {class:"text-xs text-muted-foreground", "Applies to your next message."}
            if let Some(message) = error().or(fetch_error) {AgentAlert {test_id:"omp-controls-error", "{message}"}}
        }
    }
}

pub(crate) fn source_label(source: &OmpSettingSource) -> &'static str {
    match source {
        OmpSettingSource::Conversation => "this chat",
        OmpSettingSource::Folder => "this folder",
        OmpSettingSource::Machine => "this machine",
        OmpSettingSource::Defaults => "your defaults",
        OmpSettingSource::Omp => "OMP's own config",
    }
}

pub(crate) fn choice_label(choice: &OmpChoice) -> String {
    match split_omp_model(&choice.value) {
        Some(model) => format!("{} · {}", choice.name, model.provider),
        None => choice.name.clone(),
    }
}

pub(crate) async fn fetch_settings(pump: Pump) -> Result<OmpSettingsView, String> {
    #[cfg(target_arch = "wasm32")]
    {
        pump.rpc()
            .call(&roost_client_core::client::rpc::calls::agent_chat::GetOmpSettings {})
            .await
            .map_err(|error| error.to_string())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = pump;
        Err("OMP settings are available in the browser.".into())
    }
}

fn configure(
    pump: Pump,
    id: String,
    model: bool,
    value: Option<String>,
    mut error: Signal<Option<String>>,
) {
    #[cfg(target_arch = "wasm32")]
    spawn(async move {
        use roost_client_core::client::rpc::calls::agent_chat::ConfigureAgentChat;
        let call = if model {
            let selected = value.as_deref().and_then(split_omp_model);
            ConfigureAgentChat {
                conversation_id: id,
                model_provider: Some(
                    selected
                        .as_ref()
                        .map(|model| model.provider.clone())
                        .unwrap_or_default(),
                ),
                model_id: Some(selected.map(|model| model.model_id).unwrap_or_default()),
                ..Default::default()
            }
        } else {
            ConfigureAgentChat {
                conversation_id: id,
                thinking_level: Some(value.unwrap_or_default()),
                ..Default::default()
            }
        };
        error.set(
            pump.rpc()
                .call(&call)
                .await
                .err()
                .map(|error| error.to_string()),
        );
    });
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (pump, id, model, value);
        error.set(Some("OMP settings are available in the browser.".into()));
    }
}
