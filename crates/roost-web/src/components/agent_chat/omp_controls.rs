//! Conversation-only OMP model and thinking pins with inherited source labels.
//! Mounted instead of native model controls; settings come from the shared RPC.
//! The same protocol resolver supplies the settings pane's inheritance text.

use super::{agent_alert::AgentAlert, omp_setting_menu::OmpSettingMenu};
use crate::pump::Pump;
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::*;

#[derive(Clone, PartialEq)]
struct SessionView {
    worker_build: Option<String>,
    online: bool,
    model: Option<ModelRef>,
    thinking: Option<String>,
    state: Option<OmpSessionState>,
}

#[component]
pub fn OmpModelControls(conversation: ConversationSummary, active: bool, pump: Pump) -> Element {
    let state_pump = pump.clone();
    let state_id = conversation.id.clone();
    let state_worker = conversation.worker_fp.clone();
    let session = use_memo(move || {
        let _revision = state_pump.revision()();
        session_view(&state_pump, &state_id, &state_worker)
    });
    let session = session();
    let prepare_pump = pump.clone();
    let mut preparation = use_resource(use_reactive(
        (
            &conversation.id,
            &session.worker_build,
            &session.online,
            &active,
        ),
        move |(id, _build, _online, active)| {
            let pump = prepare_pump.clone();
            async move {
                if !active {
                    return Ok(());
                }
                open_session(pump, id).await
            }
        },
    ));
    let fetch_pump = pump.clone();
    let mut settings = use_resource(use_reactive(
        (
            &conversation.id,
            &conversation.updated_ms,
            &session.model,
            &session.thinking,
            &session.state,
        ),
        move |_| {
            let pump = fetch_pump.clone();
            async move { fetch_settings(pump).await }
        },
    ));
    let error = use_signal(|| None::<String>);
    let loaded = settings.read();
    let view = loaded.as_ref().and_then(|result| result.as_ref().ok());
    let scopes = view.map_or(&[][..], |view| view.scopes.as_slice());
    let overrides = OmpSettingValues {
        model: conversation.model.as_ref().map(join_omp_model),
        thinking_level: conversation.thinking_level.clone(),
    };
    let mut effective = resolve_omp_settings(
        scopes,
        &conversation.worker_fp,
        &conversation.cwd,
        &overrides,
    );
    if effective.model.value.is_none() {
        effective.model.value = session
            .model
            .as_ref()
            .map(join_omp_model)
            .or_else(|| conversation.effective_model.as_ref().map(join_omp_model));
    }
    if effective.thinking_level.value.is_none() {
        effective.thinking_level.value = session.thinking;
    }
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
    let prepare_error = preparation
        .read()
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .cloned();
    let async_error = match session.state {
        Some(OmpSessionState::Failed { error }) => Some(error),
        _ => None,
    };
    let retry_visible = prepare_error.is_some() || fetch_error.is_some() || async_error.is_some();
    rsx! {
        div {class:"flex min-w-0 max-w-full flex-col gap-1", "data-testid":"omp-model-controls",
            div {class:"flex min-w-0 flex-wrap items-center gap-1",
                OmpSettingMenu {label:"Model",value:effective.model,inherited:inherited.model,choices:choices(true),
                    disabled:catalog.is_none_or(|catalog| catalog.models.is_empty()) && overrides.model.as_deref().is_none_or(str::is_empty),
                    on_open:move |_| settings.restart(),
                    on_choose:move |value| configure(model_pump.clone(),model_id.clone(),true,value,error)}
                OmpSettingMenu {label:"Thinking",value:effective.thinking_level,inherited:inherited.thinking_level,choices:choices(false),
                    disabled:catalog.is_none_or(|catalog| catalog.thinking_levels.is_empty()) && overrides.thinking_level.as_deref().is_none_or(str::is_empty),
                    on_open:move |_| settings.restart(),
                    on_choose:move |value| configure(thinking_pump.clone(),thinking_id.clone(),false,value,error)}
            }
            if catalog.is_none() && prepare_error.is_none() && fetch_error.is_none() && async_error.is_none() {
                p {class:"text-xs text-muted-foreground", "Connecting to OMP and loading its settings…"}
            } else {
                p {class:"text-xs text-muted-foreground", "Applies to your next message."}
            }
            if let Some(message) = error().or(prepare_error.clone()).or(fetch_error).or(async_error) {AgentAlert {test_id:"omp-controls-error", "{message}"}}
            if retry_visible {
                crate::components::ui::Button {
                    variant:crate::components::ui::ButtonVariant::Outline,
                    size:crate::components::ui::ButtonSize::Sm,
                    onclick:move |_| {preparation.restart();settings.restart();},
                    "Retry OMP settings"
                }
            }
        }
    }
}

fn session_view(pump: &Pump, id: &str, worker_fp: &str) -> SessionView {
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    let transcript = store
        .agent_chat
        .transcripts
        .get(id)
        .map(|loaded| &loaded.transcript);
    SessionView {
        worker_build: store
            .workers
            .get(worker_fp)
            .and_then(|worker| worker.git_sha.clone()),
        online: store
            .routable_worker_fps
            .as_ref()
            .is_some_and(|workers| workers.contains(worker_fp)),
        model: transcript.and_then(|transcript| transcript.model.clone()),
        thinking: transcript.and_then(|transcript| transcript.thinking_level.clone()),
        state: transcript.and_then(|transcript| transcript.omp_session.clone()),
    }
}

async fn open_session(pump: Pump, conversation_id: String) -> Result<(), String> {
    #[cfg(target_arch = "wasm32")]
    {
        pump.rpc()
            .call(
                &roost_client_core::client::rpc::calls::agent_chat::OpenOmpSession {
                    conversation_id,
                },
            )
            .await
            .map_err(|error| error.to_string())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (pump, conversation_id);
        Err("OMP settings require a browser".into())
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
