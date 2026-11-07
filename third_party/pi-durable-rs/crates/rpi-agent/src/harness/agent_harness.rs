//! Port of `packages/agent/src/harness/agent-harness.ts` @ pi 0.82.1 (2efa728)
//! — the `AgentHarness` main class (T16 final block).
//!
//! The harness wraps the low-level agent loop with a session-backed
//! persistence barrier, a phase state machine, three message queues (steer /
//! follow-up / next-turn), a two-tier subscription model (`subscribe`
//! broadcast + `on` typed hooks), and configuration setters whose session
//! writes are staged while a run is active.
//!
//! Intentional differences from upstream (structural; semantics kept):
//! - JS mutable fields become `&self` methods over `std::sync::Mutex`
//!   interior mutability (crate convention; no lock is held across an
//!   `.await`). `AbortController` is a [`CancellationToken`]; `runPromise` is
//!   a per-run "done" token awaited by [`AgentHarness::wait_for_idle`]
//!   (agent-harness.ts:329-338, :1054-1056).
//! - The pinned `StreamFn` / `AgentEventSink` / loop-hook shapes
//!   (`stream_fn.rs`, `agent_loop.rs`) are infallible: they cannot carry the
//!   errors upstream propagates by throwing out of `runAgentLoop`. Instead,
//!   hook/session/subscriber failures encountered inside loop callbacks are
//!   recorded into a per-run failure cell ([`RunShared::record_failure`]);
//!   the harness stream wrapper checks the cell before each provider call and
//!   converts a recorded failure into the same synthetic error assistant
//!   message upstream builds in `createFailureMessage` (agent-harness.ts:55-74).
//!   If the loop finishes while a failure is still recorded (no provider call
//!   consumed it), [`AgentHarness::emit_run_failure`] replays the trailing
//!   `message_start`/`message_end`/`turn_end`/`agent_end` sequence like the
//!   upstream `executeTurn` catch (agent-harness.ts:623-638). A failure of
//!   the replay itself becomes
//!   `AgentHarnessError("unknown", "Agent run failed and failure reporting failed")`,
//!   the upstream `AggregateError` message (agent-harness.ts:631-637).
//! - Two upstream propagations are unreachable through the infallible loop
//!   hooks and are approximated (neither is exercised by the upstream test
//!   suite): a `tool_call` hook failure blocks the tool with the error text
//!   as the reason and fails the run at the next provider call (upstream
//!   aborts the run before the tool executes), and a queue-drain /
//!   `prepareNextTurn` hook failure surfaces one loop iteration later than
//!   upstream (the loop finishes the in-flight step before the failure
//!   stream is produced). A `tool_result` hook failure is returned as `Err`
//!   to the loop, which downgrades it to an error tool result exactly like
//!   upstream (agent-loop.ts:743-746).
//! - `Models::stream_simple` is adapted to the pinned `StreamFn` shape by
//!   deferring the call into the produced stream ([`models_stream_fn`]); the
//!   turn stream wrapper additionally installs the provider hooks
//!   (`before_provider_request` / `before_provider_payload` /
//!   `after_provider_response`).
//! - `setSteeringMode` / `setFollowUpMode` / `setStreamOptions` are
//!   synchronous: upstream declares them `async` but they perform no I/O
//!   (agent-harness.ts:989-999, :1021-1023).
//! - Error classes collapse to [`AgentHarnessError`] with typed codes (see
//!   the `types.rs` header); `normalizeHarnessError` (agent-harness.ts:140-147)
//!   becomes per-boundary `map_err` calls since Rust errors are typed.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use rpi_ai::models::Models;
use rpi_ai::types::{
    AssistantContent, AssistantMessage, AssistantRole, ErrorReason, ImageContent, Model,
    ProviderResponse, SimpleStreamOptions, StopReason, StreamEvent, StreamOptions, TextContent,
    Usage, UserContent, UserContentBlock, UserMessage, UserRole,
};
use rpi_ai::utils::retry::{RetryCallbacks, RetryFinishedArgs, RetryPolicy, RetryScheduledArgs};
use rpi_ai::utils::text::content_text_user;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{
    AgentContext, AgentEventSink, AgentLoopConfig, AgentLoopTurnUpdate, now_millis, run_agent_loop,
    thinking_level_from_model_level,
};
use crate::compaction::branch_summarization::{
    BranchSummaryDetails, DEFAULT_BRANCH_RESERVE_TOKENS, GenerateBranchSummaryOptions,
    collect_entries_for_branch_summary, generate_branch_summary,
};
use crate::compaction::{DEFAULT_COMPACTION_SETTINGS, SummarizationArgs, compact as run_compact};
use crate::error::AgentError;
use crate::messages::{AgentMessage, convert_to_llm};
use crate::session::{SessionEntry, build_context_messages};
use crate::stream_fn::{BoxStream, StreamFn};
use crate::types::{AgentEvent, AgentTool, AgentToolResult, QueueMode, ThinkingLevel};

use super::prompt_templates::format_prompt_template_invocation;
use super::skills::format_skill_invocation;
use super::types::{
    AbortResult, AgentHarnessError, AgentHarnessErrorCode, AgentHarnessEvent, AgentHarnessOptions,
    AgentHarnessOwnEvent, AgentHarnessPhase, AgentHarnessPromptOptions, AgentHarnessResources,
    AgentHarnessStreamOptions, AgentHarnessSystemPrompt, AgentHarnessTool,
    AgentHarnessToolContextSource, AppendCompactionOptions, CompactResult, CompactionPreparation,
    HarnessHookResult, MoveToSummary, NavigateTreeResult, PendingSessionWrite, RetryOperation,
    Session, SessionContextBuildOptions, SessionError, SessionMetadata, SystemPromptContext,
    TreePreparation, TurnState, UpdateSource, apply_stream_options_patch,
};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Free helpers (agent-harness.ts:49-92)
// ---------------------------------------------------------------------------

/// `createUserMessage` (agent-harness.ts:49-53).
fn create_user_message(text: &str, images: Option<Vec<ImageContent>>) -> AgentMessage {
    let mut content = vec![UserContentBlock::Text(TextContent {
        text: text.to_owned(),
        text_signature: None,
    })];
    if let Some(images) = images {
        content.extend(images.into_iter().map(UserContentBlock::Image));
    }
    AgentMessage::User(UserMessage {
        role: UserRole::User,
        content: UserContent::Blocks(content),
        timestamp: now_millis(),
    })
}

/// `createFailureMessage` (agent-harness.ts:55-74): empty text content,
/// `stopReason` `aborted`/`error`, zeroed usage.
fn create_failure_message(model: &Model, error_message: &str, aborted: bool) -> AssistantMessage {
    AssistantMessage {
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
        error_message: Some(error_message.to_owned()),
        timestamp: now_millis(),
        deferred: None,
        end_turn: None,
        raw_stop_reason: None,
        duration_ms: None,
    }
}

/// `findDuplicateNames` (agent-harness.ts:84-92) — duplicates in
/// first-repeat order.
fn find_duplicate_names(names: &[String]) -> Vec<String> {
    let mut seen: Vec<&str> = Vec::with_capacity(names.len());
    let mut duplicates: Vec<String> = Vec::new();
    for name in names {
        if seen.contains(&name.as_str()) && !duplicates.contains(name) {
            duplicates.push(name.clone());
        }
        seen.push(name);
    }
    duplicates
}

/// `normalizeHarnessError` for session failures (agent-harness.ts:143):
/// `SessionError` maps to code `session`, message preserved.
fn session_error(error: SessionError) -> AgentHarnessError {
    AgentHarnessError::new(AgentHarnessErrorCode::Session, error.message)
}

fn busy_error(message: &str) -> AgentHarnessError {
    AgentHarnessError::new(AgentHarnessErrorCode::Busy, message)
}

// ---------------------------------------------------------------------------
// Listener and hook registration types (agent-harness.ts:138, :1058-1083)
// ---------------------------------------------------------------------------

/// `subscribe` listener (agent-harness.ts:229-247, :1058-1068) — observes
/// every harness event (low-level [`AgentEvent`]s wrapped in
/// [`AgentHarnessEvent::Agent`] plus the harness-owned events) and is awaited
/// in subscription order (barrier semantics, coding-standards §6.2). `Err` is
/// the upstream listener `throw`: it fails the emitting operation with the
/// returned harness error.
pub type AgentHarnessListener = Arc<
    dyn Fn(
            AgentHarnessEvent,
            CancellationToken,
        ) -> BoxFuture<'static, Result<(), AgentHarnessError>>
        + Send
        + Sync,
>;

/// `on` hook handler (agent-harness.ts:1070-1083) — registered per harness
/// event type; invoked sequentially in registration order when that hook
/// event is emitted. Upstream handlers throwing a non-`AgentHarnessError` get
/// it wrapped with code `hook` (`normalizeHookError`,
/// agent-harness.ts:149-151); Rust handlers return [`AgentHarnessError`]
/// directly and choose the code at construction.
pub type AgentHarnessHook = Arc<
    dyn Fn(AgentHarnessOwnEvent) -> BoxFuture<'static, Result<HarnessHookResult, AgentHarnessError>>
        + Send
        + Sync,
>;

/// Options bag of [`AgentHarness::navigate_tree`] (agent-harness.ts:792-794).
#[derive(Debug, Clone, Default)]
pub struct NavigateTreeOptions {
    pub summarize: bool,
    pub custom_instructions: Option<String>,
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
}

/// The upstream `type` tag of a harness-owned event (the serde tag strings of
/// [`AgentHarnessOwnEvent`]) — the registration key of [`AgentHarness::on`].
fn own_event_type(event: &AgentHarnessOwnEvent) -> &'static str {
    match event {
        AgentHarnessOwnEvent::QueueUpdate { .. } => "queue_update",
        AgentHarnessOwnEvent::SavePoint { .. } => "save_point",
        AgentHarnessOwnEvent::Abort { .. } => "abort",
        AgentHarnessOwnEvent::Settled { .. } => "settled",
        AgentHarnessOwnEvent::BeforeAgentStart { .. } => "before_agent_start",
        AgentHarnessOwnEvent::Context { .. } => "context",
        AgentHarnessOwnEvent::BeforeProviderRequest { .. } => "before_provider_request",
        AgentHarnessOwnEvent::BeforeProviderPayload { .. } => "before_provider_payload",
        AgentHarnessOwnEvent::AfterProviderResponse { .. } => "after_provider_response",
        AgentHarnessOwnEvent::ToolCall { .. } => "tool_call",
        AgentHarnessOwnEvent::ToolResult { .. } => "tool_result",
        AgentHarnessOwnEvent::SessionBeforeCompact { .. } => "session_before_compact",
        AgentHarnessOwnEvent::SessionCompact { .. } => "session_compact",
        AgentHarnessOwnEvent::SessionBeforeTree { .. } => "session_before_tree",
        AgentHarnessOwnEvent::SessionTree { .. } => "session_tree",
        AgentHarnessOwnEvent::RetryScheduled { .. } => "retry_scheduled",
        AgentHarnessOwnEvent::RetryAttemptStart { .. } => "retry_attempt_start",
        AgentHarnessOwnEvent::RetryFinished { .. } => "retry_finished",
        AgentHarnessOwnEvent::ModelUpdate { .. } => "model_update",
        AgentHarnessOwnEvent::ThinkingLevelUpdate { .. } => "thinking_level_update",
        AgentHarnessOwnEvent::ToolsUpdate { .. } => "tools_update",
        AgentHarnessOwnEvent::ResourcesUpdate { .. } => "resources_update",
    }
}

