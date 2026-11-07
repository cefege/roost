//! Port of `packages/agent/src/agent-loop.ts` @ pi 0.82.1 (commit 2efa728),
//! plus the loop-facing hook types of `packages/agent/src/types.ts`.
//!
//! The low-level loop is *observational*: events are pushed into an
//! [`AgentEventStream`] (or an injected [`AgentEventSink`]) without waiting for
//! consumers. The listener barrier lives in the `Agent` layer (`agent.rs`).
//!
//! Intentional differences from upstream (all structural, semantics kept):
//! - `AbortSignal | undefined` becomes `Option<CancellationToken>`; hooks
//!   always receive a token (a never-cancelled one when the caller passed
//!   `None`).
//! - `AgentLoopConfig extends SimpleStreamOptions` flattens into explicit
//!   fields: `reasoning`/`thinking_budgets` plus a base `stream_options`
//!   (`StreamOptions`) that is cloned per LLM call with the resolved
//!   `api_key`/`signal` overwritten (upstream `{...config, apiKey, signal}`).
//!   `reasoning`/`thinking_budgets` ride the config for `prepareNextTurn`
//!   semantics but are not forwarded through the pinned `StreamFn` shape
//!   (design doc §4.4 pins `StreamOptions`); the assembly layer binds them.
//! - Parallel tool execution uses a `tokio::task::JoinSet` (true concurrency)
//!   where upstream relies on the JS event loop; `tool_execution_end` is still
//!   emitted in completion order and tool-result artifacts in source order.
//! - A stream that ends without a terminal `done`/`error` event violates the
//!   `StreamFn` contract; upstream would hang on `response.result()`. Here a
//!   synthetic `error` assistant message is produced instead (logged).

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};

use futures::Stream;
use futures::future::{BoxFuture, Shared};
use futures::prelude::*;
use rpi_ai::types::{
    AssistantContent, AssistantMessage, AssistantRole, Context, Message, Model, ModelThinkingLevel,
    StopReason, StreamEvent, StreamOptions, TextContent, ThinkingBudgets, ThinkingLevel, Tool,
    ToolResultContent, ToolResultMessage, ToolResultRole, Usage,
};
use rpi_ai::utils::validation::validate_tool_arguments;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::messages::AgentMessage;
use crate::stream_fn::StreamFn;
use crate::types::{
    AgentEvent, AgentTool, AgentToolCall, AgentToolResult, AgentToolUpdateCallback,
    ToolExecutionMode,
};

/// Current time as Unix milliseconds (upstream `Date.now()`).
pub(crate) fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Agent-side `ThinkingLevel` (includes `"off"`) → pi-ai `ThinkingLevel`
/// (`"off"` maps to `None`, matching upstream
/// `thinkingLevel === "off" ? undefined : thinkingLevel`).
pub(crate) fn thinking_level_from_model_level(level: ModelThinkingLevel) -> Option<ThinkingLevel> {
    ThinkingLevel::from_model_level(level)
}

/// `config.reasoning ?? "off"` recorded on every finalized assistant message
/// (agent-loop.ts:409 @ abe508e1b; R2.7.1).
fn recorded_thinking_level(reasoning: Option<ThinkingLevel>) -> ModelThinkingLevel {
    reasoning
        .map(ThinkingLevel::to_model_level)
        .unwrap_or(ModelThinkingLevel::Off)
}

// ---------------------------------------------------------------------------
// AgentContext (types.ts)
// ---------------------------------------------------------------------------

/// `AgentContext` — context snapshot passed into the low-level agent loop
/// (#9548: the system prompt and tool declarations ride the transcript's
/// system messages; no standalone `system_prompt` field anymore).
#[derive(Clone, Default)]
pub struct AgentContext {
    /// Transcript visible to the model.
    pub messages: Vec<AgentMessage>,
    /// Tools available for execution in this run.
    pub tools: Option<Vec<Arc<dyn AgentTool>>>,
}

// ---------------------------------------------------------------------------
// Hook types (types.ts)
// ---------------------------------------------------------------------------

/// `BeforeToolCallResult` — returning `block: true` prevents execution; the
/// loop emits an error tool result instead.
#[derive(Debug, Clone, Default)]
pub struct BeforeToolCallResult {
    pub block: Option<bool>,
    /// Text shown in the error result; a default blocked message is used when
    /// omitted.
    pub reason: Option<String>,
    /// Replaces the validated arguments passed to `execute` (upstream mutates
    /// the validated args object in place; Rust hooks receive it by value, so
    /// the mutation is returned instead). Applied **without revalidation**,
    /// matching upstream.
    pub args: Option<Value>,
    /// Hint that the agent should stop after the current tool batch when this
    /// call is blocked. Early termination only happens when every finalized
    /// tool result in the batch sets this to true.
    pub terminate: Option<bool>,
}

/// `AfterToolCallResult` — partial override with field-by-field replacement
/// semantics (no deep merge). Omitted fields keep the executed result values.
#[derive(Debug, Clone, Default)]
pub struct AfterToolCallResult {
    /// Replaces the tool result content array in full.
    pub content: Option<Vec<ToolResultContent>>,
    /// Replaces the tool result details value in full.
    pub details: Option<Value>,
    /// Replaces the machine-readable `structuredContent`; `Some(None)`-style
    /// clearing follows `content` replacement (see
    /// `finalize_executed_tool_call`, agent-loop.ts:878-888 @ abe508e1b).
    pub structured_content: Option<Value>,
    /// Replaces the tool result error flag.
    pub is_error: Option<bool>,
    /// Usage from the final tool execution itself, if available. Not used for
    /// main LLM context accounting.
    pub usage: Option<Usage>,
    /// Hint that the agent should stop after the current tool batch.
    pub terminate: Option<bool>,
}

/// `BeforeToolCallContext`.
#[derive(Clone)]
pub struct BeforeToolCallContext {
    /// The assistant message that requested the tool call.
    pub assistant_message: AssistantMessage,
    /// The raw tool call block from `assistant_message.content`.
    pub tool_call: AgentToolCall,
    /// Validated tool arguments for the target tool schema.
    pub args: Value,
    /// Current agent context at the time the tool call is prepared.
    pub context: AgentContext,
}

/// `AfterToolCallContext`.
#[derive(Clone)]
pub struct AfterToolCallContext {
    /// The assistant message that requested the tool call.
    pub assistant_message: AssistantMessage,
    /// The raw tool call block from `assistant_message.content`.
    pub tool_call: AgentToolCall,
    /// Validated tool arguments for the target tool schema.
    pub args: Value,
    /// The executed tool result before any overrides are applied.
    pub result: AgentToolResult,
    /// Whether the executed tool result is currently treated as an error.
    pub is_error: bool,
    /// Current agent context at the time the tool call is finalized.
    pub context: AgentContext,
}

/// `AgentTurnContext` (types.ts:135-144 @ abe508e1b): context passed to the
/// completed-turn `finishTurn` callback.
#[derive(Clone)]
pub struct AgentTurnContext {
    /// The assistant message that completed the turn.
    pub message: AssistantMessage,
    /// Tool result messages passed to the preceding `turn_end` event.
    pub tool_results: Vec<ToolResultMessage>,
    /// Current agent context after the turn's assistant message and tool
    /// results have been appended.
    pub context: AgentContext,
    /// Messages that this loop invocation will return if it exits at this
    /// point. Prompt runs include the initial prompt messages.
    pub new_messages: Vec<AgentMessage>,
}

/// `AgentTurnDecision` (types.ts:147 @ abe508e1b): decision returned by
/// [`FinishTurnFn`]. `None` preserves normal scheduling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTurnDecision {
    /// Ensures one next provider request (tool-result/steering/follow-up
    /// scheduling can satisfy it; otherwise the loop continues once with the
    /// current context).
    Continue,
    /// Ends the run without polling queues or preparing another request.
    End,
}

/// `PrepareNextTurnContext extends AgentTurnContext` (identical
/// fields upstream).
pub type PrepareNextTurnContext = AgentTurnContext;

/// `AgentLoopTurnUpdate` — replacement runtime state applied before starting
/// another provider request.
#[derive(Clone, Default)]
pub struct AgentLoopTurnUpdate {
    /// Context for the next provider request.
    pub context: Option<AgentContext>,
    /// Messages to append before the next provider request, with normal
    /// lifecycle events (#9548, agent-loop.ts:146).
    pub messages: Option<Vec<AgentMessage>>,
    /// Model for the next provider request.
    pub model: Option<Model>,
    /// Thinking level for the next provider request (`"off"` clears
    /// `config.reasoning`).
    pub thinking_level: Option<ModelThinkingLevel>,
}

// ---------------------------------------------------------------------------
// Hook function aliases
// ---------------------------------------------------------------------------

/// `convertToLlm` — required; converts `AgentMessage[]` to LLM-compatible
/// `Message[]` before each LLM call. Contract: must not panic.
pub type ConvertToLlmFn =
    Arc<dyn Fn(Vec<AgentMessage>) -> BoxFuture<'static, Vec<Message>> + Send + Sync>;

