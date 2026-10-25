//! Controlled native inspection with cancellable refreshes and explicit actions.
//! Mounted by `AgentWorkspace`; backend snapshots own every displayed value.
//! Action failures stay visible, while component-owned tasks cannot update a closed sheet.

use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::client::rpc::calls::agent_chat::{
    AbortAgentChat, ConfigureAgentChat, InspectAgentChat, SubmitAgentChat,
};
use roost_protocol::wire::agent_chat::{
    ConversationSummary, ModelsCatalog, Transcript, TranscriptItem,
};

use super::inspector_panels::{
    ConfigurationPanel, SessionPanel, StatePanel, TasksPanel, ToolsPanel,
};
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Card, CardContent, FieldError, IconButton, IconType, Tabs,
    TabsContent, TabsList, TabsTrigger,
};
use crate::pump::Pump;

pub(super) const INSPECTOR_TABS: [(&str, &str, IconType); 5] = [
    ("session", "Session", IconType::FolderOpen),
    ("state", "State", IconType::Braces),
    ("tasks", "Tasks", IconType::Activity),
    ("tools", "Tools", IconType::Wrench),
    ("configuration", "Configuration", IconType::Settings),
];

enum InspectorAction {
    Interrupt { background: bool },
    Compact,
    Thinking(String),
    ReadOnly(bool),
}