/// Upstream `result !== undefined` (agent-harness.ts:258-260): a hook result
/// counts as "returned" unless it is [`HarnessHookResult::NoResult`] or a
/// `None`-carrying variant (upstream `undefined`).
fn hook_result_is_defined(result: &HarnessHookResult) -> bool {
    match result {
        HarnessHookResult::BeforeAgentStart(r) => r.is_some(),
        HarnessHookResult::Context(r) => r.is_some(),
        HarnessHookResult::BeforeProviderRequest(r) => r.is_some(),
        HarnessHookResult::BeforeProviderPayload(r) => r.is_some(),
        HarnessHookResult::ToolCall(r) => r.is_some(),
        HarnessHookResult::ToolResult(r) => r.is_some(),
        HarnessHookResult::SessionBeforeCompact(r) => r.is_some(),
        HarnessHookResult::SessionBeforeTree(r) => r.is_some(),
        HarnessHookResult::NoResult => false,
    }
}

// ---------------------------------------------------------------------------
// Tool binding (agent-harness.ts:347-352)
// ---------------------------------------------------------------------------

/// `bindToolContext` (agent-harness.ts:347-352): an
/// [`AgentHarnessTool<TContext>`] with the turn's resolved context baked in,
/// exposed to the loop as the crate-root [`AgentTool`].
struct BoundTool<TContext> {
    tool: Arc<dyn AgentHarnessTool<TContext>>,
    context: TContext,
}

#[async_trait]
impl<TContext: Clone + Send + Sync + 'static> AgentTool for BoundTool<TContext> {
    fn name(&self) -> &str {
        self.tool.name()
    }

    fn label(&self) -> &str {
        self.tool.label()
    }

    fn description(&self) -> &str {
        self.tool.description()
    }

    fn parameters(&self) -> &Value {
        self.tool.parameters()
    }

    fn constrained_sampling(&self) -> Option<rpi_ai::types::ConstrainedSampling> {
        self.tool.constrained_sampling()
    }

    fn execution_mode(&self) -> Option<crate::types::ToolExecutionMode> {
        self.tool.execution_mode()
    }

    fn prepare_arguments(&self, args: Value) -> Value {
        self.tool.prepare_arguments(args)
    }

    async fn execute(
        &self,
        tool_call_id: &str,
        params: Value,
        signal: CancellationToken,
        on_update: Option<crate::types::AgentToolUpdateCallback>,
    ) -> Result<AgentToolResult, AgentError> {
        self.tool
            .execute(
                tool_call_id,
                params,
                signal,
                on_update,
                self.context.clone(),
            )
            .await
    }
}

// ---------------------------------------------------------------------------
// Per-run shared state
// ---------------------------------------------------------------------------

/// State shared by the loop callbacks of one run: the active turn snapshot
/// (swapped by `prepareNextTurn`, agent-harness.ts:485-494) and the recorded
/// run failure (see the file header for the error-propagation design).
struct RunShared<TContext = ()> {
    turn_state: Mutex<TurnState<TContext>>,
    failure: Mutex<Option<AgentHarnessError>>,
}

impl<TContext: Clone> RunShared<TContext> {
    fn new(turn_state: TurnState<TContext>) -> Self {
        Self {
            turn_state: Mutex::new(turn_state),
            failure: Mutex::new(None),
        }
    }

    fn turn_state(&self) -> TurnState<TContext> {
        lock(&self.turn_state).clone()
    }

    fn replace_turn_state(&self, next: TurnState<TContext>) {
        *lock(&self.turn_state) = next;
    }

    /// First failure wins (the root cause); later failures are drop-side
    /// effects of the failing run.
    fn record_failure(&self, error: AgentHarnessError) {
        let mut failure = lock(&self.failure);
        if failure.is_none() {
            *failure = Some(error);
        }
    }

    fn take_failure(&self) -> Option<AgentHarnessError> {
        lock(&self.failure).take()
    }
}

/// The synthetic stream produced when a run failure is pending: a single
/// terminal `error` event carrying the `createFailureMessage` assistant
/// message. The loop finalizes it into `message_start`/`message_end`/
/// `turn_end`/`agent_end` — the same sequence upstream's `emitRunFailure`
/// replays (agent-harness.ts:567-579).
fn failure_stream(
    model: &Model,
    error_message: &str,
    aborted: bool,
) -> BoxStream<'static, StreamEvent> {
    let message = create_failure_message(model, error_message, aborted);
    futures::stream::once(async move {
        StreamEvent::Error {
            reason: if aborted {
                ErrorReason::Aborted
            } else {
                ErrorReason::Error
            },
            error: message,
        }
    })
    .boxed()
}

/// Adapt [`Models`] to the pinned [`StreamFn`] shape for the summarization
/// calls (`compact` / `generate_branch_summary`). The `stream_simple` call is
/// deferred into the produced stream so it runs on the caller's runtime.
fn models_stream_fn(models: &Models) -> StreamFn {
    let models = models.clone();
    Arc::new(
        move |model: Model, context: rpi_ai::types::TranscriptContext, options: StreamOptions| {
            let models = models.clone();
            futures::stream::once(async move {
                let simple = SimpleStreamOptions {
                    reasoning: options.reasoning.and_then(thinking_level_from_model_level),
                    thinking_budgets: None,
                    stream: options,
                    tool_choice: None,
                };
                // #9548: the harness materializes its prompt via the
                // transcript; the facade re-normalizes (no-op here).
                let context = rpi_ai::types::Context {
                    system_prompt: None,
                    messages: context.messages,
                    tools: None,
                };
                let stream: BoxStream<'static, StreamEvent> = models
                    .stream_simple(&model, &context, Some(simple.into()))
                    .boxed();
                stream
            })
            .flatten()
            .boxed()
        },
    )
}

/// `getMessageFromEntryForCompaction` (compaction.ts:80-85): the context
/// message an entry produces; `None` for compaction boundaries.
fn entry_message_for_compaction(entry: &SessionEntry) -> Option<AgentMessage> {
    if matches!(entry, SessionEntry::Compaction(_)) {
        return None;
    }
    crate::session::session_entry_to_context_messages(entry)
        .into_iter()
        .next()
}

/// Epoch milliseconds of an [`AgentMessage`] (untagged enum; each variant
/// carries `timestamp: i64`). Used when virtualizing a compaction's retained
/// tail into message entries (compaction.ts:636-645 @ 4181f66).
fn agent_message_epoch_ms(message: &AgentMessage) -> i64 {
    match message {
        AgentMessage::User(message) => message.timestamp,
        AgentMessage::Assistant(message) => message.timestamp,
        AgentMessage::ToolResult(message) => message.timestamp,
        AgentMessage::BashExecution(message) => message.timestamp,
        AgentMessage::Custom(message) => message.timestamp,
        AgentMessage::BranchSummary(message) => message.timestamp,
        AgentMessage::CompactionSummary(message) => message.timestamp,
        AgentMessage::System(message) => message.timestamp,
    }
}

/// `prepareCompaction` (harness compaction.ts:640-713). Two differences from
/// the coding-agent variant ported as [`prepare_compaction`]: it does NOT
/// bail out when nothing needs summarizing (the summarization call still
/// runs — the upstream "persists generated compaction usage" test compacts a
/// two-message session), and the preparation carries `retainedTail`
/// (compaction.ts:690-694). Returns the crate preparation (for
/// [`run_compact`]) alongside the retained tail; `Ok(None)` is the upstream
/// `undefined` ("Nothing to compact", agent-harness.ts:746).
#[allow(clippy::type_complexity)]
fn prepare_harness_compaction(
    branch_entries: &[SessionEntry],
    settings: &crate::compaction::CompactionSettings,
) -> Result<Option<(crate::compaction::CompactionPreparation, Vec<AgentMessage>)>, AgentHarnessError>
{
    if branch_entries.is_empty()
        || matches!(branch_entries.last(), Some(SessionEntry::Compaction(_)))
    {
        return Ok(None);
    }

    let prev_compaction_index = branch_entries
        .iter()
        .rposition(|entry| matches!(entry, SessionEntry::Compaction(_)));
    let mut previous_summary: Option<String> = None;
    // 44289550a @ 4181f66: the previous compaction's `retainedTail` is
    // virtualized into message entries (`${prevId}:retained:${index}`) and the
    // cut-point search runs over virtual + real entries; no firstKeptEntryId
    // anchor. Legacy harness entries (retained_tail absent) keep the v1
    // first_kept_entry_id anchor for backward compatibility.
    let mut compactable: std::borrow::Cow<'_, [SessionEntry]> =
        std::borrow::Cow::Borrowed(branch_entries);
    if let Some(index) = prev_compaction_index
        && let SessionEntry::Compaction(prev_compaction) = &branch_entries[index]
    {
        previous_summary = Some(prev_compaction.summary.clone());
        if let Some(tail) = &prev_compaction.retained_tail {
            let mut virtualized: Vec<SessionEntry> = tail
                .iter()
                .enumerate()
                .map(|(tail_index, message)| {
                    let virtual_id = format!("{}:retained:{}", prev_compaction.id, tail_index);
                    SessionEntry::Message(crate::session::MessageEntry {
                        parent_id: Some(if tail_index == 0 {
                            prev_compaction.id.clone()
                        } else {
                            format!("{}:retained:{}", prev_compaction.id, tail_index - 1)
                        }),
                        // Entry timestamps are ISO strings in the
                        // consolidated session skeleton; messages carry
                        // epoch ms (messages.ts: virtual entry
                        // `timestamp: message.timestamp`).
                        timestamp: crate::harness::session::repo_utils::format_iso8601_ms(
                            agent_message_epoch_ms(message).max(0) as u64,
                        ),
                        id: virtual_id,
                        message: message.clone(),
                    })
                })
                .collect();
            virtualized.extend_from_slice(&branch_entries[index + 1..]);
            compactable = std::borrow::Cow::Owned(virtualized);
        } else {
            // Legacy anchor (v1 form): start at the recorded first kept
            // entry, or right after the compaction when unknown.
            let boundary_start = branch_entries
                .iter()
                .position(|entry| {
                    Some(entry.id()) == prev_compaction.first_kept_entry_id.as_deref()
                })
                .unwrap_or(index + 1);
            compactable = std::borrow::Cow::Borrowed(&branch_entries[boundary_start..]);
        }
    }
    let boundary_end = compactable.len();

    let tokens_before =
        crate::compaction::estimate_context_tokens(&build_context_messages(branch_entries)).tokens;

    let cut_point = crate::compaction::find_cut_point(
        &compactable,
        0,
        boundary_end,
        settings.keep_recent_tokens,
    );
    let Some(first_kept_entry) = compactable.get(cut_point.first_kept_entry_index) else {
        // compaction.ts:672-674.
        return Err(AgentHarnessError::new(
            AgentHarnessErrorCode::Compaction,
            "First kept entry has no UUID - session may need migration",
        ));
    };
    let first_kept_entry_id = first_kept_entry.id().to_owned();

    let history_end = if cut_point.is_split_turn {
        cut_point
            .turn_start_index
            .unwrap_or(cut_point.first_kept_entry_index)
    } else {
        cut_point.first_kept_entry_index
    };

    let mut messages_to_summarize = Vec::new();
    for entry in &compactable[..history_end] {
        if let Some(message) = entry_message_for_compaction(entry) {
            messages_to_summarize.push(message);
        }
    }
    let mut turn_prefix_messages = Vec::new();
    if cut_point.is_split_turn {
        let turn_start = cut_point
            .turn_start_index
            .unwrap_or(cut_point.first_kept_entry_index);
        for entry in &compactable[turn_start..cut_point.first_kept_entry_index] {
            if let Some(message) = entry_message_for_compaction(entry) {
                turn_prefix_messages.push(message);
            }
        }
    }
    let mut retained_tail = Vec::new();
    for entry in &compactable[cut_point.first_kept_entry_index..boundary_end] {
        if let Some(message) = entry_message_for_compaction(entry) {
            retained_tail.push(message);
        }
    }
    let mut file_ops = crate::compaction::extract_file_operations(
        &messages_to_summarize,
        branch_entries,
        prev_compaction_index,
    );
    if cut_point.is_split_turn {
        for message in &turn_prefix_messages {
            crate::compaction::utils::extract_file_ops_from_message(message, &mut file_ops);
        }
    }

    Ok(Some((
        crate::compaction::CompactionPreparation {
            first_kept_entry_id,
            messages_to_summarize,
            turn_prefix_messages,
            is_split_turn: cut_point.is_split_turn,
            tokens_before,
            previous_summary,
            file_ops,
            settings: *settings,
        },
        retained_tail,
    )))
}

