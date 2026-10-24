//! Native inspection panels rendered by the controlled agent inspector.
//! Values and actions come from the runtime snapshot, never browser-only state.
//! Shared snapshot ownership avoids copying tool schemas for inactive tabs.

use std::rc::Rc;

use dioxus::prelude::*;
use libero::components::{
    Button, Combobox, ComboboxOption, ComboboxOptionArgs, Flex, Paper, Switch, Text, Title,
    use_combobox,
};
use roost_protocol::wire::agent_chat::{AgentInspection, AgentRunState, ModelsCatalog};

use super::model_controls::current_model_label;
use super::role_switcher::RoleSwitcher;
use crate::pump::Pump;

#[component]
pub(super) fn SessionPanel(
    data: Rc<AgentInspection>,
    catalog: Option<ModelsCatalog>,
    pending: bool,
    pump: Pump,
    on_thinking: EventHandler<String>,
    on_compact: EventHandler<MouseEvent>,
) -> Element {
    let context = &data.context;
    let percent = context
        .context_window_tokens
        .filter(|window| *window > 0)
        .map(|window| context.context_tokens.saturating_mul(100) / window);
    let thinking_state = use_combobox();
    let thinking_levels = catalog
        .as_ref()
        .map(|catalog| catalog.thinking_levels.clone())
        .unwrap_or_default();
    let thinking_level = data.conversation.thinking_level.clone().unwrap_or_default();
    let thinking_label = if thinking_level.is_empty() {
        "Agent default".to_owned()
    } else {
        thinking_level.clone()
    };
    let selected_thinking_index = thinking_levels
        .iter()
        .position(|level| level == &thinking_level);
    let model_label = current_model_label(catalog.as_ref(), &data.conversation);
    let content_sx = use_hook(|| {
        libero::sx::sx()
            .display("flex")
            .flex_direction("column")
            .gap("md")
    });
    let summary_sx = use_hook(|| libero::sx::sx().display("flex").flex_wrap("wrap").gap("sm"));
    rsx! {
        Paper {
            Flex { sx: content_sx,
                Title { size: "md", "Session" }
                RoleSwitcher {
                    trigger_id: format!("agent-inspector-model-{}", data.conversation.id),
                    trigger_label: model_label,
                    trigger_title: "Change the models for this chat and its subagents".to_owned(),
                    conversation: data.conversation.clone(),
                    catalog: catalog.clone(),
                    pump,
                }
                Combobox {
                    state: thinking_state,
                    options: thinking_levels,
                    selected_index: selected_thinking_index,
                    option: move |row: ComboboxOptionArgs<String>| {
                        let value = row.value.clone();
                        let label = value.clone();
                        rsx! {
                            ComboboxOption {
                                selected: thinking_level == value,
                                onpick: move |_| {
                                    on_thinking.call(value.clone());
                                    thinking_state.close();
                                },
                                "{label}"
                            }
                        }
                    },
                    Button {
                        variant: "outlined",
                        attributes: thinking_state.a11y_attributes(),
                        disabled: pending,
                        onclick: move |_| thinking_state.toggle(),
                        onblur: move |_| thinking_state.close(),
                        "Thinking: {thinking_label}"
                    }
                }
                Flex { sx: summary_sx,
                    Text { size: "sm", color: "muted",
                        "Context {context.context_tokens} tokens"
                        if let Some(value) = percent { " · {value}%" }
                    }
                    Text { size: "sm", color: "muted", "Input {data.usage.input_tokens} · output {data.usage.output_tokens}" }
                    Text { size: "sm", color: "muted", "Cost ${data.usage.cost_usd:.4}" }
                }
                if let Some(window) = context.context_window_tokens.filter(|window| *window > 0) {
                    progress { max: "{window}", value: "{context.context_tokens}", "aria-label": "Model context usage" }
                }
                if let Some(trigger) = context.compaction_trigger_tokens {
                    Text { size: "xs", color: "muted",
                        "Automatic compaction triggers above {trigger} tokens with {context.compaction_headroom_tokens} tokens of headroom; it keeps the latest {context.compaction_keep_tokens}."
                        if let Some(seq) = context.last_compacted_seq {
                            " Last compacted at journal entry {seq}."
                        } else {
                            " No compaction recorded."
                        }
                    }
                } else {
                    Text { size: "xs", color: "muted", "Context window metadata is unavailable; the automatic compaction trigger cannot be calculated." }
                }
                Button { variant: "outlined", size: "sm",
                    disabled: pending || data.conversation.run_state == AgentRunState::Running,
                    onclick: move |event| on_compact.call(event), "Compact now" }
            }
        }
    }
}

