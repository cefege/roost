//! Goal management stays in the chat dock while the transcript supplies state.
//! Commands use the canonical direct path, so only persisted events update the UI.
//! The composer mounts this dialog and routes each action through agent chat.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::GoalState;

use super::agent_alert::ERROR_TEXT_CLASS;
use super::composer_commands::submit_direct_command;
use crate::components::md::{
    Button, ButtonVariant, Dialog, ProgressBar, StatusDot, Surface, TextField,
};
use crate::pump::Pump;

#[derive(Clone, Copy, PartialEq, Eq)]
enum GoalDialogView {
    Main,
    Objective,
    Budget,
    ConfirmDrop,
}

#[component]
pub(super) fn GoalControls(
    goal: Option<GoalState>,
    open: Signal<bool>,
    view_request: Signal<String>,
    pending: Signal<bool>,
    command_error: Signal<Option<String>>,
    on_result: EventHandler<Result<(), String>>,
    conversation_id: String,
    pump: Pump,
    disabled: bool,
) -> Element {
    let has_goal = goal.is_some();
    let current_status = goal.as_ref().map_or("", |state| state.status.as_str());
    let status_indicator = match current_status {
        "active" => "running",
        "budget_limited" => "warn",
        "complete" => "done",
        _ => "idle",
    };
    let initial_budget = goal
        .as_ref()
        .and_then(|state| state.token_budget)
        .map_or_else(String::new, |value| value.to_string());
    let mut view = use_signal(|| {
        if has_goal {
            GoalDialogView::Main
        } else {
            GoalDialogView::Objective
        }
    });
    let mut objective = use_signal(String::new);
    let mut budget = use_signal(String::new);
    use_effect(move || {
        let request = view_request();
        if open() {
            match request.as_str() {
                "set" => view.set(GoalDialogView::Objective),
                "budget" => view.set(GoalDialogView::Budget),
                _ => view.set(if has_goal {
                    GoalDialogView::Main
                } else {
                    GoalDialogView::Objective
                }),
            }
        }
    });
    let can_replace = matches!(current_status, "active" | "budget_limited");
    let pause_pump = pump.clone();
    let pause_id = conversation_id.clone();
    let resume_pump = pump.clone();
    let resume_id = conversation_id.clone();
    let refresh_pump = pump.clone();
    let refresh_id = conversation_id.clone();
    let set_pump = pump.clone();
    let set_id = conversation_id.clone();
    let budget_pump = pump.clone();
    let budget_id = conversation_id.clone();
    let drop_pump = pump.clone();
    let drop_id = conversation_id.clone();
    let status_content = if let Some(state) = goal.as_ref() {
        rsx! {
            Surface { class: "agent-goal__details", test_id: "agent-goal-status", level: 1, pad: Some(3), border: true,
                span { class: "agent-goal__status", "data-status": state.status.as_str(),
                    StatusDot { status: status_indicator.to_string() }
                    "{goal_status_label(state.status.as_str())}"
                }
                p { class: "agent-goal__objective", "{state.objective}" }
                p { class: "agent-goal__accounting",
                    "Tokens: {state.tokens_used}"
                    if let Some(limit) = state.token_budget { " / {limit} ({limit.saturating_sub(state.tokens_used)} remaining)" }
                    " · Time: {format_duration(state.time_used_seconds)}"
                }
                if let Some(limit) = state.token_budget {
                    ProgressBar { value: Some(state.tokens_used as f64 / limit.max(1) as f64), label: "Token budget used".to_string(), meter: true }
                }
                if let Some(report) = &state.report { section { class: "agent-goal__report", h3 { "Report" } p { "{report}" } } }
                if let Some(reason) = &state.waiting_reason { p { class: "agent-goal__waiting", role: "status", "{reason}" } }
                if state.status.as_str() == "active" && state.waiting_reason.is_some() {
                    p { class: "agent-goal__continuation-hint", "Send a message in the composer to re-arm goal continuation." }
                }
            }
        }
    } else {
        rsx! { p { class: "agent-goal__empty", "No goal is set. Choose an objective to begin." } }
    };
    let paused_goal = current_status == "paused";
    let manages_goal = matches!(current_status, "active" | "paused" | "budget_limited");
    let title = match view() {
        GoalDialogView::Main => "Goal status",
        GoalDialogView::Objective => {
            if can_replace {
                "Replace goal objective"
            } else {
                "Set a goal"
            }
        }
        GoalDialogView::Budget => "Adjust token budget",
        GoalDialogView::ConfirmDrop => "Drop this goal?",
    };
    rsx! {
        Dialog {
            open: open(),
            on_close: move |_| { open.set(false); view.set(if has_goal { GoalDialogView::Main } else { GoalDialogView::Objective }); },
            headline: Some(title.to_string()),
            test_id: Some("agent-goal-dialog".to_string()),
            description: Some(rsx! { p { "Goal changes are sent to the agent runtime and reflected here from its transcript." } }),
            actions: Some(rsx! {
                if view() == GoalDialogView::Main {
                    if !has_goal || matches!(current_status, "complete" | "dropped") {
                        Button { variant: ButtonVariant::Default, "data-testid": "agent-goal-edit", onclick: move |_| view.set(GoalDialogView::Objective), "Set objective" }
                    }
                    if matches!(current_status, "active" | "budget_limited") {
                        Button { variant: ButtonVariant::Default, "data-testid": "agent-goal-pause", disabled: disabled || pending(), onclick: move |_| dispatch_goal(pause_pump.clone(), pause_id.clone(), "/goal pause".to_string(), pending, view, on_result), "Pause" }
                    }
                    if paused_goal {
                        Button { variant: ButtonVariant::Default, "data-testid": "agent-goal-resume", disabled: disabled || pending(), onclick: move |_| dispatch_goal(resume_pump.clone(), resume_id.clone(), "/goal resume".to_string(), pending, view, on_result), "Resume" }
                    }
                    if can_replace {
                        Button { variant: ButtonVariant::Outline, "data-testid": "agent-goal-edit", onclick: move |_| view.set(GoalDialogView::Objective), "Replace objective" }
                    }
                    if manages_goal {
                        Button { variant: ButtonVariant::Outline, "data-testid": "agent-goal-budget", disabled: paused_goal || pending(), onclick: move |_| { budget.set(initial_budget.clone()); view.set(GoalDialogView::Budget); }, "Adjust budget" }
                        Button { variant: ButtonVariant::Destructive, "data-testid": "agent-goal-drop", disabled: pending(), onclick: move |_| view.set(GoalDialogView::ConfirmDrop), "Drop" }
                        Button { variant: ButtonVariant::Ghost, "data-testid": "agent-goal-refresh", disabled: disabled || pending(), onclick: move |_| dispatch_goal(refresh_pump.clone(), refresh_id.clone(), "/goal show".to_string(), pending, view, on_result), "Refresh status" }
                    }
                }
                    Button { variant: ButtonVariant::Ghost, onclick: move |_| open.set(false), "Close" }
                if view() == GoalDialogView::Objective {
                    Button { variant: ButtonVariant::Outline, onclick: move |_| { if has_goal { view.set(GoalDialogView::Main); } else { open.set(false); } }, "Cancel" }
                    Button { variant: ButtonVariant::Default, "data-testid": "agent-goal-submit", disabled: disabled || pending() || objective().trim().is_empty() || paused_goal, onclick: move |_| {
                        dispatch_goal(set_pump.clone(), set_id.clone(), format!("/goal set {}", objective().trim()), pending, view, on_result);
                    }, if can_replace { "Replace goal" } else { "Start goal" } }
                }
                if view() == GoalDialogView::Budget {
                    Button { variant: ButtonVariant::Outline, onclick: move |_| view.set(GoalDialogView::Main), "Cancel" }
                    Button { variant: ButtonVariant::Default, "data-testid": "agent-goal-budget-submit", disabled: disabled || pending() || !valid_budget(&budget()) || paused_goal, onclick: move |_| {
                        dispatch_goal(budget_pump.clone(), budget_id.clone(), format!("/goal budget {}", normalize_budget(&budget())), pending, view, on_result);
                    }, "Save budget" }
                }
                if view() == GoalDialogView::ConfirmDrop {
                    Button { variant: ButtonVariant::Outline, disabled: pending(), onclick: move |_| view.set(GoalDialogView::Main), "Cancel" }
                    Button { variant: ButtonVariant::Destructive, "data-testid": "agent-goal-confirm-drop", disabled: disabled || pending(), onclick: move |_| {
                        dispatch_goal(drop_pump.clone(), drop_id.clone(), "/goal drop".to_string(), pending, view, on_result);
                    }, "Drop goal" }
                }
            }),
            div { class: "agent-goal__dialog-body",
                if view() == GoalDialogView::Main { {status_content} }
                if view() == GoalDialogView::Objective {
                    TextField { value: objective(), on_input: move |value| objective.set(value), label: Some("Objective".to_string()), placeholder: Some("What should the agent accomplish?".to_string()), input_type: Some("textarea".to_string()), rows: Some(4), test_id: "agent-goal-objective" }
                    if goal.as_ref().is_some_and(|state| state.status.as_str() == "paused") { p { role: "alert", "Resume or drop the paused goal before replacing it." } }
                }
                if view() == GoalDialogView::Budget {
                    p { "Set a positive token limit, or enter off to remove the limit. Usage is retained." }
                    TextField { value: budget(), on_input: move |value| budget.set(value), label: Some("Token budget".to_string()), placeholder: Some("e.g. 50000 or off".to_string()), input_type: Some("text".to_string()), test_id: "agent-goal-budget-value" }
                    if goal.as_ref().is_some_and(|state| state.status.as_str() == "paused") { p { role: "alert", "Resume the goal before changing its budget." } }
                }
                if view() == GoalDialogView::ConfirmDrop { p { "Dropping disables this goal. Its status and report remain in the session history." } }
                if let Some(message) = command_error() { p { class: ERROR_TEXT_CLASS, role: "alert", "{message}" } }
            }
        }
    }
}

fn goal_status_label(status: &str) -> &'static str {
    match status {
        "active" => "Active",
        "paused" => "Paused",
        "budget_limited" => "Budget limited",
        "complete" => "Complete",
        "dropped" => "Dropped",
        _ => "Unknown",
    }
}

fn valid_budget(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("off")
        || value.trim().parse::<u64>().is_ok_and(|budget| budget > 0)
}

fn normalize_budget(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("off") {
        "off".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = seconds % 3600 / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}
fn dispatch_goal(
    pump: Pump,
    conversation_id: String,
    command: String,
    mut pending: Signal<bool>,
    mut view: Signal<GoalDialogView>,
    on_result: EventHandler<Result<(), String>>,
) {
    if pending() {
        return;
    }
    pending.set(true);
    let completed = EventHandler::new(move |result: Result<(), String>| {
        pending.set(false);
        if result.is_ok() {
            view.set(GoalDialogView::Main);
        }
        on_result.call(result);
    });
    submit_direct_command(pump, conversation_id, command, completed);
}
