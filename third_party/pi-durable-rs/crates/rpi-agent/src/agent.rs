//! Port of `packages/agent/src/agent.ts` @ pi 0.82.1 (commit 2efa728), plus
//! the `AgentState` type of `packages/agent/src/types.ts`.
//!
//! Stateful wrapper around the low-level agent loop (`agent_loop.rs`). `Agent`
//! owns the current transcript, emits lifecycle events, and exposes queueing
//! APIs for steering and follow-up messages.
//!
//! Intentional differences from upstream (all structural, semantics kept):
//! - JS single-threaded mutable fields become `&self` methods over interior
//!   mutability (`Mutex`); no lock is ever held across an `.await`.
//! - `activeRun.promise` becomes a per-run "done" [`CancellationToken`];
//!   `wait_for_idle()` awaits its cancellation.
//! - `processEvents` looks up `activeRun` upstream to find the listener signal
//!   and throws when missing; that state is unreachable in both versions (the
//!   sink only exists inside a run), so the signal is passed explicitly.
//! - Thrown `Error`s become `Err(AgentError::Message)` whose `Display` is the
//!   verbatim upstream message text.
//! - `continue()` is named [`Agent::continue_run`] (`continue` is a Rust
//!   keyword).

use std::collections::HashSet;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use futures::future::BoxFuture;
use rpi_ai::types::{
    ApiKind, AssistantContent, AssistantMessage, AssistantRole, ImageContent, Model, ModelCost,
    StopReason, StreamOptions, TextContent, ThinkingBudgets, Transport, Usage, UserContent,
    UserContentBlock, UserMessage, UserRole,
};
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{
    AfterToolCallFn, AgentContext, AgentEventSink, AgentLoopConfig, AgentLoopTurnUpdate,
    AgentRequestUpdate, AgentTurnContext, AgentTurnDecision, BeforeToolCallFn, ConvertToLlmFn,
    FinishTurnFn, GetApiKeyFn, GetQueuedMessagesFn, NestedCallSummaryFn, PrepareNextTurnContext,
    PrepareNextTurnFn, PrepareRequestContext, PrepareRequestFn, TransformContextFn, now_millis,
    run_agent_loop, run_agent_loop_continue, thinking_level_from_model_level,
};
use crate::error::AgentError;
use crate::messages::AgentMessage;
use crate::stream_fn::StreamFn;
use crate::types::{AgentEvent, AgentTool, QueueMode, ThinkingLevel, ToolExecutionMode};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Agent event listener (upstream `Agent.subscribe` listener). Listeners are
/// awaited in subscription order as part of the run settlement; they receive
/// the active run's abort signal.
pub type AgentListener =
    Arc<dyn Fn(AgentEvent, CancellationToken) -> BoxFuture<'static, ()> + Send + Sync>;

/// `finishTurn` (agent.ts:125 @ abe508e1b, agent-side variant). Receives the
/// active run's abort signal as the second argument (upstream
/// `this.signal`).
pub type FinishTurnAgentFn = Arc<
    dyn Fn(AgentTurnContext, CancellationToken) -> BoxFuture<'static, Option<AgentTurnDecision>>
        + Send
        + Sync,
>;

/// `prepareRequest` (agent-side variant; agent.ts). Receives the active run's
/// abort signal as the second argument (upstream `this.signal`).
pub type PrepareRequestAgentFn = Arc<
    dyn Fn(
            PrepareRequestContext,
            CancellationToken,
        ) -> BoxFuture<'static, Option<AgentRequestUpdate>>
        + Send
        + Sync,
>;

/// `prepareNextTurn` (signal-only variant, agent.ts:191-193).
pub type PrepareNextTurnSignalFn =
    Arc<dyn Fn(CancellationToken) -> BoxFuture<'static, Option<AgentLoopTurnUpdate>> + Send + Sync>;

/// `prepareNextTurnWithContext` (agent.ts:194-197).
pub type PrepareNextTurnWithContextFn = Arc<
    dyn Fn(
            PrepareNextTurnContext,
            CancellationToken,
        ) -> BoxFuture<'static, Option<AgentLoopTurnUpdate>>
        + Send
        + Sync,
>;

// ---------------------------------------------------------------------------
// AgentState (types.ts:327-352)
// ---------------------------------------------------------------------------

/// `AgentState` — public agent state snapshot.
///
/// `tools` and `messages` use copy-on-assign semantics upstream; in Rust the
/// snapshot returned by [`Agent::state`] is already a copy, and the setters
/// ([`Agent::set_tools`]/[`Agent::set_messages`]) copy the provided arrays.
#[derive(Clone)]
pub struct AgentState {
    /// System prompt sent with each model request.
    pub system_prompt: String,
    /// Active model used for future turns.
    pub model: Model,
    /// Requested reasoning level for future turns.
    pub thinking_level: ThinkingLevel,
    /// Available tools.
    pub tools: Vec<Arc<dyn AgentTool>>,
    /// Conversation transcript.
    pub messages: Vec<AgentMessage>,
    /// True while the agent is processing a prompt or continuation. Remains
    /// true until awaited `agent_end` listeners settle.
    pub is_streaming: bool,
    /// Partial assistant message for the current streamed response, if any.
    /// V15-13 (rpi#53): shared `Arc` — the per-delta reduction is a
    /// refcount bump, not a deep copy. Only assistant partials are tracked
    /// (`MessageUpdate` fires for assistant streams only); non-assistant
    /// `MessageStart` events clear the slot.
    pub streaming_message: Option<Arc<AssistantMessage>>,
    /// Tool call ids currently executing.
    pub pending_tool_calls: HashSet<String>,
    /// Error message from the most recent failed or aborted assistant turn.
    pub error_message: Option<String>,
}

