//! Durable goal policy split by persistence, user commands, and continuation lifecycle.
//! The journal is authoritative; the shared transcript type is its public view.
//! Runtime commands, model tools and recovery call this policy.

mod commands;
mod continuation;
mod persistence;

use roost_protocol::wire::agent_chat::GoalState;

pub(crate) const TOOL_GOAL: &str = "goal";

pub(super) fn describe(state: &GoalState) -> String {
    let tokens = state.token_budget.map_or_else(
        || format!("{} tokens used", state.tokens_used),
        |budget| {
            format!(
                "{} / {budget} tokens used ({} remaining)",
                state.tokens_used,
                budget.saturating_sub(state.tokens_used)
            )
        },
    );
    let mut output = format!(
        "Objective: {}\nStatus: {}\nTokens: {}\nTime spent: {}s",
        state.objective,
        state.status.as_str(),
        tokens,
        state.time_used_seconds,
    );
    if let Some(reason) = &state.waiting_reason {
        output.push_str(&format!("\nWaiting: {reason}"));
    }
    if let Some(report) = &state.report {
        output.push_str(&format!("\nReport: {report}"));
    }
    output
}
