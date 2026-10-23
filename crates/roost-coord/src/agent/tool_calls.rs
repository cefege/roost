//! Worker tool-call routing and completion correlation.
//!
//! The coordinator sends calls only to routable workers that acknowledged the
//! native-tools capability; generation retirement fails every matching waiter.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use roost_agent::{AgentRuntime, OwnerConversationId, WorkerConversationNamespace};
use roost_protocol::versioning::CAPABILITY_AGENT_TOOLS_V1;
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::job_ledger::JobLedger;
use super::tool_calls_jobs::ActiveToolCall;
use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::db::CoordDb;
mod dispatch_guard;
mod lifecycle;

const MAX_TOOL_RESULT_BYTES: usize = 4 * 1024 * 1024;
const RESULT_TRUNCATION_NOTE: &str = "\n[tool result truncated at 4 MiB]";

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
    /// Worker-local namespace used for per-conversation tool state.
    pub conversation_id: WorkerConversationNamespace,
    /// Persisted conversation whose current policy governs this call.
    pub owner_conversation_id: OwnerConversationId,
    pub cwd: String,
    pub tool: String,
    pub args_json: String,
    pub timeout_ms: u32,
    pub checkpoint_id: u64,
}

#[derive(Debug)]
pub(super) struct PendingCall {
    pub(super) worker_fp: String,
    pub(super) worker_label: String,
    pub(super) conversation_id: String,
    pub(super) tool_name: String,
    completion: oneshot::Sender<Result<WorkerToolOutcome, String>>,
    output: mpsc::Sender<String>,
}

/// Pending tool calls, routable workers, and background jobs.
pub struct ToolCallRegistry {
    pub(super) workers: Arc<WorkerRegistry>,
    database: CoordDb,
    pub(super) pending: Mutex<HashMap<String, PendingCall>>,
    pub(super) waiting_calls: Mutex<HashMap<String, ActiveToolCall>>,
    pub(super) jobs: Mutex<JobLedger>,
    /// Bound after construction because the executor owns this registry.
    pub(super) runtime: OnceLock<AgentRuntime>,
}

impl std::fmt::Debug for ToolCallRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolCallRegistry")
            .field("runtime_bound", &self.runtime.get().is_some())
            .finish_non_exhaustive()
    }
}

impl ToolCallRegistry {
    #[must_use]
    pub fn new(workers: Arc<WorkerRegistry>, database: CoordDb) -> Self {
        Self {
            workers,
            database,
            pending: Mutex::new(HashMap::new()),
            waiting_calls: Mutex::new(HashMap::new()),
            jobs: Mutex::new(JobLedger::default()),
            runtime: OnceLock::new(),
        }
    }