/// `transformContext` — optional transform applied to the context messages
/// before `convertToLlm`. Contract: must not panic.
pub type TransformContextFn = Arc<
    dyn Fn(Vec<AgentMessage>, CancellationToken) -> BoxFuture<'static, Vec<AgentMessage>>
        + Send
        + Sync,
>;

/// `getApiKey` — resolves an API key dynamically for each LLM call (provider
/// id in, key out). Contract: must not panic; return `None` when no key is
/// available.
pub type GetApiKeyFn = Arc<dyn Fn(String) -> BoxFuture<'static, Option<String>> + Send + Sync>;

/// `PrepareRequestContext` (types.ts:173-177 @ abe508e1b): runtime state
/// available immediately before a conversational provider request.
#[derive(Clone)]
pub struct PrepareRequestContext {
    /// Context for the provider request.
    pub context: AgentContext,
    /// Model for the provider request.
    pub model: Model,
    /// Thinking level for the provider request (`"off"` when none).
    pub thinking_level: ModelThinkingLevel,
}

/// `AgentRequestUpdate` (types.ts:180 @ abe508e1b): replacement runtime
/// state for the provider request being prepared (`messages` excluded).
#[derive(Clone, Default)]
pub struct AgentRequestUpdate {
    /// Replacement context for the provider request.
    pub context: Option<AgentContext>,
    /// Replacement model for the provider request.
    pub model: Option<Model>,
    /// Replacement thinking level for the provider request (`"off"` clears
    /// `config.reasoning`).
    pub thinking_level: Option<ModelThinkingLevel>,
}

/// `prepareRequest` (types.ts:271 @ abe508e1b): called immediately before
/// every conversational provider request, including the first. Pending
/// messages have already been appended and emitted when this callback runs.
/// The returned context/model/thinking level replaces the runtime values for
/// this and later requests in the run. This hook does not poll queues.
/// Contract: must not panic.
pub type PrepareRequestFn = Arc<
    dyn Fn(PrepareRequestContext) -> BoxFuture<'static, Option<AgentRequestUpdate>> + Send + Sync,
>;

/// `finish_turn` — returning `Some(End)` makes the loop emit `agent_end`
/// and exit before polling steering/follow-up queues.
/// `FinishTurn` (types.ts:155-158 @ abe508e1b): called after a completed
/// assistant turn and all of its tool-result messages, but before
/// `turn_end`. On a normal turn, [`AgentTurnDecision::Continue`] ensures one
/// next provider request; [`AgentTurnDecision::End`] ends the run. Returning
/// `None` preserves normal scheduling. Error and aborted responses remain
/// hard exits but still invoke this callback (their decision is ignored).
pub type FinishTurnFn =
    Arc<dyn Fn(AgentTurnContext) -> BoxFuture<'static, Option<AgentTurnDecision>> + Send + Sync>;

/// `prepareNextTurn` — called after `turn_end` when the loop will
/// continue, immediately before the next turn starts. Return replacement
/// context/model/thinking state to affect that turn; return `None` to keep
/// using the current context/config (types.ts:224-230 @ 9841914).
///
/// Terminal turns (stop / abort) never trigger it: the loop checks
/// `finish_turn` and the queued-message gates first (#6879).
pub type PrepareNextTurnFn = Arc<
    dyn Fn(PrepareNextTurnContext) -> BoxFuture<'static, Option<AgentLoopTurnUpdate>> + Send + Sync,
>;

/// `getSteeringMessages` / `getFollowUpMessages` — queue drain hooks.
/// Contract: must not panic; return `vec![]` when empty.
pub type GetQueuedMessagesFn = Arc<dyn Fn() -> BoxFuture<'static, Vec<AgentMessage>> + Send + Sync>;

/// `beforeToolCall` — called after argument validation, before execution.
pub type BeforeToolCallFn = Arc<
    dyn Fn(
            BeforeToolCallContext,
            CancellationToken,
        ) -> BoxFuture<'static, Option<BeforeToolCallResult>>
        + Send
        + Sync,
>;

/// `afterToolCall` — called after execution, before `tool_execution_end` and
/// tool-result message events are emitted. Upstream wraps the hook in
/// try/catch and downgrades a throw to an error result (agent-loop.ts:743-746);
/// returning `Err` here gets the same treatment. Contract: must not panic.
pub type AfterToolCallFn = Arc<
    dyn Fn(
            AfterToolCallContext,
            CancellationToken,
        ) -> BoxFuture<'static, Result<Option<AfterToolCallResult>, AgentError>>
        + Send
        + Sync,
>;

/// `nestedCallSummary` — session hook returning the bounded record of the
/// nested calls (`ctx.executeTool()`) one model-issued tool call made, once
/// that call finished. The record is written to the tool result message's
/// `nestedCalls` and its usage folded into the message usage (V16-06 FR-E).
pub type NestedCallSummaryFn =
    Arc<dyn Fn(&str) -> Option<crate::nested_tool_calls::NestedCallSummary> + Send + Sync>;

/// `AgentEventSink` — shared event sink. `Fn` (not `FnMut`) so parallel tool
/// tasks can each hold a clone, mirroring the single upstream `emit` closure.
pub type AgentEventSink = Arc<dyn Fn(AgentEvent) -> BoxFuture<'static, ()> + Send + Sync>;

// ---------------------------------------------------------------------------
// AgentLoopConfig (types.ts)
// ---------------------------------------------------------------------------

/// `AgentLoopConfig` — configuration for one agent loop invocation.
pub struct AgentLoopConfig {
    /// Model used for provider requests.
    pub model: Model,
    /// `SimpleStreamOptions.reasoning` (`None` = thinking off). Carried for
    /// `prepareNextTurn` semantics; not forwarded through the pinned
    /// `StreamFn` shape (see file header).
    pub reasoning: Option<ThinkingLevel>,
    /// `SimpleStreamOptions.thinkingBudgets` (same forwarding caveat).
    pub thinking_budgets: Option<ThinkingBudgets>,
    /// Base stream options cloned per LLM call; `api_key` and `signal` are
    /// overwritten with the per-call values (upstream
    /// `{...config, apiKey: resolvedApiKey, signal}`). The static
    /// `stream_options.api_key` is the fallback key (upstream `config.apiKey`).
    pub stream_options: StreamOptions,
    /// Tool execution mode (upstream default: parallel).
    pub tool_execution: ToolExecutionMode,
    pub convert_to_llm: ConvertToLlmFn,
    pub transform_context: Option<TransformContextFn>,
    pub get_api_key: Option<GetApiKeyFn>,
    pub finish_turn: Option<FinishTurnFn>,
    /// `prepareRequest` (agent-loop.ts:218-235 @ abe508e1b).
    pub prepare_request: Option<PrepareRequestFn>,
    pub prepare_next_turn: Option<PrepareNextTurnFn>,
    pub get_steering_messages: Option<GetQueuedMessagesFn>,
    pub get_follow_up_messages: Option<GetQueuedMessagesFn>,
    pub before_tool_call: Option<BeforeToolCallFn>,
    pub after_tool_call: Option<AfterToolCallFn>,
    /// V16-06 FR-E: bounded nested-call record lookup for finished
    /// model-issued tool calls (session-owned runner).
    pub nested_call_summary: Option<NestedCallSummaryFn>,
}

// ---------------------------------------------------------------------------
// AgentEventStream (mirrors rpi-ai AssistantMessageEventStream shape)
// ---------------------------------------------------------------------------

struct Inner {
    tx: Mutex<Option<mpsc::UnboundedSender<AgentEvent>>>,
    rx: Mutex<mpsc::UnboundedReceiver<AgentEvent>>,
    result_tx: Mutex<Option<oneshot::Sender<Vec<AgentMessage>>>>,
    result_rx: Shared<oneshot::Receiver<Vec<AgentMessage>>>,
    done: AtomicBool,
}

/// Observational event stream returned by [`agent_loop`]/[`agent_loop_continue`]
/// (upstream `EventStream<AgentEvent, AgentMessage[]>` from
/// `createAgentStream`, agent-loop.ts:145-150).
///
/// #9055 (`b2602be77`): upstream swapped the O(n) `Array.shift()` queue for
/// a two-stack FifoQueue; this port is mpsc-channel-backed (`poll_recv` is
/// O(1)), so the buffered-drain path was never quadratic. Regression
/// semantics + linearity tests live in `tests/agent_loop_test.rs`.
///
/// - [`push`](Self::push) is ignored after an `agent_end` event or
///   [`end`](Self::end).
/// - [`result`](Self::result) resolves with the `agent_end` messages (or the
///   value passed to `end`).
#[derive(Clone)]
pub struct AgentEventStream {
    inner: Arc<Inner>,
}

impl Default for AgentEventStream {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentEventStream {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (result_tx, result_rx) = oneshot::channel();
        Self {
            inner: Arc::new(Inner {
                tx: Mutex::new(Some(tx)),
                rx: Mutex::new(rx),
                result_tx: Mutex::new(Some(result_tx)),
                result_rx: result_rx.shared(),
                done: AtomicBool::new(false),
            }),
        }
    }

