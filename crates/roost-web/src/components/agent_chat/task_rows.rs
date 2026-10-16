//! The task box body: the subagent conversations a `task` call started, shown
//! as OMP's agent rows. The last four are shown and the rest fold into one
//! summary line. Window and counting are pure; the component reads live run
//! state from the store so ordinary tool cards do not re-render on every delta.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::AgentRunState;

use crate::components::md::{Button, ButtonSize, ButtonVariant, List, ListRow, StatusDot};
use crate::pump::use_store;
use crate::router_state::use_navigate;

/// Agent rows shown before the rest fold, matching OMP's agent tree.
pub(super) const VISIBLE_AGENT_ROWS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentStatus {
    Done,
    Running,
    Failed,
}

impl AgentStatus {
    pub(super) fn from_run_state(run_state: AgentRunState) -> Self {
        match run_state {
            AgentRunState::Idle => Self::Done,
            AgentRunState::Running => Self::Running,
            AgentRunState::Failed => Self::Failed,
        }
    }

    fn dot_status(self) -> &'static str {
        match self {
            Self::Done => "ok",
            Self::Running => "running",
            Self::Failed => "error",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Done => "Done",
            Self::Running => "Running",
            Self::Failed => "Failed",
        }
    }
}

/// How many of `total` agent rows fold out of view.
pub(super) fn hidden_agent_rows(total: usize, open: bool) -> usize {
    if open {
        0
    } else {
        total.saturating_sub(VISIBLE_AGENT_ROWS)
    }
}

/// The folded-agents summary. `pending` counts children the store does not hold
/// yet, because they can have no row to fold.
pub(super) fn hidden_agents_summary(hidden: &[AgentStatus], pending: usize) -> String {
    let count = |wanted: AgentStatus| hidden.iter().filter(|status| **status == wanted).count();
    let mut parts = vec![
        format!("{} done", count(AgentStatus::Done)),
        format!("{} running", count(AgentStatus::Running)),
        format!("{pending} pending"),
    ];
    let failed = count(AgentStatus::Failed);
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    let noun = if hidden.len() == 1 { "agent" } else { "agents" };
    format!("… {} more {noun} ({})", hidden.len(), parts.join(" · "))
}

#[component]
pub fn TaskRows(child_ids: Vec<String>, open: bool, on_expand: EventHandler<bool>) -> Element {
    let pump = use_store();
    let _revision = pump.revision()();
    let navigate = use_navigate();
    let resolved: Vec<(String, String, String, AgentStatus)> = {
        let core = pump.core();
        let core = core.borrow();
        child_ids
            .iter()
            .filter_map(|child_id| {
                core.store()
                    .agent_chat
                    .conversations
                    .get(child_id)
                    .map(|child| {
                        (
                            child_id.clone(),
                            child
                                .agent
                                .clone()
                                .unwrap_or_else(|| "Subagent".to_string()),
                            child.title.clone(),
                            AgentStatus::from_run_state(child.run_state),
                        )
                    })
            })
            .collect()
    };
    let pending = child_ids.len().saturating_sub(resolved.len());
    if resolved.is_empty() {
        return rsx! {};
    }
    let hidden_count = hidden_agent_rows(resolved.len(), open);
    let hidden_statuses: Vec<AgentStatus> =
        resolved[..hidden_count].iter().map(|row| row.3).collect();
    let summary = hidden_agents_summary(&hidden_statuses, pending);
    let shown_rows = &resolved[hidden_count..];
    let can_collapse = open && resolved.len() > VISIBLE_AGENT_ROWS;
    rsx! {
        nav { class: "agent-chat__task-children", aria_label: "Subagent conversations",
            List {
                for (child_id, agent, title, status) in shown_rows.iter().cloned() {
                    ListRow {
                        key: "{child_id}",
                        dense: true,
                        class: "agent-chat__task-child",
                        href: crate::routes::agent_href(&child_id),
                        on_navigate: navigate,
                        headline: rsx! { "{agent}: {title}" },
                        support: rsx! {
                            StatusDot { status: status.dot_status().to_string(), title: status.label().to_string() }
                            span { "{status.label()}" }
                        },
                    }
                }
            }
        }
        if hidden_count > 0 {
            p { class: "agent-chat__task-hidden", "{summary}" }
            Button { variant: ButtonVariant::Link, size: ButtonSize::Sm, class: "agent-chat__step-more",
                onclick: move |_| on_expand.call(true), "Show all" }
        }
        if can_collapse {
            Button { variant: ButtonVariant::Link, size: ButtonSize::Sm, class: "agent-chat__step-more",
                onclick: move |_| on_expand.call(false), "Collapse" }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{AgentStatus, hidden_agent_rows, hidden_agents_summary};

    #[test]
    fn only_the_last_four_rows_stay_visible_until_opened() {
        assert_eq!(hidden_agent_rows(6, false), 2);
        assert_eq!(hidden_agent_rows(4, false), 0);
        assert_eq!(hidden_agent_rows(6, true), 0);
    }

    #[test]
    fn the_summary_counts_hidden_agents_by_state() {
        let hidden = [
            AgentStatus::Done,
            AgentStatus::Running,
            AgentStatus::Running,
            AgentStatus::Failed,
        ];
        assert_eq!(
            hidden_agents_summary(&hidden, 1),
            "… 4 more agents (1 done · 2 running · 1 pending · 1 failed)"
        );
    }

    #[test]
    fn one_hidden_agent_is_singular() {
        assert_eq!(
            hidden_agents_summary(&[AgentStatus::Done], 0),
            "… 1 more agent (1 done · 0 running · 0 pending)"
        );
    }
}
