//! Ported from oh-my-pi packages/coding-agent/src/tools/todo.ts (MIT).
//! Applies structured task-list operations to the latest journal snapshot and
//! renders the committed state as the tool result.

use serde::Deserialize;
use serde_json::Value;

use crate::journal;
use crate::records::Entry;
use crate::tool_round::{CallResult, CallScope};
use crate::traits::ToolOutcome;
use roost_protocol::wire::agent_chat::{TodoPhase, TodoStatus, TodoTask};

#[derive(Debug, Deserialize)]
struct TodoArgs {
    op: String,
    #[serde(default)]
    list: Vec<PhaseInput>,
    task: Option<String>,
    phase: Option<String>,
    #[serde(default)]
    items: Vec<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PhaseInput {
    phase: String,
    items: Vec<String>,
}

pub(crate) async fn run(scope: &CallScope<'_>, args: &str) -> CallResult {
    let entries = match scope.runtime.inner.store.entries(&scope.record.id).await {
        Ok(entries) => entries,
        Err(error) => return CallResult::failure(error.to_string()),
    };
    let effective = journal::effective_entries(&entries);
    let current = effective
        .iter()
        .rev()
        .find_map(|(_, entry)| match entry {
            Entry::Todo { phases } => Some(phases.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let parsed: TodoArgs = match serde_json::from_str::<Value>(args).ok().and_then(|value| {
        if value.get("op").is_none() {
            let inferred = if value.get("list").is_some()
                || current.is_empty() && value.get("items").is_some()
            {
                "init"
            } else if value.get("phase").is_some() && value.get("items").is_some() {
                "append"
            } else {
                return None;
            };
            serde_json::from_value({
                let mut repaired = value;
                repaired["op"] = Value::String(inferred.into());
                repaired
            })
            .ok()
        } else {
            serde_json::from_value(value).ok()
        }
    }) {
        Some(parsed) => parsed,
        None => return CallResult::failure("Invalid todo arguments."),
    };
    let operation = parsed.op.clone();
    let mut phases = current;
    let mut errors = Vec::new();
    let result = apply(&mut phases, parsed, &mut errors);
    if !errors.is_empty() {
        return CallResult {
            outcome: ToolOutcome {
                is_error: true,
                content: format!("Errors: {}\n{}", errors.join("; "), render(&phases)),
                details_json: "{}".into(),
            },
            end: None,
        };
    }
    if result {
        normalize(&mut phases);
        if let Err(error) = scope
            .runtime
            .append(
                &scope.record.id,
                Entry::Todo {
                    phases: phases.clone(),
                },
            )
            .await
        {
            return CallResult::failure(error.to_string());
        }
        tracing::info!(conversation_id = %scope.record.id, operation, task_count = phases.iter().map(|phase| phase.tasks.len()).sum::<usize>(), "agent todo list updated");
    }
    CallResult::plain(ToolOutcome {
        is_error: false,
        content: render(&phases),
        details_json: "{}".into(),
    })
}

fn apply(phases: &mut Vec<TodoPhase>, args: TodoArgs, errors: &mut Vec<String>) -> bool {
    match args.op.as_str() {
        "init" => {
            let inputs = if !args.list.is_empty() {
                args.list
            } else if !args.items.is_empty() {
                vec![PhaseInput {
                    phase: args.phase.unwrap_or_else(|| "Tasks".into()),
                    items: args.items,
                }]
            } else {
                errors.push("Missing list for init operation".into());
                return false;
            };
            let mut names = std::collections::HashSet::new();
            let mut tasks = std::collections::HashSet::new();
            for phase in &inputs {
                if phase.phase.trim().is_empty() {
                    errors.push("Phase names cannot be empty".into());
                }
                if phase.items.is_empty() {
                    errors.push(format!("Phase {:?} has no tasks", phase.phase));
                }
                if !names.insert(phase.phase.as_str()) {
                    errors.push(format!("Duplicate phase {}", phase.phase));
                }
                for task in &phase.items {
                    if !tasks.insert(task.as_str()) {
                        errors.push(format!("Duplicate task {task}"));
                    }
                }
            }
            if !errors.is_empty() {
                return false;
            }
            *phases = inputs
                .into_iter()
                .map(|phase| TodoPhase {
                    name: phase.phase,
                    tasks: phase
                        .items
                        .into_iter()
                        .map(|content| TodoTask {
                            content,
                            status: TodoStatus::Pending,
                            blocker: None,
                        })
                        .collect(),
                })
                .collect();
            true
        }
        "view" => false,
        "append" => {
            let Some(name) = args.phase.filter(|name| !name.trim().is_empty()) else {
                errors.push("Missing phase name for append operation".into());
                return false;
            };
            if args.items.is_empty() {
                errors.push("Missing items for append operation".into());
                return false;
            }
            let mut seen = std::collections::HashSet::new();
            for content in &args.items {
                if !seen.insert(content.as_str())
                    || phases
                        .iter()
                        .flat_map(|phase| &phase.tasks)
                        .any(|task| task.content == *content)
                {
                    errors.push(format!("Task {content:?} already exists"));
                }
            }
            if !errors.is_empty() {
                return false;
            }
            let index = phases
                .iter()
                .position(|phase| phase.name == name)
                .unwrap_or_else(|| {
                    phases.push(TodoPhase {
                        name: name.clone(),
                        tasks: Vec::new(),
                    });
                    phases.len() - 1
                });
            phases[index]
                .tasks
                .extend(args.items.into_iter().map(|content| TodoTask {
                    content,
                    status: TodoStatus::Pending,
                    blocker: None,
                }));
            true
        }
        "rm" => {
            if let Some(task) = args.task {
                let Some((phase, index)) = find_task(phases, &task) else {
                    errors.push(format!("Task {task:?} not found"));
                    return false;
                };
                phases[phase].tasks.remove(index);
            } else if let Some(name) = args.phase {
                let Some(phase) = phases.iter_mut().find(|phase| phase.name == name) else {
                    errors.push(format!("Phase {name:?} not found"));
                    return false;
                };
                phase.tasks.clear();
            } else {
                for phase in phases {
                    phase.tasks.clear();
                }
            }
            true
        }
        "start" | "done" | "drop" | "block" | "unblock" => {
            let indices = if let Some(task) = args.task.as_deref() {
                match find_task(phases, task) {
                    Some(index) => vec![index],
                    None => {
                        errors.push(format!("Task {task:?} not found"));
                        return false;
                    }
                }
            } else if let Some(name) = args.phase.as_deref() {
                match phases.iter().position(|phase| phase.name == name) {
                    Some(index) => (0..phases[index].tasks.len())
                        .map(|task| (index, task))
                        .collect(),
                    None => {
                        errors.push(format!("Phase {name:?} not found"));
                        return false;
                    }
                }
            } else if args.op == "start" || args.op == "block" || args.op == "unblock" {
                errors.push(format!("{} requires a task or phase target", args.op));
                return false;
            } else {
                phases
                    .iter()
                    .enumerate()
                    .flat_map(|(phase, row)| (0..row.tasks.len()).map(move |task| (phase, task)))
                    .collect()
            };
            if args.op == "start" && indices.len() == 1 {
                for phase in phases.iter_mut() {
                    for task in &mut phase.tasks {
                        if task.status == TodoStatus::InProgress {
                            task.status = TodoStatus::Pending;
                        }
                    }
                }
            }
            for (phase, task) in indices {
                let target = &mut phases[phase].tasks[task];
                match args.op.as_str() {
                    "start" => target.status = TodoStatus::InProgress,
                    "done" => target.status = TodoStatus::Completed,
                    "drop" => target.status = TodoStatus::Abandoned,
                    "block"
                        if matches!(
                            target.status,
                            TodoStatus::Pending | TodoStatus::InProgress | TodoStatus::Blocked
                        ) =>
                    {
                        target.status = TodoStatus::Blocked;
                        target.blocker = args
                            .reason
                            .as_deref()
                            .map(|reason| reason.split_whitespace().collect::<Vec<_>>().join(" "))
                            .filter(|reason| !reason.is_empty());
                    }
                    "unblock" if target.status == TodoStatus::Blocked => {
                        target.status = TodoStatus::Pending;
                        target.blocker = None;
                    }
                    _ => {}
                }
            }
            true
        }
        other => {
            errors.push(format!("Unknown todo operation {other:?}"));
            false
        }
    }
}

fn find_task(phases: &[TodoPhase], content: &str) -> Option<(usize, usize)> {
    phases.iter().enumerate().find_map(|(phase_idx, phase)| {
        phase
            .tasks
            .iter()
            .position(|task| task.content == content)
            .map(|task_idx| (phase_idx, task_idx))
    })
}

fn normalize(phases: &mut [TodoPhase]) {
    let mut found = false;
    for task in phases.iter_mut().flat_map(|phase| &mut phase.tasks) {
        if task.status == TodoStatus::InProgress {
            if found {
                task.status = TodoStatus::Pending;
            } else {
                found = true;
            }
        }
    }
    if !found
        && let Some(task) = phases
            .iter_mut()
            .flat_map(|phase| &mut phase.tasks)
            .find(|task| task.status == TodoStatus::Pending)
    {
        task.status = TodoStatus::InProgress;
    }
}

fn render(phases: &[TodoPhase]) -> String {
    let count = phases.iter().map(|phase| phase.tasks.len()).sum::<usize>();
    if count == 0 {
        return "Todo list is empty.".into();
    }
    let done = phases
        .iter()
        .flat_map(|phase| &phase.tasks)
        .filter(|task| matches!(task.status, TodoStatus::Completed | TodoStatus::Abandoned))
        .count();
    let open = phases
        .iter()
        .flat_map(|phase| &phase.tasks)
        .filter(|task| {
            matches!(
                task.status,
                TodoStatus::Pending | TodoStatus::InProgress | TodoStatus::Blocked
            )
        })
        .count();
    let mut lines = vec![format!("Overall: {done}/{count} done, {open} open.")];
    for phase in phases {
        lines.push(format!("  {}:", phase.name));
        for task in &phase.tasks {
            let marker = if task.status == TodoStatus::Completed {
                "[X]"
            } else {
                "[ ]"
            };
            let status = match task.status {
                TodoStatus::InProgress => " (in progress)".into(),
                TodoStatus::Abandoned => " (dropped)".into(),
                TodoStatus::Blocked => format!(
                    " (blocked{})",
                    task.blocker
                        .as_ref()
                        .map(|reason| format!(": {reason}"))
                        .unwrap_or_default()
                ),
                _ => String::new(),
            };
            lines.push(format!("    - {marker} {}{status}", task.content));
        }
    }
    lines.join("\n")
}
