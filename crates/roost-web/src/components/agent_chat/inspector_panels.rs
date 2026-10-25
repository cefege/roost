//! Native inspection panels rendered by the controlled agent inspector.
//! Values and actions come from the runtime snapshot, never browser-only state.
//! Shared snapshot ownership avoids copying tool schemas for inactive tabs.

use std::rc::Rc;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentInspection, AgentRunState, ModelsCatalog};

use super::model_controls::current_model_label;
use super::role_switcher::RoleSwitcher;
use crate::components::ui::field::scoped_element_id;

use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Card, CardContent, CardHeader, CardTitle, Collapsible,
    CollapsibleContent, CollapsibleTrigger, Icon, IconType, Select, SelectOption, SwitchRow,
};
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
    let thinking_options = catalog
        .as_ref()
        .map(|catalog| {
            catalog
                .thinking_levels
                .iter()
                .map(|level| SelectOption::new(level.clone(), level.clone()))
                .collect()
        })
        .unwrap_or_default();
    let model_label = current_model_label(catalog.as_ref(), &data.conversation);
    rsx! {
        div { class: "flex flex-col gap-3",
            Card { CardHeader { CardTitle { "Session" } }
                CardContent { class: "flex flex-col gap-3 text-sm",
                    RoleSwitcher {
                        trigger_id: format!("agent-inspector-model-{}", data.conversation.id),
                        trigger_label: model_label,
                        trigger_title: "Change the models for this chat and its subagents".to_owned(),
                        conversation: data.conversation.clone(),
                        catalog: catalog.clone(),
                        pump,
                    }
                    Select {
                        label: "Thinking",
                        value: data.conversation.thinking_level.clone().unwrap_or_default(),
                        placeholder: "Agent default",
                        disabled: pending,
                        options: thinking_options,
                        on_change: move |level| on_thinking.call(level),
                    }
                    div { class: "flex flex-wrap gap-x-4 gap-y-1 text-muted-foreground",
                        span {
                            "Context {context.context_tokens} tokens"
                            if let Some(value) = percent { " · {value}%" }
                        }
                        span { "Input {data.usage.input_tokens} · output {data.usage.output_tokens}" }
                        span { "Cost ${data.usage.cost_usd:.4}" }
                    }
                    if let Some(window) = context.context_window_tokens.filter(|window| *window > 0) {
                        progress { class: "h-2 w-full accent-primary", max: "{window}", value: "{context.context_tokens}", "aria-label": "Model context usage" }
                    }
                    if let Some(trigger) = context.compaction_trigger_tokens {
                        p { class: "text-xs text-muted-foreground",
                            "Automatic compaction triggers above {trigger} tokens with {context.compaction_headroom_tokens} tokens of headroom; it keeps the latest {context.compaction_keep_tokens}."
                            if let Some(seq) = context.last_compacted_seq {
                                " Last compacted at journal entry {seq}."
                            } else {
                                " No compaction recorded."
                            }
                        }
                    } else {
                        p { class: "text-xs text-muted-foreground", "Context window metadata is unavailable; the automatic compaction trigger cannot be calculated." }
                    }
                    Button { variant: ButtonVariant::Outline, size: ButtonSize::Sm,
                        disabled: pending || data.conversation.run_state == AgentRunState::Running,
                        onclick: move |event| on_compact.call(event), "Compact now" }
                }
            }
        }
    }
}

