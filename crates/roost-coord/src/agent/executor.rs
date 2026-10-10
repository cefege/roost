//! The harness's `ToolExecutor` over the coordinator's worker tool-call
//! registry: each call becomes a `DAgentToolCall` frame on the worker's live
//! link, and its output and result come back through the registry.

use std::sync::Arc;

use futures::future::BoxFuture;
use roost_agent::{ToolCall, ToolExecutor, ToolOutcome};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::tool_calls::{AgentToolCallSpec, ToolCallRegistry};

#[derive(Debug)]
pub struct WorkerToolExecutor {
    registry: Arc<ToolCallRegistry>,
}

impl WorkerToolExecutor {
    pub fn new(registry: Arc<ToolCallRegistry>) -> Self {
        Self { registry }
    }
}

impl ToolExecutor for WorkerToolExecutor {
    fn execute<'a>(
        &'a self,
        worker_fp: &'a str,
        call: ToolCall,
        out: mpsc::Sender<String>,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ToolOutcome, String>> {
        Box::pin(async move {
            let spec = AgentToolCallSpec {
                call_id: call.call_id,
                conversation_id: call.conversation_id,
                cwd: call.cwd,
                tool: call.tool,
                args_json: call.args_json,
                timeout_ms: call.timeout_ms,
            };
            let deadline = std::time::Duration::from_millis(u64::from(spec.timeout_ms));
            // The deadline cancels rather than drops the call, so the registry
            // still sends the worker its cancel frame and clears the pending entry.
            let call_cancel = cancel.child_token();
            let timer = {
                let call_cancel = call_cancel.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(deadline).await;
                    call_cancel.cancel();
                })
            };
            let result = self
                .registry
                .execute(worker_fp, spec, out, call_cancel.clone())
                .await;
            timer.abort();
            match result {
                Ok(outcome) => Ok(ToolOutcome {
                    is_error: outcome.is_error,
                    content: outcome.content,
                    details_json: outcome.details_json,
                }),
                Err(_) if call_cancel.is_cancelled() && !cancel.is_cancelled() => Err(format!(
                    "tool call timed out after {} s",
                    deadline.as_secs()
                )),
                Err(error) => Err(error),
            }
        })
    }

    fn close_conversation<'a>(
        &'a self,
        worker_fp: &'a str,
        conversation_id: &'a str,
    ) -> BoxFuture<'a, ()> {
        self.registry.close_conversation(worker_fp, conversation_id);
        Box::pin(async {})
    }
}
