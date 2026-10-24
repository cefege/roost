//! Goal management stays in the chat dock while the transcript supplies state.
//! Commands use the canonical direct path, so only persisted events update the UI.
//! The composer mounts this dialog and routes each action through agent chat.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::GoalState;

use super::composer_commands::submit_direct_command;
use libero::components::{Button, Dialog, Flex, ProgressBar, Text, TextField, Textarea, Title};
use libero::hooks::{ModalScope, use_modal};
use std::{cell::RefCell, rc::Rc};

use crate::components::overlay_generation::OverlayGeneration;
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
    let generation = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let previous_open = use_hook(|| Rc::new(RefCell::new(None::<bool>)));
    let has_goal = goal.is_some();
    let current_status = goal.as_ref().map_or("", |state| state.status.as_str());
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
    let paused_goal = current_status == "paused";
    let manages_goal = matches!(current_status, "active" | "paused" | "budget_limited");
    let can_replace = has_goal && matches!(current_status, "active" | "budget_limited");
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
    let dialog_content_sx = use_hook(|| {
        libero::sx::sx()
            .max_height("calc(100dvh - 5rem)")
            .overflow_y("auto")
    });
    let dialog_generation = Rc::clone(&generation);
    let dialog = use_modal(move |scope: ModalScope<u64>| {
        let Some(token) = scope.try_args() else {
            return rsx! {};
        };
        let dialog_generation = Rc::clone(&dialog_generation);
        let pause_generation = Rc::clone(&dialog_generation);
        let resume_generation = Rc::clone(&dialog_generation);
        let refresh_generation = Rc::clone(&dialog_generation);
        let set_generation = Rc::clone(&dialog_generation);
        let budget_generation = Rc::clone(&dialog_generation);
        let drop_generation = Rc::clone(&dialog_generation);
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
        let pause_action_pump = pause_pump.clone();
        let pause_action_id = pause_id.clone();
        let resume_action_pump = resume_pump.clone();
        let resume_action_id = resume_id.clone();
        let refresh_action_pump = refresh_pump.clone();
        let refresh_action_id = refresh_id.clone();
        let set_action_pump = set_pump.clone();
        let set_action_id = set_id.clone();
        let budget_action_pump = budget_pump.clone();
        let budget_action_id = budget_id.clone();
        let drop_action_pump = drop_pump.clone();
        let drop_action_id = drop_id.clone();
        let initial_budget = initial_budget.clone();
        rsx! {
            Dialog { title, size: "md", "data-testid": "agent-goal-dialog",
                Flex { sx: dialog_content_sx.clone(), direction: "column", gap: "md",
                    Text { size: "sm", color: "muted",
                        "Goal changes are sent to the agent runtime and reflected here from its transcript."
                    }
                    if view() == GoalDialogView::Main {
                        if let Some(state) = goal.as_ref() {
                            Flex { direction: "column", gap: "sm", "data-testid": "agent-goal-status",
                                Flex { direction: "row", align: "center", gap: "sm",
                                    Text { size: "sm", "data-status": state.status.as_str(), "{goal_status_label(state.status.as_str())}" }
                                }
                                Text { size: "sm", "{state.objective}" }
                                Text { size: "xs", color: "muted",
                                    "Tokens: {state.tokens_used}"
                                    if let Some(limit) = state.token_budget { " / {limit} ({limit.saturating_sub(state.tokens_used)} remaining)" }
                                    " · Time: {format_duration(state.time_used_seconds)}"
                                }
                                if let Some(limit) = state.token_budget {
                                    ProgressBar { aria_label: "Token budget used", value: Some(state.tokens_used as f64 / limit.max(1) as f64 * 100.0), max: 100.0 }
                                }
                                if let Some(report) = &state.report {
                                    Flex { direction: "column", gap: "xs",
                                        Title { size: "sm", "Report" }
                                        Text { size: "sm", "{report}" }
                                    }
                                }
                                if let Some(reason) = &state.waiting_reason { Text { size: "sm", color: "muted", role: "status", "{reason}" } }
                                if state.status.as_str() == "active" && state.waiting_reason.is_some() {
                                    Text { size: "xs", color: "muted", "Send a message in the composer to re-arm goal continuation." }
                                }
                            }
                        } else {
                            Text { size: "sm", color: "muted", "No goal is set. Choose an objective to begin." }
                        }
                    }
                    if view() == GoalDialogView::Objective {
                        Textarea { value: objective(), oninput: move |value| objective.set(value), label: "Objective", placeholder: "What should the agent accomplish?", rows: 4, "data-testid": "agent-goal-objective" }
                        if goal.as_ref().is_some_and(|state| state.status.as_str() == "paused") { Text { color: "error", role: "alert", "Resume or drop the paused goal before replacing it." } }
                    }
                    if view() == GoalDialogView::Budget {
                        Text { size: "sm", "Set a positive token limit, or enter off to remove the limit. Usage is retained." }
                        TextField { value: budget(), oninput: move |value| budget.set(value), label: "Token budget", placeholder: "e.g. 50000 or off", "data-testid": "agent-goal-budget-value" }
                        if goal.as_ref().is_some_and(|state| state.status.as_str() == "paused") { Text { color: "error", role: "alert", "Resume the goal before changing its budget." } }
                    }
                    if view() == GoalDialogView::ConfirmDrop { Text { size: "sm", "Dropping disables this goal. Its status and report remain in the session history." } }
                    if let Some(message) = command_error() { Text { color: "error", role: "alert", "{message}" } }
                    Flex { direction: "row", gap: "sm", justify: "end",
                        if view() == GoalDialogView::Main {
                            if !has_goal || matches!(current_status, "complete" | "dropped") {
                                Button { variant: "filled", "data-testid": "agent-goal-edit", onclick: move |_| view.set(GoalDialogView::Objective), "Set objective" }
                            }
                            if matches!(current_status, "active" | "budget_limited") {
                                Button { variant: "filled", "data-testid": "agent-goal-pause", disabled: disabled || pending(), onclick: move |_| dispatch_goal(pause_action_pump.clone(), pause_action_id.clone(), "/goal pause".to_string(), pending, view, on_result, Rc::clone(&pause_generation), token), "Pause" }
                            }
                            if paused_goal {
                                Button { variant: "filled", "data-testid": "agent-goal-resume", disabled: disabled || pending(), onclick: move |_| dispatch_goal(resume_action_pump.clone(), resume_action_id.clone(), "/goal resume".to_string(), pending, view, on_result, Rc::clone(&resume_generation), token), "Resume" }
                            }
                            if can_replace {
                                Button { variant: "outlined", "data-testid": "agent-goal-edit", onclick: move |_| view.set(GoalDialogView::Objective), "Replace objective" }
                            }
                            if manages_goal {
                                Button { variant: "outlined", "data-testid": "agent-goal-budget", disabled: paused_goal || pending(), onclick: move |_| { budget.set(initial_budget.clone()); view.set(GoalDialogView::Budget); }, "Adjust budget" }
                                Button { variant: "filled", color: "error", "data-testid": "agent-goal-drop", disabled: pending(), onclick: move |_| view.set(GoalDialogView::ConfirmDrop), "Drop" }
                                Button { variant: "standard", color: "muted", "data-testid": "agent-goal-refresh", disabled: disabled || pending(), onclick: move |_| dispatch_goal(refresh_action_pump.clone(), refresh_action_id.clone(), "/goal show".to_string(), pending, view, on_result, Rc::clone(&refresh_generation), token), "Refresh status" }
                            }
                        }
                        Button { variant: "standard", color: "muted", onclick: move |_| scope.close(), "Close" }
                        if view() == GoalDialogView::Objective {
                            Button { variant: "outlined", onclick: move |_| { if has_goal { view.set(GoalDialogView::Main); } else { scope.close(); } }, "Cancel" }
                            Button { variant: "filled", "data-testid": "agent-goal-submit", disabled: disabled || pending() || objective().trim().is_empty() || paused_goal, onclick: move |_| {
                                dispatch_goal(set_action_pump.clone(), set_action_id.clone(), format!("/goal set {}", objective().trim()), pending, view, on_result, Rc::clone(&set_generation), token);
                            }, if can_replace { "Replace goal" } else { "Start goal" } }
                        }
                        if view() == GoalDialogView::Budget {
                            Button { variant: "outlined", onclick: move |_| view.set(GoalDialogView::Main), "Cancel" }
                            Button { variant: "filled", "data-testid": "agent-goal-budget-submit", disabled: disabled || pending() || !valid_budget(&budget()) || paused_goal, onclick: move |_| {
                                dispatch_goal(budget_action_pump.clone(), budget_action_id.clone(), format!("/goal budget {}", normalize_budget(&budget())), pending, view, on_result, Rc::clone(&budget_generation), token);
                            }, "Save budget" }
                        }
                        if view() == GoalDialogView::ConfirmDrop {
                            Button { variant: "outlined", disabled: pending(), onclick: move |_| view.set(GoalDialogView::Main), "Cancel" }
                            Button { variant: "filled", color: "error", "data-testid": "agent-goal-confirm-drop", disabled: disabled || pending(), onclick: move |_| {
                                dispatch_goal(drop_action_pump.clone(), drop_action_id.clone(), "/goal drop".to_string(), pending, view, on_result, Rc::clone(&drop_generation), token);
                            }, "Drop goal" }
                        }
                    }
                }
            }
        }
    });
    let sync_dialog = dialog;
    let sync_generation = Rc::clone(&generation);
    let sync_previous = Rc::clone(&previous_open);
    use_effect(move || {
        let is_open = open();
        let was_open = *sync_previous.borrow();
        if was_open == Some(is_open) {
            return;
        }
        *sync_previous.borrow_mut() = Some(is_open);
        if is_open {
            let token = sync_generation.borrow_mut().begin();
            let completion_generation = Rc::clone(&sync_generation);
            let mut open = open;
            sync_dialog.open_with(token).onresult(move |_| {
                if completion_generation.borrow_mut().finish(token) {
                    open.set(false);
                }
            });
        } else {
            sync_generation.borrow_mut().invalidate();
            sync_dialog.close();
        }
    });
    let drop_generation = Rc::clone(&generation);
    use_drop(move || drop_generation.borrow_mut().invalidate());
    rsx! {}
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
#[allow(clippy::too_many_arguments)]
fn dispatch_goal(
    pump: Pump,
    conversation_id: String,
    command: String,
    mut pending: Signal<bool>,
    mut view: Signal<GoalDialogView>,
    on_result: EventHandler<Result<(), String>>,
    generation: Rc<RefCell<OverlayGeneration>>,
    token: u64,
) {
    if pending() || !generation.borrow().is_current(token) {
        return;
    }
    pending.set(true);
    let completed_generation = Rc::clone(&generation);
    let completed = EventHandler::new(move |result: Result<(), String>| {
        if !completed_generation.borrow().is_current(token) {
            pending.set(false);
            return;
        }
        pending.set(false);
        if result.is_ok() {
            view.set(GoalDialogView::Main);
        }
        on_result.call(result);
    });
    submit_direct_command(pump, conversation_id, command, completed);
}