// ---------------------------------------------------------------------------
// AgentHarness (agent-harness.ts:171-1084)
// ---------------------------------------------------------------------------

/// Hook handler map: event type tag → registration-ordered handlers.
type HookHandlerMap = HashMap<String, Vec<(u64, AgentHarnessHook)>>;

/// `AgentHarness` (agent-harness.ts:171-1084). All methods take `&self` (run
/// methods take `&Arc<Self>` because the loop callbacks capture the harness);
/// wrap in `Arc` to share across tasks.
///
/// `TContext` is the application tool-context type (upstream `TContext`,
/// defaulting to `()` where upstream defaults to `undefined`). It must be
/// `Default`: a harness constructed without `tool_context` resolves it to
/// `TContext::default()` per turn (upstream passes `undefined`).
pub struct AgentHarness<TContext = ()> {
    session: Arc<dyn Session<Metadata = SessionMetadata>>,
    /// Upstream `readonly models` (agent-harness.ts:178).
    pub models: Models,
    phase: Mutex<AgentHarnessPhase>,
    run_abort: Mutex<Option<CancellationToken>>,
    run_done: Mutex<Option<CancellationToken>>,
    pending_session_writes: Mutex<Vec<PendingSessionWrite>>,
    model: Mutex<Model>,
    thinking_level: Mutex<ThinkingLevel>,
    system_prompt: Mutex<Option<AgentHarnessSystemPrompt<TContext>>>,
    tool_context: Mutex<Option<AgentHarnessToolContextSource<TContext>>>,
    stream_options: Mutex<AgentHarnessStreamOptions>,
    retry: Option<RetryPolicy>,
    resources: Mutex<AgentHarnessResources>,
    /// Insertion-ordered (upstream `Map<string, TTool>`).
    tools: Mutex<Vec<Arc<dyn AgentHarnessTool<TContext>>>>,
    active_tool_names: Mutex<Vec<String>>,
    steer_queue: Mutex<Vec<AgentMessage>>,
    steering_mode: Mutex<QueueMode>,
    follow_up_queue: Mutex<Vec<AgentMessage>>,
    follow_up_mode: Mutex<QueueMode>,
    next_turn_queue: Mutex<Vec<AgentMessage>>,
    /// `Arc` so the returned unsubscribe closures can deregister ('static).
    listeners: Arc<Mutex<Vec<(u64, AgentHarnessListener)>>>,
    /// Registration-ordered per event type (upstream `Map<string, Set>`).
    hook_handlers: Arc<Mutex<HookHandlerMap>>,
    next_registration_id: AtomicU64,
}

