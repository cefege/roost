//! Nested tool calls (`ctx.executeTool()`), ported from
//! `packages/coding-agent/src/core/nested-tool-calls.ts` @ abe508e1b
//! (V16-06 FR-E).
//!
//! A tool may run other tools through the extension API (`ctx.executeTool`);
//! the agent loop does not know about those calls. The session runs each one
//! through the agent's tool pipeline with its own hooks, emits
//! `tool_execution_*` events carrying `parentToolCallId`, and records the
//! calls and their usage on the model-issued call's tool result message.
//!
//! Nothing here runs until a tool calls `ctx.executeTool()`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rpi_ai::types::{NestedToolCallRecord, NestedToolCalls, Usage};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::agent_loop::AgentToolCallOutcome;
use crate::error::AgentError;
use crate::types::{
    AgentTool, AgentToolCall, AgentToolResult, AgentToolUpdateCallback, ToolExecutionMode,
};

/// Limits of the nested-call record on a tool result: arguments over the
/// per-call or total size are omitted, calls beyond the count are dropped,
/// and the record is marked incomplete when any of that happens
/// (`NESTED_CALL_LIMITS`, nested-tool-calls.ts:26-31).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NestedCallLimits {
    pub max_calls: usize,
    pub max_argument_bytes_per_call: usize,
    pub max_argument_bytes_total: usize,
    pub max_error_chars: usize,
}

pub const NESTED_CALL_LIMITS: NestedCallLimits = NestedCallLimits {
    max_calls: 256,
    max_argument_bytes_per_call: 8 * 1024,
    max_argument_bytes_total: 32 * 1024,
    max_error_chars: 500,
};

/// What the nested calls of one model-issued tool call leave on its tool
/// result message (`NestedCallSummary`, nested-tool-calls.ts:36-42).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NestedCallSummary {
    /// Becomes `nestedCalls`. `None` when no nested call was made.
    pub calls: Option<NestedToolCalls>,
    /// Summed `usage` of the nested results, added to the message's `usage`.
    pub usage: Option<Usage>,
}

/// `tool_execution_*` events of nested calls (nested-tool-calls.ts:111-128).
#[derive(Debug, Clone)]
pub enum NestedToolExecutionEvent {
    Start {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        parent_tool_call_id: String,
    },
    Update {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        partial_result: AgentToolResult,
        parent_tool_call_id: String,
    },
    End {
        tool_call_id: String,
        tool_name: String,
        result: AgentToolResult,
        is_error: bool,
        /// `outcome.durationMs` (nested-tool-calls.ts:246): absent when the
        /// nested tool did not run.
        duration_ms: Option<u64>,
        parent_tool_call_id: String,
    },
}

/// The session-side backing of a [`NestedCallRunner`]
/// (`NestedToolCallHost`, nested-tool-calls.ts:130-143).
#[async_trait::async_trait]
pub trait NestedToolCallHost: Send + Sync {
    /// Tools nested calls resolve against.
    fn get_tools(&self) -> Vec<Arc<dyn AgentTool>>;
    /// Whether every nested call runs exclusively, as when the agent
    /// executes tool calls sequentially.
    fn is_sequential(&self) -> bool;
    /// Run the call through the tool pipeline, with hooks that report
    /// `parent_tool_call_id`.
    async fn run_tool_call(
        &self,
        tool_call: AgentToolCall,
        parent_tool_call_id: String,
        signal: Option<CancellationToken>,
        on_update: AgentToolUpdateCallback,
    ) -> Result<AgentToolCallOutcome, AgentError>;
    /// Emit one nested `tool_execution_*` event.
    async fn emit(&self, event: NestedToolExecutionEvent);
}

/// `NestedCallRecorder` (nested-tool-calls.ts:47-101): collects the nested
/// calls of one model-issued tool call, including calls made by nested
/// tools.
#[derive(Debug)]
pub struct NestedCallRecorder {
    calls: Vec<NestedToolCallRecord>,
    started_at: HashMap<usize, Instant>,
    complete: bool,
    argument_bytes: usize,
    /// Summed usage of every nested result, including calls dropped from the
    /// record.
    usage: Option<Usage>,
}

impl Default for NestedCallRecorder {
    fn default() -> Self {
        Self {
            calls: Vec::new(),
            started_at: HashMap::new(),
            complete: true,
            argument_bytes: 0,
            usage: None,
        }
    }
}

