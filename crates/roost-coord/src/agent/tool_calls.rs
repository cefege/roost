//! Worker tool-call routing and completion correlation.
//!
//! The coordinator sends calls only to routable workers that acknowledged the
//! native-tools capability; generation retirement fails every matching waiter.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use roost_protocol::versioning::CAPABILITY_AGENT_TOOLS_V1;
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::coord_core::worker_lifecycle::{LinkEnd, WorkerLifecycleObserver};
use crate::db::CoordDb;

const MAX_TOOL_RESULT_BYTES: usize = 4 * 1024 * 1024;
const RESULT_TRUNCATION_NOTE: &str = "\n[tool result truncated at 4 MiB]";

/// The result returned by a worker's tool host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerToolOutcome {
    pub is_error: bool,
    pub content: String,
    pub details_json: String,
}

/// Immutable tool call fields sent to a worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentToolCallSpec {
    pub call_id: String,
    pub conversation_id: String,
    pub cwd: String,
    pub tool: String,
    pub args_json: String,
    pub timeout_ms: u32,
}

#[derive(Debug)]
struct PendingCall {
    worker_fp: String,
    worker_label: String,
    completion: oneshot::Sender<Result<WorkerToolOutcome, String>>,
    output: mpsc::Sender<String>,
}

/// Pending tool calls and the live worker generation registry they target.
#[derive(Debug)]
pub struct ToolCallRegistry {
    workers: Arc<WorkerRegistry>,
    database: CoordDb,
    pending: Mutex<HashMap<String, PendingCall>>,
}

