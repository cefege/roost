//! Compact goal progress sits beside the composer controls.
//! The transcript remains authoritative; opening the chip only reveals management.
//! Status, budget progress and waiting details are projections of GoalState.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::GoalState;

use crate::components::md::Chip;

#[component]
pub(super) fn GoalDock(
    goal: Option<GoalState>,
    mut view_request: Signal<String>,
    on_open: EventHandler<()>,
) -> Element {
    let label = goal.as_ref().map_or_else(
        || "Set goal".to_owned(),
        |state| {
            let status = match state.status.as_str() {
                "active" => "Active",
                "paused" => "Paused",
                "budget_limited" => "Budget limited",
                "complete" => "Complete",
                "dropped" => "Dropped",
                _ => "Unknown",
            };
            let progress = state.token_budget.map_or_else(
                || format!("{} tokens", state.tokens_used),
                |limit| format!("{} / {limit}", state.tokens_used),
            );
            format!("Goal · {status} · {progress}")
        },
    );
    let title = goal.as_ref().map_or_else(
        || "Manage agent goal".to_owned(),
        |state| {
            format!(
                "{}{}{}",
                state.objective,
                state.waiting_reason.as_ref().map_or("", |_| "\nWaiting: "),
                state.waiting_reason.as_deref().unwrap_or_default()
            )
        },
    );
    let waiting = goal.as_ref().and_then(|state| state.waiting_reason.clone());
    rsx! {
        Chip {
            label,
            icon: Some("flag".to_string()),
            selected: goal.is_some().then_some(true),
            title: Some(title),
            test_id: Some("agent-chat-goal".to_string()),
            onclick: Some(EventHandler::new(move |_| { view_request.set(String::new()); on_open.call(()); })),
        }
        if let Some(reason) = waiting {
            span { class: "agent-chat__goal-fact", "data-testid": "agent-chat-goal-waiting", role: "status", "{reason}" }
        }
    }
}