    fn resolve_result(&self, messages: Vec<AgentMessage>) {
        // First resolution wins (upstream promise resolve is idempotent).
        if let Some(tx) = lock(&self.inner.result_tx).take() {
            let _ = tx.send(messages);
        }
    }

    /// Push an event to the stream.
    pub fn push(&self, event: AgentEvent) {
        if self.inner.done.load(Ordering::SeqCst) {
            return;
        }
        if let AgentEvent::AgentEnd { messages } = &event {
            self.inner.done.store(true, Ordering::SeqCst);
            self.resolve_result(messages.clone());
        }
        // A closed channel (consumer dropped / ended) is not an error upstream.
        if let Some(tx) = lock(&self.inner.tx).as_ref() {
            let _ = tx.send(event);
        }
    }

    /// End the stream. The provided messages resolve pending `result()` calls
    /// (unless already resolved by an `agent_end` event).
    pub fn end(&self, messages: Vec<AgentMessage>) {
        self.inner.done.store(true, Ordering::SeqCst);
        self.resolve_result(messages);
        // Dropping the sender closes the channel: iterators terminate after
        // draining buffered events.
        lock(&self.inner.tx).take();
    }

    /// The final `agent_end` messages (resolves on the terminal event; an
    /// abandoned producer resolves to an empty list, upstream `[]`).
    pub fn result(&self) -> BoxFuture<'static, Vec<AgentMessage>> {
        self.inner
            .result_rx
            .clone()
            .map(|r| r.ok().unwrap_or_default())
            .boxed()
    }
}

impl Stream for AgentEventStream {
    type Item = AgentEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        lock(&self.inner.rx).poll_recv(cx)
    }
}

// ---------------------------------------------------------------------------
// Entry points (agent-loop.ts:31-143)
// ---------------------------------------------------------------------------

/// `agentLoop` — start an agent loop with new prompt messages. The prompts
/// are added to the context and events are emitted for them
/// (agent-loop.ts:31-54).
///
/// Cancellation mirrors the upstream `AbortSignal`: pass a
/// [`CancellationToken`] and cancel it to abort the run.
pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<CancellationToken>,
    stream_fn: StreamFn,
) -> AgentEventStream {
    let stream = AgentEventStream::new();
    let sink_stream = stream.clone();
    let emit: AgentEventSink = Arc::new(move |event| {
        let stream = sink_stream.clone();
        Box::pin(async move {
            stream.push(event);
        })
    });
    let end_stream = stream.clone();
    tokio::spawn(async move {
        let messages = run_agent_loop(prompts, context, config, emit, signal, stream_fn).await;
        end_stream.end(messages);
    });
    stream
}

/// `agentLoopContinue` — continue an agent loop from the current context
/// without adding a new message (agent-loop.ts:64-93). Validation errors are
/// returned synchronously, mirroring the upstream throw-before-stream.
pub fn agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<CancellationToken>,
    stream_fn: StreamFn,
) -> Result<AgentEventStream, AgentError> {
    validate_continuation(&context)?;

    let stream = AgentEventStream::new();
    let sink_stream = stream.clone();
    let emit: AgentEventSink = Arc::new(move |event| {
        let stream = sink_stream.clone();
        Box::pin(async move {
            stream.push(event);
        })
    });
    let end_stream = stream.clone();
    tokio::spawn(async move {
        // Validation already ran above (identical checks), so `Err` is
        // unreachable here; upstream would leave the stream open on a
        // rejection, we resolve with an empty list instead.
        let messages = run_agent_loop_continue(context, config, emit, signal, stream_fn)
            .await
            .unwrap_or_default();
        end_stream.end(messages);
    });
    Ok(stream)
}

fn validate_continuation(context: &AgentContext) -> Result<(), AgentError> {
    // The all-system guard lives in `Agent::continue_run`
    // (agent.ts:382-384), not here — the loop layer only checks the
    // empty/assistant-tail shape (agent-loop.ts:71-78/:131-139).
    if context.messages.is_empty() {
        return Err(AgentError::Message(
            "Cannot continue: no messages in context".to_owned(),
        ));
    }
    if matches!(context.messages.last(), Some(AgentMessage::Assistant(_))) {
        return Err(AgentError::Message(
            "Cannot continue from message role: assistant".to_owned(),
        ));
    }
    Ok(())
}

/// `runAgentLoop` (agent-loop.ts:95-118). The `Agent` layer drives this
/// directly with its barrier sink.
pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<CancellationToken>,
    stream_fn: StreamFn,
) -> Vec<AgentMessage> {
    // #9548 (agent-loop.ts:106-116): declare tool loadout changes to the
    // model before the run — the difference between the committed transcript
    // and the executable set becomes a system message.
    let initial_messages = declare_tool_changes(&context, &prompts.clone());
    let mut new_messages: Vec<AgentMessage> = initial_messages.clone();
    let mut current_context = AgentContext {
        messages: context
            .messages
            .iter()
            .cloned()
            .chain(initial_messages.iter().cloned())
            .collect(),
        tools: context.tools.clone(),
    };

    emit(AgentEvent::AgentStart).await;
    emit(AgentEvent::TurnStart).await;
    for message in &initial_messages {
        emit(AgentEvent::MessageStart {
            message: message.clone(),
        })
        .await;
        emit(AgentEvent::MessageEnd {
            message: message.clone(),
        })
        .await;
    }

    run_loop(
        &mut current_context,
        &mut new_messages,
        config,
        &signal,
        &emit,
        &stream_fn,
    )
    .await;
    new_messages
}

/// `runAgentLoopContinue` (agent-loop.ts:120-143).
pub async fn run_agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    emit: AgentEventSink,
    signal: Option<CancellationToken>,
    stream_fn: StreamFn,
) -> Result<Vec<AgentMessage>, AgentError> {
    validate_continuation(&context)?;

    let mut new_messages: Vec<AgentMessage> = Vec::new();
    let mut current_context = context;

    emit(AgentEvent::AgentStart).await;
    emit(AgentEvent::TurnStart).await;

    run_loop(
        &mut current_context,
        &mut new_messages,
        config,
        &signal,
        &emit,
        &stream_fn,
    )
    .await;
    Ok(new_messages)
}

// ---------------------------------------------------------------------------
// runLoop (agent-loop.ts:155-275)
// ---------------------------------------------------------------------------

fn is_aborted(signal: &Option<CancellationToken>) -> bool {
    signal.as_ref().is_some_and(CancellationToken::is_cancelled)
}