impl ToolCallRegistry {
    #[must_use]
    pub fn new(workers: Arc<WorkerRegistry>, database: CoordDb) -> Self {
        Self {
            workers,
            database,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Send one tool call and await its worker result.
    pub async fn execute(
        &self,
        worker_fp: &str,
        call: AgentToolCallSpec,
        out: mpsc::Sender<String>,
        cancel: CancellationToken,
    ) -> Result<WorkerToolOutcome, String> {
        let worker = self.workers.current_routable(
            &roost_protocol::wire::WorkerFp::try_from(worker_fp.to_owned())
                .map_err(|_| format!("machine {worker_fp} is offline"))?,
        );
        let label = self
            .worker_label(worker_fp)
            .await
            .unwrap_or_else(|| worker_fp.to_owned());
        let Some(worker) = worker else {
            return Err(format!("machine {label} is offline"));
        };
        if !worker.capabilities.contains(CAPABILITY_AGENT_TOOLS_V1) {
            return Err(format!("machine {label} is offline"));
        }
        let offline_message = format!("machine {label} is offline");
        let (completion, completed) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if pending.contains_key(&call.call_id) {
                return Err("duplicate agent tool call id".to_owned());
            }
            pending.insert(
                call.call_id.clone(),
                PendingCall {
                    worker_fp: worker_fp.to_owned(),
                    worker_label: label.clone(),
                    completion,
                    output: out,
                },
            );
        }
        let sent = worker.send(CoordWorkerDownstream::AgentToolCall(
            roost_proto::DAgentToolCall {
                call_id: call.call_id.clone(),
                conversation_id: call.conversation_id,
                cwd: call.cwd,
                tool: call.tool,
                args_json: call.args_json,
                timeout_ms: call.timeout_ms,
                ..Default::default()
            },
        ));
        if sent == 0 {
            self.remove_pending(&call.call_id);
            return Err(offline_message);
        }
        tokio::select! {
            result = completed => {
                self.remove_pending(&call.call_id);
                result.unwrap_or_else(|_| Err("worker disconnected".to_owned()))
            }
            () = cancel.cancelled() => {
                worker.send(CoordWorkerDownstream::AgentToolCancel(roost_proto::DAgentToolCancel {
                    call_id: call.call_id.clone(),
                    ..Default::default()
                }));
                self.remove_pending(&call.call_id);
                Err("agent tool call cancelled".to_owned())
            }
        }
    }

    /// Release worker-local conversation state after its coordinator conversation ends.
    pub fn close_conversation(&self, worker_fp: &str, conversation_id: &str) {
        let Ok(worker_fp) = roost_protocol::wire::WorkerFp::try_from(worker_fp.to_owned()) else {
            return;
        };
        if let Some(worker) = self.workers.current_routable(&worker_fp)
            && worker.capabilities.contains(CAPABILITY_AGENT_TOOLS_V1)
        {
            worker.send(CoordWorkerDownstream::AgentConversationClosed(
                roost_proto::DAgentConversationClosed {
                    conversation_id: conversation_id.to_owned(),
                    ..Default::default()
                },
            ));
        }
    }

    /// Deliver one output chunk or final result from an authenticated worker frame.
    pub fn receive(&self, worker_fp: &str, frame: CoordWorkerUpstream) -> bool {
        match frame {
            CoordWorkerUpstream::AgentToolOutput(output) => {
                let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
                let Some(call) = pending
                    .get(&output.call_id)
                    .filter(|call| call.worker_fp == worker_fp)
                else {
                    return false;
                };
                let _ = call.output.try_send(output.chunk);
                true
            }
            CoordWorkerUpstream::AgentToolResult(result) => {
                let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
                if !pending
                    .get(&result.call_id)
                    .is_some_and(|call| call.worker_fp == worker_fp)
                {
                    return false;
                }
                let Some(call) = pending.remove(&result.call_id) else {
                    return false;
                };
                drop(pending);
                let (content, truncated) = truncate_utf8(
                    result.content,
                    MAX_TOOL_RESULT_BYTES - RESULT_TRUNCATION_NOTE.len(),
                );
                let outcome = WorkerToolOutcome {
                    is_error: result.is_error || truncated,
                    content: if truncated {
                        format!("{content}{RESULT_TRUNCATION_NOTE}")
                    } else {
                        content
                    },
                    details_json: result.details_json,
                };
                let _ = call.completion.send(Ok(outcome));
                true
            }
            _ => false,
        }
    }

    fn remove_pending(&self, call_id: &str) -> Option<PendingCall> {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(call_id)
    }

    async fn worker_label(&self, worker_fp: &str) -> Option<String> {
        sqlx::query_scalar::<_, String>(
            "SELECT label FROM workers WHERE fp = $1 AND deleted_at_ms IS NULL",
        )
        .bind(worker_fp)
        .fetch_optional(self.database.pool())
        .await
        .ok()
        .flatten()
    }

    fn reject_worker(&self, worker_fp: &str, reason: &str) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let call_ids: Vec<String> = pending
            .iter()
            .filter(|(_, call)| call.worker_fp == worker_fp)
            .map(|(call_id, _)| call_id.clone())
            .collect();
        let rejected = call_ids.len();
        for call_id in call_ids {
            if let Some(call) = pending.remove(&call_id) {
                let _ = call
                    .completion
                    .send(Err(format!("machine {} disconnected", call.worker_label)));
            }
        }
        if rejected > 0 {
            tracing::info!(
                worker_fp,
                count = rejected,
                reason,
                "worker tool calls failed with their link generation"
            );
        }
    }
}

impl WorkerLifecycleObserver for ToolCallRegistry {
    fn acknowledge_capabilities(
        &self,
        advertised: &std::collections::BTreeSet<String>,
    ) -> Vec<&'static str> {
        advertised
            .contains(CAPABILITY_AGENT_TOOLS_V1)
            .then_some(CAPABILITY_AGENT_TOOLS_V1)
            .into_iter()
            .collect()
    }

    fn on_superseded(&self, superseded: &Arc<WorkerHandle>) {
        self.reject_worker(superseded.worker_fp.as_str(), "superseded");
    }

    fn on_closed(&self, handle: &Arc<WorkerHandle>, _end: LinkEnd) {
        self.reject_worker(handle.worker_fp.as_str(), "closed");
    }
}

fn truncate_utf8(text: String, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}