type MutableAgentState = AgentState;

/// `DEFAULT_MODEL` (agent.ts:47-58).
fn default_model() -> Model {
    Model {
        id: "unknown".to_owned(),
        name: "unknown".to_owned(),
        api: ApiKind::from("unknown"),
        provider: "unknown".to_owned(),
        base_url: String::new(),
        reasoning: false,
        thinking_level_map: None,
        input: Vec::new(),
        input_limits: None,
        cost: ModelCost::default(),
        prompt_cache: None,
        context_window: 0,
        max_tokens: 0,
        headers: None,
        compat: None,
        sampling_params: None,
        sampling_params_by_thinking_level: None,
    }
}

/// `createMutableAgentState` (agent.ts:67-94) — initial-state input for
/// [`AgentOptions`]. Unset fields fall back to the upstream defaults
/// (`""` / `DEFAULT_MODEL` / `"off"` / `[]` / `[]`).
#[derive(Default)]
pub struct InitialAgentState {
    pub system_prompt: Option<String>,
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
    pub tools: Option<Vec<Arc<dyn AgentTool>>>,
    pub messages: Option<Vec<AgentMessage>>,
}

// ---------------------------------------------------------------------------
// AgentOptions (agent.ts:96-121)
// ---------------------------------------------------------------------------

/// Options for constructing an [`Agent`].
pub struct AgentOptions {
    pub initial_state: InitialAgentState,
    pub convert_to_llm: Option<ConvertToLlmFn>,
    pub transform_context: Option<TransformContextFn>,
    pub stream_fn: StreamFn,
    pub get_api_key: Option<GetApiKeyFn>,
    pub on_payload: Option<rpi_ai::types::OnPayloadCallback>,
    pub on_response: Option<rpi_ai::types::OnResponseCallback>,
    pub before_tool_call: Option<BeforeToolCallFn>,
    pub after_tool_call: Option<AfterToolCallFn>,
    /// V16-06 FR-E: bounded nested-call record lookup (session-installed;
    /// see [`crate::agent_loop::NestedCallSummaryFn`]).
    pub nested_call_summary: Option<NestedCallSummaryFn>,
    pub finish_turn: Option<FinishTurnAgentFn>,
    pub prepare_request: Option<PrepareRequestAgentFn>,
    pub prepare_next_turn: Option<PrepareNextTurnSignalFn>,
    pub prepare_next_turn_with_context: Option<PrepareNextTurnWithContextFn>,
    pub steering_mode: Option<QueueMode>,
    pub follow_up_mode: Option<QueueMode>,
    pub session_id: Option<String>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub transport: Option<Transport>,
    pub max_retry_delay_ms: Option<u64>,
    pub tool_execution: Option<ToolExecutionMode>,
}

impl AgentOptions {
    /// `streamFn` is required upstream; there is no default provider inside
    /// this crate (coding-standards §4.2).
    pub fn new(stream_fn: StreamFn) -> Self {
        Self {
            initial_state: InitialAgentState::default(),
            convert_to_llm: None,
            transform_context: None,
            stream_fn,
            get_api_key: None,
            on_payload: None,
            on_response: None,
            before_tool_call: None,
            after_tool_call: None,
            nested_call_summary: None,
            finish_turn: None,
            prepare_request: None,
            prepare_next_turn: None,
            prepare_next_turn_with_context: None,
            steering_mode: None,
            follow_up_mode: None,
            session_id: None,
            thinking_budgets: None,
            transport: None,
            max_retry_delay_ms: None,
            tool_execution: None,
        }
    }
}

// ---------------------------------------------------------------------------
// PendingMessageQueue (agent.ts:123-157)
// ---------------------------------------------------------------------------

struct PendingMessageQueue {
    messages: Vec<AgentMessage>,
    mode: QueueMode,
}

impl PendingMessageQueue {
    fn new(mode: QueueMode) -> Self {
        Self {
            messages: Vec::new(),
            mode,
        }
    }

    fn enqueue(&mut self, message: AgentMessage) {
        self.messages.push(message);
    }

    fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    /// `peek()` (agent.ts:159-162 @ abe508e1b): the next batch a drain would
    /// deliver, without consuming it.
    fn peek(&self) -> Vec<AgentMessage> {
        if self.mode == QueueMode::All {
            return self.messages.clone();
        }
        self.messages.first().cloned().into_iter().collect()
    }

    fn drain(&mut self) -> Vec<AgentMessage> {
        if self.mode == QueueMode::All {
            return std::mem::take(&mut self.messages);
        }
        if self.messages.is_empty() {
            return Vec::new();
        }
        vec![self.messages.remove(0)]
    }

    fn clear(&mut self) {
        self.messages.clear();
    }
}

// ---------------------------------------------------------------------------
// ActiveRun (agent.ts:159-163)
// ---------------------------------------------------------------------------

/// Per-run handles. `signal` is the abort token (upstream
/// `abortController.signal`); cancelling `done` resolves `wait_for_idle()`
/// (upstream `activeRun.promise`).
#[derive(Clone)]
struct ActiveRun {
    signal: CancellationToken,
    done: CancellationToken,
}

// ---------------------------------------------------------------------------
// Prompt input (normalizePromptInput, agent.ts:379-396)
// ---------------------------------------------------------------------------