/// `runLoop` — main loop logic shared by both entry points. Mutates
/// `new_messages` in place like the upstream shared array.
async fn run_loop(
    current_context: &mut AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    mut config: AgentLoopConfig,
    signal: &Option<CancellationToken>,
    emit: &AgentEventSink,
    stream_function: &StreamFn,
) {
    // `lastCompletedTurn` (agent-loop.ts:167): data for the
    // `prepare_next_turn` hook, set after each `turn_end`. `None` on the
    // first iteration — the entry points already emitted `turn_start`.
    let mut last_completed_turn: Option<(AssistantMessage, Vec<ToolResultMessage>)> = None;
    // `explicitContinuation` (agent-loop.ts:167): set by
    // `finishTurn { action: "continue" }`; when no natural request was
    // selected the loop runs one context-only turn.
    let mut explicit_continuation = false;
    // Check for steering messages at start (user may have typed while waiting).
    let mut pending_messages: Vec<AgentMessage> = match &config.get_steering_messages {
        Some(get_steering) => get_steering().await,
        None => Vec::new(),
    };

    // Outer loop: continues when queued follow-up messages arrive after the
    // agent would stop.
    loop {
        let mut has_more_tool_calls = true;

        // Inner loop: process tool calls and steering messages.
        while has_more_tool_calls || !pending_messages.is_empty() {
            // Only prepare for a turn the loop will actually start
            // (agent-loop.ts:178-199 @ 9841914, #6879): reaching this point
            // means `finish_turn` did not exit and either tool
            // results or queued messages keep the loop going. Terminal
            // turns never trigger prepare, so compaction and other
            // prepare-time side effects vanish from them.
            let mut prepared_messages: Vec<AgentMessage> = Vec::new();
            if let Some((message, tool_results)) = last_completed_turn.as_ref() {
                if let Some(prepare_next_turn) = &config.prepare_next_turn {
                    let next_turn_context = PrepareNextTurnContext {
                        message: message.clone(),
                        tool_results: tool_results.clone(),
                        context: current_context.clone(),
                        new_messages: new_messages.clone(),
                    };
                    if let Some(update) = prepare_next_turn(next_turn_context).await {
                        if let Some(next_context) = update.context {
                            *current_context = next_context;
                        }
                        if let Some(model) = update.model {
                            config.model = model;
                        }
                        if let Some(thinking_level) = update.thinking_level {
                            config.reasoning = thinking_level_from_model_level(thinking_level);
                        }
                        // #9548 (agent-loop.ts:196): messages returned by the
                        // hook are appended before the next provider request.
                        prepared_messages = update.messages.unwrap_or_default();
                    }
                }
                // Preparation can be long-running (for example, compaction).
                // Pick up steering queued while it ran. Only poll again if
                // the earlier poll returned nothing; otherwise
                // one-at-a-time mode would deliver two messages in this
                // turn (agent-loop.ts:186-191).
                if pending_messages.is_empty() {
                    pending_messages = match &config.get_steering_messages {
                        Some(get_steering) => get_steering().await,
                        None => Vec::new(),
                    };
                }
                emit(AgentEvent::TurnStart).await;
            }

            // Process prepared and queued messages before the next assistant
            // response, declaring tool loadout changes first (#9548,
            // agent-loop.ts:208-215).
            pending_messages.splice(0..0, prepared_messages.drain(..));
            let messages_to_inject = declare_tool_changes(current_context, &pending_messages);
            pending_messages.clear();
            for message in messages_to_inject {
                emit(AgentEvent::MessageStart {
                    message: message.clone(),
                })
                .await;
                emit(AgentEvent::MessageEnd {
                    message: message.clone(),
                })
                .await;
                current_context.messages.push(message.clone());
                new_messages.push(message);
            }

            // `prepareRequest` (agent-loop.ts:218-235 @ abe508e1b): called
            // immediately before every conversational provider request,
            // including the first. The replacement context/model/thinking
            // level applies to this and later requests in the run. This hook
            // does not poll queues.
            if let Some(prepare_request) = &config.prepare_request {
                let request_update = prepare_request(PrepareRequestContext {
                    context: current_context.clone(),
                    model: config.model.clone(),
                    thinking_level: config
                        .reasoning
                        .map(ThinkingLevel::to_model_level)
                        .unwrap_or(ModelThinkingLevel::Off),
                })
                .await;
                if let Some(update) = request_update {
                    if let Some(context) = update.context {
                        *current_context = context;
                    }
                    if let Some(model) = update.model {
                        config.model = model;
                    }
                    if let Some(thinking_level) = update.thinking_level {
                        config.reasoning = thinking_level_from_model_level(thinking_level);
                    }
                }
            }

            // Stream assistant response.
            let message =
                stream_assistant_response(current_context, &config, signal, emit, stream_function)
                    .await;
            new_messages.push(AgentMessage::Assistant(message.clone()));

            if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
                // Error and aborted responses remain hard exits; `finishTurn`
                // still runs (extension boundaries observe the turn) but its
                // decision is ignored (agent-loop.ts:245-252 @ abe508e1b).
                if let Some(finish_turn) = &config.finish_turn {
                    let turn = AgentTurnContext {
                        message: message.clone(),
                        tool_results: Vec::new(),
                        context: current_context.clone(),
                        new_messages: new_messages.clone(),
                    };
                    let _ = finish_turn(turn).await;
                }
                emit(AgentEvent::TurnEnd {
                    message: AgentMessage::Assistant(message.clone()),
                    tool_results: Vec::new(),
                })
                .await;
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                })
                .await;
                return;
            }

            // Check for tool calls.
            let tool_calls: Vec<AgentToolCall> = message
                .content
                .iter()
                .filter_map(|c| match c {
                    AssistantContent::ToolCall(tc) => Some(tc.clone()),
                    _ => None,
                })
                .collect();

            let mut tool_results: Vec<ToolResultMessage> = Vec::new();
            has_more_tool_calls = false;
            if !tool_calls.is_empty() {
                // A "length" stop means the output was cut off by the token
                // limit, so every tool call in the message may carry truncated
                // arguments. Fail them all instead of executing potentially
                // borked calls.
                let executed_batch = if message.stop_reason == StopReason::Length {
                    fail_tool_calls_from_truncated_message(&tool_calls, emit).await
                } else {
                    execute_tool_calls(
                        current_context,
                        &message,
                        &tool_calls,
                        &config,
                        signal,
                        emit,
                    )
                    .await
                };
                tool_results = executed_batch.messages;
                has_more_tool_calls = !executed_batch.terminate;

                for result in &tool_results {
                    current_context
                        .messages
                        .push(AgentMessage::ToolResult(result.clone()));
                    new_messages.push(AgentMessage::ToolResult(result.clone()));
                }
            }

            // `lastCompletedTurn = { message, toolResults, ... }`
            // (agent-loop.ts:249-255) and `finishTurn` run after the
            // assistant message and all tool results, before `turn_end`.
            last_completed_turn = Some((message.clone(), tool_results.clone()));
            let decision = match &config.finish_turn {
                Some(finish_turn) => {
                    finish_turn(AgentTurnContext {
                        message: message.clone(),
                        tool_results: tool_results.clone(),
                        context: current_context.clone(),
                        new_messages: new_messages.clone(),
                    })
                    .await
                }
                None => None,
            };

            emit(AgentEvent::TurnEnd {
                message: AgentMessage::Assistant(message.clone()),
                tool_results: tool_results.clone(),
            })
            .await;

            if decision == Some(AgentTurnDecision::End) {
                emit(AgentEvent::AgentEnd {
                    messages: new_messages.clone(),
                })
                .await;
                return;
            }
            explicit_continuation = decision == Some(AgentTurnDecision::Continue);

            pending_messages = match &config.get_steering_messages {
                Some(get_steering) => get_steering().await,
                None => Vec::new(),
            };
            if has_more_tool_calls || !pending_messages.is_empty() {
                explicit_continuation = false;
            }
        }

        // Agent would stop here. Check for follow-up messages.
        let follow_up_messages = match &config.get_follow_up_messages {
            Some(get_follow_up) => get_follow_up().await,
            None => Vec::new(),
        };
        if !follow_up_messages.is_empty() {
            // Set as pending so the inner loop processes them.
            explicit_continuation = false;
            pending_messages = follow_up_messages;
            continue;
        }

        // No natural request was selected, so fulfill the continuation
        // decision with one context-only turn (agent-loop.ts:309-314 @
        // abe508e1b).
        if explicit_continuation {
            explicit_continuation = false;
            continue;
        }

        // No more messages, exit.
        break;
    }

    emit(AgentEvent::AgentEnd {
        messages: new_messages.clone(),
    })
    .await;
}

// ---------------------------------------------------------------------------
// streamAssistantResponse (agent-loop.ts:281-372)
// ---------------------------------------------------------------------------

fn stream_event_partial(event: &StreamEvent) -> Option<&std::sync::Arc<AssistantMessage>> {
    match event {
        StreamEvent::TextStart { partial, .. }
        | StreamEvent::TextDelta { partial, .. }
        | StreamEvent::TextEnd { partial, .. }
        | StreamEvent::ThinkingStart { partial, .. }
        | StreamEvent::ThinkingDelta { partial, .. }
        | StreamEvent::ThinkingEnd { partial, .. }
        | StreamEvent::ToolCallStart { partial, .. }
        | StreamEvent::ToolCallDelta { partial, .. }
        | StreamEvent::ToolCallEnd { partial, .. } => Some(partial),
        StreamEvent::Start { .. } | StreamEvent::Done { .. } | StreamEvent::Error { .. } => None,
    }
}

/// Shared tail of the upstream done/error branch and the post-loop block:
/// append the final message, emit `message_start` when the stream never
/// started (no partial was ever streamed), then emit `message_end`.
///
/// V15-13 (rpi#53 root cause 3): the streamed partial never enters
/// `context.messages` — during streaming it is shared by `Arc` through the
/// `MessageUpdate` events only, and the final message is appended exactly
/// once here. (Pre-V15-13 the partial was pushed at `start` and the tail
/// replaced with a full clone on every delta; the observable message list
/// is identical — nothing reads `context.messages` between `start` and
/// `done`/`error`.)
async fn finalize_streamed_message(
    context: &mut AgentContext,
    final_message: AssistantMessage,
    added_partial: bool,
    emit: &AgentEventSink,
) -> AssistantMessage {
    context
        .messages
        .push(AgentMessage::Assistant(final_message.clone()));
    if !added_partial {
        emit(AgentEvent::MessageStart {
            message: AgentMessage::Assistant(final_message.clone()),
        })
        .await;
    }
    emit(AgentEvent::MessageEnd {
        message: AgentMessage::Assistant(final_message.clone()),
    })
    .await;
    final_message
}

