//! One tool round: run the assistant's tool calls (concurrently when every
//! call only reads), stream their live output into the transcript, and record
//! each result. Harness tools run here; worker tools go through `ToolExecutor`.

use futures::future::join_all;
use roost_protocol::wire::agent_chat::{ChatEvent, LspArgs, TOOL_BASH, TOOL_LSP, TranscriptItem};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::projection::tool_item_id;
use crate::records::{ConversationRecord, Entry};
use crate::runtime::{AgentRuntime, RunEnd};
use crate::toolset::{
    LSP_READ_ACTIONS, TOOL_FIND, TOOL_PROPOSE_PLAN, TOOL_TASK, TOOL_YIELD, Toolset, is_read_only,
};
use crate::traits::{ToolCall, ToolOutcome};
use crate::{find, plan_mode, subagents};

/// Live output kept in a running tool card; older output is trimmed.
const LIVE_OUTPUT_CHARS: u64 = 64 * 1024;
const DEFAULT_TIMEOUT_MS: u32 = 120_000;
/// Slack over a bash command's own timeout for process teardown and transfer.
const BASH_GRACE_SECS: u64 = 30;

/// One call's result and whether it ends the run.
pub(crate) struct CallResult {
    pub outcome: ToolOutcome,
    pub end: Option<RunEnd>,
}

impl CallResult {
    pub fn plain(outcome: ToolOutcome) -> Self {
        Self { outcome, end: None }
    }

    pub fn failure(message: impl Into<String>) -> Self {
        Self::plain(ToolOutcome {
            is_error: true,
            content: message.into(),
            details_json: "{}".into(),
        })
    }
}

/// Context every call in a round shares.
pub(crate) struct CallScope<'a> {
    pub runtime: &'a AgentRuntime,
    pub record: &'a ConversationRecord,
    pub toolset: &'a Toolset,
    pub depth: u32,
    pub cancel: &'a CancellationToken,
}

pub(crate) async fn execute_round(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    toolset: &Toolset,
    depth: u32,
    calls: Vec<(String, String, String)>,
    cancel: &CancellationToken,
) -> Result<Option<RunEnd>, AgentError> {
    let running: Vec<ChatEvent> = calls
        .iter()
        .map(|(call_id, name, args)| ChatEvent::Item {
            item: TranscriptItem::Tool {
                id: tool_item_id(call_id),
                call_id: call_id.clone(),
                tool_name: name.clone(),
                args_json: args.clone(),
                output: String::new(),
                is_error: false,
                running: true,
                children: Vec::new(),
            },
        })
        .collect();
    runtime.emit(&record.id, running).await;
    let scope = CallScope {
        runtime,
        record,
        toolset,
        depth,
        cancel,
    };
    let results: Vec<CallResult> = if calls.iter().all(|(_, name, _)| is_read_only(name)) {
        join_all(
            calls
                .iter()
                .map(|(call_id, name, args)| run_call(&scope, call_id, name, args)),
        )
        .await
    } else {
        let mut results = Vec::with_capacity(calls.len());
        for (call_id, name, args) in &calls {
            results.push(run_call(&scope, call_id, name, args).await);
        }
        results
    };
    let mut end = None;
    for ((call_id, name, _), result) in calls.into_iter().zip(results) {
        tracing::debug!(conversation_id = %record.id, call_id, tool = name, is_error = result.outcome.is_error, "agent tool call finished");
        runtime
            .append(
                &record.id,
                Entry::ToolResult {
                    call_id,
                    tool_name: name,
                    text: result.outcome.content,
                    is_error: result.outcome.is_error,
                    details_json: result.outcome.details_json,
                },
            )
            .await?;
        if end.is_none() {
            end = result.end;
        }
    }
    Ok(end)
}