/// Input accepted by [`Agent::prompt`] (upstream
/// `string | AgentMessage | AgentMessage[]`).
pub enum PromptInput {
    Text(String),
    TextWithImages {
        text: String,
        images: Vec<ImageContent>,
    },
    /// Boxed to keep the enum small (`clippy::large_enum_variant`).
    Message(Box<AgentMessage>),
    Messages(Vec<AgentMessage>),
}

impl From<String> for PromptInput {
    fn from(text: String) -> Self {
        PromptInput::Text(text)
    }
}

impl From<&str> for PromptInput {
    fn from(text: &str) -> Self {
        PromptInput::Text(text.to_owned())
    }
}

impl From<AgentMessage> for PromptInput {
    fn from(message: AgentMessage) -> Self {
        PromptInput::Message(Box::new(message))
    }
}

impl From<Vec<AgentMessage>> for PromptInput {
    fn from(messages: Vec<AgentMessage>) -> Self {
        PromptInput::Messages(messages)
    }
}

impl PromptInput {
    fn into_messages(self) -> Vec<AgentMessage> {
        match self {
            PromptInput::Text(text) => Self::text_messages(text, Vec::new()),
            PromptInput::TextWithImages { text, images } => Self::text_messages(text, images),
            PromptInput::Message(message) => vec![*message],
            PromptInput::Messages(messages) => messages,
        }
    }

    fn text_messages(text: String, images: Vec<ImageContent>) -> Vec<AgentMessage> {
        let mut content: Vec<UserContentBlock> = vec![UserContentBlock::Text(TextContent {
            text,
            text_signature: None,
        })];
        content.extend(images.into_iter().map(UserContentBlock::Image));
        vec![AgentMessage::User(UserMessage {
            role: UserRole::User,
            content: UserContent::Blocks(content),
            timestamp: now_millis(),
        })]
    }
}

/// `defaultConvertToLlm` (agent.ts:34-42 @ #9548): keep
/// system/user/assistant/toolResult — the transcript's system messages pass
/// through so the prompt and tool declarations ride the LLM context.
fn default_convert_to_llm() -> ConvertToLlmFn {
    Arc::new(|messages: Vec<AgentMessage>| {
        Box::pin(async move {
            messages
                .into_iter()
                .filter_map(|message| match message {
                    AgentMessage::User(u) => Some(rpi_ai::types::Message::User(u)),
                    AgentMessage::Assistant(a) => Some(rpi_ai::types::Message::Assistant(a)),
                    AgentMessage::ToolResult(t) => Some(rpi_ai::types::Message::ToolResult(t)),
                    AgentMessage::System(s) => Some(rpi_ai::types::Message::System(s)),
                    _ => None,
                })
                .collect()
        })
    })
}

// ---------------------------------------------------------------------------
// Agent (agent.ts:171-577)
// ---------------------------------------------------------------------------

/// `Transport` → `u8` for the interior-mutable field. Kept local: the
/// decoders only need a total (lossless) mapping.
fn transport_to_u8(transport: Transport) -> u8 {
    match transport {
        Transport::Sse => 0,
        Transport::Websocket => 1,
        Transport::WebsocketCached => 2,
        Transport::Auto => 3,
    }
}

fn transport_from_u8(value: u8) -> Transport {
    match value {
        0 => Transport::Sse,
        1 => Transport::Websocket,
        2 => Transport::WebsocketCached,
        _ => Transport::Auto,
    }
}

/// Stateful wrapper around the low-level agent loop.
///
/// All methods take `&self`; wrap in `Arc` to share across tasks. Event
/// listeners are awaited in subscription order (barrier semantics,
/// requirements §4.3.16): the agent does not become idle until all awaited
/// listeners for `agent_end` have settled.
pub struct Agent {
    state: Arc<Mutex<MutableAgentState>>,
    listeners: Arc<Mutex<Vec<(u64, AgentListener)>>>,
    next_listener_id: AtomicU64,
    steering_queue: Arc<Mutex<PendingMessageQueue>>,
    follow_up_queue: Arc<Mutex<PendingMessageQueue>>,
    active_run: Arc<Mutex<Option<ActiveRun>>>,

    pub convert_to_llm: ConvertToLlmFn,
    /// `transformContext` — interior-mutable so the session can wrap it
    /// post-construction (#9548 `_installAgentForcedPromptProjection`).
    transform_context: std::sync::RwLock<Option<TransformContextFn>>,
    pub stream_function: StreamFn,
    pub get_api_key: Option<GetApiKeyFn>,
    pub on_payload: Option<rpi_ai::types::OnPayloadCallback>,
    pub on_response: Option<rpi_ai::types::OnResponseCallback>,
    pub before_tool_call: Option<BeforeToolCallFn>,
    pub after_tool_call: Option<AfterToolCallFn>,
    /// `nestedCallSummary` — interior-mutable because the session installs
    /// its runner after building the shared agent (V16-06 FR-E).
    nested_call_summary: RwLock<Option<NestedCallSummaryFn>>,
    /// `finishTurn` — interior-mutable because the session installs the
    /// boundary-dispatch chain after building the shared agent (upstream
    /// assigns `this.agent.finishTurn` post-construction,
    /// agent-session.ts:858-868 @ abe508e1b).
    finish_turn: RwLock<Option<FinishTurnAgentFn>>,
    /// `prepareRequest` — interior-mutable because the session layer installs
    /// the canonical-context projection after building the shared agent
    /// (upstream assigns `this.agent.prepareRequest` post-construction,
    /// agent-session.ts:754-755).
    prepare_request: RwLock<Option<PrepareRequestAgentFn>>,
    /// `prepareNextTurn` (signal-only) / `prepareNextTurnWithContext` —
    /// interior-mutable because the session layer installs the next-turn
    /// refresh chain after building the shared agent (upstream assigns
    /// `this.agent.prepareNextTurnWithContext` post-construction,
    /// agent-session.ts:563-580).
    prepare_next_turn: RwLock<Option<PrepareNextTurnSignalFn>>,
    prepare_next_turn_with_context: RwLock<Option<PrepareNextTurnWithContextFn>>,
    /// Session identifier forwarded to providers for cache-aware backends.
    pub session_id: Option<String>,
    /// Optional per-level thinking token budgets forwarded to the stream
    /// function.
    pub thinking_budgets: Option<ThinkingBudgets>,
    /// Preferred transport forwarded to the stream function.
    ///
    /// Interior-mutable (`AtomicU8`) because the interactive settings chain
    /// reassigns the live agent's transport
    /// (`this.session.agent.transport = transport`,
    /// interactive-mode.ts:4996-4999): the shared `Arc<Agent>` is also held
    /// by the running loop, so a plain field could not be updated after
    /// construction. Read via [`Agent::transport`], written via
    /// [`Agent::set_transport`].
    transport: AtomicU8,
    /// Optional cap for provider-requested retry delays.
    pub max_retry_delay_ms: Option<u64>,
    /// Tool execution strategy for assistant messages with multiple tool calls.
    pub tool_execution: ToolExecutionMode,
}