/// `declareToolChanges` (agent-loop.ts:281-325 @ #9548): declare tool
/// loadout changes to the model.
///
/// `context.tools` is what the runtime can execute; the transcript's system
/// messages declare what the model may call. Before each request the
/// difference becomes `toolsAdded` and `toolsRemoved` on a system message.
/// When a pending system message exists, its tool fields are treated as
/// intent and replaced with the delta between the committed transcript and
/// the executable set, so replay always yields exactly `context.tools`.
/// Otherwise a new system message is inserted before the first non-system
/// pending message.
fn declare_tool_changes(
    context: &AgentContext,
    pending_messages: &[AgentMessage],
) -> Vec<AgentMessage> {
    let system_index = pending_messages
        .iter()
        .rposition(|message| matches!(message, AgentMessage::System(_)));
    let baseline: Vec<AgentMessage> = match system_index {
        Some(index) => pending_messages
            .iter()
            .enumerate()
            .map(|(i, message)| {
                if i == index {
                    match message {
                        AgentMessage::System(system) => {
                            AgentMessage::System(with_tool_changes(system, &NO_CHANGES))
                        }
                        _ => unreachable!("rposition matched a system message"),
                    }
                } else {
                    message.clone()
                }
            })
            .collect(),
        None => pending_messages.to_vec(),
    };
    let declared_tools: Vec<Tool> = context
        .tools
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|tool| {
            rpi_ai::utils::transcript::to_tool_declaration(&Tool {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                parameters: tool.parameters().clone(),
                constrained_sampling: tool.constrained_sampling(),
            })
        })
        .collect();
    let mut replay_messages: Vec<Message> = context
        .messages
        .iter()
        .filter_map(agent_message_to_llm_ref)
        .collect();
    replay_messages.extend(
        baseline
            .iter()
            .filter_map(agent_message_to_llm_ref)
            .collect::<Vec<_>>(),
    );
    let changes = rpi_ai::utils::transcript::get_tool_state_changes(
        &rpi_ai::utils::transcript::get_current_tools(&replay_messages),
        &declared_tools,
    );
    let unchanged = changes.tools_added.is_empty() && changes.tools_removed.is_empty();

    if let Some(index) = system_index {
        // Keep the caller's message object when it already declares no tool
        // changes.
        let pending = match &pending_messages[index] {
            AgentMessage::System(system) => system,
            _ => unreachable!("rposition matched a system message"),
        };
        if unchanged
            && pending.tools_added.as_ref().is_none_or(|t| t.is_empty())
            && pending.tools_removed.as_ref().is_none_or(|t| t.is_empty())
        {
            return pending_messages.to_vec();
        }
        return baseline
            .into_iter()
            .enumerate()
            .map(|(i, message)| match (i == index, message) {
                (true, AgentMessage::System(system)) => {
                    AgentMessage::System(with_tool_changes(&system, &changes))
                }
                (_, message) => message,
            })
            .collect();
    }
    if unchanged {
        return pending_messages.to_vec();
    }
    let update = AgentMessage::System(with_tool_changes(
        &rpi_ai::types::SystemMessage {
            role: Default::default(),
            content: Default::default(),
            sections: None,
            tools_added: None,
            tools_removed: None,
            timestamp: now_millis(),
        },
        &changes,
    ));
    let insert_index = pending_messages
        .iter()
        .position(|message| !matches!(message, AgentMessage::System(_)))
        .unwrap_or(pending_messages.len());
    let mut result = Vec::with_capacity(pending_messages.len() + 1);
    result.extend(pending_messages[..insert_index].iter().cloned());
    result.push(update);
    result.extend(pending_messages[insert_index..].iter().cloned());
    result
}

/// `NO_CHANGES` (agent-loop.ts:327).
const NO_CHANGES: rpi_ai::utils::transcript::ToolStateChanges =
    rpi_ai::utils::transcript::ToolStateChanges {
        tools_added: Vec::new(),
        tools_removed: Vec::new(),
    };

/// `withToolChanges` (agent-loop.ts:329-337): copy a system message with its
/// tool fields replaced by `changes`; empty lists omit the field.
fn with_tool_changes(
    message: &rpi_ai::types::SystemMessage,
    changes: &rpi_ai::utils::transcript::ToolStateChanges,
) -> rpi_ai::types::SystemMessage {
    rpi_ai::types::SystemMessage {
        role: message.role,
        content: message.content.clone(),
        sections: message.sections.clone(),
        tools_added: (!changes.tools_added.is_empty()).then(|| changes.tools_added.clone()),
        tools_removed: (!changes.tools_removed.is_empty()).then(|| changes.tools_removed.clone()),
        timestamp: message.timestamp,
    }
}

/// Reference projection of an [`AgentMessage`] onto the LLM [`Message`] union
/// (system/user/assistant/toolResult only — the replay helpers read no
/// others).
fn agent_message_to_llm_ref(message: &AgentMessage) -> Option<Message> {
    match message {
        AgentMessage::System(system) => Some(Message::System(system.clone())),
        AgentMessage::User(user) => Some(Message::User(user.clone())),
        AgentMessage::Assistant(assistant) => Some(Message::Assistant(assistant.clone())),
        AgentMessage::ToolResult(result) => Some(Message::ToolResult(result.clone())),
        _ => None,
    }
}

/// `streamAssistantResponse` — stream one assistant response from the LLM.
/// This is where `AgentMessage[]` gets transformed to `Message[]`.
async fn stream_assistant_response(
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    signal: &Option<CancellationToken>,
    emit: &AgentEventSink,
    stream_function: &StreamFn,
) -> AssistantMessage {
    // Apply context transform if configured (AgentMessage[] → AgentMessage[]).
    let messages = match &config.transform_context {
        Some(transform) => {
            transform(context.messages.clone(), signal.clone().unwrap_or_default()).await
        }
        None => context.messages.clone(),
    };

    // Convert to LLM-compatible messages (AgentMessage[] → Message[]).
    let llm_messages = (config.convert_to_llm)(messages).await;

    // Build LLM context (#9548, agent-loop.ts:355): the prompt and tool
    // declarations already ride the transcript's system messages; the
    // request is normalized without a standalone system prompt or tool list.
    let llm_context = rpi_ai::utils::transcript::normalize_context(&Context {
        system_prompt: None,
        messages: llm_messages,
        tools: None,
    });

    // Resolve API key (important for expiring tokens). Upstream `||` treats an
    // empty string as missing, hence the non-empty filter.
    let resolved_api_key = match &config.get_api_key {
        Some(get_api_key) => get_api_key(config.model.provider.clone()).await,
        None => None,
    }
    .filter(|key| !key.is_empty())
    .or_else(|| config.stream_options.api_key.clone());

    let mut options = config.stream_options.clone();
    options.api_key = resolved_api_key;
    options.signal = signal.clone();
    // Upstream spreads the whole config (`{...config, apiKey, signal}`), so
    // `config.reasoning` — the live thinking level, updated by
    // `prepareNextTurn` — reaches the request. The pinned `StreamFn` shape
    // takes plain `StreamOptions`, so bind the channel here instead (design
    // doc §4.4 D-013). Without this the level set via /settings, the thinking
    // selector or `setThinkingLevel` never reaches the provider request.
    options.reasoning = config.reasoning.map(|level| level.to_model_level());

    let mut response = stream_function(config.model.clone(), llm_context, options);

    // Whether `StreamEvent::Start` has been seen — only then are delta
    // updates forwarded. (Upstream tracks the last partial message here;
    // only the "started" predicate is read, so a flag avoids re-cloning the
    // whole accumulating message on every delta.)
    let mut stream_started = false;
    // Whether a partial was streamed (`Start` seen) — gates the
    // `MessageStart` emission in `finalize_streamed_message`.
    let mut added_partial = false;

    while let Some(event) = response.next().await {
        match event {
            StreamEvent::Start { partial } => {
                stream_started = true;
                // V15-13 (rpi#53 root cause 3): the partial no longer
                // enters `context.messages` here; the final message is
                // appended once at finalize. `MessageStart` fires once per
                // stream, so one extraction clone per turn is fine.
                added_partial = true;
                emit(AgentEvent::MessageStart {
                    message: AgentMessage::Assistant((*partial).clone()),
                })
                .await;
            }
            StreamEvent::Done { mut message, .. } => {
                // `Object.assign(await response.result(), { thinkingLevel:
                // config.reasoning ?? "off" })` (agent-loop.ts:409): record
                // the requested level on every finalized response (R2.7.1).
                message.thinking_level = Some(recorded_thinking_level(config.reasoning));
                return finalize_streamed_message(context, message, added_partial, emit).await;
            }
            StreamEvent::Error { mut error, .. } => {
                error.thinking_level = Some(recorded_thinking_level(config.reasoning));
                return finalize_streamed_message(context, error, added_partial, emit).await;
            }
            other => {
                if stream_started && let Some(partial) = stream_event_partial(&other) {
                    // V15-13 zero-copy delta path: the accumulated
                    // partial is shared by `Arc` straight into the
                    // event (and from there into the agent state, every
                    // listener, and the UI queue); no deep copies per
                    // delta, and `context.messages` stays untouched
                    // until finalize.
                    emit(AgentEvent::MessageUpdate {
                        message: std::sync::Arc::clone(partial),
                        assistant_message_event: std::sync::Arc::new(other),
                    })
                    .await;
                }
            }
        }
    }

    // The stream ended without a terminal done/error event, violating the
    // StreamFn contract. Upstream would await `response.result()` here (which
    // cannot resolve for such a stream); synthesize an error message instead.
    tracing::warn!("stream ended without a terminal done/error event");
    let final_message = AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: config.model.api.clone(),
        provider: config.model.provider.clone(),
        model: config.model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: Some(recorded_thinking_level(config.reasoning)),
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        error_message: Some("Stream ended without a terminal done/error event".to_owned()),
        timestamp: now_millis(),
        deferred: None,
        end_turn: None,
        raw_stop_reason: None,
        duration_ms: None,
    };
    finalize_streamed_message(context, final_message, added_partial, emit).await
}

// ---------------------------------------------------------------------------
// Tool execution (agent-loop.ts:374-792)
// ---------------------------------------------------------------------------

