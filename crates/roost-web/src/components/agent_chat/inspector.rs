//! Controlled native inspection with cancellable refreshes and explicit actions.
//! Mounted by `AgentWorkspace`; backend snapshots own every displayed value.
//! Action failures stay visible, while component-owned tasks cannot update a closed sheet.

use std::rc::Rc;

use dioxus::prelude::*;
use libero::components::{Pictogram, SvgData};
use roost_client_core::client::rpc::calls::agent_chat::{
    AbortAgentChat, ConfigureAgentChat, InspectAgentChat, SubmitAgentChat,
};
use roost_protocol::wire::agent_chat::{
    ConversationSummary, ModelsCatalog, Transcript, TranscriptItem,
};

use super::inspector_panels::{
    ConfigurationPanel, SessionPanel, StatePanel, TasksPanel, ToolsPanel,
};
use crate::pump::Pump;
use libero::components::{ActionIcon, Button, Flex, Paper, Text, Title};
use libero::sx::sx;

pub(super) const INSPECTOR_TABS: [(&str, &str, SvgData); 5] = [
    (
        "session",
        "Session",
        pictogram_icons_lucide::folder_open::outlined,
    ),
    ("state", "State", pictogram_icons_lucide::braces::outlined),
    ("tasks", "Tasks", pictogram_icons_lucide::activity::outlined),
    ("tools", "Tools", pictogram_icons_lucide::wrench::outlined),
    (
        "configuration",
        "Configuration",
        pictogram_icons_lucide::settings::outlined,
    ),
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
    let root_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .min_height("0")
            .min_width("0")
            .flex("1")
    });
    let header_sx = use_hook(|| {
        sx().display("flex")
            .align_items("flex-start")
            .justify_content("space-between")
            .gap("sm")
            .padding("md")
    });
    let identity_sx = use_hook(|| sx().display("flex").flex_direction("column").min_width("0"));
    let tab_list_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("row")
            .gap("xs")
            .overflow_x("auto")
            .with("overscroll-behavior-y", "auto")
    });
    let tab_button_sx = use_hook(|| sx().flex_shrink("0"));
    let panel_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .min_height("0")
            .flex("1")
            .selector("&[hidden]", sx().display("none"))
    });
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
        Paper { sx: root_sx, "data-testid": "agent-inspector",
            Flex { sx: header_sx,
                Flex { sx: identity_sx,
                    Text { size: "xs", color: "muted", "Native agent" }
                    Title { size: "md", "{conversation.title}" }
                    Text { size: "xs", color: "muted", "{conversation.worker_label} · {conversation.cwd}" }
                }
                ActionIcon { aria_label: "Close inspector", title: "Close inspector", onclick: move |_| on_close.call(()),
                    Pictogram { icon: pictogram_icons_lucide::x::outlined }
                }
            }
            if let Some(message) = action_error() {
                Text { size: "sm", color: "error", "{message}" }
            }
            Flex { sx: tab_list_sx, role: "tablist", "aria-label": "Agent inspection sections",
                for (key, label, _) in INSPECTOR_TABS {
                    {
                        let selected = selected_tab == key;
                        let tab_id = inspector_tab_id(key);
                        let tab_panel_id = inspector_panel_id(key);
                        rsx! {
                            Button {
                                key: "{key}",
                                sx: tab_button_sx.clone(),
                                variant: if selected { "filled" } else { "standard" },
                                role: "tab",
                                id: "{tab_id}",
                                "aria-controls": "{tab_panel_id}",
                                "aria-selected": selected,
                                tabindex: if selected { "0" } else { "-1" },
                                onclick: move |_| on_tab_change.call(key.to_owned()),
                                onkeydown: move |event: KeyboardEvent| {
                                    let pressed_key = event.key();
                                    let step = match &pressed_key {
                                        Key::Named(NamedKey::ArrowRight) => 1,
                                        Key::Named(NamedKey::ArrowLeft) => -1,
                                        Key::Named(NamedKey::Home) => 0,
                                        Key::Named(NamedKey::End) => INSPECTOR_TABS.len() as isize - 1,
                                        _ => return,
                                    };
                                    event.prevent_default();
                                    let Some(index) = INSPECTOR_TABS.iter().position(|(tab, _, _)| *tab == key) else {
                                        return;
                                    };
                                    let next = match &pressed_key {
                                        Key::Named(NamedKey::Home) => 0,
                                        Key::Named(NamedKey::End) => INSPECTOR_TABS.len() - 1,
                                        _ => (index as isize + step).rem_euclid(INSPECTOR_TABS.len() as isize) as usize,
                                    };
                                    let next_key = INSPECTOR_TABS[next].0;
                                    on_tab_change.call(next_key.to_owned());
                                    focus_inspector_tab(next_key);
                                },
                                "{label}"
                            }
                        }
                    }
                }
            }
            match selected {
                None => rsx! {
                    Flex { sx: panel_sx.clone(), Text { size: "sm", color: "muted", "Loading native agent state…" } }
                },
                Some(Err(message)) => rsx! {
                    Flex { sx: panel_sx.clone(),
                        Text { size: "sm", color: "error", "Inspection unavailable: {message}" }
                        Button { variant: "outlined", size: "sm", onclick: move |_| refresh_counter += 1, "Retry" }
                    }
                },
                Some(Ok(data)) => rsx! {
                    Flex { sx: panel_sx.clone(), id: "{inspector_panel_id(\"session\")}", role: "tabpanel",
                        "aria-labelledby": "{inspector_tab_id(\"session\")}", hidden: selected_tab != "session",
                        SessionPanel { data: Rc::clone(&data), catalog: catalog.clone(), pending: action_pending(), pump: pump.clone(),
                            on_thinking: move |level| on_action.call(InspectorAction::Thinking(level)),
                            on_compact: move |_| on_action.call(InspectorAction::Compact) }
                    }
                    Flex { sx: panel_sx.clone(), id: "{inspector_panel_id(\"state\")}", role: "tabpanel",
                        "aria-labelledby": "{inspector_tab_id(\"state\")}", hidden: selected_tab != "state",
                        StatePanel { data: Rc::clone(&data) }
                    }
                    Flex { sx: panel_sx.clone(), id: "{inspector_panel_id(\"tasks\")}", role: "tabpanel",
                        "aria-labelledby": "{inspector_tab_id(\"tasks\")}", hidden: selected_tab != "tasks",
                        TasksPanel { data: Rc::clone(&data), pending: action_pending(),
                            on_stop: move |_| on_action.call(InspectorAction::Interrupt { background: false }),
                            on_stop_all: move |_| on_action.call(InspectorAction::Interrupt { background: true }) }
                    }
                    Flex { sx: panel_sx.clone(), id: "{inspector_panel_id(\"tools\")}", role: "tabpanel",
                        "aria-labelledby": "{inspector_tab_id(\"tools\")}", hidden: selected_tab != "tools",
                        ToolsPanel { data: Rc::clone(&data), pending: action_pending(),
                            on_read_only: move |checked| on_action.call(InspectorAction::ReadOnly(checked)) }
                    }
                    Flex { sx: panel_sx, id: "{inspector_panel_id(\"configuration\")}", role: "tabpanel",
                        "aria-labelledby": "{inspector_tab_id(\"configuration\")}", hidden: selected_tab != "configuration",
                        ConfigurationPanel { data }
                    }
                },
            }
        }
    }
}

fn inspector_tab_id(key: &str) -> String {
    format!("agent-inspector-tab-{key}")
}

fn inspector_panel_id(key: &str) -> String {
    format!("agent-inspector-panel-{key}")
}

#[cfg(target_arch = "wasm32")]
fn focus_inspector_tab(key: &str) {
    use wasm_bindgen::JsCast;

    if let Some(element) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(&inspector_tab_id(key)))
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = element.focus();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn focus_inspector_tab(_key: &str) {}

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