async fn run_call(scope: &CallScope<'_>, call_id: &str, name: &str, args: &str) -> CallResult {
    if !scope.toolset.offers(name) {
        return CallResult::failure(format!("Tool `{name}` is not available here."));
    }
    let (out, out_rx) = mpsc::channel::<String>(64);
    let forwarder = tokio::spawn(forward_output(
        scope.runtime.clone(),
        scope.record.id.clone(),
        tool_item_id(call_id),
        out_rx,
    ));
    let result = match name {
        TOOL_TASK => subagents::run_task(scope, call_id, args, out).await,
        TOOL_FIND => find::run_find(scope, args, out).await,
        TOOL_PROPOSE_PLAN => {
            drop(out);
            plan_mode::propose(scope, args).await
        }
        TOOL_YIELD => {
            drop(out);
            yield_result(args)
        }
        _ => run_worker_tool(scope, call_id, name, args, out).await,
    };
    // Every sender is gone once the call returns, so the forwarder drains and ends.
    let _ = forwarder.await;
    result
}

fn yield_result(args: &str) -> CallResult {
    let result = serde_json::from_str::<serde_json::Value>(args)
        .ok()
        .and_then(|value| {
            value
                .get("result")
                .and_then(|text| text.as_str())
                .map(str::to_owned)
        });
    match result {
        Some(result) => CallResult {
            outcome: ToolOutcome {
                is_error: false,
                content: "Result delivered.".into(),
                details_json: "{}".into(),
            },
            end: Some(RunEnd::Yielded(result)),
        },
        None => CallResult::failure("yield requires a string `result`."),
    }
}

async fn run_worker_tool(
    scope: &CallScope<'_>,
    call_id: &str,
    name: &str,
    args: &str,
    out: mpsc::Sender<String>,
) -> CallResult {
    if name == TOOL_LSP && scope.toolset.lsp_read_only {
        let action = serde_json::from_str::<LspArgs>(args)
            .ok()
            .map(|parsed| {
                serde_json::to_value(parsed.action)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
            })
            .unwrap_or_default();
        if !action
            .as_deref()
            .is_some_and(|action| LSP_READ_ACTIONS.contains(&action))
        {
            return CallResult::failure(format!(
                "Only these lsp actions are available here: {}.",
                LSP_READ_ACTIONS.join(", ")
            ));
        }
    }
    let call = ToolCall {
        call_id: call_id.to_owned(),
        conversation_id: scope.record.id.clone(),
        cwd: scope.record.cwd.clone(),
        tool: name.to_owned(),
        args_json: args.to_owned(),
        timeout_ms: timeout_ms(name, args),
    };
    match scope
        .runtime
        .inner
        .tools
        .execute(
            &scope.record.worker_fp,
            call,
            out,
            scope.cancel.child_token(),
        )
        .await
    {
        Ok(outcome) => CallResult::plain(outcome),
        Err(error) => CallResult::failure(error),
    }
}

/// The coordinator-side deadline for a worker call. bash carries its own
/// timeout in seconds; the deadline adds grace over it.
pub(crate) fn timeout_ms(name: &str, args: &str) -> u32 {
    if name != TOOL_BASH {
        return DEFAULT_TIMEOUT_MS;
    }
    let seconds = serde_json::from_str::<serde_json::Value>(args)
        .ok()
        .and_then(|value| value.get("timeout").and_then(serde_json::Value::as_u64))
        .unwrap_or(120)
        .min(3600);
    u32::try_from((seconds + BASH_GRACE_SECS) * 1000).unwrap_or(u32::MAX)
}

async fn forward_output(
    runtime: AgentRuntime,
    conversation_id: String,
    item_id: String,
    mut chunks: mpsc::Receiver<String>,
) {
    let mut shown: u64 = 0;
    while let Some(chunk) = chunks.recv().await {
        shown += chunk.chars().count() as u64;
        let trim_start = (shown > LIVE_OUTPUT_CHARS).then(|| {
            let trimmed = shown - LIVE_OUTPUT_CHARS / 2;
            shown -= trimmed;
            trimmed
        });
        runtime
            .emit(
                &conversation_id,
                vec![ChatEvent::ToolOutput {
                    item_id: item_id.clone(),
                    trim_start,
                    append: Some(chunk),
                    set: None,
                }],
            )
            .await;
    }
}
