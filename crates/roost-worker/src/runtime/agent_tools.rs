//! Worker-side owner for coordinator-dispatched tool calls.
//!
//! Calls are independently cancellable; the live stream is bounded per call,
//! while the final result retains the complete bounded tool output.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use roost_agent_tools::{ToolCallRequest, ToolHost, ToolOutcome};
use roost_proto::{
    DAgentConversationClosed, DAgentToolCall, DAgentToolCancel, WAgentToolOutput, WAgentToolResult,
};
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::link_ports::AgentToolsPort;
use crate::uplink::Uplink;

const LIVE_FRAME_BYTES: usize = 16 * 1024;
const LIVE_BYTES_PER_SECOND: usize = 32 * 1024;
const FINAL_RESULT_BYTES: usize = 4 * 1024 * 1024;
const IDLE_EVICTION_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5 * 60);
const RESULT_TRUNCATION_NOTE: &str = "\n[tool result truncated at 4 MiB]";

/// Owns the shared conversation tool host and the calls currently executing.
pub struct AgentToolsOwner {
    host: Arc<ToolHost>,
    uplink: Uplink,
    calls: Arc<Mutex<HashMap<String, Arc<CancellationToken>>>>,
    idle_eviction: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for AgentToolsOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentToolsOwner")
            .field(
                "active_calls",
                &self
                    .calls
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .len(),
            )
            .finish()
    }
}

impl AgentToolsOwner {
    #[must_use]
    pub fn new(data_dir: PathBuf, uplink: Uplink) -> Self {
        let host = Arc::new(ToolHost::new(data_dir));
        let eviction_host = Arc::clone(&host);
        let idle_eviction = tokio::spawn(async move {
            let mut interval = tokio::time::interval(IDLE_EVICTION_INTERVAL);
            loop {
                interval.tick().await;
                eviction_host.evict_idle().await;
            }
        });
        Self {
            host,
            uplink,
            calls: Arc::new(Mutex::new(HashMap::new())),
            idle_eviction,
        }
    }

    fn start_call(&self, request: DAgentToolCall) {
        tracing::info!(call_id = %request.call_id, tool = %request.tool, "worker agent tool call started");
        let cancel = Arc::new(CancellationToken::new());
        {
            let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(previous) = calls.insert(request.call_id.clone(), Arc::clone(&cancel)) {
                previous.cancel();
            }
        }
        let host = Arc::clone(&self.host);
        let uplink = self.uplink.clone();
        let calls = Arc::clone(&self.calls);
        let call_id = request.call_id.clone();
        tokio::spawn(async move {
            let (output_tx, mut output_rx) = mpsc::channel::<String>(64);
            let tool_call = ToolCallRequest {
                conversation_id: request.conversation_id,
                cwd: PathBuf::from(request.cwd),
                tool: request.tool,
                args_json: request.args_json,
                timeout_ms: request.timeout_ms,
            };
            let call_cancel = cancel.as_ref().clone();
            let mut host_call =
                tokio::spawn(async move { host.call(tool_call, output_tx, call_cancel).await });
            let mut window_start = Instant::now();
            let mut bytes_sent = 0usize;
            let mut pending_output = String::new();
            let mut output_flush = tokio::time::interval(std::time::Duration::from_millis(50));
            let outcome = loop {
                tokio::select! {
                    Some(chunk) = output_rx.recv() => {
                        pending_output.push_str(&chunk);
                        forward_ready_output(
                            &mut pending_output, false, &call_id, &uplink,
                            &mut window_start, &mut bytes_sent,
                        );
                    }
                    _ = output_flush.tick() => {
                        forward_ready_output(
                            &mut pending_output, true, &call_id, &uplink,
                            &mut window_start, &mut bytes_sent,
                        );
                    }
                    result = &mut host_call => {
                        while let Ok(chunk) = output_rx.try_recv() {
                            pending_output.push_str(&chunk);
                        }
                        forward_ready_output(
                            &mut pending_output, true, &call_id, &uplink,
                            &mut window_start, &mut bytes_sent,
                        );
                        break result.unwrap_or_else(|error| ToolOutcome::failure(format!("tool task failed: {error}")));
                    }
                }
            };
            let outcome = bounded_result(outcome);
            tracing::info!(
                call_id,
                is_error = outcome.is_error,
                "worker agent tool call finished"
            );
            uplink.send(CoordWorkerUpstream::AgentToolResult(WAgentToolResult {
                call_id: call_id.clone(),
                is_error: outcome.is_error,
                content: outcome.content,
                details_json: outcome.details_json,
                ..Default::default()
            }));
            let mut calls = calls.lock().unwrap_or_else(PoisonError::into_inner);
            if calls
                .get(&call_id)
                .is_some_and(|active| Arc::ptr_eq(active, &cancel))
            {
                calls.remove(&call_id);
            }
        });
    }

    fn cancel_call(&self, call_id: &str) {
        if let Some(cancel) = self
            .calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(call_id)
        {
            cancel.cancel();
            tracing::info!(call_id, "worker agent tool call cancelled");
        }
    }

    fn close_conversation_state(&self, conversation_id: String) {
        tracing::info!(conversation_id, "worker agent tool conversation closed");
        let host = Arc::clone(&self.host);
        tokio::spawn(async move {
            host.close_conversation(&conversation_id).await;
        });
    }

    fn cancel_all(&self) {
        let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
        let count = calls.len();
        for (_, cancel) in calls.drain() {
            cancel.cancel();
        }
        if count > 0 {
            tracing::info!(
                count,
                "worker agent tool calls cancelled with coordinator link"
            );
        }
    }
}

impl Drop for AgentToolsOwner {
    fn drop(&mut self) {
        self.cancel_all();
        self.idle_eviction.abort();
    }
}

impl AgentToolsPort for AgentToolsOwner {
    fn call(&self, request: DAgentToolCall) {
        self.start_call(request);
    }
    fn cancel(&self, request: DAgentToolCancel) {
        self.cancel_call(&request.call_id);
    }
    fn close_conversation(&self, request: DAgentConversationClosed) {
        self.close_conversation_state(request.conversation_id);
    }
    fn close_all(&self) {
        self.cancel_all();
    }
}

fn bounded_result(mut outcome: ToolOutcome) -> ToolOutcome {
    if outcome.content.len() > FINAL_RESULT_BYTES {
        let mut end = FINAL_RESULT_BYTES - RESULT_TRUNCATION_NOTE.len();
        while !outcome.content.is_char_boundary(end) {
            end -= 1;
        }
        outcome.content.truncate(end);
        outcome.content.push_str(RESULT_TRUNCATION_NOTE);
        outcome.is_error = true;
    }
    outcome
}

fn forward_ready_output(
    buffer: &mut String,
    flush: bool,
    call_id: &str,
    uplink: &Uplink,
    window_start: &mut Instant,
    bytes_sent: &mut usize,
) {
    while buffer.len() >= LIVE_FRAME_BYTES || (flush && !buffer.is_empty()) {
        let mut end = buffer.len().min(LIVE_FRAME_BYTES);
        while !buffer.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            return;
        }
        let chunk: String = buffer.drain(..end).collect();
        if window_start.elapsed().as_secs() >= 1 {
            *window_start = Instant::now();
            *bytes_sent = 0;
        }
        if bytes_sent.saturating_add(chunk.len()) <= LIVE_BYTES_PER_SECOND {
            *bytes_sent += chunk.len();
            uplink.send(CoordWorkerUpstream::AgentToolOutput(WAgentToolOutput {
                call_id: call_id.to_owned(),
                chunk,
                ..Default::default()
            }));
        }
    }
}