impl NestedCallRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a call as it starts. Returns `None` when the call is dropped
    /// over the count limit.
    pub fn start(&mut self, id: &str, name: &str, arguments: &Map<String, Value>) -> Option<usize> {
        if self.calls.len() >= NESTED_CALL_LIMITS.max_calls {
            self.complete = false;
            return None;
        }
        let mut record = NestedToolCallRecord {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: None,
            arguments_bytes: None,
            status: "unfinished".to_owned(),
            duration_ms: None,
            error: None,
        };
        let json = serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_owned());
        let bytes = json.len();
        if bytes > NESTED_CALL_LIMITS.max_argument_bytes_per_call
            || self.argument_bytes + bytes > NESTED_CALL_LIMITS.max_argument_bytes_total
        {
            record.arguments_bytes = Some(bytes as u64);
            self.complete = false;
        } else {
            record.arguments = Some(Value::Object(arguments.clone()));
            self.argument_bytes += bytes;
        }
        let index = self.calls.len();
        self.calls.push(record);
        self.started_at.insert(index, Instant::now());
        Some(index)
    }

    /// Mark the call finished (`finish`, nested-tool-calls.ts:79-85).
    pub fn finish(&mut self, index: Option<usize>, is_error: bool, error_text: &str) {
        let Some(index) = index else {
            return;
        };
        let Some(record) = self.calls.get_mut(index) else {
            return;
        };
        record.status = if is_error { "error" } else { "ok" }.to_owned();
        record.duration_ms = Some(
            self.started_at
                .remove(&index)
                .map(|started| started.elapsed().as_millis() as u64)
                .unwrap_or(0),
        );
        if is_error && !error_text.is_empty() {
            record.error = Some(
                error_text
                    .chars()
                    .take(NESTED_CALL_LIMITS.max_error_chars)
                    .collect(),
            );
        }
    }

    /// Add one nested result's usage to the running total.
    pub fn add_usage(&mut self, usage: Usage) {
        self.usage = Some(match self.usage.take() {
            Some(existing) => existing.combined(&usage),
            None => usage,
        });
    }

    /// Summed usage of every nested result.
    pub fn total_usage(&self) -> Option<Usage> {
        self.usage.clone()
    }

    /// Copy of the record so far, or `None` when no nested call was made
    /// (`snapshot`, nested-tool-calls.ts:96-100).
    pub fn snapshot(&self) -> Option<NestedToolCalls> {
        if self.calls.is_empty() && self.complete {
            return None;
        }
        let calls = self.calls.clone();
        let complete = self.complete
            && calls
                .iter()
                .all(|call| call.status.as_str() != "unfinished");
        Some(NestedToolCalls { calls, complete })
    }
}

#[derive(Default)]
struct ScopeState {
    recorder: Arc<Mutex<NestedCallRecorder>>,
    next_id: u64,
    /// Set inside a call that holds the exclusive queue, so its own nested
    /// calls do not wait on it.
    holds_queue: bool,
}

/// `NestedToolCallOptions` (nested-tool-calls.ts:103-108).
#[derive(Default)]
pub struct NestedToolCallOptions {
    /// Defaults to the calling tool's signal.
    pub signal: Option<CancellationToken>,
    /// Receives partial results of the nested tool, in addition to
    /// `tool_execution_update` events.
    pub on_update: Option<AgentToolUpdateCallback>,
}

/// `NestedToolCallRunner` (nested-tool-calls.ts:160-261): runs nested calls
/// on behalf of a model-issued call, serializing exclusive ones.
pub struct NestedCallRunner {
    host: Arc<dyn NestedToolCallHost>,
    scopes: Mutex<HashMap<String, ScopeState>>,
    /// `queueTail` upstream: exclusive nested calls run one at a time.
    exclusive: Arc<tokio::sync::Semaphore>,
}