#[component]
pub(super) fn StatePanel(data: Rc<AgentInspection>) -> Element {
    rsx! { div { class: "flex flex-col gap-3",
        Card { CardHeader { CardTitle { "Durable state" } }
            CardContent { class: "flex flex-col gap-2 text-sm",
                p { "Run: {data.conversation.run_state.as_str()}" }
                p { "Mode: {data.conversation.mode}" }
                if let Some(goal) = data.goal.as_ref() {
                    h3 { class: "font-medium", "Goal: {goal.objective}" }
                    p { class: "text-muted-foreground", "{goal.status:?} · {goal.tokens_used} tokens · {goal.time_used_seconds}s" }
                    if let Some(reason) = goal.waiting_reason.as_ref() { p { "Waiting: {reason}" } }
                } else { p { class: "text-muted-foreground", "No durable goal is active." } }
                div { class: "mt-3 border-t border-border pt-3",
                    h3 { class: "font-medium", "Inbox" }
                    if data.inbox.is_empty() {
                        p { class: "text-muted-foreground", "No queued messages." }
                    } else {
                        for message in &data.inbox {
                            p { class: "whitespace-pre-wrap text-muted-foreground", "{message.text}" }
                        }
                    }
                }
            }
        }
    } }
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
    rsx! { div { class: "flex flex-col gap-3",
        Card { CardHeader { CardTitle { "Current activity" } }
            CardContent { class: "flex flex-col gap-3",
                if has_stoppable_work {
                    Button { variant: ButtonVariant::Destructive, size: ButtonSize::Sm, disabled: pending,
                        onclick: move |event| on_stop_all.call(event), "Stop all work and background jobs" }
                }
                if data.activity.is_empty() {
                    p { class: "text-sm text-muted-foreground", "No running tools, subagents, or background jobs." }
                } else {
                    for activity in &data.activity {
                        div { class: "flex items-start justify-between gap-3 border-b border-border pb-2 last:border-0",
                            div { class: "min-w-0",
                                p { class: "break-words text-sm font-medium", "{activity.label}" }
                                p { class: "break-words text-xs text-muted-foreground",
                                    "{activity.kind} · {activity.state}"
                                    if let Some(detail) = &activity.detail { " · {detail}" }
                                }
                            }
                            if activity.kind == "run" && activity.state == "running" {
                                Button { variant: ButtonVariant::Outline, size: ButtonSize::Sm, disabled: pending,
                                    onclick: move |event| on_stop.call(event), "Stop run" }
                            }
                        }
                    }
                }
            }
        }
    } }
}

#[component]
pub(super) fn ToolsPanel(
    data: Rc<AgentInspection>,
    pending: bool,
    on_read_only: EventHandler<bool>,
) -> Element {
    rsx! { div { class: "flex flex-col gap-3",
        SwitchRow { headline: "Read only", checked: data.read_only, disabled: pending,
            support: "Only reading tools are offered. Newly dispatched calls cannot write or run shell commands.",
            test_id: "agent-inspector-read-only",
            on_checked_change: move |checked| on_read_only.call(checked) }
        p { class: "text-xs text-muted-foreground", "{data.tools.len()} offered tools. Interrupted tool calls are not automatically replayed." }
        if data.tools.is_empty() { p { class: "text-sm text-muted-foreground", "No tools are offered for this conversation." } }
        for (index, tool) in data.tools.iter().enumerate() {
            ToolDefinition { key: "{tool.name}", data: Rc::clone(&data), index }
        }
    } }
}

#[component]
fn ToolDefinition(data: Rc<AgentInspection>, index: usize) -> Element {
    let mut open = use_signal(|| false);
    let panel_id = use_hook(|| scoped_element_id("agent-inspector-tool"));
    let Some(tool) = data.tools.get(index) else {
        return rsx! {};
    };
    rsx! {
        Card {
            Collapsible { open: open(), on_open_change: move |next| open.set(next),
                CollapsibleTrigger { class: "w-full justify-between", controls_id: panel_id.clone(),
                    span { class: "truncate", "{tool.name}" }
                    Icon { icon: if open() { IconType::ChevronDown } else { IconType::ChevronRight }, class: "size-4 shrink-0" }
                }
                CollapsibleContent {
                    if open() {
                        div { id: panel_id,
                            CardContent { class: "flex min-w-0 flex-col gap-2 pb-4",
                                p { class: "text-sm text-muted-foreground", "{tool.description}" }
                                if let Some(restriction) = tool.restriction.as_ref() { p { class: "text-xs text-muted-foreground", "Restriction: {restriction}" } }
                                pre { class: "overflow-x-auto rounded-md bg-muted p-3 text-xs", "{serde_json::to_string_pretty(&tool.parameters).unwrap_or_else(|_| \"Schema unavailable\".to_owned())}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub(super) fn ConfigurationPanel(data: Rc<AgentInspection>) -> Element {
    rsx! { Card { CardHeader { CardTitle { "Configuration" } }
        CardContent { class: "flex flex-col gap-2 text-sm",
            p { "Scope: this conversation" }
            p { class: "break-words", "Worker: {data.conversation.worker_label}" }
            p { class: "break-words", "Working directory: {data.conversation.cwd}" }
            p { "Model selection and conversation routing use the workspace controls. Tool availability follows this conversation's mode." }
        }
    } }
}