#[component]
pub fn AgentInspector(
    conversation: ConversationSummary,
    transcript: Transcript,
    catalog: Option<ModelsCatalog>,
    pump: Pump,
    on_close: EventHandler<()>,
    selected_tab: String,
    on_tab_change: EventHandler<String>,
) -> Element {
    let _revision = pump.revision()();
    let mut action_pending = use_signal(|| false);
    let mut action_error = use_signal(|| None::<String>);
    let mut refresh_counter = use_signal(|| 0_u64);
    let activity_key: Vec<_> = transcript
        .items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::Tool {
                id,
                running,
                queued,
                ..
            } => Some((
                id.clone(),
                if *queued {
                    "queued"
                } else if *running {
                    "running"
                } else {
                    "finished"
                },
            )),
            TranscriptItem::BackgroundJob { id, state, .. } => Some((id.clone(), state.as_str())),
            _ => None,
        })
        .collect();
    let child_key: Vec<_> = {
        let core = pump.core();
        let core = core.borrow();
        core.store()
            .agent_chat
            .conversations
            .values()
            .filter(|child| child.parent_id.as_deref() == Some(conversation.id.as_str()))
            .map(|child| (child.id.clone(), child.updated_ms))
            .collect()
    };
    let refresh_key = (
        conversation.id.clone(),
        transcript.items.len(),
        transcript.run_state,
        transcript
            .queued
            .iter()
            .map(|message| message.id.clone())
            .collect::<Vec<_>>(),
        transcript.usage.context_tokens,
        conversation.updated_ms,
        activity_key,
        child_key,
    );
    let refresh_pump = pump.clone();
    let inspection = use_resource(use_reactive((&refresh_key,), move |(key,)| {
        let _ = refresh_counter();
        let pump = refresh_pump.clone();
        async move {
            pump.rpc()
                .call(&InspectAgentChat {
                    conversation_id: key.0,
                })
                .await
                .map(Rc::new)
                .map_err(|problem| problem.to_string())
        }
    }));
    let action_pump = pump.clone();
    let action_conversation_id = conversation.id.clone();
    let on_action = EventHandler::new(move |action: InspectorAction| {
        if *action_pending.peek() {
            return;
        }
        action_pending.set(true);
        action_error.set(None);
        let pump = action_pump.clone();
        let conversation_id = action_conversation_id.clone();
        spawn(async move {
            let result = perform_action(&pump, conversation_id, action).await;
            action_pending.set(false);
            action_error.set(result.err());
            refresh_counter += 1;
        });
    });
    let selected = inspection.read().as_ref().cloned();
    rsx! {
        section { class: "flex h-full min-h-0 min-w-0 flex-col bg-card text-card-foreground", "data-testid": "agent-inspector",
            header { class: "flex shrink-0 items-start justify-between gap-3 border-b border-border px-4 py-3",
                div { class: "min-w-0",
                    p { class: "text-xs font-medium uppercase tracking-wide text-muted-foreground", "Native agent" }
                    h2 { class: "truncate text-base font-semibold", "{conversation.title}" }
                    p { class: "truncate text-xs text-muted-foreground", "{conversation.worker_label} · {conversation.cwd}" }
                }
                IconButton { icon: IconType::X, label: "Close inspector", title: "Close inspector", size: ButtonSize::IconSm, onclick: move |_| on_close.call(()) }
            }
            if let Some(message) = action_error() {
                div { class: "shrink-0 px-4 py-2", FieldError { "{message}" } }
            }
            Tabs { class: "min-h-0 min-w-0 flex-1", value: selected_tab, on_value_change: move |value| on_tab_change.call(value),
                TabsList { class: "w-full shrink-0 justify-start overflow-x-auto", aria_label: "Agent inspection sections",
                    for (key, label, _) in INSPECTOR_TABS {
                        TabsTrigger { class: "flex-none", value: key.to_owned(), "{label}" }
                    }
                }
                div { class: "min-h-0 min-w-0 flex-1 overflow-y-auto p-4",
                    match selected {
                        None => rsx! { p { class: "text-sm text-muted-foreground", "Loading native agent state…" } },
                        Some(Err(message)) => rsx! {
                            Card { CardContent { class: "flex flex-col gap-3",
                                FieldError { "Inspection unavailable: {message}" }
                                Button { variant: ButtonVariant::Outline, size: ButtonSize::Sm,
                                    onclick: move |_| refresh_counter += 1, "Retry" }
                            } }
                        },
                        Some(Ok(data)) => rsx! {
                            TabsContent { value: "session",
                                SessionPanel { data: Rc::clone(&data), catalog: catalog.clone(), pending: action_pending(), pump: pump.clone(),
                                    on_thinking: move |level| on_action.call(InspectorAction::Thinking(level)),
                                    on_compact: move |_| on_action.call(InspectorAction::Compact) }
                            }
                            TabsContent { value: "state", StatePanel { data: Rc::clone(&data) } }
                            TabsContent { value: "tasks", TasksPanel { data: Rc::clone(&data), pending: action_pending(),
                                on_stop: move |_| on_action.call(InspectorAction::Interrupt { background: false }),
                                on_stop_all: move |_| on_action.call(InspectorAction::Interrupt { background: true }) } }
                            TabsContent { value: "tools", ToolsPanel { data: Rc::clone(&data), pending: action_pending(),
                                on_read_only: move |checked| on_action.call(InspectorAction::ReadOnly(checked)) } }
                            TabsContent { value: "configuration", ConfigurationPanel { data } }
                        },
                    }
                }
            }
        }
    }
}

async fn perform_action(
    pump: &Pump,
    conversation_id: String,
    action: InspectorAction,
) -> Result<(), String> {
    let result = match action {
        InspectorAction::Interrupt { background } => {
            pump.rpc()
                .call(&AbortAgentChat {
                    conversation_id,
                    request_id: super::request_ids::new_request_id(),
                    background: Some(background),
                })
                .await
        }
        InspectorAction::Compact => {
            pump.rpc()
                .call(&SubmitAgentChat {
                    conversation_id,
                    text: "/compact".to_owned(),
                    request_id: super::request_ids::new_request_id(),
                    when_busy: "steer".to_owned(),
                })
                .await
        }
        InspectorAction::Thinking(level) => pump
            .rpc()
            .call(&ConfigureAgentChat {
                conversation_id,
                thinking_level: Some(level),
                ..Default::default()
            })
            .await
            .map(|_| ()),
        InspectorAction::ReadOnly(read_only) => pump
            .rpc()
            .call(&ConfigureAgentChat {
                conversation_id,
                read_only: Some(read_only),
                ..Default::default()
            })
            .await
            .map(|_| ()),
    };
    result.map_err(|problem| format!("Action failed: {problem}"))
}
