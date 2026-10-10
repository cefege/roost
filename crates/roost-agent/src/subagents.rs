//! Ported from oh-my-pi packages/coding-agent/src/task/ (MIT), without worktree isolation.
//! The `task` tool: each task becomes a child conversation on the same machine
//! and folder that runs to its `yield`; the parent's result is every child's
//! report. At most 8 children run at once; children of children stop at depth 2.

use std::sync::Arc;

use futures::future::join_all;
use roost_protocol::wire::agent_chat::{AgentRunState, ChatEvent, TranscriptItem};
use serde::Deserialize;
use tokio::sync::{Semaphore, mpsc};

use crate::error::AgentError;
use crate::projection::tool_item_id;
use crate::records::{ConversationRecord, Entry, Mode, Role, model_ref};
use crate::roles;
use crate::runtime::{AgentRuntime, RunEnd, now_ms, random_id};
use crate::tool_round::{CallResult, CallScope};
use crate::toolset::{MAX_DEPTH, SubagentKind};
use crate::traits::ToolOutcome;

const MAX_CONCURRENT_CHILDREN: usize = 8;
const RESULT_BYTES: usize = 50 * 1024;
const TITLE_CHARS: usize = 60;

#[derive(Debug, Deserialize)]
struct TaskArgs {
    #[serde(default)]
    context: String,
    tasks: Vec<TaskSpec>,
}

#[derive(Debug, Deserialize)]
struct TaskSpec {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    agent: Option<String>,
    task: String,
}

pub(crate) async fn run_task(
    scope: &CallScope<'_>,
    call_id: &str,
    args: &str,
    out: mpsc::Sender<String>,
) -> CallResult {
    let parsed: TaskArgs = match serde_json::from_str(args) {
        Ok(parsed) => parsed,
        Err(error) => return CallResult::failure(format!("invalid task arguments: {error}")),
    };
    if parsed.tasks.is_empty() {
        return CallResult::failure("task needs at least one entry in `tasks`.");
    }
    if scope.depth >= MAX_DEPTH {
        return CallResult::failure("Subagents at this depth cannot spawn further subagents.");
    }
    let plan_mode = scope.record.mode == Mode::Plan;
    let mut children = Vec::new();
    for (index, spec) in parsed.tasks.iter().enumerate() {
        let kind = match spec.agent.as_deref() {
            None => SubagentKind::Task,
            Some(name) => match SubagentKind::from_name(name) {
                Some(kind) => kind,
                None => {
                    return CallResult::failure(format!(
                        "unknown agent `{name}`; use scout, task or reviewer"
                    ));
                }
            },
        };
        if plan_mode && kind != SubagentKind::Scout {
            return CallResult::failure("Plan mode allows only `scout` subagents.");
        }
        let name = spec
            .name
            .clone()
            .unwrap_or_else(|| format!("{}{}", kind.as_str(), index + 1));
        match create_child(scope, kind, &name, &spec.task, &parsed.context).await {
            Ok(child) => children.push((name, kind, child)),
            Err(error) => {
                return CallResult::failure(format!("could not start subagent {name}: {error}"));
            }
        }
    }
    let child_ids: Vec<String> = children
        .iter()
        .map(|(_, _, child)| child.id.clone())
        .collect();
    scope
        .runtime
        .emit(
            &scope.record.id,
            vec![ChatEvent::Item {
                item: TranscriptItem::Tool {
                    id: tool_item_id(call_id),
                    call_id: call_id.to_owned(),
                    tool_name: crate::toolset::TOOL_TASK.into(),
                    args_json: args.to_owned(),
                    output: String::new(),
                    is_error: false,
                    running: true,
                    children: child_ids.clone(),
                },
            }],
        )
        .await;
    let permits = Arc::new(Semaphore::new(MAX_CONCURRENT_CHILDREN));
    let reports = join_all(children.iter().map(|(name, _, child)| {
        let permits = Arc::clone(&permits);
        let out = out.clone();
        async move {
            let _permit = permits.acquire().await;
            let _ = out.send(format!("{name}: running\n")).await;
            let end = run_child(scope, &child.id).await;
            let status = match &end {
                Ok(RunEnd::Yielded(_) | RunEnd::Completed) => "done",
                Ok(RunEnd::Aborted) => "aborted",
                Ok(RunEnd::Failed(_)) | Err(_) => "failed",
            };
            let _ = out.send(format!("{name}: {status}\n")).await;
            end
        }
    }))
    .await;
    let mut content = String::new();
    let mut failures = 0;
    for ((name, kind, child), end) in children.iter().zip(reports) {
        let report = match end {
            Ok(RunEnd::Yielded(text)) => text,
            Ok(RunEnd::Completed) => last_text(scope.runtime, &child.id).await,
            Ok(RunEnd::Aborted) => {
                failures += 1;
                "Aborted before finishing.".into()
            }
            Ok(RunEnd::Failed(error)) | Err(AgentError::Model(error)) => {
                failures += 1;
                format!("Failed: {error}")
            }
            Err(error) => {
                failures += 1;
                format!("Failed: {error}")
            }
        };
        content.push_str(&format!(
            "## {name} ({})\n{}\n\n",
            kind.as_str(),
            cap(&report)
        ));
    }
    CallResult::plain(ToolOutcome {
        is_error: failures == children.len(),
        content: content.trim_end().to_owned(),
        details_json: serde_json::json!({ "children": child_ids }).to_string(),
    })
}

