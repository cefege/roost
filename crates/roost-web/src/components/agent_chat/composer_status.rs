//! Composer chrome beneath the input: the status line (usage, run state, worker,
//! advisor, goal) and the pinned banner for request failures that remain durable in
//! history. Dismissing the banner never removes transcript items or failure records;
//! the active error clears only when the next transcript attempt clears it.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{ConversationSummary, GoalState, ModelsCatalog, Transcript};

use super::agent_alert::AgentAlert;
use super::composer_commands::{advisor_title, submit_direct_command};
use super::goal_dock::GoalDock;
use super::header::{HeaderUsage, current_context_window, header_usage};
use crate::components::ui::{
    Button, ButtonVariant, Chip, Collapsible, CollapsibleContent, CollapsibleTrigger, IconType,
    Progress,
};
use crate::pump::Pump;

#[component]
pub(super) fn ComposerErrorBanner(
    error: Option<String>,
    running: bool,
    on_stop: EventHandler<()>,
    on_switch_model: EventHandler<()>,
) -> Element {
    let mut dismissed = use_signal(|| false);
    let mut details_open = use_signal(|| false);
    let mut previous_error = use_signal(|| error.clone());
    use_effect(use_reactive((&error,), move |(current_error,)| {
        if current_error != previous_error.peek().clone() {
            previous_error.set(current_error.clone());
            dismissed.set(false);
            details_open.set(false);
        }
        if current_error.is_none() {
            dismissed.set(false);
        }
    }));
    let Some(error_text) = error else {
        return rsx! {};
    };
    if dismissed() {
        return rsx! {};
    }
    let headline = error_headline(&error_text);
    let has_details = error_text.len() > 80 || error_text.lines().count() > 1;
    rsx! {
        AgentAlert { test_id: "agent-chat-pinned-error",
            Collapsible {
                open: details_open() && has_details,
                on_open_change: move |next: bool| details_open.set(next),
                div { class: "flex flex-wrap items-center gap-2",
                    strong { class: "min-w-0 flex-1 break-words font-semibold", "{headline}" }
                    if has_details {
                        CollapsibleTrigger {
                            test_id: "agent-chat-error-details",
                            controls_id: "agent-chat-pinned-error-details",
                            if details_open() { "Less" } else { "Details" }
                        }
                    }
                    Button {
                        variant: ButtonVariant::Ghost,
                        "data-testid": "agent-chat-error-dismiss",
                        onclick: move |_| dismissed.set(true),
                        "Dismiss"
                    }
                }
                CollapsibleContent {
                    pre {
                        id: "agent-chat-pinned-error-details",
                        class: "m-0 max-h-48 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted p-2 font-mono text-xs text-foreground",
                        "{error_text}"
                    }
                }
            }
            div { class: "flex flex-wrap items-center gap-2",
                if running {
                    Button {
                        variant: ButtonVariant::Destructive,
                        "data-testid": "agent-chat-error-stop",
                        title: "Interrupt foreground work. Background jobs continue.",
                        onclick: move |_| on_stop.call(()),
                        "Interrupt"
                    }
                }
                Button {
                    variant: ButtonVariant::Outline,
                    "data-testid": "agent-chat-error-switch-model",
                    onclick: move |_| on_switch_model.call(()),
                    "Switch model"
                }
            }
        }
    }
}

fn error_headline(error: &str) -> String {
    error
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("The request failed.")
        .to_owned()
}

/// Usage for the status line, read from the same totals and window the header uses
/// so the two never disagree. `None` until the conversation has spent something.
pub(super) fn composer_usage(
    transcript: &Transcript,
    catalog: Option<&ModelsCatalog>,
    conversation: &ConversationSummary,
) -> Option<HeaderUsage> {
    let totals = &transcript.usage;
    header_usage(
        totals.context_tokens,
        totals.input_tokens + totals.output_tokens,
        totals.cost_usd,
        current_context_window(catalog, conversation),
    )
}

/// The line under the composer. Each value comes from the store; a value the store
/// does not carry is left out rather than estimated.
#[component]
pub(super) fn ComposerStatusLine(
    conversation: ConversationSummary,
    pump: Pump,
    goal: Option<GoalState>,
    goal_view_request: Signal<String>,
    on_goal_open: EventHandler<()>,
    command_error: Signal<Option<String>>,
    usage: Option<HeaderUsage>,
    cost_usd: f64,
    running: bool,
    failed: bool,
) -> Element {
    let mut advisor_pending = use_signal(|| false);
    let mut command_error = command_error;
    let advisor_done = EventHandler::new(move |result: Result<(), String>| {
        advisor_pending.set(false);
        command_error.set(result.err());
    });
    let advisor_pump = pump.clone();
    let advisor_conversation = conversation.id.clone();
    let advisor_on = conversation.advisor;
    let advisor_busy = advisor_pending();
    let fraction = usage.as_ref().and_then(|usage| usage.fraction);
    let usage_title = usage.as_ref().map(|usage| usage.title.clone());
    let percent = fraction.map(|fraction| format!("{:.0}%", fraction * 100.0));
    let cost = format!("${cost_usd:.4}");
    let worker = format!("on {}", conversation.worker_label);
    let run_label = if running {
        Some("Working")
    } else if failed {
        Some("Failed")
    } else {
        None
    };
    rsx! {
        div {
            class: "flex flex-wrap items-center justify-center gap-x-3 gap-y-1 px-3 text-xs text-muted-foreground",
            "data-testid": "agent-chat-status",
            if let Some(title) = usage_title {
                span { class: "inline-flex items-center gap-2", title: "{title}",
                    if let Some(fraction) = fraction {
                        Progress { value: Some(fraction), label: "Context used", meter: true, class: "w-12" }
                    }
                    if let Some(percent) = percent.as_ref() {
                        span { "{percent}" }
                    }
                    span { "{cost}" }
                }
            }
            if let Some(label) = run_label {
                span { class: if failed { "text-destructive" } else { "text-primary" }, "{label}" }
            }
            if !conversation.worker_label.is_empty() {
                span { class: "min-w-0 truncate", "{worker}" }
            }
            if conversation.advisor || advisor_busy {
                Chip { label: "Advisor".to_string(), icon: Some(IconType::Brain), selected: Some(advisor_on),
                    title: Some(advisor_title(advisor_on, advisor_busy).to_string()),
                    test_id: Some("agent-advisor-toggle".to_string()),
                    disabled: advisor_busy,
                    busy: advisor_busy,
                    onclick: Some(EventHandler::new(move |_| {
                        if advisor_pending() {
                            return;
                        }
                        advisor_pending.set(true);
                        submit_direct_command(advisor_pump.clone(), advisor_conversation.clone(), "/advisor".to_string(), advisor_done);
                    })),
                }
            }
            GoalDock { goal, view_request: goal_view_request, on_open: on_goal_open }
        }
    }
}