impl Agent {
    pub fn new(options: AgentOptions) -> Self {
        let initial = options.initial_state;
        // #9548 (agent.ts:76-85): `systemPrompt` and `tools` become the
        // leading system message unless `messages` already starts with one.
        let tools = initial.tools.unwrap_or_default();
        let mut messages = initial.messages.unwrap_or_default();
        let declared_tools: Vec<rpi_ai::types::Tool> = tools
            .iter()
            .map(|tool| rpi_ai::types::Tool {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                parameters: tool.parameters().clone(),
                constrained_sampling: tool.constrained_sampling(),
            })
            .collect();
        let initial_message = rpi_ai::utils::transcript::create_initial_system_message(
            initial.system_prompt.as_deref(),
            Some(&declared_tools),
        );
        if !matches!(messages.first(), Some(AgentMessage::System(_)))
            && let Some(initial_message) = initial_message
        {
            messages.insert(0, AgentMessage::System(initial_message));
        }
        Self {
            state: Arc::new(Mutex::new(AgentState {
                system_prompt: String::new(),
                model: initial.model.clone().unwrap_or_else(default_model),
                thinking_level: initial.thinking_level.unwrap_or(ThinkingLevel::Off),
                tools,
                messages,
                is_streaming: false,
                streaming_message: None,
                pending_tool_calls: HashSet::new(),
                error_message: None,
            })),
            listeners: Arc::new(Mutex::new(Vec::new())),
            next_listener_id: AtomicU64::new(0),
            steering_queue: Arc::new(Mutex::new(PendingMessageQueue::new(
                options.steering_mode.unwrap_or(QueueMode::OneAtATime),
            ))),
            follow_up_queue: Arc::new(Mutex::new(PendingMessageQueue::new(
                options.follow_up_mode.unwrap_or(QueueMode::OneAtATime),
            ))),
            active_run: Arc::new(Mutex::new(None)),
            convert_to_llm: options
                .convert_to_llm
                .unwrap_or_else(default_convert_to_llm),
            transform_context: std::sync::RwLock::new(options.transform_context),
            stream_function: options.stream_fn,
            get_api_key: options.get_api_key,
            on_payload: options.on_payload,
            on_response: options.on_response,
            before_tool_call: options.before_tool_call,
            after_tool_call: options.after_tool_call,
            nested_call_summary: RwLock::new(options.nested_call_summary),
            finish_turn: RwLock::new(options.finish_turn),
            prepare_request: RwLock::new(options.prepare_request),
            prepare_next_turn: RwLock::new(options.prepare_next_turn),
            prepare_next_turn_with_context: RwLock::new(options.prepare_next_turn_with_context),
            session_id: options.session_id,
            thinking_budgets: options.thinking_budgets,
            transport: AtomicU8::new(transport_to_u8(
                options.transport.unwrap_or(Transport::Auto),
            )),
            max_retry_delay_ms: options.max_retry_delay_ms,
            tool_execution: options
                .tool_execution
                .unwrap_or(ToolExecutionMode::Parallel),
        }
    }

