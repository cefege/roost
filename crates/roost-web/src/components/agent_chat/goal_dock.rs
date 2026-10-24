//! The goal chip in the composer's footer, shown while the conversation has a
//! goal; `/goal` sets one. The transcript remains authoritative; opening the
//! chip only reveals management. Status and budget are projections of GoalState.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::GoalState;

use crate::components::ui::{Chip, IconType};

#[component]
pub(super) fn GoalDock(
    goal: Option<GoalState>,
    mut view_request: Signal<String>,
    on_open: EventHandler<()>,
) -> Element {
    // A finished goal stays in the transcript; the footer shows only live ones.
    let Some(state) = goal.filter(|state| !matches!(state.status.as_str(), "complete" | "dropped"))
    else {
        return rsx! {};
    };
    let status = match state.status.as_str() {
        "active" => "Active",
        "paused" => "Paused",
        "budget_limited" => "Budget limited",
        _ => "Unknown",
    };
    let progress = state.token_budget.map_or_else(
        || format!("{} tokens", state.tokens_used),
        |limit| format!("{} / {limit} tokens", state.tokens_used),
    );
    let label = format!("Goal · {status}");
    let title = format!(
        "{}\n{progress}{}{}",
        state.objective,
        state.waiting_reason.as_ref().map_or("", |_| "\nWaiting: "),
        state.waiting_reason.as_deref().unwrap_or_default()
    );
    let waiting = state.waiting_reason.clone();
    rsx! {
        Chip {
            label,
            icon: Some(IconType::Flag),
            selected: Some(true),
            title: Some(title),
            test_id: Some("agent-chat-goal".to_string()),
            onclick: Some(EventHandler::new(move |_| { view_request.set(String::new()); on_open.call(()); })),
        }
        if let Some(reason) = waiting {
            span { class: "text-sm text-muted-foreground", "data-testid": "agent-chat-goal-waiting", role: "status", "{reason}" }
        }
    }
}