    /// Wait for the worker and dispatch once; link loss never replays a call.
    pub async fn execute(
        &self,
        worker_fp: &str,
        call: AgentToolCallSpec,
        out: mpsc::Sender<String>,
        cancel: CancellationToken,
    ) -> Result<WorkerToolOutcome, String> {
        let job_command = super::tool_calls_jobs::bash_command(&call.tool, &call.args_json);
        let conversation_id = call.conversation_id.0.clone();
        let fingerprint = roost_protocol::wire::WorkerFp::try_from(worker_fp.to_owned())
            .map_err(|_| format!("invalid machine fingerprint {worker_fp}"))?;
        let label = self
            .worker_label(worker_fp)
            .await
            .unwrap_or_else(|| worker_fp.to_owned());
        {
            let mut waiting = self
                .waiting_calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if pending.contains_key(&call.call_id) || waiting.contains_key(&call.call_id) {
                return Err("duplicate agent tool call id".to_owned());
            }
            waiting.insert(
                call.call_id.clone(),
                ActiveToolCall {
                    call_id: call.call_id.clone(),
                    conversation_id: conversation_id.clone(),
                    tool_name: call.tool.clone(),
                    worker_label: label.clone(),
                    state: "queued",
                },
            );
        }
        let Some(worker) = self.await_worker(&fingerprint, &label, &cancel).await else {
            self.remove_waiting(&call.call_id);
            return Ok(cancelled_before_dispatch());
        };
        let policy = dispatch_guard::allows(
            self.runtime.get(),
            &call.owner_conversation_id,
            &call.tool,
            &call.args_json,
        )
        .await;
        if cancel.is_cancelled() {
            self.remove_waiting(&call.call_id);
            return Ok(cancelled_before_dispatch());
        }
        let allowed = match policy {
            Ok(allowed) => allowed,
            Err(error) => {
                self.remove_waiting(&call.call_id);
                return Err(error);
            }
        };
        if !allowed {
            self.remove_waiting(&call.call_id);
            return Ok(WorkerToolOutcome {
                is_error: true,
                content: format!("Tool `{}` is disabled in read-only mode.", call.tool),
                details_json: "{}".to_owned(),
            });
        }
        if !worker.capabilities.contains(CAPABILITY_AGENT_TOOLS_V1) {
            self.remove_waiting(&call.call_id);
            return Err(format!("machine {label} does not support agent tools"));
        }
        let offline_message = format!("machine {label} disconnected before dispatch");
        let deadline = std::time::Duration::from_millis(u64::from(call.timeout_ms));
        let (completion, completed) = oneshot::channel();
        {
            let mut waiting = self
                .waiting_calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if pending.contains_key(&call.call_id) {
                waiting.remove(&call.call_id);
                return Err("duplicate agent tool call id".to_owned());
            }
            pending.insert(
                call.call_id.clone(),
                PendingCall {
                    worker_fp: worker_fp.to_owned(),
                    worker_label: label.clone(),
                    conversation_id: conversation_id.clone(),
                    tool_name: call.tool.clone(),
                    completion,
                    output: out,
                },
            );
            waiting.remove(&call.call_id);
        }
        tracing::debug!(conversation_id = %conversation_id, call_id = %call.call_id, tool = %call.tool, worker_label = %label, "agent tool call dispatched");
        let sent = worker.send(CoordWorkerDownstream::AgentToolCall(
            roost_proto::DAgentToolCall {
                call_id: call.call_id.clone(),
                conversation_id: call.conversation_id.0,
                cwd: call.cwd,
                tool: call.tool,
                args_json: call.args_json,
                timeout_ms: call.timeout_ms,
                checkpoint_id: call.checkpoint_id,
                ..Default::default()
            },
        ));
        if sent == 0 {
            self.remove_pending(&call.call_id);
            return Err(offline_message);
        }
        let (interruption, cancelled) = tokio::select! {
            result = completed => {
                self.remove_pending(&call.call_id);
                let outcome = result.unwrap_or_else(|_| Err("worker disconnected after dispatch; the tool outcome is unknown. Inspect current state before repeating the call.".to_owned()));
                if let Ok(outcome) = &outcome {
                    self.note_job_started(worker_fp, &label, &conversation_id, job_command, &outcome.details_json);
                }
                return outcome;
            }
            () = cancel.cancelled() => (
                "Cancellation requested; the final tool outcome is unknown. Inspect current state before repeating the call.".to_owned(),
                true,
            ),
            () = tokio::time::sleep(deadline) => (
                format!("tool call timed out after {} s; its outcome is unknown", deadline.as_secs()),
                false,
            ),
        };
        worker.send(CoordWorkerDownstream::AgentToolCancel(
            roost_proto::DAgentToolCancel {
                call_id: call.call_id.clone(),
                ..Default::default()
            },
        ));
        self.remove_pending(&call.call_id);
        if cancelled {
            tracing::info!(worker_fp, worker_label = %label, call_id = %call.call_id, "agent tool cancellation requested; final outcome unknown");
        } else {
            tracing::warn!(worker_fp, worker_label = %label, call_id = %call.call_id, "agent tool deadline elapsed; final outcome unknown");
        }
        Ok(WorkerToolOutcome {
            is_error: !cancelled,
            content: interruption,
            details_json: serde_json::json!({
                "cancelled": cancelled,
                "timed_out": !cancelled,
                "outcome_unknown": true,
            })
            .to_string(),
        })
    }

    async fn await_worker(
        &self,
        fingerprint: &roost_protocol::wire::WorkerFp,
        label: &str,
        cancel: &CancellationToken,
    ) -> Option<Arc<WorkerHandle>> {
        let mut waiting = false;
        loop {
            if cancel.is_cancelled() {
                return None;
            }
            if let Some(worker) = self.workers.current_routable(fingerprint) {
                if waiting {
                    tracing::info!(worker_fp = %fingerprint, worker_label = label, "agent tool resumed after worker reconnect");
                }
                return Some(worker);
            }
            if !waiting {
                tracing::warn!(worker_fp = %fingerprint, worker_label = label, "agent tool waiting for offline worker");
                waiting = true;
            }
            tokio::select! {
                () = cancel.cancelled() => return None,
                () = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
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

    /// Deliver one output chunk, final result, or settled background job from an
    /// authenticated worker frame.
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
                let (content, truncated) = lifecycle::truncate_utf8(
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
            CoordWorkerUpstream::AgentJobResult(result) => {
                self.receive_job_result(worker_fp, result)
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

    fn remove_waiting(&self, call_id: &str) {
        self.waiting_calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(call_id);
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
        self.lose_worker_jobs(worker_fp);
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
                    .send(Err(format!("machine {} disconnected after dispatch; the tool outcome is unknown. Inspect current state before repeating the call.", call.worker_label)));
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

fn cancelled_before_dispatch() -> WorkerToolOutcome {
    WorkerToolOutcome {
        is_error: false,
        content: "Cancelled before dispatch.".to_owned(),
        details_json: r#"{"cancelled":true}"#.to_owned(),
    }
}