/// `ExecutedToolCallBatch`.
struct ExecutedToolCallBatch {
    messages: Vec<ToolResultMessage>,
    terminate: bool,
}

/// `PreparedToolCall`.
struct PreparedToolCall {
    tool_call: AgentToolCall,
    tool: Arc<dyn AgentTool>,
    args: Value,
}

/// `PreparedToolCall | ImmediateToolCallOutcome`.
enum Preparation {
    Prepared(PreparedToolCall),
    Immediate {
        result: AgentToolResult,
        is_error: bool,
    },
}

/// `ExecutedToolCallOutcome` (agent-loop.ts:667-672): the raw `execute()`
/// result plus its monotonic duration.
struct ExecutedToolCallOutcome {
    result: AgentToolResult,
    is_error: bool,
    /// `durationMs` — always present on this path (the tool ran).
    duration_ms: u64,
}

/// `FinalizedToolCallOutcome` (= upstream `AgentToolCallOutcome`, whose
/// `durationMs` is optional): `None` when the tool never ran (immediate
/// preflight outcomes, truncated-message failures, panics).
struct FinalizedToolCallOutcome {
    tool_call: AgentToolCall,
    result: AgentToolResult,
    is_error: bool,
    duration_ms: Option<u64>,
}

/// `AgentToolCallOutcome` (packages/agent/src/types.ts:449-453 @
/// abe508e1b): the final outcome
/// of a tool call after hooks ran. Nested calls (`ctx.executeTool()`) return
/// this shape too.
#[derive(Debug, Clone)]
pub struct AgentToolCallOutcome {
    pub tool_call: AgentToolCall,
    pub result: AgentToolResult,
    pub is_error: bool,
    /// Milliseconds `execute()` took, measured with a monotonic clock;
    /// absent when the tool did not run (types.ts:449-454 @ abe508e1b,
    /// #10549).
    pub duration_ms: Option<u64>,
}

/// Options for [`run_tool_call`] (`RunToolCallOptions`,
/// agent-loop.ts:790-802 @ abe508e1b).
pub struct RunToolCallOptions {
    /// Tools the call resolves against.
    pub tools: Vec<Arc<dyn AgentTool>>,
    /// The assistant message that issued the call.
    pub assistant_message: AssistantMessage,
    /// Current agent context at the time the call is prepared.
    pub context: AgentContext,
    pub before_tool_call: Option<BeforeToolCallFn>,
    pub after_tool_call: Option<AfterToolCallFn>,
    pub signal: Option<CancellationToken>,
    /// Receives partial results of the nested tool (in addition to the
    /// `tool_execution_update` events the caller emits).
    pub on_update: Option<AgentToolUpdateCallback>,
}

/// `runToolCall` (agent-loop.ts:810-820 @ abe508e1b): run one tool call
/// through the full pipeline (schema validation, `beforeToolCall`,
/// execution, `afterToolCall`). Used by the loop's batch runners and by the
/// session for nested `ctx.executeTool()` calls; callers emit the
/// `tool_execution_*` events.
///
/// Never panics; tool failures come back as `is_error: true` results.
pub async fn run_tool_call(
    tool_call: AgentToolCall,
    options: RunToolCallOptions,
) -> AgentToolCallOutcome {
    let context = AgentContext {
        messages: options.context.messages,
        tools: Some(options.tools),
    };
    let preparation = prepare_tool_call(
        &context,
        &options.assistant_message,
        &tool_call,
        &options.before_tool_call,
        &options.signal,
    )
    .await;
    let preparation = match preparation {
        Preparation::Immediate { result, is_error } => {
            return AgentToolCallOutcome {
                tool_call,
                result,
                is_error,
                // The tool did not run (unknown tool / blocked by
                // `beforeToolCall`): absent, not zero.
                duration_ms: None,
            };
        }
        Preparation::Prepared(prepared) => prepared,
    };
    let executed =
        execute_prepared_tool_call_with_sink(&preparation, &options.signal, options.on_update)
            .await;
    let finalized = finalize_executed_tool_call(
        &context,
        &options.assistant_message,
        &preparation,
        executed,
        &options.after_tool_call,
        &options.signal,
    )
    .await;
    AgentToolCallOutcome {
        tool_call: finalized.tool_call,
        result: finalized.result,
        is_error: finalized.is_error,
        duration_ms: finalized.duration_ms,
    }
}

/// `failToolCallsFromTruncatedMessage` (agent-loop.ts:381-406).
async fn fail_tool_calls_from_truncated_message(
    tool_calls: &[AgentToolCall],
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut messages: Vec<ToolResultMessage> = Vec::new();
    for tool_call in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: Value::Object(tool_call.arguments.clone()),

            parent_tool_call_id: None,
        })
        .await;
        let finalized = FinalizedToolCallOutcome {
            tool_call: tool_call.clone(),
            result: create_error_tool_result(format!(
                "Tool call \"{}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
                tool_call.name
            )),
            is_error: true,
            duration_ms: None,
        };
        emit_tool_execution_end(&finalized, emit).await;
        let tool_result_message = create_tool_result_message(&finalized, None);
        emit_tool_result_message(&tool_result_message, emit).await;
        messages.push(tool_result_message);
    }
    ExecutedToolCallBatch {
        messages,
        terminate: false,
    }
}

/// `executeToolCalls` (agent-loop.ts:411-426).
async fn execute_tool_calls(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[AgentToolCall],
    config: &AgentLoopConfig,
    signal: &Option<CancellationToken>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let has_sequential_tool_call = tool_calls.iter().any(|tc| {
        current_context
            .tools
            .as_ref()
            .and_then(|tools| tools.iter().find(|t| t.name() == tc.name))
            .and_then(|t| t.execution_mode())
            == Some(ToolExecutionMode::Sequential)
    });
    if config.tool_execution == ToolExecutionMode::Sequential || has_sequential_tool_call {
        return execute_tool_calls_sequential(
            current_context,
            assistant_message,
            tool_calls,
            config,
            signal,
            emit,
        )
        .await;
    }
    execute_tool_calls_parallel(
        current_context,
        assistant_message,
        tool_calls,
        config,
        signal,
        emit,
    )
    .await
}

/// `executeToolCallsSequential` (agent-loop.ts:433-487).
async fn execute_tool_calls_sequential(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[AgentToolCall],
    config: &AgentLoopConfig,
    signal: &Option<CancellationToken>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut finalized_calls: Vec<FinalizedToolCallOutcome> = Vec::new();
    let mut messages: Vec<ToolResultMessage> = Vec::new();

    for tool_call in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: Value::Object(tool_call.arguments.clone()),

            parent_tool_call_id: None,
        })
        .await;

        let preparation = prepare_tool_call(
            current_context,
            assistant_message,
            tool_call,
            &config.before_tool_call,
            signal,
        )
        .await;
        let finalized = match preparation {
            Preparation::Immediate { result, is_error } => FinalizedToolCallOutcome {
                tool_call: tool_call.clone(),
                result,
                is_error,
                duration_ms: None,
            },
            Preparation::Prepared(prepared) => {
                // rc.12 review (P2): a panicking tool must not kill the whole
                // run — this path previously awaited
                // `execute_prepared_tool_call` directly on the run task, so a
                // panic skipped `run_agent_prompt`'s cleanup (the run stayed
                // active forever; every later prompt was rejected with
                // "Agent is already processing" until restart). Spawn like
                // the parallel path and fill the JoinError with an error
                // result (same containment rc.11 P1-4 established there).
                let task_emit = emit.clone();
                let task_context = current_context.clone();
                let task_assistant_message = assistant_message.clone();
                let task_after_tool_call = config.after_tool_call.clone();
                let task_signal = signal.clone();

                let handle = tokio::spawn(async move {
                    let executed =
                        execute_prepared_tool_call(&prepared, &task_signal, &task_emit).await;
                    finalize_executed_tool_call(
                        &task_context,
                        &task_assistant_message,
                        &prepared,
                        executed,
                        &task_after_tool_call,
                        &task_signal,
                    )
                    .await
                });
                match handle.await {
                    Ok(finalized) => finalized,
                    Err(join_error) => {
                        tracing::error!(%join_error, "sequential tool task failed");
                        FinalizedToolCallOutcome {
                            tool_call: tool_call.clone(),
                            result: create_error_tool_result(format!(
                                "Tool task failed: {join_error}"
                            )),
                            is_error: true,
                            duration_ms: None,
                        }
                    }
                }
            }
        };

        emit_tool_execution_end(&finalized, emit).await;
        let summary = config
            .nested_call_summary
            .as_ref()
            .and_then(|summary| summary(&finalized.tool_call.id));
        let tool_result_message = create_tool_result_message(&finalized, summary.as_ref());
        emit_tool_result_message(&tool_result_message, emit).await;
        finalized_calls.push(finalized);
        messages.push(tool_result_message);

        if is_aborted(signal) {
            break;
        }
    }

    ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&finalized_calls),
    }
}

