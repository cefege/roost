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
                checkpoint_id: call.checkpoint_id,
            };
            self.registry
                .execute(worker_fp, spec, out, cancel)
                .await
                .map(|outcome| ToolOutcome {
                    is_error: outcome.is_error,
                    content: outcome.content,
                    details_json: outcome.details_json,
                })
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
