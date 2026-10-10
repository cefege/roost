//! The project instruction files of a conversation's folder, fetched from its
//! worker through the hidden `context_files` tool and cached for 60 s per
//! machine and folder. `context` feeds the system prompt, `watchdog` the advisor.

use std::time::Duration;

use roost_protocol::wire::agent_chat::{ContextFiles, TOOL_CONTEXT_FILES};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::records::ConversationRecord;
use crate::runtime::{AgentRuntime, random_id};
use crate::traits::ToolCall;

const CACHE_AGE: Duration = Duration::from_secs(60);
const FETCH_TIMEOUT_MS: u32 = 15_000;

/// The folder's context files; empty when the worker cannot be reached.
pub(crate) async fn context_files(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    cancel: &CancellationToken,
) -> ContextFiles {
    let key = format!("{}\n{}", record.worker_fp, record.cwd);
    if let Some(cached) = runtime.cached_context(&key, CACHE_AGE) {
        return cached;
    }
    let (out, _discarded) = mpsc::channel(4);
    let call = ToolCall {
        call_id: random_id("ctx-"),
        conversation_id: record.id.clone(),
        cwd: record.cwd.clone(),
        tool: TOOL_CONTEXT_FILES.to_owned(),
        args_json: "{}".to_owned(),
        timeout_ms: FETCH_TIMEOUT_MS,
    };
    let fetched = runtime
        .inner
        .tools
        .execute(&record.worker_fp, call, out, cancel.child_token())
        .await;
    let files = match fetched {
        Ok(outcome) if !outcome.is_error => serde_json::from_str::<ContextFiles>(&outcome.content)
            .unwrap_or_else(|error| {
                tracing::warn!(conversation_id = %record.id, %error, "context_files returned malformed JSON");
                empty()
            }),
        Ok(outcome) => {
            tracing::warn!(conversation_id = %record.id, error = %outcome.content, "context_files failed");
            empty()
        }
        Err(error) => {
            tracing::warn!(conversation_id = %record.id, %error, "context_files unreachable");
            return empty();
        }
    };
    runtime.store_context(&key, files.clone());
    files
}

fn empty() -> ContextFiles {
    ContextFiles {
        context: String::new(),
        watchdog: String::new(),
    }
}