#[component]
pub(super) fn StatePanel(data: Rc<AgentInspection>) -> Element {
    let content_sx = use_hook(|| {
        libero::sx::sx()
            .display("flex")
            .flex_direction("column")
            .gap("sm")
    });
    rsx! {
        Paper {
            Flex { sx: content_sx,
                Title { size: "md", "Durable state" }
                Text { size: "sm", "Run: {data.conversation.run_state.as_str()}" }
                Text { size: "sm", "Mode: {data.conversation.mode}" }
                if let Some(goal) = data.goal.as_ref() {
                    Title { size: "sm", "Goal: {goal.objective}" }
                    Text { size: "sm", color: "muted", "{goal.status:?} · {goal.tokens_used} tokens · {goal.time_used_seconds}s" }
                    if let Some(reason) = goal.waiting_reason.as_ref() { Text { size: "sm", "Waiting: {reason}" } }
                } else {
                    Text { size: "sm", color: "muted", "No durable goal is active." }
                }
                Title { size: "sm", "Inbox" }
                if data.inbox.is_empty() {
                    Text { size: "sm", color: "muted", "No queued messages." }
                } else {
                    for message in &data.inbox {
                        Text { size: "sm", color: "muted", "{message.text}" }
                    }
                }
            }
        }
    }
}

#[component]
pub(super) fn TasksPanel(
    data: Rc<AgentInspection>,
    pending: bool,
    on_stop: EventHandler<MouseEvent>,
    on_stop_all: EventHandler<MouseEvent>,
) -> Element {
    let has_stoppable_work = data
        .activity
        .iter()
        .any(|activity| activity.state == "running");
    let content_sx = use_hook(|| {
        libero::sx::sx()
            .display("flex")
            .flex_direction("column")
            .gap("md")
    });
    let row_sx = use_hook(|| {
        libero::sx::sx()
            .display("flex")
            .align_items("flex-start")
            .justify_content("space-between")
            .gap("md")
    });
    rsx! {
        Paper {
            Flex { sx: content_sx,
                Title { size: "md", "Current activity" }
                if has_stoppable_work {
                    Button { variant: "outlined", color: "error", size: "sm", disabled: pending,
                        onclick: move |event| on_stop_all.call(event), "Stop all work and background jobs" }
                }
                if data.activity.is_empty() {
                    Text { size: "sm", color: "muted", "No running tools, subagents, or background jobs." }
                } else {
                    for activity in &data.activity {
                        Flex { sx: row_sx.clone(),
                            Flex { direction: "column", gap: "xs",
                                Text { size: "sm", "{activity.label}" }
                                Text { size: "xs", color: "muted",
                                    "{activity.kind} · {activity.state}"
                                    if let Some(detail) = &activity.detail { " · {detail}" }
                                }
                            }
                            if activity.kind == "run" && activity.state == "running" {
                                Button { variant: "outlined", size: "xs", disabled: pending,
                                    onclick: move |event| on_stop.call(event), "Stop run" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub(super) fn ToolsPanel(
    data: Rc<AgentInspection>,
    pending: bool,
    on_read_only: EventHandler<bool>,
) -> Element {
    let content_sx = use_hook(|| {
        libero::sx::sx()
            .display("flex")
            .flex_direction("column")
            .gap("md")
    });
    rsx! {
        Flex { sx: content_sx,
            Paper {
                Switch { label: "Read only", checked: data.read_only, disabled: pending,
                    "data-testid": "agent-inspector-read-only",
                    onchange: move |checked| on_read_only.call(checked) }
                Text { size: "xs", color: "muted", "Only reading tools are offered. Newly dispatched calls cannot write or run shell commands." }
            }
            Text { size: "xs", color: "muted", "{data.tools.len()} offered tools. Interrupted tool calls are not automatically replayed." }
            if data.tools.is_empty() { Text { size: "sm", color: "muted", "No tools are offered for this conversation." } }
            for (index, tool) in data.tools.iter().enumerate() {
                ToolDefinition { key: "{tool.name}", data: Rc::clone(&data), index }
            }
        }
    }
}

#[component]
fn ToolDefinition(data: Rc<AgentInspection>, index: usize) -> Element {
    let Some(tool) = data.tools.get(index) else {
        return rsx! {};
    };
    let tool_sx = use_hook(|| {
        libero::sx::sx()
            .display("flex")
            .flex_direction("column")
            .gap("sm")
    });
    rsx! {
        Paper {
            details {
                summary { "{tool.name}" }
                Flex { sx: tool_sx,
                    Text { size: "sm", color: "muted", "{tool.description}" }
                    if let Some(restriction) = tool.restriction.as_ref() { Text { size: "xs", color: "muted", "Restriction: {restriction}" } }
                    pre { "{serde_json::to_string_pretty(&tool.parameters).unwrap_or_else(|_| \"Schema unavailable\".to_owned())}" }
                }
            }
        }
    }
}

#[component]
pub(super) fn ConfigurationPanel(data: Rc<AgentInspection>) -> Element {
    let content_sx = use_hook(|| {
        libero::sx::sx()
            .display("flex")
            .flex_direction("column")
            .gap("sm")
    });
    rsx! {
        Paper {
            Flex { sx: content_sx,
                Title { size: "md", "Configuration" }
                Text { size: "sm", "Scope: this conversation" }
                Text { size: "sm", "Worker: {data.conversation.worker_label}" }
                Text { size: "sm", "Working directory: {data.conversation.cwd}" }
                Text { size: "sm", "Model selection and conversation routing use the workspace controls. Tool availability follows this conversation's mode." }
            }
        }
    }
}