impl<TContext: Clone + Default + Send + Sync + 'static> AgentHarness<TContext> {
    /// Constructor (agent-harness.ts:199-223). Validation failures are
    /// returned as `invalid_argument` errors (upstream throws).
    pub fn new(options: AgentHarnessOptions<TContext>) -> Result<Self, AgentHarnessError> {
        let tools = options.tools;
        Self::validate_unique_names(
            &tools
                .iter()
                .map(|tool| tool.name().to_owned())
                .collect::<Vec<_>>(),
            "Duplicate tool name(s)",
        )?;
        let active_tool_names = options
            .active_tool_names
            .unwrap_or_else(|| tools.iter().map(|tool| tool.name().to_owned()).collect());
        Self::validate_unique_names(&active_tool_names, "Duplicate active tool name(s)")?;
        Self::validate_tool_names_against(&active_tool_names, &tools)?;
        Ok(Self {
            session: options.session,
            models: options.models,
            phase: Mutex::new(AgentHarnessPhase::Idle),
            run_abort: Mutex::new(None),
            run_done: Mutex::new(None),
            pending_session_writes: Mutex::new(Vec::new()),
            model: Mutex::new(options.model),
            thinking_level: Mutex::new(options.thinking_level.unwrap_or(ThinkingLevel::Off)),
            system_prompt: Mutex::new(options.system_prompt),
            tool_context: Mutex::new(options.tool_context),
            stream_options: Mutex::new(options.stream_options.unwrap_or_default()),
            retry: options.retry,
            resources: Mutex::new(options.resources),
            tools: Mutex::new(tools),
            active_tool_names: Mutex::new(active_tool_names),
            steer_queue: Mutex::new(Vec::new()),
            steering_mode: Mutex::new(options.steering_mode.unwrap_or(QueueMode::OneAtATime)),
            follow_up_queue: Mutex::new(Vec::new()),
            follow_up_mode: Mutex::new(options.follow_up_mode.unwrap_or(QueueMode::OneAtATime)),
            next_turn_queue: Mutex::new(Vec::new()),
            listeners: Arc::new(Mutex::new(Vec::new())),
            hook_handlers: Arc::new(Mutex::new(HashMap::new())),
            next_registration_id: AtomicU64::new(0),
        })
    }

    // ------------------------------------------------------------------
    // Subscription model (agent-harness.ts:225-266, :1058-1083)
    // ------------------------------------------------------------------

    /// `subscribe` (agent-harness.ts:1058-1068): pure observation of every
    /// harness event; listeners are awaited in subscription order. Returns an
    /// unsubscribe closure.
    pub fn subscribe(&self, listener: AgentHarnessListener) -> Box<dyn FnOnce() + Send> {
        let id = self.next_registration_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.listeners).push((id, listener));
        let listeners = Arc::clone(&self.listeners);
        Box::new(move || {
            lock(&listeners).retain(|(listener_id, _)| *listener_id != id);
        })
    }

    /// `on` (agent-harness.ts:1070-1083): register a hook handler for one
    /// harness event type (upstream `type` tag strings, e.g.
    /// `"before_agent_start"`). Returns an unsubscribe closure.
    pub fn on(&self, event_type: &str, handler: AgentHarnessHook) -> Box<dyn FnOnce() + Send> {
        let id = self.next_registration_id.fetch_add(1, Ordering::SeqCst);
        lock(&self.hook_handlers)
            .entry(event_type.to_owned())
            .or_default()
            .push((id, handler));
        let handlers = Arc::clone(&self.hook_handlers);
        let event_type = event_type.to_owned();
        Box::new(move || {
            if let Some(entries) = lock(&handlers).get_mut(&event_type) {
                entries.retain(|(handler_id, _)| *handler_id != id);
            }
        })
    }

    fn listener_snapshot(&self) -> Vec<AgentHarnessListener> {
        lock(&self.listeners)
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect()
    }

    fn hook_snapshot(&self, event_type: &str) -> Vec<AgentHarnessHook> {
        lock(&self.hook_handlers)
            .get(event_type)
            .map(|handlers| handlers.iter().map(|(_, h)| Arc::clone(h)).collect())
            .unwrap_or_default()
    }

    /// `emitAny` (agent-harness.ts:239-247): fan out to `subscribe` listeners,
    /// awaited one by one; the first listener error stops the iteration and
    /// propagates (already an [`AgentHarnessError`], so `normalizeHookError`
    /// passes it through, agent-harness.ts:149-151).
    async fn emit_any(
        &self,
        event: AgentHarnessEvent,
        signal: Option<&CancellationToken>,
    ) -> Result<(), AgentHarnessError> {
        for listener in self.listener_snapshot() {
            listener(event.clone(), signal.cloned().unwrap_or_default()).await?;
        }
        Ok(())
    }

    /// `emitOwn` (agent-harness.ts:229-237).
    async fn emit_own(
        &self,
        event: AgentHarnessOwnEvent,
        signal: Option<&CancellationToken>,
    ) -> Result<(), AgentHarnessError> {
        self.emit_any(AgentHarnessEvent::Harness(event), signal)
            .await
    }

    /// `emitHook` (agent-harness.ts:249-266): run the handlers registered for
    /// the event's type in order; the last defined result wins.
    async fn emit_hook(
        &self,
        event: AgentHarnessOwnEvent,
    ) -> Result<Option<HarnessHookResult>, AgentHarnessError> {
        let handlers = self.hook_snapshot(own_event_type(&event));
        let mut last_result = None;
        for handler in handlers {
            let result = handler(event.clone()).await?;
            if hook_result_is_defined(&result) {
                last_result = Some(result);
            }
        }
        Ok(last_result)
    }

    /// `retryCallbacks` (agent-harness.ts:268-275): bridge compaction /
    /// branch-summary retry notifications to harness events. Emission errors
    /// are dropped: the retry machinery cannot surface them (upstream
    /// callbacks return `void`; a throwing listener would reject inside the
    /// retry helper — an untestable edge, noted in the header).
    fn retry_callbacks(self: &Arc<Self>, operation: RetryOperation) -> RetryCallbacks {
        let on_retry_scheduled = {
            let this = Arc::clone(self);
            Box::new(
                move |(attempt, max_attempts, delay_ms, error_message): RetryScheduledArgs| {
                    let this = Arc::clone(&this);
                    Box::pin(async move {
                        let _ = this
                            .emit_own(
                                AgentHarnessOwnEvent::RetryScheduled {
                                    operation,
                                    attempt: u64::from(attempt),
                                    max_attempts: u64::from(max_attempts),
                                    delay_ms,
                                    error_message,
                                },
                                None,
                            )
                            .await;
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                },
            )
        };
        let on_retry_attempt_start = {
            let this = Arc::clone(self);
            Box::new(move |(): ()| {
                let this = Arc::clone(&this);
                Box::pin(async move {
                    let _ = this
                        .emit_own(AgentHarnessOwnEvent::RetryAttemptStart { operation }, None)
                        .await;
                })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            })
        };
        let on_retry_finished = {
            let this = Arc::clone(self);
            Box::new(move |_args: RetryFinishedArgs| {
                let this = Arc::clone(&this);
                Box::pin(async move {
                    let _ = this
                        .emit_own(AgentHarnessOwnEvent::RetryFinished { operation }, None)
                        .await;
                })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            })
        };
        RetryCallbacks {
            on_retry_scheduled: Some(on_retry_scheduled),
            on_retry_attempt_start: Some(on_retry_attempt_start),
            on_retry_finished: Some(on_retry_finished),
        }
    }

    /// `emitBeforeProviderRequest` (agent-harness.ts:277-301): chained patch
    /// application over the turn's snapshot stream options.
    async fn emit_before_provider_request(
        &self,
        model: &Model,
        session_id: &str,
        stream_options: AgentHarnessStreamOptions,
    ) -> Result<AgentHarnessStreamOptions, AgentHarnessError> {
        let handlers = self.hook_snapshot("before_provider_request");
        let mut current = stream_options;
        for handler in handlers {
            let event = AgentHarnessOwnEvent::BeforeProviderRequest {
                model: Box::new(model.clone()),
                session_id: session_id.to_owned(),
                stream_options: current.clone(),
            };
            if let HarnessHookResult::BeforeProviderRequest(Some(result)) = handler(event).await? {
                current = apply_stream_options_patch(&current, result.stream_options.as_ref());
            }
        }
        Ok(current)
    }

    /// `emitBeforeProviderPayload` (agent-harness.ts:303-318): chained
    /// payload replacement.
    async fn emit_before_provider_payload(
        &self,
        model: &Model,
        payload: Value,
    ) -> Result<Value, AgentHarnessError> {
        let handlers = self.hook_snapshot("before_provider_payload");
        let mut current = payload;
        for handler in handlers {
            let event = AgentHarnessOwnEvent::BeforeProviderPayload {
                model: Box::new(model.clone()),
                payload: current.clone(),
            };
            if let HarnessHookResult::BeforeProviderPayload(Some(result)) = handler(event).await? {
                current = result.payload;
            }
        }
        Ok(current)
    }

    /// `emitQueueUpdate` (agent-harness.ts:320-327).
    async fn emit_queue_update(&self) -> Result<(), AgentHarnessError> {
        let (steer, follow_up, next_turn) = {
            let steer = lock(&self.steer_queue).clone();
            let follow_up = lock(&self.follow_up_queue).clone();
            let next_turn = lock(&self.next_turn_queue).clone();
            (steer, follow_up, next_turn)
        };
        self.emit_own(
            AgentHarnessOwnEvent::QueueUpdate {
                steer,
                follow_up,
                next_turn,
            },
            None,
        )
        .await
    }

    /// `startRunPromise` (agent-harness.ts:329-338): the returned token is
    /// cancelled by [`AgentHarness::finish_run`] (upstream promise resolve).
    fn start_run(&self) -> CancellationToken {
        let done = CancellationToken::new();
        *lock(&self.run_done) = Some(done.clone());
        done
    }

    fn finish_run(&self, done: CancellationToken) {
        *lock(&self.run_done) = None;
        done.cancel();
    }

    /// `resolveToolContext` (agent-harness.ts:340-345). A missing source
    /// resolves to `TContext::default()` (upstream `undefined`).
    async fn resolve_tool_context(&self) -> TContext {
        let source = lock(&self.tool_context).clone();
        match source {
            Some(AgentHarnessToolContextSource::Static(context)) => context,
            Some(AgentHarnessToolContextSource::Provider(provider)) => provider().await,
            None => TContext::default(),
        }
    }

    /// `createTurnState` (agent-harness.ts:354-387): resolve the per-turn
    /// snapshot; `systemPrompt` / `toolContext` callbacks run once per
    /// snapshot.
    async fn create_turn_state(&self) -> Result<TurnState<TContext>, AgentHarnessError> {
        let context = self
            .session
            .build_context(SessionContextBuildOptions::default())
            .await
            .map_err(session_error)?;
        let resources = self.get_resources();
        let session_metadata = self.session.get_metadata().await.map_err(session_error)?;
        let tool_context = self.resolve_tool_context().await;
        let tools = lock(&self.tools).clone();
        let active_tool_names = lock(&self.active_tool_names).clone();
        let active_tools: Vec<Arc<dyn AgentHarnessTool<TContext>>> = active_tool_names
            .iter()
            .filter_map(|name| tools.iter().find(|tool| tool.name() == name).cloned())
            .collect();
        let model = lock(&self.model).clone();
        let thinking_level = *lock(&self.thinking_level);
        let system_prompt_source = lock(&self.system_prompt).clone();
        let system_prompt = match system_prompt_source {
            Some(AgentHarnessSystemPrompt::Static(prompt)) => prompt,
            Some(AgentHarnessSystemPrompt::Dynamic(callback)) => {
                callback(SystemPromptContext {
                    session: Arc::clone(&self.session),
                    model: model.clone(),
                    thinking_level,
                    active_tools: active_tools.clone(),
                    resources: resources.clone(),
                })
                .await
            }
            None => "You are a helpful assistant.".to_owned(),
        };
        Ok(TurnState {
            messages: context.messages,
            resources,
            tool_context,
            stream_options: lock(&self.stream_options).clone(),
            session_id: session_metadata.id,
            system_prompt,
            model,
            thinking_level,
            tools,
            active_tools,
        })
    }

    /// `createContext` (agent-harness.ts:389-398, post-#9548): the turn's
    /// prompt and tool declarations ride a leading system message in the
    /// transcript (upstream sends them through the normalized `AiContext`).
    fn create_context(
        &self,
        turn_state: &TurnState<TContext>,
        system_prompt: Option<String>,
    ) -> AgentContext {
        let tools: Vec<Arc<dyn AgentTool>> = turn_state
            .active_tools
            .iter()
            .map(|tool| {
                Arc::new(BoundTool {
                    tool: Arc::clone(tool),
                    context: turn_state.tool_context.clone(),
                }) as Arc<dyn AgentTool>
            })
            .collect();
        let mut messages = turn_state.messages.clone();
        let declared_tools: Vec<rpi_ai::types::Tool> = tools
            .iter()
            .map(|tool| rpi_ai::types::Tool {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                parameters: tool.parameters().clone(),
                constrained_sampling: tool.constrained_sampling(),
            })
            .collect();
        let effective_prompt = system_prompt.unwrap_or_else(|| turn_state.system_prompt.clone());
        let initial = rpi_ai::utils::transcript::create_initial_system_message(
            Some(&effective_prompt),
            Some(&declared_tools),
        );
        if !matches!(messages.first(), Some(AgentMessage::System(_)))
            && let Some(initial) = initial
        {
            messages.insert(0, AgentMessage::System(initial));
        }
        AgentContext {
            messages,
            tools: Some(tools),
        }
    }

    /// `createStreamFn` (agent-harness.ts:400-428). The wrapper reads the
    /// current turn snapshot per call, runs the `before_provider_request`
    /// chain, and installs the payload/response hooks. A pending run failure
    /// short-circuits the provider call with the synthetic failure stream
    /// (see the file header).
    fn create_stream_fn(self: &Arc<Self>, shared: Arc<RunShared<TContext>>) -> StreamFn {
        let this = Arc::clone(self);
        Arc::new(
            move |model: Model,
                  context: rpi_ai::types::TranscriptContext,
                  options: StreamOptions| {
                let this = Arc::clone(&this);
                let shared = Arc::clone(&shared);
                futures::stream::once(async move {
                    let aborted = options
                        .signal
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled);
                    // Failure gate (error-propagation design, file header).
                    if let Some(failure) = shared.take_failure() {
                        let stream: BoxStream<'static, StreamEvent> =
                            failure_stream(&model, &failure.message, aborted);
                        return stream;
                    }
                    let turn_state = shared.turn_state();
                    let session_id = turn_state.session_id.clone();
                    let request_options = match this
                        .emit_before_provider_request(
                            &model,
                            &session_id,
                            turn_state.stream_options,
                        )
                        .await
                    {
                        Ok(request_options) => request_options,
                        Err(error) => return failure_stream(&model, &error.message, aborted),
                    };

                    let on_payload: rpi_ai::types::OnPayloadCallback = {
                        let this = Arc::clone(&this);
                        let shared = Arc::clone(&shared);
                        let model = model.clone();
                        Arc::new(move |payload: Value, hook_model: &Model| {
                            let this = Arc::clone(&this);
                            let shared = Arc::clone(&shared);
                            let hook_model = hook_model.clone();
                            let _ = &model;
                            Box::pin(async move {
                                match this
                                    .emit_before_provider_payload(&hook_model, payload)
                                    .await
                                {
                                    Ok(payload) => Some(payload),
                                    // Upstream would reject inside the provider;
                                    // record and keep the payload (file header).
                                    Err(error) => {
                                        shared.record_failure(error);
                                        None
                                    }
                                }
                            })
                        })
                    };
                    let on_response: rpi_ai::types::OnResponseCallback = {
                        let this = Arc::clone(&this);
                        let shared = Arc::clone(&shared);
                        let signal = options.signal.clone().unwrap_or_default();
                        Arc::new(move |response: ProviderResponse, _model: &Model| {
                            let this = Arc::clone(&this);
                            let shared = Arc::clone(&shared);
                            let signal = signal.clone();
                            Box::pin(async move {
                                let headers: BTreeMap<String, String> =
                                    response.headers.into_iter().collect();
                                if let Err(error) = this
                                    .emit_own(
                                        AgentHarnessOwnEvent::AfterProviderResponse {
                                            status: response.status,
                                            headers,
                                        },
                                        Some(&signal),
                                    )
                                    .await
                                {
                                    shared.record_failure(error);
                                }
                            })
                        })
                    };

                    let simple = SimpleStreamOptions {
                        stream: StreamOptions {
                            cache_retention: request_options.cache_retention,
                            metadata: request_options
                                .metadata
                                .map(|metadata| metadata.into_iter().collect()),
                            // The loop binds `reasoning` into `StreamOptions`
                            // (agent_loop.rs:828); keep it there so providers that
                            // read the plain stream options see it, and mirror it
                            // into the simple options below (upstream passes
                            // `reasoning` at the simple-options level,
                            // agent-harness.ts:421).
                            reasoning: options.reasoning,
                            session_id: Some(session_id),
                            transport: request_options.transport,
                            request: rpi_ai::ProviderRequestOptions {
                                headers: request_options.headers.map(|headers| {
                                    headers.into_iter().map(|(k, v)| (k, Some(v))).collect()
                                }),
                                max_retries: request_options.max_retries,
                                max_retry_delay_ms: request_options.max_retry_delay_ms,
                                on_payload: Some(on_payload),
                                on_response: Some(on_response),
                                signal: options.signal.clone(),
                                timeout_ms: request_options.timeout_ms,
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                        reasoning: options.reasoning.and_then(thinking_level_from_model_level),
                        thinking_budgets: None,
                        tool_choice: None,
                    };
                    let context = rpi_ai::types::Context {
                        system_prompt: None,
                        messages: context.messages,
                        tools: None,
                    };
                    this.models
                        .stream_simple(&model, &context, Some(simple.into()))
                        .boxed()
                })
                .flatten()
                .boxed()
            },
        )
    }

    /// `drainQueuedMessages` (agent-harness.ts:430-440): dequeue first, then
    /// notify; a notification failure restores the messages and propagates.
    async fn drain_queued_messages(
        &self,
        queue: &Mutex<Vec<AgentMessage>>,
        mode: QueueMode,
    ) -> Result<Vec<AgentMessage>, AgentHarnessError> {
        let messages = {
            let mut queue = lock(queue);
            match mode {
                QueueMode::All => std::mem::take(&mut *queue),
                QueueMode::OneAtATime => {
                    if queue.is_empty() {
                        Vec::new()
                    } else {
                        vec![queue.remove(0)]
                    }
                }
            }
        };
        if messages.is_empty() {
            return Ok(messages);
        }
        match self.emit_queue_update().await {
            Ok(()) => Ok(messages),
            Err(error) => {
                let mut queue = lock(queue);
                let mut restored = messages;
                restored.append(&mut queue);
                *queue = restored;
                Err(error)
            }
        }
    }

    /// `createLoopConfig` (agent-harness.ts:442-498).
    fn create_loop_config(self: &Arc<Self>, shared: Arc<RunShared<TContext>>) -> AgentLoopConfig {
        let turn_state = shared.turn_state();

        let transform_context: crate::agent_loop::TransformContextFn = {
            let this = Arc::clone(self);
            let shared = Arc::clone(&shared);
            Arc::new(
                move |messages: Vec<AgentMessage>, _signal: CancellationToken| {
                    let this = Arc::clone(&this);
                    let shared = Arc::clone(&shared);
                    Box::pin(async move {
                        let event = AgentHarnessOwnEvent::Context {
                            messages: messages.clone(),
                        };
                        match this.emit_hook(event).await {
                            Ok(Some(HarnessHookResult::Context(Some(result)))) => result.messages,
                            Ok(_) => messages,
                            Err(error) => {
                                shared.record_failure(error);
                                messages
                            }
                        }
                    })
                },
            )
        };

        let before_tool_call: crate::agent_loop::BeforeToolCallFn = {
            let this = Arc::clone(self);
            let shared = Arc::clone(&shared);
            Arc::new(
                move |hook_context: crate::agent_loop::BeforeToolCallContext, _signal| {
                    let this = Arc::clone(&this);
                    let shared = Arc::clone(&shared);
                    Box::pin(async move {
                        let event = AgentHarnessOwnEvent::ToolCall {
                            tool_call_id: hook_context.tool_call.id.clone(),
                            tool_name: hook_context.tool_call.name.clone(),
                            input: hook_context.args.as_object().cloned().unwrap_or_default(),
                        };
                        match this.emit_hook(event).await {
                            Ok(Some(HarnessHookResult::ToolCall(Some(result)))) => {
                                Some(crate::agent_loop::BeforeToolCallResult {
                                    block: result.block,
                                    reason: result.reason,
                                    args: None,
                                    terminate: None,
                                })
                            }
                            Ok(_) => None,
                            // Upstream lets the throw abort the run before the
                            // tool executes; the infallible hook shape cannot, so
                            // block the tool and fail the run at the next provider
                            // call (file header).
                            Err(error) => {
                                let reason = error.message.clone();
                                shared.record_failure(error);
                                Some(crate::agent_loop::BeforeToolCallResult {
                                    block: Some(true),
                                    reason: Some(reason),
                                    args: None,
                                    terminate: None,
                                })
                            }
                        }
                    })
                },
            )
        };

        let after_tool_call: crate::agent_loop::AfterToolCallFn = {
            let this = Arc::clone(self);
            Arc::new(
                move |hook_context: crate::agent_loop::AfterToolCallContext, _signal| {
                    let this = Arc::clone(&this);
                    Box::pin(async move {
                        let event = AgentHarnessOwnEvent::ToolResult {
                            tool_call_id: hook_context.tool_call.id.clone(),
                            tool_name: hook_context.tool_call.name.clone(),
                            input: hook_context.args.as_object().cloned().unwrap_or_default(),
                            content: hook_context.result.content.clone(),
                            details: hook_context.result.details.clone(),
                            is_error: hook_context.is_error,
                            usage: hook_context.result.usage,
                        };
                        match this.emit_hook(event).await {
                            Ok(Some(HarnessHookResult::ToolResult(Some(patch)))) => {
                                Ok(Some(crate::agent_loop::AfterToolCallResult {
                                    content: patch.content,
                                    details: patch.details,
                                    // The harness hook result has no structured
                                    // content field; None preserves it unless
                                    // `content` is replaced (V16-06 FR-C).
                                    structured_content: None,
                                    is_error: patch.is_error,
                                    usage: patch.usage,
                                    terminate: patch.terminate,
                                }))
                            }
                            Ok(_) => Ok(None),
                            // The loop downgrades `Err` to an error tool result,
                            // matching upstream's try/catch (agent-loop.ts:743-746).
                            Err(error) => Err(AgentError::Message(error.message)),
                        }
                    })
                },
            )
        };

        let prepare_next_turn: crate::agent_loop::PrepareNextTurnFn = {
            let this = Arc::clone(self);
            let shared = Arc::clone(&shared);
            Arc::new(move |_context: crate::agent_loop::PrepareNextTurnContext| {
                let this = Arc::clone(&this);
                let shared = Arc::clone(&shared);
                Box::pin(async move {
                    // agent-harness.ts:485-494: flush pending writes, rebuild
                    // the turn snapshot, hand the loop the new context.
                    if let Err(error) = this.flush_pending_session_writes().await {
                        shared.record_failure(error);
                        return None;
                    }
                    let next_turn_state = match this.create_turn_state().await {
                        Ok(turn_state) => turn_state,
                        Err(error) => {
                            shared.record_failure(error);
                            return None;
                        }
                    };
                    let update = AgentLoopTurnUpdate {
                        context: Some(this.create_context(&next_turn_state, None)),
                        messages: None,
                        model: Some(next_turn_state.model.clone()),
                        thinking_level: Some(next_turn_state.thinking_level),
                    };
                    shared.replace_turn_state(next_turn_state);
                    Some(update)
                })
            })
        };

        let get_steering_messages: crate::agent_loop::GetQueuedMessagesFn = {
            let this = Arc::clone(self);
            let shared = Arc::clone(&shared);
            Arc::new(move || {
                let this = Arc::clone(&this);
                let shared = Arc::clone(&shared);
                Box::pin(async move {
                    let mode = *lock(&this.steering_mode);
                    match this.drain_queued_messages(&this.steer_queue, mode).await {
                        Ok(messages) => messages,
                        Err(error) => {
                            shared.record_failure(error);
                            Vec::new()
                        }
                    }
                })
            })
        };

        let get_follow_up_messages: crate::agent_loop::GetQueuedMessagesFn = {
            let this = Arc::clone(self);
            let shared = Arc::clone(&shared);
            Arc::new(move || {
                let this = Arc::clone(&this);
                let shared = Arc::clone(&shared);
                Box::pin(async move {
                    let mode = *lock(&this.follow_up_mode);
                    match this
                        .drain_queued_messages(&this.follow_up_queue, mode)
                        .await
                    {
                        Ok(messages) => messages,
                        Err(error) => {
                            shared.record_failure(error);
                            Vec::new()
                        }
                    }
                })
            })
        };

        AgentLoopConfig {
            model: turn_state.model.clone(),
            reasoning: thinking_level_from_model_level(turn_state.thinking_level),
            thinking_budgets: None,
            stream_options: StreamOptions::default(),
            tool_execution: crate::types::ToolExecutionMode::Parallel,
            convert_to_llm: Arc::new(|messages| Box::pin(async move { convert_to_llm(&messages) })),
            transform_context: Some(transform_context),
            get_api_key: None,
            finish_turn: None,
            prepare_request: None,
            prepare_next_turn: Some(prepare_next_turn),
            get_steering_messages: Some(get_steering_messages),
            get_follow_up_messages: Some(get_follow_up_messages),
            before_tool_call: Some(before_tool_call),
            after_tool_call: Some(after_tool_call),
            nested_call_summary: None,
        }
    }

    // ------------------------------------------------------------------
    // Validation (agent-harness.ts:500-510)
    // ------------------------------------------------------------------

    /// `validateUniqueNames` (agent-harness.ts:500-504).
    fn validate_unique_names(names: &[String], message: &str) -> Result<(), AgentHarnessError> {
        let duplicates = find_duplicate_names(names);
        if !duplicates.is_empty() {
            return Err(AgentHarnessError::new(
                AgentHarnessErrorCode::InvalidArgument,
                format!("{message}: {}", duplicates.join(", ")),
            ));
        }
        Ok(())
    }

    /// `validateToolNames` (agent-harness.ts:506-510) against an explicit
    /// tool list.
    fn validate_tool_names_against(
        tool_names: &[String],
        tools: &[Arc<dyn AgentHarnessTool<TContext>>],
    ) -> Result<(), AgentHarnessError> {
        Self::validate_unique_names(tool_names, "Duplicate active tool name(s)")?;
        let missing: Vec<String> = tool_names
            .iter()
            .filter(|name| !tools.iter().any(|tool| tool.name() == name.as_str()))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(AgentHarnessError::new(
                AgentHarnessErrorCode::InvalidArgument,
                format!("Unknown tool(s): {}", missing.join(", ")),
            ));
        }
        Ok(())
    }

    fn validate_tool_names(&self, tool_names: &[String]) -> Result<(), AgentHarnessError> {
        let tools = lock(&self.tools).clone();
        Self::validate_tool_names_against(tool_names, &tools)
    }

    // ------------------------------------------------------------------
    // Persistence barrier (agent-harness.ts:512-655)
    // ------------------------------------------------------------------

    /// `flushPendingSessionWrites` (agent-harness.ts:512-536): FIFO; on a
    /// failure the write stays queued (upstream shifts only after the await).
    /// The `Compaction` / `BranchSummary` forms of [`PendingSessionWrite`]
    /// have no upstream flush branch (the if/else chain at :515-533 skips
    /// them) and are never pushed by the harness — they are dropped here too.
    async fn flush_pending_session_writes(&self) -> Result<(), AgentHarnessError> {
        loop {
            let write = lock(&self.pending_session_writes).first().cloned();
            let Some(write) = write else {
                return Ok(());
            };
            match write {
                PendingSessionWrite::Message { message } => {
                    self.session.append_message(message).await
                }
                PendingSessionWrite::ModelChange { provider, model_id } => {
                    self.session.append_model_change(&provider, &model_id).await
                }
                PendingSessionWrite::ThinkingLevelChange { thinking_level } => {
                    self.session
                        .append_thinking_level_change(&thinking_level)
                        .await
                }
                PendingSessionWrite::ActiveToolsChange { active_tool_names } => {
                    self.session
                        .append_active_tools_change(&active_tool_names)
                        .await
                }
                PendingSessionWrite::Custom { custom_type, data } => {
                    self.session.append_custom_entry(&custom_type, data).await
                }
                PendingSessionWrite::CustomMessage {
                    custom_type,
                    content,
                    display,
                    details,
                } => {
                    self.session
                        .append_custom_message_entry(&custom_type, content, display, details)
                        .await
                }
                PendingSessionWrite::Label { target_id, label } => {
                    self.session
                        .append_label(&target_id, label.as_deref())
                        .await
                }
                PendingSessionWrite::SessionInfo { name } => {
                    self.session
                        .append_session_name(name.as_deref().unwrap_or(""))
                        .await
                }
                PendingSessionWrite::Leaf { target_id } => self
                    .session
                    .storage()
                    .set_leaf_id(target_id)
                    .await
                    .map(|_| String::new()),
                // No upstream flush branch (see the doc comment above).
                PendingSessionWrite::Compaction { .. }
                | PendingSessionWrite::BranchSummary { .. } => Ok(String::new()),
            }
            .map_err(session_error)?;
            lock(&self.pending_session_writes).remove(0);
        }
    }

    /// `handleAgentEvent` (agent-harness.ts:538-565): the persistence barrier.
    async fn handle_agent_event(
        &self,
        event: AgentEvent,
        signal: &CancellationToken,
    ) -> Result<(), AgentHarnessError> {
        match &event {
            // :539-543 — persist first, then emit.
            AgentEvent::MessageEnd { message } => {
                self.session
                    .append_message(message.clone())
                    .await
                    .map_err(session_error)?;
                self.emit_any(AgentHarnessEvent::Agent(event), Some(signal))
                    .await
            }
            // :544-556 — emit (capturing errors), flush, rethrow, save point.
            AgentEvent::TurnEnd { .. } => {
                let event_error = self
                    .emit_any(AgentHarnessEvent::Agent(event), Some(signal))
                    .await
                    .err();
                let had_pending_mutations = !lock(&self.pending_session_writes).is_empty();
                self.flush_pending_session_writes().await?;
                if let Some(error) = event_error {
                    return Err(error);
                }
                // agent-harness.ts:554 — save_point is emitted WITHOUT the
                // signal (unlike settled at :561); passing a cancelled token
                // to listeners would be observable after abort.
                self.emit_own(
                    AgentHarnessOwnEvent::SavePoint {
                        had_pending_mutations,
                    },
                    None,
                )
                .await
            }
            // :557-563 — flush, phase to idle, emit, settled.
            AgentEvent::AgentEnd { .. } => {
                self.flush_pending_session_writes().await?;
                *lock(&self.phase) = AgentHarnessPhase::Idle;
                self.emit_any(AgentHarnessEvent::Agent(event), Some(signal))
                    .await?;
                let next_turn_count = lock(&self.next_turn_queue).len() as u64;
                self.emit_own(
                    AgentHarnessOwnEvent::Settled { next_turn_count },
                    Some(signal),
                )
                .await
            }
            _ => {
                self.emit_any(AgentHarnessEvent::Agent(event), Some(signal))
                    .await
            }
        }
    }

    /// `emitRunFailure` (agent-harness.ts:567-579): replay the trailing event
    /// sequence for a synthesized failure message through the barrier.
    async fn emit_run_failure(
        &self,
        model: &Model,
        error_message: &str,
        aborted: bool,
        signal: &CancellationToken,
    ) -> Result<Vec<AgentMessage>, AgentHarnessError> {
        let failure_message =
            AgentMessage::Assistant(create_failure_message(model, error_message, aborted));
        self.handle_agent_event(
            AgentEvent::MessageStart {
                message: failure_message.clone(),
            },
            signal,
        )
        .await?;
        self.handle_agent_event(
            AgentEvent::MessageEnd {
                message: failure_message.clone(),
            },
            signal,
        )
        .await?;
        self.handle_agent_event(
            AgentEvent::TurnEnd {
                message: failure_message.clone(),
                tool_results: Vec::new(),
            },
            signal,
        )
        .await?;
        self.handle_agent_event(
            AgentEvent::AgentEnd {
                messages: vec![failure_message.clone()],
            },
            signal,
        )
        .await?;
        Ok(vec![failure_message])
    }

    /// `executeTurn` (agent-harness.ts:581-656).
    async fn execute_turn(
        self: &Arc<Self>,
        turn_state: TurnState<TContext>,
        text: &str,
        options: Option<AgentHarnessPromptOptions>,
    ) -> Result<AssistantMessage, AgentHarnessError> {
        let images = options.and_then(|options| options.images);
        let mut messages = vec![create_user_message(text, images.clone())];

        // :588-597 — nextTurn messages bypass the loop's queue polls: they are
        // spliced in front of the new user message here.
        let queued_messages = std::mem::take(&mut *lock(&self.next_turn_queue));
        if !queued_messages.is_empty() {
            if let Err(error) = self.emit_queue_update().await {
                let mut queue = lock(&self.next_turn_queue);
                let mut restored = queued_messages;
                restored.append(&mut queue);
                *queue = restored;
                return Err(error);
            }
            messages = queued_messages.into_iter().chain(messages).collect();
        }

        // :598-605 — before_agent_start hook (errors propagate before the run
        // starts; no failure replay upstream).
        let before_result = self
            .emit_hook(AgentHarnessOwnEvent::BeforeAgentStart {
                prompt: text.to_owned(),
                images,
                system_prompt: turn_state.system_prompt.clone(),
                resources: turn_state.resources.clone(),
            })
            .await?;
        let before_result = match before_result {
            Some(HarnessHookResult::BeforeAgentStart(result)) => result,
            _ => None,
        };
        if let Some(extra) = before_result
            .as_ref()
            .and_then(|result| result.messages.clone())
        {
            messages.extend(extra);
        }
        let system_prompt_override = before_result.and_then(|result| result.system_prompt);

        let run_signal = CancellationToken::new();
        *lock(&self.run_abort) = Some(run_signal.clone());
        let shared = Arc::new(RunShared::new(turn_state));

        let run_result = self
            .run_loop_and_replay_failure(
                Arc::clone(&shared),
                messages,
                system_prompt_override,
                &run_signal,
            )
            .await;
        // :649-655 — finally: flush, then clear the run abort handle.
        let flush_result = self.flush_pending_session_writes().await;
        *lock(&self.run_abort) = None;
        let new_messages = match (run_result, flush_result) {
            // A flush failure overrides the run outcome (upstream `finally`).
            (Ok(messages), Ok(())) => messages,
            (Ok(_), Err(error)) => return Err(error),
            (Err(error), Ok(())) => return Err(error),
            (Err(_), Err(error)) => return Err(error),
        };
        new_messages
            .iter()
            .rev()
            .find_map(|message| match message {
                AgentMessage::Assistant(assistant) => Some(assistant.clone()),
                _ => None,
            })
            .ok_or_else(|| {
                AgentHarnessError::new(
                    AgentHarnessErrorCode::InvalidState,
                    "AgentHarness prompt completed without an assistant message",
                )
            })
    }

    /// The `runResultPromise` body (agent-harness.ts:613-639): run the loop,
    /// then replay a recorded failure that no provider call consumed.
    async fn run_loop_and_replay_failure(
        self: &Arc<Self>,
        shared: Arc<RunShared<TContext>>,
        messages: Vec<AgentMessage>,
        system_prompt_override: Option<String>,
        run_signal: &CancellationToken,
    ) -> Result<Vec<AgentMessage>, AgentHarnessError> {
        let context = self.create_context(&shared.turn_state(), system_prompt_override);
        let config = self.create_loop_config(Arc::clone(&shared));
        let stream_fn = self.create_stream_fn(Arc::clone(&shared));

        let sink: AgentEventSink = {
            let this = Arc::clone(self);
            let shared = Arc::clone(&shared);
            let signal = run_signal.clone();
            Arc::new(move |event| {
                let this = Arc::clone(&this);
                let shared = Arc::clone(&shared);
                let signal = signal.clone();
                Box::pin(async move {
                    if let Err(error) = this.handle_agent_event(event, &signal).await {
                        shared.record_failure(error);
                    }
                })
            })
        };

        let new_messages = run_agent_loop(
            messages,
            context,
            config,
            sink,
            Some(run_signal.clone()),
            stream_fn,
        )
        .await;

        match shared.take_failure() {
            Some(failure) => {
                let model = shared.turn_state().model;
                let aborted = run_signal.is_cancelled();
                // agent-harness.ts:631-637: a replay failure masks the run
                // failure with the AggregateError message.
                self.emit_run_failure(&model, &failure.message, aborted, run_signal)
                    .await
                    .map_err(|_| {
                        AgentHarnessError::new(
                            AgentHarnessErrorCode::Unknown,
                            "Agent run failed and failure reporting failed",
                        )
                    })
            }
            None => Ok(new_messages),
        }
    }

    /// Shared skeleton of `prompt` / `skill` / `promptFromTemplate`
    /// (agent-harness.ts:658-705): idle gate → `turn` phase → run → on error
    /// restore `idle` and rethrow → finish the run promise.
    async fn run_turn(
        self: &Arc<Self>,
        resolve_prompt: impl FnOnce(
            &TurnState<TContext>,
        ) -> Result<
            (String, Option<AgentHarnessPromptOptions>),
            AgentHarnessError,
        >,
    ) -> Result<AssistantMessage, AgentHarnessError> {
        if *lock(&self.phase) != AgentHarnessPhase::Idle {
            return Err(busy_error("AgentHarness is busy"));
        }
        *lock(&self.phase) = AgentHarnessPhase::Turn;
        let done = self.start_run();
        let result = match self.create_turn_state().await {
            Ok(turn_state) => match resolve_prompt(&turn_state) {
                Ok((text, options)) => self.execute_turn(turn_state, &text, options).await,
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        // agent-harness.ts:665-667, :783-788: failures restore the idle phase.
        if result.is_err() {
            *lock(&self.phase) = AgentHarnessPhase::Idle;
        }
        self.finish_run(done);
        result
    }

    /// `prompt` (agent-harness.ts:658-671).
    pub async fn prompt(
        self: &Arc<Self>,
        text: &str,
        options: Option<AgentHarnessPromptOptions>,
    ) -> Result<AssistantMessage, AgentHarnessError> {
        let text = text.to_owned();
        self.run_turn(|_turn_state| Ok((text, options))).await
    }

    /// `skill` (agent-harness.ts:673-688).
    pub async fn skill(
        self: &Arc<Self>,
        name: &str,
        additional_instructions: Option<&str>,
    ) -> Result<AssistantMessage, AgentHarnessError> {
        let name = name.to_owned();
        let additional_instructions = additional_instructions.map(str::to_owned);
        self.run_turn(|turn_state| {
            let skill = turn_state
                .resources
                .skills
                .as_ref()
                .and_then(|skills| skills.iter().find(|skill| skill.name == name))
                .cloned()
                .ok_or_else(|| {
                    AgentHarnessError::new(
                        AgentHarnessErrorCode::InvalidArgument,
                        format!("Unknown skill: {name}"),
                    )
                })?;
            Ok((
                format_skill_invocation(&skill, additional_instructions.as_deref()),
                None,
            ))
        })
        .await
    }

    /// `promptFromTemplate` (agent-harness.ts:690-705).
    pub async fn prompt_from_template(
        self: &Arc<Self>,
        name: &str,
        args: &[String],
    ) -> Result<AssistantMessage, AgentHarnessError> {
        let name = name.to_owned();
        let args = args.to_vec();
        self.run_turn(|turn_state| {
            let template = turn_state
                .resources
                .prompt_templates
                .as_ref()
                .and_then(|templates| templates.iter().find(|template| template.name == name))
                .cloned()
                .ok_or_else(|| {
                    AgentHarnessError::new(
                        AgentHarnessErrorCode::InvalidArgument,
                        format!("Unknown prompt template: {name}"),
                    )
                })?;
            Ok((format_prompt_template_invocation(&template, &args), None))
        })
        .await
    }

    // ------------------------------------------------------------------
    // Queues (agent-harness.ts:707-722)
    // ------------------------------------------------------------------

    /// `steer` (agent-harness.ts:707-711).
    pub async fn steer(
        &self,
        text: &str,
        options: Option<AgentHarnessPromptOptions>,
    ) -> Result<(), AgentHarnessError> {
        if *lock(&self.phase) == AgentHarnessPhase::Idle {
            return Err(AgentHarnessError::new(
                AgentHarnessErrorCode::InvalidState,
                "Cannot steer while idle",
            ));
        }
        lock(&self.steer_queue).push(create_user_message(
            text,
            options.and_then(|options| options.images),
        ));
        self.emit_queue_update().await
    }

    /// `followUp` (agent-harness.ts:713-717).
    pub async fn follow_up(
        &self,
        text: &str,
        options: Option<AgentHarnessPromptOptions>,
    ) -> Result<(), AgentHarnessError> {
        if *lock(&self.phase) == AgentHarnessPhase::Idle {
            return Err(AgentHarnessError::new(
                AgentHarnessErrorCode::InvalidState,
                "Cannot follow up while idle",
            ));
        }
        lock(&self.follow_up_queue).push(create_user_message(
            text,
            options.and_then(|options| options.images),
        ));
        self.emit_queue_update().await
    }

    /// `nextTurn` (agent-harness.ts:719-722) — queueable in any phase.
    pub async fn next_turn(
        &self,
        text: &str,
        options: Option<AgentHarnessPromptOptions>,
    ) -> Result<(), AgentHarnessError> {
        lock(&self.next_turn_queue).push(create_user_message(
            text,
            options.and_then(|options| options.images),
        ));
        self.emit_queue_update().await
    }

    /// `appendMessage` (agent-harness.ts:724-734): immediate while idle,
    /// staged into [`PendingSessionWrite`] during a run.
    pub async fn append_message(&self, message: AgentMessage) -> Result<(), AgentHarnessError> {
        if *lock(&self.phase) == AgentHarnessPhase::Idle {
            self.session
                .append_message(message)
                .await
                .map_err(session_error)?;
        } else {
            lock(&self.pending_session_writes).push(PendingSessionWrite::Message { message });
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Compaction (agent-harness.ts:736-789)
    // ------------------------------------------------------------------

    /// `compact` (agent-harness.ts:736-789).
    pub async fn compact(
        self: &Arc<Self>,
        custom_instructions: Option<&str>,
    ) -> Result<CompactResult, AgentHarnessError> {
        if *lock(&self.phase) != AgentHarnessPhase::Idle {
            return Err(busy_error("compact() requires idle harness"));
        }
        *lock(&self.phase) = AgentHarnessPhase::Compaction;
        let result = self.compact_inner(custom_instructions).await;
        *lock(&self.phase) = AgentHarnessPhase::Idle;
        result
    }

    async fn compact_inner(
        self: &Arc<Self>,
        custom_instructions: Option<&str>,
    ) -> Result<CompactResult, AgentHarnessError> {
        let model = lock(&self.model).clone();
        let branch_entries = self.session.get_branch(None).await.map_err(session_error)?;
        let Some((preparation, retained_tail)) =
            prepare_harness_compaction(&branch_entries, &DEFAULT_COMPACTION_SETTINGS)?
        else {
            return Err(AgentHarnessError::new(
                AgentHarnessErrorCode::Compaction,
                "Nothing to compact",
            ));
        };
        let hook_preparation = CompactionPreparation {
            first_kept_entry_id: preparation.first_kept_entry_id.clone(),
            messages_to_summarize: preparation.messages_to_summarize.clone(),
            turn_prefix_messages: preparation.turn_prefix_messages.clone(),
            retained_tail: retained_tail.clone(),
            is_split_turn: preparation.is_split_turn,
            tokens_before: preparation.tokens_before,
            previous_summary: preparation.previous_summary.clone(),
            file_ops: preparation.file_ops.clone(),
            settings: preparation.settings,
        };
        let hook_result = self
            .emit_hook(AgentHarnessOwnEvent::SessionBeforeCompact {
                preparation: hook_preparation,
                branch_entries: branch_entries.clone(),
                custom_instructions: custom_instructions.map(str::to_owned),
                signal: CancellationToken::new(),
            })
            .await?;
        let hook_result = match hook_result {
            Some(HarnessHookResult::SessionBeforeCompact(result)) => result,
            _ => None,
        };
        if hook_result.as_ref().and_then(|result| result.cancel) == Some(true) {
            return Err(AgentHarnessError::new(
                AgentHarnessErrorCode::Compaction,
                "Compaction cancelled",
            ));
        }
        let provided = hook_result.and_then(|result| result.compaction);
        let from_hook = provided.is_some();
        let result = match provided {
            Some(provided) => provided,
            None => {
                let stream_fn = models_stream_fn(&self.models);
                let args = SummarizationArgs {
                    thinking_level: Some(*lock(&self.thinking_level)),
                    retry: self.retry,
                    ..Default::default()
                };
                let callbacks = self.retry_callbacks(RetryOperation::Compaction);
                let compacted = run_compact(
                    &preparation,
                    &model,
                    custom_instructions,
                    &stream_fn,
                    &args,
                    Some(&callbacks),
                )
                .await
                .map_err(|error| {
                    AgentHarnessError::new(AgentHarnessErrorCode::Compaction, error.to_string())
                })?;
                CompactResult {
                    summary: compacted.summary,
                    tokens_before: compacted.tokens_before,
                    usage: compacted.usage,
                    retained_tail,
                    details: compacted.details,
                }
            }
        };
        let entry_id = self
            .session
            .append_compaction(
                &result.summary,
                // 44289550a @ 4181f66: harness compaction entries anchor via
                // `retainedTail` (checkpoint semantics); no firstKeptEntryId.
                None,
                result.tokens_before,
                AppendCompactionOptions {
                    details: result.details.clone(),
                    from_hook: Some(from_hook),
                    usage: result.usage.clone(),
                    retained_tail: Some(result.retained_tail.clone()),
                },
            )
            .await
            .map_err(session_error)?;
        if let Some(SessionEntry::Compaction(entry)) = self
            .session
            .get_entry(&entry_id)
            .await
            .map_err(session_error)?
        {
            self.emit_own(
                AgentHarnessOwnEvent::SessionCompact {
                    compaction_entry: entry,
                    from_hook,
                },
                None,
            )
            .await?;
        }
        Ok(result)
    }

    // ------------------------------------------------------------------
    // Tree navigation (agent-harness.ts:791-882)
    // ------------------------------------------------------------------

    /// `navigateTree` (agent-harness.ts:791-882).
    pub async fn navigate_tree(
        self: &Arc<Self>,
        target_id: &str,
        options: NavigateTreeOptions,
    ) -> Result<NavigateTreeResult, AgentHarnessError> {
        if *lock(&self.phase) != AgentHarnessPhase::Idle {
            return Err(busy_error("navigateTree() requires idle harness"));
        }
        *lock(&self.phase) = AgentHarnessPhase::BranchSummary;
        let result = self.navigate_tree_inner(target_id, options).await;
        *lock(&self.phase) = AgentHarnessPhase::Idle;
        result
    }

    async fn navigate_tree_inner(
        self: &Arc<Self>,
        target_id: &str,
        options: NavigateTreeOptions,
    ) -> Result<NavigateTreeResult, AgentHarnessError> {
        let old_leaf_id = self.session.get_leaf_id().await.map_err(session_error)?;
        if old_leaf_id.as_deref() == Some(target_id) {
            return Ok(NavigateTreeResult {
                cancelled: false,
                editor_text: None,
                summary_entry: None,
            });
        }
        let target_entry = self
            .session
            .get_entry(target_id)
            .await
            .map_err(session_error)?
            .ok_or_else(|| {
                AgentHarnessError::new(
                    AgentHarnessErrorCode::InvalidArgument,
                    format!("Entry {target_id} not found"),
                )
            })?;
        // The crate collector takes the two root-first branches
        // (branch_summarization.rs:123-135); `get_branch` stops at the latest
        // compaction, matching the crate's session-manager caller.
        let old_path = self.session.get_branch(None).await.map_err(session_error)?;
        let target_path = self
            .session
            .get_branch(Some(target_id))
            .await
            .map_err(session_error)?;
        let collected =
            collect_entries_for_branch_summary(&old_path, old_leaf_id.as_deref(), &target_path);
        let preparation = TreePreparation {
            target_id: target_id.to_owned(),
            old_leaf_id: old_leaf_id.clone(),
            common_ancestor_id: collected.common_ancestor_id,
            entries_to_summarize: collected.entries.clone(),
            user_wants_summary: options.summarize,
            custom_instructions: options.custom_instructions.clone(),
            replace_instructions: options.replace_instructions,
            label: options.label.clone(),
        };
        let hook_result = self
            .emit_hook(AgentHarnessOwnEvent::SessionBeforeTree {
                preparation,
                signal: CancellationToken::new(),
            })
            .await?;
        let hook_result = match hook_result {
            Some(HarnessHookResult::SessionBeforeTree(result)) => result,
            _ => None,
        };
        if hook_result.as_ref().and_then(|result| result.cancel) == Some(true) {
            return Ok(NavigateTreeResult {
                cancelled: true,
                editor_text: None,
                summary_entry: None,
            });
        }
        let hook_summary = hook_result
            .as_ref()
            .and_then(|result| result.summary.clone());
        let mut summary_text = hook_summary.as_ref().map(|summary| summary.summary.clone());
        let mut summary_details = hook_summary
            .as_ref()
            .and_then(|summary| summary.details.clone());
        let mut summary_usage = hook_summary
            .as_ref()
            .and_then(|summary| summary.usage.clone());
        let needs_summary = summary_text.as_ref().is_none_or(String::is_empty)
            && options.summarize
            && !collected.entries.is_empty();
        if needs_summary {
            let model = lock(&self.model).clone();
            let stream_fn = models_stream_fn(&self.models);
            let args = SummarizationArgs {
                signal: Some(CancellationToken::new()),
                retry: self.retry,
                ..Default::default()
            };
            let callbacks = self.retry_callbacks(RetryOperation::BranchSummary);
            let gen_options = GenerateBranchSummaryOptions {
                model: &model,
                stream_fn: &stream_fn,
                args: &args,
                custom_instructions: hook_result
                    .as_ref()
                    .and_then(|result| result.custom_instructions.as_deref())
                    .or(options.custom_instructions.as_deref()),
                replace_instructions: hook_result
                    .as_ref()
                    .and_then(|result| result.replace_instructions)
                    .or(options.replace_instructions)
                    .unwrap_or(false),
                reserve_tokens: DEFAULT_BRANCH_RESERVE_TOKENS,
                callbacks: Some(&callbacks),
            };
            let branch_summary = generate_branch_summary(&collected.entries, &gen_options).await;
            if branch_summary.aborted == Some(true) {
                return Ok(NavigateTreeResult {
                    cancelled: true,
                    editor_text: None,
                    summary_entry: None,
                });
            }
            if let Some(error) = branch_summary.error {
                return Err(AgentHarnessError::new(
                    AgentHarnessErrorCode::BranchSummary,
                    error,
                ));
            }
            summary_text = branch_summary.summary;
            summary_usage = branch_summary.usage;
            let details = BranchSummaryDetails {
                read_files: branch_summary.read_files.unwrap_or_default(),
                modified_files: branch_summary.modified_files.unwrap_or_default(),
            };
            summary_details = Some(serde_json::to_value(&details).map_err(|error| {
                AgentHarnessError::new(AgentHarnessErrorCode::BranchSummary, error.to_string())
            })?);
        }
        let from_hook = hook_summary.is_some();
        let (new_leaf_id, editor_text) = match &target_entry {
            SessionEntry::Message(entry) => match &entry.message {
                AgentMessage::User(user) => (
                    entry.parent_id.clone(),
                    Some(content_text_user(&user.content, "")),
                ),
                _ => (Some(target_id.to_owned()), None),
            },
            SessionEntry::CustomMessage(entry) => (
                entry.parent_id.clone(),
                Some(content_text_user(&entry.content, "")),
            ),
            _ => (Some(target_id.to_owned()), None),
        };
        let summary = summary_text.map(|summary| MoveToSummary {
            summary,
            details: summary_details,
            usage: summary_usage,
            from_hook: Some(from_hook),
        });
        let summary_id = self
            .session
            .move_to(new_leaf_id.as_deref(), summary)
            .await
            .map_err(session_error)?;
        let mut summary_entry = None;
        if let Some(summary_id) = summary_id
            && let Some(SessionEntry::BranchSummary(entry)) = self
                .session
                .get_entry(&summary_id)
                .await
                .map_err(session_error)?
        {
            summary_entry = Some(entry);
        }
        let current_leaf_id = self.session.get_leaf_id().await.map_err(session_error)?;
        self.emit_own(
            AgentHarnessOwnEvent::SessionTree {
                new_leaf_id: current_leaf_id,
                old_leaf_id,
                summary_entry: summary_entry.clone(),
                from_hook: Some(from_hook),
            },
            None,
        )
        .await?;
        Ok(NavigateTreeResult {
            cancelled: false,
            editor_text,
            summary_entry,
        })
    }

    // ------------------------------------------------------------------
    // Getters and setters (agent-harness.ts:884-1023)
    // ------------------------------------------------------------------

    /// `getModel` (agent-harness.ts:884-886).
    pub fn get_model(&self) -> Model {
        lock(&self.model).clone()
    }

    /// `setModel` (agent-harness.ts:888-901): persist immediately while idle,
    /// stage into pending writes during a run.
    pub async fn set_model(&self, model: Model) -> Result<(), AgentHarnessError> {
        let previous_model = lock(&self.model).clone();
        if *lock(&self.phase) == AgentHarnessPhase::Idle {
            self.session
                .append_model_change(&model.provider, &model.id)
                .await
                .map_err(session_error)?;
        } else {
            lock(&self.pending_session_writes).push(PendingSessionWrite::ModelChange {
                provider: model.provider.clone(),
                model_id: model.id.clone(),
            });
        }
        *lock(&self.model) = model.clone();
        self.emit_own(
            AgentHarnessOwnEvent::ModelUpdate {
                model: Box::new(model),
                previous_model: Some(Box::new(previous_model)),
                source: UpdateSource::Set,
            },
            None,
        )
        .await
    }

    /// `getThinkingLevel` (agent-harness.ts:903-905).
    pub fn get_thinking_level(&self) -> ThinkingLevel {
        *lock(&self.thinking_level)
    }

    /// `setThinkingLevel` (agent-harness.ts:907-920).
    pub async fn set_thinking_level(&self, level: ThinkingLevel) -> Result<(), AgentHarnessError> {
        let previous_level = *lock(&self.thinking_level);
        if *lock(&self.phase) == AgentHarnessPhase::Idle {
            self.session
                .append_thinking_level_change(level.as_str())
                .await
                .map_err(session_error)?;
        } else {
            lock(&self.pending_session_writes).push(PendingSessionWrite::ThinkingLevelChange {
                thinking_level: level.as_str().to_owned(),
            });
        }
        *lock(&self.thinking_level) = level;
        self.emit_own(
            AgentHarnessOwnEvent::ThinkingLevelUpdate {
                level,
                previous_level,
            },
            None,
        )
        .await
    }

    /// `getTools` (agent-harness.ts:922-924) — insertion order, copy.
    pub fn get_tools(&self) -> Vec<Arc<dyn AgentHarnessTool<TContext>>> {
        lock(&self.tools).clone()
    }

    /// `setTools` (agent-harness.ts:926-955).
    pub async fn set_tools(
        &self,
        tools: Vec<Arc<dyn AgentHarnessTool<TContext>>>,
        active_tool_names: Option<Vec<String>>,
    ) -> Result<(), AgentHarnessError> {
        Self::validate_unique_names(
            &tools
                .iter()
                .map(|tool| tool.name().to_owned())
                .collect::<Vec<_>>(),
            "Duplicate tool name(s)",
        )?;
        let next_active_tool_names =
            active_tool_names.unwrap_or_else(|| lock(&self.active_tool_names).clone());
        Self::validate_tool_names_against(&next_active_tool_names, &tools)?;
        let previous_tool_names = lock(&self.tools)
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect::<Vec<_>>();
        let previous_active_tool_names = lock(&self.active_tool_names).clone();
        if *lock(&self.phase) == AgentHarnessPhase::Idle {
            self.session
                .append_active_tools_change(&next_active_tool_names)
                .await
                .map_err(session_error)?;
        } else {
            lock(&self.pending_session_writes).push(PendingSessionWrite::ActiveToolsChange {
                active_tool_names: next_active_tool_names.clone(),
            });
        }
        *lock(&self.tools) = tools;
        *lock(&self.active_tool_names) = next_active_tool_names;
        let tool_names = lock(&self.tools)
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect();
        let active_tool_names = lock(&self.active_tool_names).clone();
        self.emit_own(
            AgentHarnessOwnEvent::ToolsUpdate {
                tool_names,
                previous_tool_names,
                active_tool_names,
                previous_active_tool_names,
                source: UpdateSource::Set,
            },
            None,
        )
        .await
    }

    /// `getActiveTools` (agent-harness.ts:957-959).
    pub fn get_active_tools(&self) -> Vec<Arc<dyn AgentHarnessTool<TContext>>> {
        let tools = lock(&self.tools).clone();
        lock(&self.active_tool_names)
            .iter()
            .filter_map(|name| tools.iter().find(|tool| tool.name() == name).cloned())
            .collect()
    }

    /// `setActiveTools` (agent-harness.ts:961-983).
    pub async fn set_active_tools(&self, tool_names: Vec<String>) -> Result<(), AgentHarnessError> {
        self.validate_tool_names(&tool_names)?;
        let previous_tool_names = lock(&self.tools)
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect::<Vec<_>>();
        let previous_active_tool_names = lock(&self.active_tool_names).clone();
        if *lock(&self.phase) == AgentHarnessPhase::Idle {
            self.session
                .append_active_tools_change(&tool_names)
                .await
                .map_err(session_error)?;
        } else {
            lock(&self.pending_session_writes).push(PendingSessionWrite::ActiveToolsChange {
                active_tool_names: tool_names.clone(),
            });
        }
        *lock(&self.active_tool_names) = tool_names;
        let current_tool_names = lock(&self.tools)
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect();
        let current_active_tool_names = lock(&self.active_tool_names).clone();
        self.emit_own(
            AgentHarnessOwnEvent::ToolsUpdate {
                tool_names: current_tool_names,
                previous_tool_names,
                active_tool_names: current_active_tool_names,
                previous_active_tool_names,
                source: UpdateSource::Set,
            },
            None,
        )
        .await
    }

    /// `getSteeringMode` (agent-harness.ts:985-987).
    pub fn get_steering_mode(&self) -> QueueMode {
        *lock(&self.steering_mode)
    }

    /// `setSteeringMode` (agent-harness.ts:989-991) — sync (upstream `async`
    /// performs no I/O).
    pub fn set_steering_mode(&self, mode: QueueMode) {
        *lock(&self.steering_mode) = mode;
    }

    /// `getFollowUpMode` (agent-harness.ts:993-995).
    pub fn get_follow_up_mode(&self) -> QueueMode {
        *lock(&self.follow_up_mode)
    }

    /// `setFollowUpMode` (agent-harness.ts:997-999) — sync.
    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        *lock(&self.follow_up_mode) = mode;
    }

    /// `getResources` (agent-harness.ts:1001-1006) — cloned vectors.
    pub fn get_resources(&self) -> AgentHarnessResources {
        lock(&self.resources).clone()
    }

    /// `setResources` (agent-harness.ts:1008-1015).
    pub async fn set_resources(
        &self,
        resources: AgentHarnessResources,
    ) -> Result<(), AgentHarnessError> {
        let previous_resources = self.get_resources();
        *lock(&self.resources) = resources;
        self.emit_own(
            AgentHarnessOwnEvent::ResourcesUpdate {
                resources: self.get_resources(),
                previous_resources,
            },
            None,
        )
        .await
    }

    /// `getStreamOptions` (agent-harness.ts:1017-1019) — clone.
    pub fn get_stream_options(&self) -> AgentHarnessStreamOptions {
        lock(&self.stream_options).clone()
    }

    /// `setStreamOptions` (agent-harness.ts:1021-1023) — sync; takes effect at
    /// the next turn snapshot.
    pub fn set_stream_options(&self, stream_options: AgentHarnessStreamOptions) {
        *lock(&self.stream_options) = stream_options;
    }

    // ------------------------------------------------------------------
    // Abort and idle (agent-harness.ts:1025-1056)
    // ------------------------------------------------------------------

    /// `abort` (agent-harness.ts:1025-1052): clear steer + follow-up (never
    /// next-turn), cancel the run, then notify; step errors are aggregated.
    pub async fn abort(&self) -> Result<AbortResult, AgentHarnessError> {
        let cleared_steer = std::mem::take(&mut *lock(&self.steer_queue));
        let cleared_follow_up = std::mem::take(&mut *lock(&self.follow_up_queue));
        if let Some(signal) = lock(&self.run_abort).clone() {
            signal.cancel();
        }
        let mut errors: Vec<AgentHarnessError> = Vec::new();
        if let Err(error) = self.emit_queue_update().await {
            errors.push(error);
        }
        self.wait_for_idle().await;
        if let Err(error) = self
            .emit_own(
                AgentHarnessOwnEvent::Abort {
                    cleared_steer: cleared_steer.clone(),
                    cleared_follow_up: cleared_follow_up.clone(),
                },
                None,
            )
            .await
        {
            errors.push(error);
        }
        if !errors.is_empty() {
            // Upstream wraps multiple failures in an AggregateError with this
            // message; a single failure passes through (agent-harness.ts:1047-1050).
            if errors.len() == 1 {
                return Err(errors.remove(0));
            }
            return Err(AgentHarnessError::new(
                AgentHarnessErrorCode::Hook,
                "Abort completed with errors",
            ));
        }
        Ok(AbortResult {
            cleared_steer,
            cleared_follow_up,
        })
    }

    /// `waitForIdle` (agent-harness.ts:1054-1056): resolve when the current
    /// run — including awaited listeners — has finished.
    pub async fn wait_for_idle(&self) {
        let done = lock(&self.run_done).clone();
        if let Some(done) = done {
            done.cancelled().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{CompactionEntry, MessageEntry};
    use rpi_ai::types::{UserContent, UserMessage};

    fn user_message(text: &str, timestamp: i64) -> AgentMessage {
        AgentMessage::User(UserMessage {
            role: Default::default(),
            content: UserContent::Text(text.to_owned()),
            timestamp,
        })
    }

    fn message_entry(id: &str, message: AgentMessage) -> SessionEntry {
        SessionEntry::Message(MessageEntry {
            id: id.to_owned(),
            parent_id: None,
            timestamp: "2026-01-01T00:00:00.000Z".to_owned(),
            message,
        })
    }

    fn compaction_entry(
        id: &str,
        first_kept_entry_id: Option<&str>,
        retained_tail: Option<Vec<AgentMessage>>,
    ) -> SessionEntry {
        SessionEntry::Compaction(CompactionEntry {
            id: id.to_owned(),
            parent_id: None,
            timestamp: "2026-01-01T00:00:00.000Z".to_owned(),
            summary: "prev summary".to_owned(),
            first_kept_entry_id: first_kept_entry_id.map(str::to_owned),
            tokens_before: 100,
            retained_tail,
            details: None,
            usage: None,
            from_hook: None,
            system_message: None,
        })
    }

    fn user_texts(messages: &[AgentMessage]) -> Vec<&str> {
        messages
            .iter()
            .map(|message| match message {
                AgentMessage::User(user) => match &user.content {
                    UserContent::Text(text) => text.as_str(),
                    other => panic!("unexpected content: {other:?}"),
                },
                other => panic!("unexpected message: {other:?}"),
            })
            .collect()
    }

    fn prepared_sequence(
        preparation: &crate::compaction::CompactionPreparation,
        retained_tail: &[AgentMessage],
    ) -> Vec<String> {
        let mut all = preparation.messages_to_summarize.clone();
        all.extend(preparation.turn_prefix_messages.iter().cloned());
        all.extend(retained_tail.iter().cloned());
        user_texts(&all).into_iter().map(str::to_owned).collect()
    }

    /// 44289550a @ 4181f66 (compaction.ts:633-645): a previous compaction's
    /// `retainedTail` is virtualized into message entries for the next
    /// preparation — the retained messages lead the compactable sequence.
    #[test]
    fn prepare_virtualizes_previous_compaction_retained_tail() {
        let entries = vec![
            message_entry("e1", user_message("first", 1)),
            compaction_entry(
                "c1",
                None,
                Some(vec![user_message("tail-a", 2), user_message("tail-b", 3)]),
            ),
            message_entry("e3", user_message("after", 4)),
        ];
        let (preparation, retained_tail) =
            prepare_harness_compaction(&entries, &DEFAULT_COMPACTION_SETTINGS)
                .expect("prepare")
                .expect("something to compact");
        assert_eq!(
            preparation.previous_summary.as_deref(),
            Some("prev summary")
        );
        assert_eq!(
            prepared_sequence(&preparation, &retained_tail),
            vec!["tail-a", "tail-b", "after"]
        );
    }

    /// Legacy harness entries (first_kept_entry_id anchor, no retainedTail)
    /// keep the v1 boundary behavior for backward compatibility.
    #[test]
    fn prepare_falls_back_to_first_kept_entry_anchor_for_legacy_entries() {
        let entries = vec![
            message_entry("e1", user_message("first", 1)),
            message_entry("e2", user_message("second", 2)),
            compaction_entry("c1", Some("e2"), None),
            message_entry("e3", user_message("after", 4)),
        ];
        let (preparation, retained_tail) =
            prepare_harness_compaction(&entries, &DEFAULT_COMPACTION_SETTINGS)
                .expect("prepare")
                .expect("something to compact");
        assert_eq!(
            preparation.previous_summary.as_deref(),
            Some("prev summary")
        );
        assert_eq!(
            prepared_sequence(&preparation, &retained_tail),
            vec!["second", "after"]
        );
    }
}