/// `executeToolCallsParallel` (agent-loop.ts:489-554).
///
/// Preflight is always sequential; `tool_execution_end` for prepared calls is
/// emitted in completion order from the spawned tasks; tool-result artifacts
/// are emitted afterwards in assistant source order.
async fn execute_tool_calls_parallel(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_calls: &[AgentToolCall],
    config: &AgentLoopConfig,
    signal: &Option<CancellationToken>,
    emit: &AgentEventSink,
) -> ExecutedToolCallBatch {
    // One slot per preflighted tool call, in source order.
    let mut slots: Vec<Option<FinalizedToolCallOutcome>> = Vec::new();
    // Prepared calls wait for the whole preflight pass before spawning —
    // upstream defers the closures to `Promise.all(finalizedCalls.map(…))`
    // (agent-loop.ts:546-549), so a later preflight abort (#8935/#8936)
    // stops earlier prepared tools via the spawn-entry signal check before
    // they enter `execute`.
    let mut deferred: Vec<(usize, PreparedToolCall)> = Vec::new();
    let mut set = tokio::task::JoinSet::new();

    for tool_call in tool_calls {
        emit(AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: Value::Object(tool_call.arguments.clone()),

            parent_tool_call_id: None,
        })
        .await;

        let preparation = prepare_tool_call(
            current_context,
            assistant_message,
            tool_call,
            &config.before_tool_call,
            signal,
        )
        .await;
        match preparation {
            Preparation::Immediate { result, is_error } => {
                let finalized = FinalizedToolCallOutcome {
                    tool_call: tool_call.clone(),
                    result,
                    is_error,
                    duration_ms: None,
                };
                emit_tool_execution_end(&finalized, emit).await;
                slots.push(Some(finalized));
                if is_aborted(signal) {
                    break;
                }
                continue;
            }
            Preparation::Prepared(prepared) => {
                let index = slots.len();
                slots.push(None);
                deferred.push((index, prepared));
                if is_aborted(signal) {
                    break;
                }
            }
        }
    }

    // Deferred execution pass — the upstream closure invocation point
    // (agent-loop.ts:546-549). Each spawned task first re-checks the abort
    // signal: an abort that landed during a later preflight skips `execute`
    // entirely and synthesizes the standard aborted error result, walking
    // the full `tool_execution_end` event path (#8936, agent-loop.ts:521-530).
    for (index, prepared) in deferred {
        let task_emit = emit.clone();
        let task_context = current_context.clone();
        let task_assistant_message = assistant_message.clone();
        let task_after_tool_call = config.after_tool_call.clone();
        let task_signal = signal.clone();
        set.spawn(async move {
            if is_aborted(&task_signal) {
                let finalized = FinalizedToolCallOutcome {
                    tool_call: prepared.tool_call.clone(),
                    result: create_error_tool_result("Operation aborted".to_owned()),
                    is_error: true,
                    // The abort landed before `execute` was entered.
                    duration_ms: None,
                };
                emit_tool_execution_end(&finalized, &task_emit).await;
                return (index, finalized);
            }
            let executed = execute_prepared_tool_call(&prepared, &task_signal, &task_emit).await;
            let finalized = finalize_executed_tool_call(
                &task_context,
                &task_assistant_message,
                &prepared,
                executed,
                &task_after_tool_call,
                &task_signal,
            )
            .await;
            emit_tool_execution_end(&finalized, &task_emit).await;
            (index, finalized)
        });
    }

    let mut join_errors: Vec<String> = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((index, finalized)) => slots[index] = Some(finalized),
            // Tasks are infallible by construction; a JoinError means a panic
            // or abort. Fill the missing slots with error results below.
            Err(error) => {
                tracing::error!(%error, "parallel tool task failed");
                join_errors.push(error.to_string());
            }
        }
    }
    if !join_errors.is_empty() {
        let message = join_errors.join("; ");
        for (index, slot) in slots.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = Some(FinalizedToolCallOutcome {
                    tool_call: tool_calls[index].clone(),
                    result: create_error_tool_result(format!("Tool task failed: {message}")),
                    is_error: true,
                    duration_ms: None,
                });
                // P1-4: the `tool_execution_start` for this call was already
                // emitted before spawn, and the panicking task died before
                // its final `emit_tool_execution_end` — pair the event here
                // (still before the tool-result message) so start/end
                // consumers (`pending_tool_calls`, extension spinners, RPC)
                // do not leak a permanently "running" tool.
                emit_tool_execution_end(slot.as_ref().expect("just filled"), emit).await;
            }
        }
    }

    let finalized_calls: Vec<FinalizedToolCallOutcome> = slots.into_iter().flatten().collect();
    let mut messages: Vec<ToolResultMessage> = Vec::new();
    for finalized in &finalized_calls {
        let summary = config
            .nested_call_summary
            .as_ref()
            .and_then(|summary| summary(&finalized.tool_call.id));
        let tool_result_message = create_tool_result_message(finalized, summary.as_ref());
        emit_tool_result_message(&tool_result_message, emit).await;
        messages.push(tool_result_message);
    }

    ExecutedToolCallBatch {
        messages,
        terminate: should_terminate_tool_batch(&finalized_calls),
    }
}

/// `shouldTerminateToolBatch` (agent-loop.ts:582-584).
fn should_terminate_tool_batch(finalized_calls: &[FinalizedToolCallOutcome]) -> bool {
    !finalized_calls.is_empty()
        && finalized_calls
            .iter()
            .all(|finalized| finalized.result.terminate == Some(true))
}

/// `prepareToolCallArguments` (agent-loop.ts:586-598).
fn prepare_tool_call_arguments(
    tool: &Arc<dyn AgentTool>,
    tool_call: &AgentToolCall,
) -> AgentToolCall {
    let original = Value::Object(tool_call.arguments.clone());
    let prepared = tool.prepare_arguments(original.clone());
    if prepared == original {
        return tool_call.clone();
    }
    let mut prepared_call = tool_call.clone();
    if let Value::Object(map) = prepared {
        prepared_call.arguments = map;
    }
    prepared_call
}

/// `prepareToolCall` (agent-loop.ts:600-664). Validation/lookup failures
/// become immediate error results; they never interrupt the loop (upstream
/// catch-all).
async fn prepare_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    tool_call: &AgentToolCall,
    before_tool_call: &Option<BeforeToolCallFn>,
    signal: &Option<CancellationToken>,
) -> Preparation {
    let Some(tool) = current_context
        .tools
        .as_ref()
        .and_then(|tools| tools.iter().find(|t| t.name() == tool_call.name))
        .cloned()
    else {
        return Preparation::Immediate {
            result: create_error_tool_result(format!("Tool {} not found", tool_call.name)),
            is_error: true,
        };
    };

    let prepared_call = prepare_tool_call_arguments(&tool, tool_call);
    let schema_tool = Tool {
        name: tool.name().to_owned(),
        description: tool.description().to_owned(),
        parameters: tool.parameters().clone(),
        constrained_sampling: tool.constrained_sampling(),
    };
    let mut validated_args = match validate_tool_arguments(&schema_tool, &prepared_call) {
        Ok(args) => args,
        Err(message) => {
            return Preparation::Immediate {
                result: create_error_tool_result(message),
                is_error: true,
            };
        }
    };

    if let Some(before_tool_call) = before_tool_call {
        let before_context = BeforeToolCallContext {
            assistant_message: assistant_message.clone(),
            tool_call: tool_call.clone(),
            args: validated_args.clone(),
            context: current_context.clone(),
        };
        let before_result =
            before_tool_call(before_context, signal.clone().unwrap_or_default()).await;
        if is_aborted(signal) {
            return Preparation::Immediate {
                result: create_error_tool_result("Operation aborted".to_owned()),
                is_error: true,
            };
        }
        if let Some(result) = before_result {
            if result.block == Some(true) {
                let mut error_result = create_error_tool_result(
                    result
                        .reason
                        .unwrap_or_else(|| "Tool execution was blocked".to_owned()),
                );
                // Upstream agent-loop.ts:638-640: propagate the terminate hint
                // onto the synthesized error tool result so the batch-level
                // `every()` check sees it.
                if result.terminate == Some(true) {
                    error_result.terminate = Some(true);
                }
                return Preparation::Immediate {
                    result: error_result,
                    is_error: true,
                };
            }
            // Mutated hook args reach `execute` without revalidation
            // (upstream in-place mutation of the validated args object).
            if let Some(mutated_args) = result.args {
                validated_args = mutated_args;
            }
        }
    }
    if is_aborted(signal) {
        return Preparation::Immediate {
            result: create_error_tool_result("Operation aborted".to_owned()),
            is_error: true,
        };
    }
    Preparation::Prepared(PreparedToolCall {
        tool_call: tool_call.clone(),
        tool,
        args: validated_args,
    })
}