    /// Subscribe to agent lifecycle events (agent.ts:243-246).
    ///
    /// Listener futures are awaited in subscription order and are included in
    /// the current run's settlement. Returns an unsubscribe closure.
    pub fn subscribe(&self, listener: AgentListener) -> Box<dyn FnOnce() + Send> {
        let id = self.next_listener_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.listeners).push((id, listener));
        let listeners = self.listeners.clone();
        Box::new(move || {
            lock(&listeners).retain(|(listener_id, _)| *listener_id != id);
        })
    }

    /// Post-construction install of `prepareNextTurnWithContext`
    /// (`this.agent.prepareNextTurnWithContext = ...`, agent-session.ts:569).
    /// Replaces any previous chain; pass `None` to clear.
    pub fn set_prepare_next_turn_with_context(&self, hook: Option<PrepareNextTurnWithContextFn>) {
        *self
            .prepare_next_turn_with_context
            .write()
            .unwrap_or_else(|e| e.into_inner()) = hook;
    }

    /// Post-construction read of the installed `prepareNextTurnWithContext`
    /// chain (used when re-wrapping, agent-session.ts:565-568).
    pub fn prepare_next_turn_with_context(&self) -> Option<PrepareNextTurnWithContextFn> {
        self.prepare_next_turn_with_context
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Post-construction read of the installed signal-only `prepareNextTurn`
    /// chain (the `?? this.agent.prepareNextTurn` fallback,
    /// agent-session.ts:537-540).
    pub fn prepare_next_turn_signal_only(&self) -> Option<PrepareNextTurnSignalFn> {
        self.prepare_next_turn
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Post-construction install/clear of the `finishTurn` hook
    /// (`this.agent.finishTurn = ...`, agent-session.ts:858-868 @
    /// abe508e1b).
    pub fn set_finish_turn(&self, hook: Option<FinishTurnAgentFn>) {
        *self.finish_turn.write().unwrap_or_else(|e| e.into_inner()) = hook;
    }

    /// Current `finishTurn` hook (cloned; the loop config wraps it with the
    /// active run's signal).
    pub fn finish_turn(&self) -> Option<FinishTurnAgentFn> {
        self.finish_turn
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Post-construction install/clear of the `prepareRequest` hook
    /// (`this.agent.prepareRequest = ...`, agent-session.ts:760-761 @
    /// abe508e1b).
    pub fn set_prepare_request(&self, hook: Option<PrepareRequestAgentFn>) {
        *self
            .prepare_request
            .write()
            .unwrap_or_else(|e| e.into_inner()) = hook;
    }

    /// Current `prepareRequest` hook (cloned; the loop config wraps it with
    /// the active run's signal).
    pub fn prepare_request(&self) -> Option<PrepareRequestAgentFn> {
        self.prepare_request
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The currently installed `transformContext` hook.
    pub fn transform_context(&self) -> Option<TransformContextFn> {
        self.transform_context
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Replace the `transformContext` hook (session-level wrapping, #9548).
    pub fn set_transform_context(&self, transform: Option<TransformContextFn>) {
        *self
            .transform_context
            .write()
            .unwrap_or_else(|e| e.into_inner()) = transform;
    }

    /// Install the nested-call summary hook (V16-06 FR-E); the session
    /// replaces it on the shared agent without rebuilding the loop config.
    pub fn set_nested_call_summary(&self, summary: Option<NestedCallSummaryFn>) {
        *self
            .nested_call_summary
            .write()
            .unwrap_or_else(|e| e.into_inner()) = summary;
    }

    /// Current agent state (snapshot copy). `system_prompt` is derived at
    /// snapshot time from the transcript's system messages (#9548 getter
    /// semantics, agent.ts:88-90); writing it directly is not possible —
    /// prompt changes travel as system messages in `messages`.
    pub fn state(&self) -> AgentState {
        let mut snapshot = lock(&self.state).clone();
        let llm_messages: Vec<rpi_ai::types::Message> = snapshot
            .messages
            .iter()
            .filter_map(|message| match message {
                AgentMessage::System(system) => {
                    Some(rpi_ai::types::Message::System(system.clone()))
                }
                _ => None,
            })
            .collect();
        snapshot.system_prompt =
            rpi_ai::utils::transcript::get_current_system_prompt(&llm_messages);
        snapshot
    }

    pub fn set_model(&self, model: Model) {
        lock(&self.state).model = model;
    }

    /// Current preferred transport.
    pub fn transport(&self) -> Transport {
        transport_from_u8(self.transport.load(Ordering::Relaxed))
    }

    /// `this.session.agent.transport = transport` (interactive-mode.ts:4998): reassign the live agent's transport; the next request reads it
    /// when assembling [`StreamOptions`].
    pub fn set_transport(&self, transport: Transport) {
        self.transport
            .store(transport_to_u8(transport), Ordering::Relaxed);
    }

    pub fn set_thinking_level(&self, thinking_level: ThinkingLevel) {
        lock(&self.state).thinking_level = thinking_level;
    }

    /// Assigning `tools` copies the provided top-level array (upstream setter).
    pub fn set_tools(&self, tools: Vec<Arc<dyn AgentTool>>) {
        lock(&self.state).tools = tools;
    }

    /// Assigning `messages` copies the provided top-level array (upstream
    /// setter).
    pub fn set_messages(&self, messages: Vec<AgentMessage>) {
        lock(&self.state).messages = messages;
    }

    /// Controls how queued steering messages are drained.
    pub fn steering_mode(&self) -> QueueMode {
        lock(&self.steering_queue).mode
    }

    pub fn set_steering_mode(&self, mode: QueueMode) {
        lock(&self.steering_queue).mode = mode;
    }

    /// Controls how queued follow-up messages are drained.
    pub fn follow_up_mode(&self) -> QueueMode {
        lock(&self.follow_up_queue).mode
    }

    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        lock(&self.follow_up_queue).mode = mode;
    }

    /// Queue a message to be injected after the current assistant turn
    /// finishes.
    pub fn steer(&self, message: AgentMessage) {
        lock(&self.steering_queue).enqueue(message);
    }

    /// Queue a message to run only after the agent would otherwise stop.
    pub fn follow_up(&self, message: AgentMessage) {
        lock(&self.follow_up_queue).enqueue(message);
    }

    /// Remove all queued steering messages.
    pub fn clear_steering_queue(&self) {
        lock(&self.steering_queue).clear();
    }

    /// Remove all queued follow-up messages.
    pub fn clear_follow_up_queue(&self) {
        lock(&self.follow_up_queue).clear();
    }

    /// Remove all queued steering and follow-up messages.
    pub fn clear_all_queues(&self) {
        self.clear_steering_queue();
        self.clear_follow_up_queue();
    }

    /// Returns true when either queue still contains pending messages.
    pub fn has_queued_messages(&self) -> bool {
        lock(&self.steering_queue).has_items() || lock(&self.follow_up_queue).has_items()
    }

    /// `peekQueuedMessages` (agent.ts:330-334 @ abe508e1b): preview the next
    /// queued batch without consuming it. Steering first, then follow-up.
    pub fn peek_queued_messages(&self) -> Vec<AgentMessage> {
        let steering = lock(&self.steering_queue).peek();
        if steering.is_empty() {
            lock(&self.follow_up_queue).peek()
        } else {
            steering
        }
    }

    /// Active abort signal for the current run, if any.
    pub fn signal(&self) -> Option<CancellationToken> {
        lock(&self.active_run)
            .as_ref()
            .map(|run| run.signal.clone())
    }

    /// Abort the current run, if one is active.
    pub fn abort(&self) {
        if let Some(run) = lock(&self.active_run).as_ref() {
            run.signal.cancel();
        }
    }

    /// Resolve when the current run and all awaited event listeners have
    /// finished (after `agent_end` listeners settle).
    pub async fn wait_for_idle(&self) {
        let done = lock(&self.active_run).as_ref().map(|run| run.done.clone());
        if let Some(done) = done {
            done.cancelled().await;
        }
    }

    /// `isStreaming` — true until awaited `agent_end` listeners settle.
    pub fn is_streaming(&self) -> bool {
        lock(&self.state).is_streaming
    }

    /// Clear transcript state, runtime state, and queued messages.
    ///
    /// Returns an error while a run is active (upstream agent.ts:332-345); the
    /// transcript and all other state is left untouched in that case.
    pub fn reset(&self) -> Result<(), AgentError> {
        if lock(&self.active_run).is_some() {
            return Err(AgentError::Message(
                "Agent is already processing. Wait for completion before resetting.".to_owned(),
            ));
        }
        {
            // #9548 (agent.ts:353-354): keep the replayed prompt/tool
            // baseline — the transcript restarts from the current prompt.
            let mut state = lock(&self.state);
            let llm_messages: Vec<rpi_ai::types::Message> = state
                .messages
                .iter()
                .filter_map(|message| match message {
                    AgentMessage::System(system) => {
                        Some(rpi_ai::types::Message::System(system.clone()))
                    }
                    _ => None,
                })
                .collect();
            let baseline = rpi_ai::utils::transcript::get_current_system_message(&llm_messages);
            state.messages = baseline.map(AgentMessage::System).into_iter().collect();
            state.is_streaming = false;
            state.streaming_message = None;
            state.pending_tool_calls = HashSet::new();
            state.error_message = None;
        }
        self.clear_follow_up_queue();
        self.clear_steering_queue();
        Ok(())
    }

    /// Start a new prompt from text, a single message, or a batch of messages
    /// (agent.ts:337-347).
    pub async fn prompt(&self, input: impl Into<PromptInput>) -> Result<(), AgentError> {
        if lock(&self.active_run).is_some() {
            return Err(AgentError::Message(
                "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion."
                    .to_owned(),
            ));
        }
        let messages = input.into().into_messages();
        self.run_prompt_messages(messages, false).await
    }

    /// `prompt(input: string, images?: ImageContent[])`.
    pub async fn prompt_with_images(
        &self,
        text: &str,
        images: Vec<ImageContent>,
    ) -> Result<(), AgentError> {
        self.prompt(PromptInput::TextWithImages {
            text: text.to_owned(),
            images,
        })
        .await
    }

    /// Continue from the current transcript (agent.ts:377-404, upstream
    /// `continue()`). The last message must be a user or tool-result message
    /// (a system-only transcript — e.g. right after `reset()` — is rejected
    /// with `"No messages to continue from"`, agent.ts:382-384); with an
    /// assistant tail the steering queue is drained first (skipping
    /// the initial steering poll), then the follow-up queue.
    pub async fn continue_run(&self) -> Result<(), AgentError> {
        if lock(&self.active_run).is_some() {
            return Err(AgentError::Message(
                "Agent is already processing. Wait for completion before continuing.".to_owned(),
            ));
        }

        let last_message = {
            let messages = &lock(&self.state).messages;
            // #9548 (agent.ts:382-384): a transcript holding only system
            // messages (e.g. right after `reset()`) has nothing to continue
            // from either — rejected here, before a run starts, so no error
            // turn is synthesized. `all` is vacuously true on an empty
            // transcript, mirroring upstream's `every`.
            if messages
                .iter()
                .all(|message| matches!(message, AgentMessage::System(_)))
            {
                return Err(AgentError::Message(
                    "No messages to continue from".to_owned(),
                ));
            }
            messages.last().cloned()
        };
        // Unreachable in practice (the guard above rejects empty); kept to
        // satisfy exhaustiveness — upstream's redundant `!lastMessage`.
        let Some(last_message) = last_message else {
            return Err(AgentError::Message(
                "No messages to continue from".to_owned(),
            ));
        };

        if matches!(last_message, AgentMessage::Assistant(_)) {
            let queued_steering = lock(&self.steering_queue).drain();
            if !queued_steering.is_empty() {
                return self.run_prompt_messages(queued_steering, true).await;
            }

            let queued_follow_ups = lock(&self.follow_up_queue).drain();
            if !queued_follow_ups.is_empty() {
                return self.run_prompt_messages(queued_follow_ups, false).await;
            }

            return Err(AgentError::Message(
                "Cannot continue from message role: assistant".to_owned(),
            ));
        }

        self.run_continuation().await
    }

    /// `runPromptMessages` (agent.ts:398-412).
    async fn run_prompt_messages(
        &self,
        messages: Vec<AgentMessage>,
        skip_initial_steering_poll: bool,
    ) -> Result<(), AgentError> {
        let context = self.create_context_snapshot();
        let config = self.create_loop_config(skip_initial_steering_poll);
        let stream_function = self.stream_function.clone();
        self.run_with_lifecycle(move |signal, emit| async move {
            run_agent_loop(
                messages,
                context,
                config,
                emit,
                Some(signal),
                stream_function,
            )
            .await;
            Ok(())
        })
        .await
    }

    /// `runContinuation` (agent.ts:414-424). Loop-level validation failures
    /// flow into `handleRunFailure` via the executor error, as upstream.
    async fn run_continuation(&self) -> Result<(), AgentError> {
        let context = self.create_context_snapshot();
        let config = self.create_loop_config(false);
        let stream_function = self.stream_function.clone();
        self.run_with_lifecycle(move |signal, emit| async move {
            run_agent_loop_continue(context, config, emit, Some(signal), stream_function)
                .await
                .map(|_| ())
        })
        .await
    }

    /// `createContextSnapshot` (agent.ts:426-432).
    fn create_context_snapshot(&self) -> AgentContext {
        let state = lock(&self.state);
        AgentContext {
            messages: state.messages.clone(),
            tools: Some(state.tools.clone()),
        }
    }

    /// `createLoopConfig` (agent.ts:434-469).
    fn create_loop_config(&self, skip_initial_steering_poll: bool) -> AgentLoopConfig {
        let (model, reasoning) = {
            let state = lock(&self.state);
            (
                state.model.clone(),
                thinking_level_from_model_level(state.thinking_level),
            )
        };

        let skip = Arc::new(AtomicBool::new(skip_initial_steering_poll));
        let get_steering_messages: GetQueuedMessagesFn = {
            let skip = skip.clone();
            let queue = self.steering_queue.clone();
            Arc::new(move || {
                let skip = skip.clone();
                let queue = queue.clone();
                Box::pin(async move {
                    if skip.swap(false, Ordering::SeqCst) {
                        return Vec::new();
                    }
                    lock(&queue).drain()
                })
            })
        };
        let get_follow_up_messages: GetQueuedMessagesFn = {
            let queue = self.follow_up_queue.clone();
            Arc::new(move || {
                let queue = queue.clone();
                Box::pin(async move { lock(&queue).drain() })
            })
        };

        let prepare_next_turn: Option<PrepareNextTurnFn> = {
            let with_context = self
                .prepare_next_turn_with_context
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let signal_only = self
                .prepare_next_turn
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if with_context.is_some() || signal_only.is_some() {
                let active_run = self.active_run.clone();
                Some(Arc::new(move |context: PrepareNextTurnContext| {
                    let with_context = with_context.clone();
                    let signal_only = signal_only.clone();
                    let active_run = active_run.clone();
                    Box::pin(async move {
                        // Upstream reads `this.signal` at call time.
                        let signal = lock(&active_run)
                            .as_ref()
                            .map(|run| run.signal.clone())
                            .unwrap_or_default();
                        if let Some(prepare) = with_context {
                            return prepare(context, signal).await;
                        }
                        match signal_only {
                            Some(prepare) => prepare(signal).await,
                            None => None,
                        }
                    })
                }))
            } else {
                None
            }
        };

        let finish_turn: Option<FinishTurnFn> = match self
            .finish_turn
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            Some(agent_callback) => {
                let active_run = self.active_run.clone();
                Some(Arc::new(move |context: AgentTurnContext| {
                    let agent_callback = agent_callback.clone();
                    let active_run = active_run.clone();
                    Box::pin(async move {
                        // Upstream reads `this.signal` at call time (agent.ts:461).
                        let signal = lock(&active_run)
                            .as_ref()
                            .map(|run| run.signal.clone())
                            .unwrap_or_default();
                        agent_callback(context, signal).await
                    })
                }))
            }
            _ => None,
        };

        let prepare_request: Option<PrepareRequestFn> = match self.prepare_request() {
            Some(agent_callback) => {
                let active_run = self.active_run.clone();
                Some(Arc::new(move |context: PrepareRequestContext| {
                    let agent_callback = agent_callback.clone();
                    let active_run = active_run.clone();
                    Box::pin(async move {
                        // Upstream reads `this.signal` at call time.
                        let signal = lock(&active_run)
                            .as_ref()
                            .map(|run| run.signal.clone())
                            .unwrap_or_default();
                        agent_callback(context, signal).await
                    })
                }))
            }
            None => None,
        };

        AgentLoopConfig {
            model,
            reasoning,
            thinking_budgets: self.thinking_budgets.clone(),
            stream_options: StreamOptions {
                session_id: self.session_id.clone(),
                transport: Some(self.transport()),
                request: rpi_ai::ProviderRequestOptions {
                    on_payload: self.on_payload.clone(),
                    on_response: self.on_response.clone(),
                    max_retry_delay_ms: self.max_retry_delay_ms,
                    ..Default::default()
                },
                ..Default::default()
            },
            tool_execution: self.tool_execution,
            convert_to_llm: self.convert_to_llm.clone(),
            transform_context: self
                .transform_context
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            get_api_key: self.get_api_key.clone(),
            finish_turn,
            prepare_request,
            prepare_next_turn,
            get_steering_messages: Some(get_steering_messages),
            get_follow_up_messages: Some(get_follow_up_messages),
            before_tool_call: self.before_tool_call.clone(),
            after_tool_call: self.after_tool_call.clone(),
            nested_call_summary: self
                .nested_call_summary
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }

    /// `runWithLifecycle` (agent.ts:471-494).
    async fn run_with_lifecycle<F, Fut>(&self, executor: F) -> Result<(), AgentError>
    where
        F: FnOnce(CancellationToken, AgentEventSink) -> Fut,
        Fut: Future<Output = Result<(), AgentError>>,
    {
        let run = ActiveRun {
            signal: CancellationToken::new(),
            done: CancellationToken::new(),
        };
        {
            let mut active_run = lock(&self.active_run);
            if active_run.is_some() {
                return Err(AgentError::Message(
                    "Agent is already processing.".to_owned(),
                ));
            }
            *active_run = Some(run.clone());
        }

        {
            let mut state = lock(&self.state);
            state.is_streaming = true;
            state.streaming_message = None;
            state.error_message = None;
        }

        let emit = self.make_sink(run.signal.clone());
        let result = executor(run.signal.clone(), emit).await;
        if let Err(error) = result {
            let aborted = run.signal.is_cancelled();
            self.handle_run_failure(&error, aborted, &run.signal).await;
        }
        self.finish_run();
        Ok(())
    }

    /// The barrier sink handed to the low-level loop
    /// (`(event) => this.processEvents(event)`).
    fn make_sink(&self, signal: CancellationToken) -> AgentEventSink {
        let state = self.state.clone();
        let listeners = self.listeners.clone();
        Arc::new(move |event| {
            let state = state.clone();
            let listeners = listeners.clone();
            let signal = signal.clone();
            Box::pin(async move {
                process_events(&state, &listeners, &event, &signal).await;
            })
        })
    }

    /// `handleRunFailure` (agent.ts:496-512): synthesize a failure assistant
    /// message (empty text content, stopReason aborted|error) and emit the
    /// full trailing event sequence through the barrier.
    async fn handle_run_failure(
        &self,
        error: &AgentError,
        aborted: bool,
        signal: &CancellationToken,
    ) {
        let model = lock(&self.state).model.clone();
        let failure_message = AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContent::Text(TextContent {
                text: String::new(),
                text_signature: None,
            })],
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: if aborted {
                StopReason::Aborted
            } else {
                StopReason::Error
            },
            error_message: Some(error.to_string()),
            timestamp: now_millis(),
            deferred: None,
            end_turn: None,
            raw_stop_reason: None,
            duration_ms: None,
        };
        let message = AgentMessage::Assistant(failure_message);
        let state = self.state.clone();
        let listeners = self.listeners.clone();
        process_events(
            &state,
            &listeners,
            &AgentEvent::MessageStart {
                message: message.clone(),
            },
            signal,
        )
        .await;
        process_events(
            &state,
            &listeners,
            &AgentEvent::MessageEnd {
                message: message.clone(),
            },
            signal,
        )
        .await;
        process_events(
            &state,
            &listeners,
            &AgentEvent::TurnEnd {
                message: message.clone(),
                tool_results: Vec::new(),
            },
            signal,
        )
        .await;
        process_events(
            &state,
            &listeners,
            &AgentEvent::AgentEnd {
                messages: vec![message],
            },
            signal,
        )
        .await;
    }

    /// `finishRun` (agent.ts:514-520).
    fn finish_run(&self) {
        {
            let mut state = lock(&self.state);
            state.is_streaming = false;
            state.streaming_message = None;
            state.pending_tool_calls = HashSet::new();
        }
        let run = lock(&self.active_run).take();
        if let Some(run) = run {
            run.done.cancel();
        }
    }
}

/// `processEvents` (agent.ts:529-576): reduce internal state for a loop
/// event, then await all listeners in subscription order.
///
/// `agent_end` only means no further loop events will be emitted; the run is
/// idle later, after all awaited listeners for `agent_end` finish and
/// `finish_run` clears runtime-owned state.
async fn process_events(
    state: &Arc<Mutex<MutableAgentState>>,
    listeners: &Arc<Mutex<Vec<(u64, AgentListener)>>>,
    event: &AgentEvent,
    signal: &CancellationToken,
) {
    {
        let mut state = lock(state);
        match event {
            AgentEvent::MessageStart { message } => {
                // Once per message (not per delta) — an extraction clone is
                // fine here; the streaming hot path is the update arm below.
                state.streaming_message = match message {
                    AgentMessage::Assistant(assistant) => Some(Arc::new(assistant.clone())),
                    _ => None,
                };
            }
            AgentEvent::MessageUpdate { message, .. } => {
                // V15-13: refcount share (was a full message clone per delta).
                state.streaming_message = Some(Arc::clone(message));
            }
            AgentEvent::MessageEnd { message } => {
                state.streaming_message = None;
                state.messages.push(message.clone());
            }
            AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                state.pending_tool_calls.insert(tool_call_id.clone());
            }
            AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                state.pending_tool_calls.remove(tool_call_id);
            }
            AgentEvent::TurnEnd { message, .. } => {
                if let AgentMessage::Assistant(assistant) = message
                    && let Some(error_message) = &assistant.error_message
                {
                    state.error_message = Some(error_message.clone());
                }
            }
            AgentEvent::AgentEnd { .. } => {
                state.streaming_message = None;
            }
            AgentEvent::AgentStart
            | AgentEvent::TurnStart
            | AgentEvent::ToolExecutionUpdate { .. } => {}
        }
    }

    let snapshot: Vec<AgentListener> = lock(listeners)
        .iter()
        .map(|(_, listener)| listener.clone())
        .collect();
    for listener in snapshot {
        listener(event.clone(), signal.clone()).await;
    }
}