async fn create_child(
    scope: &CallScope<'_>,
    kind: SubagentKind,
    name: &str,
    task: &str,
    context: &str,
) -> Result<ConversationRecord, AgentError> {
    let runtime = scope.runtime;
    let settings = runtime.settings().await?;
    let role = match kind {
        SubagentKind::Scout => Some(Role::Smol),
        SubagentKind::Reviewer => Some(Role::Slow),
        SubagentKind::Task => settings
            .model_roles
            .contains_key(&Role::Task)
            .then_some(Role::Task),
    };
    let role_model = match role {
        Some(role) => roles::resolve_role(runtime.inner.llm.as_ref(), &settings, role).await,
        None => None,
    };
    let model = role_model
        .map(|resolved| model_ref(&resolved.info.provider, &resolved.info.id))
        .or_else(|| scope.record.model.clone());
    let now = now_ms();
    let title: String = format!("{name}: {task}")
        .chars()
        .take(TITLE_CHARS)
        .collect();
    let child = ConversationRecord {
        id: random_id("c-"),
        title,
        worker_fp: scope.record.worker_fp.clone(),
        worker_label: scope.record.worker_label.clone(),
        worker_os: scope.record.worker_os.clone(),
        cwd: scope.record.cwd.clone(),
        model,
        thinking_level: scope.record.thinking_level.clone(),
        mode: Mode::Normal,
        pre_plan_model: None,
        parent_id: Some(scope.record.id.clone()),
        agent: Some(kind.as_str().to_owned()),
        advisor: Some(false),
        run_state: AgentRunState::Idle,
        error: None,
        created_ms: now,
        updated_ms: now,
    };
    runtime.inner.store.save_conversation(&child).await?;
    runtime.publish_summary(&child).await;
    runtime.set_child_context(&child.id, context.to_owned());
    runtime
        .append(
            &child.id,
            Entry::User {
                text: task.to_owned(),
            },
        )
        .await?;
    tracing::info!(parent_id = %scope.record.id, child_id = %child.id, agent = kind.as_str(), "subagent created");
    Ok(child)
}

async fn run_child(scope: &CallScope<'_>, child_id: &str) -> Result<RunEnd, AgentError> {
    let Some(done) = scope
        .runtime
        .start_run(child_id, Some(scope.cancel))
        .await?
    else {
        return Err(AgentError::FailedPrecondition(
            "subagent is already running".into(),
        ));
    };
    done.await
        .map_err(|_| AgentError::Model("subagent run ended without a result".into()))
}

async fn last_text(runtime: &AgentRuntime, id: &str) -> String {
    let Ok(entries) = runtime.inner.store.entries(id).await else {
        return String::new();
    };
    entries
        .iter()
        .rev()
        .find_map(|(_, entry)| match entry {
            Entry::Assistant { blocks, .. } => {
                let text: Vec<&str> = blocks
                    .iter()
                    .filter_map(|block| match block {
                        roost_llm::AssistantBlock::Text { text } if !text.trim().is_empty() => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect();
                (!text.is_empty()).then(|| text.join("\n"))
            }
            _ => None,
        })
        .unwrap_or_else(|| "(no report)".into())
}

fn cap(report: &str) -> String {
    if report.len() <= RESULT_BYTES {
        return report.to_owned();
    }
    let mut end = RESULT_BYTES;
    while !report.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… report truncated at 50 KiB", &report[..end])
}