/// `executePreparedToolCall` (agent-loop.ts:666-707).
///
/// Update settle semantics: updates emitted after `execute` returns are
/// ignored; already-queued update events are awaited before returning.
/// `Err` from `execute` becomes an error result.
/// Build the loop's `tool_execution_update` emit sink, parking genuinely
/// async emits in `update_events` for the caller to settle.
fn emit_tool_update_sink(
    prepared: &PreparedToolCall,
    emit: &AgentEventSink,
    update_events: Arc<Mutex<Vec<BoxFuture<'static, ()>>>>,
) -> crate::types::AgentToolUpdateCallback {
    let emit = emit.clone();
    let tool_call_id = prepared.tool_call.id.clone();
    let tool_name = prepared.tool_call.name.clone();
    let args = Value::Object(prepared.tool_call.arguments.clone());
    Box::new(move |partial_result: AgentToolResult| {
        let event = AgentEvent::ToolExecutionUpdate {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            args: args.clone(),
            partial_result: serde_json::to_value(&partial_result).unwrap_or(Value::Null),
            parent_tool_call_id: None,
        };
        let mut update = emit(event);
        // Upstream emits the update synchronously (`emitEvent` enqueues
        // into the event stream before returning). The Rust sink returns
        // a future, and parking it in the settle list until `execute`
        // returns batches every partial frame of a long tool call into
        // one post-execute avalanche — live progress never reaches the
        // UI while the tool runs. Poll once with a noop waker instead:
        // sinks whose first step is a synchronous enqueue (the stream
        // sink) complete here, frame by frame; genuinely async sinks
        // (the Agent barrier) park pending in the settle list and keep
        // the original await-before-return semantics.
        {
            let waker = futures::task::noop_waker();
            let mut cx = std::task::Context::from_waker(&waker);
            if update.as_mut().poll(&mut cx).is_pending() {
                lock(&update_events).push(update);
            }
        }
    })
}

async fn execute_prepared_tool_call(
    prepared: &PreparedToolCall,
    signal: &Option<CancellationToken>,
    emit: &AgentEventSink,
) -> ExecutedToolCallOutcome {
    let update_events: Arc<Mutex<Vec<BoxFuture<'static, ()>>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = emit_tool_update_sink(prepared, emit, update_events.clone());
    let outcome = execute_prepared_tool_call_with_sink(prepared, signal, Some(sink)).await;
    // Await already-queued update events (upstream `Promise.all(updateEvents)`;
    // awaited sequentially here to keep event order deterministic).
    let queued = std::mem::take(&mut *lock(&update_events));
    for update in queued {
        update.await;
    }
    outcome
}

/// Like [`execute_prepared_tool_call`] but with a caller-provided update
/// sink (used by [`run_tool_call`] for nested `ctx.executeTool()` calls,
/// which emit their own `tool_execution_update` events). Updates emitted
/// after `execute` returns are ignored (settle semantics).
async fn execute_prepared_tool_call_with_sink(
    prepared: &PreparedToolCall,
    signal: &Option<CancellationToken>,
    sink: Option<crate::types::AgentToolUpdateCallback>,
) -> ExecutedToolCallOutcome {
    let accepting_updates = Arc::new(AtomicBool::new(true));
    // `const startedAt = performance.now()` (agent-loop.ts:821).
    let started_at = std::time::Instant::now();
    let on_update: crate::types::AgentToolUpdateCallback = {
        let accepting_updates = accepting_updates.clone();
        Box::new(move |partial_result: AgentToolResult| {
            if !accepting_updates.load(Ordering::SeqCst) {
                return;
            }
            if let Some(sink) = &sink {
                sink(partial_result);
            }
        })
    };

    let result = prepared
        .tool
        .execute(
            &prepared.tool_call.id,
            prepared.args.clone(),
            signal.clone().unwrap_or_default(),
            Some(on_update),
        )
        .await;
    // `const startedAt = performance.now(); const elapsed = () =>
    // Math.round(performance.now() - startedAt)` (agent-loop.ts:820-825):
    // one monotonic clock around `execute()`, the same data source the
    // tool-result message records (R2.7.2).
    let duration_ms = started_at.elapsed().as_millis() as u64;
    accepting_updates.store(false, Ordering::SeqCst);

    match result {
        Ok(result) => {
            // Upstream `executePreparedToolCall`: `isError: result.isError
            // === true` — a tool may report a data-carrying failure without
            // throwing (V16-06 FR-C R2).
            let is_error = result.is_error.unwrap_or(false);
            ExecutedToolCallOutcome {
                result,
                is_error,
                duration_ms,
            }
        }
        Err(error) => ExecutedToolCallOutcome {
            result: create_error_tool_result(error.to_string()),
            is_error: true,
            duration_ms,
        },
    }
}

/// `finalizeExecutedToolCall` (agent-loop.ts:853-903). The five
/// `afterToolCall` fields replace independently (no deep merge). Upstream
/// wraps the hook in try/catch and downgrades a throw to an error result;
/// `Err` from the Rust hook gets the same treatment.
async fn finalize_executed_tool_call(
    current_context: &AgentContext,
    assistant_message: &AssistantMessage,
    prepared: &PreparedToolCall,
    executed: ExecutedToolCallOutcome,
    after_tool_call: &Option<AfterToolCallFn>,
    signal: &Option<CancellationToken>,
) -> FinalizedToolCallOutcome {
    let mut result = executed.result;
    let mut is_error = executed.is_error;

    if let Some(after_tool_call) = after_tool_call {
        let after_context = AfterToolCallContext {
            assistant_message: assistant_message.clone(),
            tool_call: prepared.tool_call.clone(),
            args: prepared.args.clone(),
            result: result.clone(),
            is_error,
            context: current_context.clone(),
        };
        match after_tool_call(after_context, signal.clone().unwrap_or_default()).await {
            Ok(Some(after_result)) => {
                // Upstream `finalizeExecutedToolCall` (agent-loop.ts:878-888):
                // structured content not replaced along with the content may
                // no longer match it, so replacing `content` without
                // restating it drops the structured content.
                let structured_content = after_result.structured_content.clone().or_else(|| {
                    if after_result.content.is_some() {
                        None
                    } else {
                        result.structured_content.clone()
                    }
                });
                if let Some(content) = after_result.content {
                    result.content = content;
                }
                if let Some(details) = after_result.details {
                    result.details = details;
                }
                result.structured_content = structured_content;
                if let Some(usage) = after_result.usage {
                    result.usage = Some(usage);
                }
                if let Some(terminate) = after_result.terminate {
                    result.terminate = Some(terminate);
                }
                if let Some(after_is_error) = after_result.is_error {
                    is_error = after_is_error;
                }
            }
            Ok(None) => {}
            // Upstream catch (agent-loop.ts:743-746): a failing hook replaces
            // the whole outcome with an error result carrying the error text.
            Err(error) => {
                result = create_error_tool_result(error.to_string());
                is_error = true;
            }
        }
    }

    FinalizedToolCallOutcome {
        tool_call: prepared.tool_call.clone(),
        result,
        is_error,
        duration_ms: Some(executed.duration_ms),
    }
}

/// `createErrorToolResult` (agent-loop.ts:756-761).
fn create_error_tool_result(message: String) -> AgentToolResult {
    AgentToolResult {
        content: vec![ToolResultContent::Text(TextContent {
            text: message,
            text_signature: None,
        })],
        details: json!({}),
        ..Default::default()
    }
}

/// `emitToolExecutionEnd` (agent-loop.ts:763-771).
async fn emit_tool_execution_end(finalized: &FinalizedToolCallOutcome, emit: &AgentEventSink) {
    emit(AgentEvent::ToolExecutionEnd {
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        result: serde_json::to_value(&finalized.result).unwrap_or(Value::Null),
        is_error: finalized.is_error,
        duration_ms: finalized.duration_ms,
        parent_tool_call_id: None,
    })
    .await;
}

/// `createToolResultMessage` (agent-loop.ts:773-787).
fn create_tool_result_message(
    finalized: &FinalizedToolCallOutcome,
    nested: Option<&crate::nested_tool_calls::NestedCallSummary>,
) -> ToolResultMessage {
    // `_handleAgentEvent` (agent-session.ts:1074-1086 @ abe508e1b): the
    // bounded nested-call record rides the message, and the nested usage is
    // combined into the message usage. Nested results never enter the
    // transcript themselves.
    let nested_calls = nested.and_then(|summary| summary.calls.clone());
    let usage = match (
        finalized.result.usage.clone(),
        nested.and_then(|summary| summary.usage.clone()),
    ) {
        (None, None) => None,
        (direct, nested) => Some(
            direct
                .unwrap_or_default()
                .combined(&nested.unwrap_or_default()),
        ),
    };
    ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        // Untyped tools can return results without content; normalize so the
        // null never enters session history or provider payloads.
        content: finalized.result.content.clone(),
        // `AgentToolResult.details` is a required `Value`; `null` maps to the
        // upstream `undefined` (key omitted on the wire).
        details: if finalized.result.details.is_null() {
            None
        } else {
            Some(finalized.result.details.clone())
        },
        usage,
        nested_calls,
        is_error: finalized.is_error,
        timestamp: now_millis(),
        duration_ms: finalized.duration_ms,
    }
}

/// `emitToolResultMessage` (agent-loop.ts:789-792).
async fn emit_tool_result_message(message: &ToolResultMessage, emit: &AgentEventSink) {
    emit(AgentEvent::MessageStart {
        message: AgentMessage::ToolResult(message.clone()),
    })
    .await;
    emit(AgentEvent::MessageEnd {
        message: AgentMessage::ToolResult(message.clone()),
    })
    .await;
}