impl NestedCallRunner {
    pub fn new(host: Arc<dyn NestedToolCallHost>) -> Self {
        Self {
            host,
            scopes: Mutex::new(HashMap::new()),
            exclusive: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    /// Run `name` on behalf of the call `caller_id`. The nested call gets the
    /// id `<caller_id>/<n>`. Never rejects for tool failures: they come back
    /// as `is_error: true`.
    pub async fn execute(
        &self,
        caller_id: &str,
        name: &str,
        args: Value,
        options: NestedToolCallOptions,
    ) -> Result<AgentToolCallOutcome, AgentError> {
        let arguments = args.as_object().cloned().unwrap_or_default();
        let (recorder, tool_call_id, holds_queue, exclusive) = {
            let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
            let scope = scopes
                .entry(caller_id.to_owned())
                .or_insert_with(|| ScopeState {
                    next_id: 1,
                    ..ScopeState::default()
                });
            let tool_call_id = format!("{caller_id}/{}", scope.next_id);
            scope.next_id += 1;
            let execution_mode = self
                .host
                .get_tools()
                .iter()
                .find(|tool| tool.name() == name)
                .and_then(|tool| tool.execution_mode());
            let exclusive = !scope.holds_queue
                && (self.host.is_sequential()
                    || execution_mode == Some(ToolExecutionMode::Sequential));
            (
                scope.recorder.clone(),
                tool_call_id,
                scope.holds_queue || exclusive,
                exclusive,
            )
        };
        let record = recorder.lock().unwrap_or_else(|e| e.into_inner()).start(
            &tool_call_id,
            name,
            &arguments,
        );
        let tool_call = AgentToolCall {
            id: tool_call_id.clone(),
            name: name.to_owned(),
            arguments,
            thought_signature: None,
            namespace: None,
        };
        self.host
            .emit(NestedToolExecutionEvent::Start {
                tool_call_id: tool_call.id.clone(),
                tool_name: name.to_owned(),
                args: Value::Object(tool_call.arguments.clone()),
                parent_tool_call_id: caller_id.to_owned(),
            })
            .await;

        // `queueTail` upstream: exclusive nested calls run one at a time.
        // `acquire_owned` only fails when the semaphore is closed, which
        // never happens (the runner owns it): degrade to no permit rather
        // than unwrap.
        let permit = if exclusive {
            self.exclusive.clone().acquire_owned().await.ok()
        } else {
            None
        };
        // Calls below this one share its recorder.
        {
            let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
            scopes.insert(
                tool_call.id.clone(),
                ScopeState {
                    recorder: recorder.clone(),
                    next_id: 1,
                    holds_queue,
                },
            );
        }

        // The pipeline sink forwards user updates and emits
        // `tool_execution_update`; pending emits settle after execution.
        let pending_updates: Arc<Mutex<Vec<futures::future::BoxFuture<'static, ()>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let on_update: AgentToolUpdateCallback = {
            let user = options.on_update;
            let host = self.host.clone();
            let args = Value::Object(tool_call.arguments.clone());
            let tool_call_id = tool_call.id.clone();
            let tool_name = name.to_owned();
            let parent = caller_id.to_owned();
            let pending = pending_updates.clone();
            Box::new(move |partial_result: AgentToolResult| {
                if let Some(user) = &user {
                    user(partial_result.clone());
                }
                // Own the host Arc inside the future so it stays `'static`.
                let emit_host = host.clone();
                let event = NestedToolExecutionEvent::Update {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    args: args.clone(),
                    partial_result,
                    parent_tool_call_id: parent.clone(),
                };
                let mut update: futures::future::BoxFuture<'static, ()> =
                    Box::pin(async move { emit_host.emit(event).await });
                // Poll once: sinks whose first step enqueues synchronously
                // complete here; genuinely async sinks park and settle after
                // execution (same rule as the loop's `on_update` sink).
                let waker = futures::task::noop_waker();
                let mut cx = std::task::Context::from_waker(&waker);
                if update.as_mut().poll(&mut cx).is_pending() {
                    let mut pending = pending.lock().unwrap_or_else(|e| e.into_inner());
                    pending.push(update);
                }
            })
        };

        let outcome = self
            .host
            .run_tool_call(
                tool_call.clone(),
                caller_id.to_owned(),
                options.signal,
                on_update,
            )
            .await;

        // Await already-queued update events (upstream `Promise.all`, awaited
        // sequentially to keep event order deterministic).
        let queued =
            std::mem::take(&mut *pending_updates.lock().unwrap_or_else(|e| e.into_inner()));
        for update in queued {
            update.await;
        }
        {
            let mut scopes = self.scopes.lock().unwrap_or_else(|e| e.into_inner());
            scopes.remove(&tool_call.id);
        }
        drop(permit);

        // Never rejects for tool failures: the outcome carries them.
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => AgentToolCallOutcome {
                tool_call: tool_call.clone(),
                result: error_tool_result(error.to_string()),
                is_error: true,
                duration_ms: None,
            },
        };

        {
            let mut recorder = recorder.lock().unwrap_or_else(|e| e.into_inner());
            recorder.finish(record, outcome.is_error, &text_of(&outcome.result));
            // Nested results are not persisted, so their usage is only
            // counted through the recorder.
            if let Some(usage) = outcome.result.usage.clone() {
                recorder.add_usage(usage);
            }
        }
        self.host
            .emit(NestedToolExecutionEvent::End {
                tool_call_id: tool_call.id,
                tool_name: name.to_owned(),
                result: outcome.result.clone(),
                is_error: outcome.is_error,
                duration_ms: outcome.duration_ms,
                parent_tool_call_id: caller_id.to_owned(),
            })
            .await;
        Ok(outcome)
    }

    /// Remove and return the record of the nested calls a model-issued call
    /// made (`takeRecord`, nested-tool-calls.ts:251-256).
    pub fn take_record(&self, tool_call_id: &str) -> Option<NestedCallSummary> {
        let scope = self
            .scopes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(tool_call_id)?;
        let recorder = scope.recorder.lock().unwrap_or_else(|e| e.into_inner());
        Some(NestedCallSummary {
            calls: recorder.snapshot(),
            usage: recorder.total_usage(),
        })
    }

    /// Drop every scope (`clear`, nested-tool-calls.ts:258-260).
    pub fn clear(&self) {
        self.scopes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
}

fn error_tool_result(message: String) -> AgentToolResult {
    AgentToolResult {
        content: vec![rpi_ai::types::ToolResultContent::Text(
            rpi_ai::types::TextContent {
                text: message,
                text_signature: None,
            },
        )],
        details: serde_json::json!({}),
        ..Default::default()
    }
}

/// `textOf` (nested-tool-calls.ts:153-158): join the text blocks of a result.
fn text_of(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| match block {
            rpi_ai::types::ToolResultContent::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
