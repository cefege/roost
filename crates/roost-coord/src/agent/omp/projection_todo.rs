//! Canonical task lists from OMP's detailed todo results and flat ACP plans.
//! The ACP projection calls these adapters before applying the shared event fold.
//! Flat progress must not erase native phase boundaries, blockers, or dropped tasks.

use agent_client_protocol::v1::schema::{Plan, PlanEntryStatus};
use roost_protocol::wire::agent_chat::{
    TodoPhase, TodoStatus, TodoTask, Transcript, TranscriptItem,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct TodoSnapshot {
    phases: Vec<TodoPhase>,
}

pub(super) fn from_tool(item: &TranscriptItem) -> Option<TranscriptItem> {
    let TranscriptItem::Tool {
        tool_name,
        details_json,
        running: false,
        is_error: false,
        ..
    } = item
    else {
        return None;
    };
    if tool_name != "todo" {
        return None;
    }
    let snapshot = serde_json::from_str::<TodoSnapshot>(details_json).ok()?;
    Some(todo_item(snapshot.phases))
}

pub(super) fn from_plan(transcript: &Transcript, plan: Plan) -> Option<TranscriptItem> {
    let previous = transcript.items.iter().rev().find_map(|item| match item {
        TranscriptItem::Todo { phases, .. } => Some(phases),
        _ => None,
    });
    if let Some(phases) = previous.filter(|phases| {
        phases.iter().map(|phase| phase.tasks.len()).sum::<usize>() == plan.entries.len()
    }) {
        let progress_change = phases
            .iter()
            .flat_map(|phase| &phase.tasks)
            .zip(&plan.entries)
            .try_fold(false, |changed, (task, entry)| {
                (task.content == entry.content)
                    .then_some(changed || !status_matches(task.status, &entry.status))
            });
        if let Some(changed) = progress_change {
            if !changed {
                return None;
            }
            let mut phases = phases.clone();
            for (task, entry) in phases
                .iter_mut()
                .flat_map(|phase| &mut phase.tasks)
                .zip(plan.entries)
            {
                if !status_matches(task.status, &entry.status) {
                    task.status = flat_status(&entry.status);
                    task.blocker = None;
                }
            }
            return Some(todo_item(phases));
        }
    }
    let phases = if plan.entries.is_empty() {
        Vec::new()
    } else {
        vec![TodoPhase {
            name: "Plan".into(),
            tasks: plan
                .entries
                .into_iter()
                .map(|entry| TodoTask {
                    content: entry.content,
                    status: flat_status(&entry.status),
                    blocker: None,
                })
                .collect(),
        }]
    };
    Some(todo_item(phases))
}

fn status_matches(current: TodoStatus, incoming: &PlanEntryStatus) -> bool {
    let current = match current {
        TodoStatus::Blocked => TodoStatus::Pending,
        TodoStatus::Abandoned => TodoStatus::Completed,
        status => status,
    };
    current == flat_status(incoming)
}

fn flat_status(status: &PlanEntryStatus) -> TodoStatus {
    match status {
        PlanEntryStatus::InProgress => TodoStatus::InProgress,
        PlanEntryStatus::Completed => TodoStatus::Completed,
        _ => TodoStatus::Pending,
    }
}

fn todo_item(phases: Vec<TodoPhase>) -> TranscriptItem {
    TranscriptItem::Todo {
        id: "todo-omp".into(),
        phases,
    }
}
