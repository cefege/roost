//! Port of `packages/coding-agent/src/core/compaction/compaction.ts` @ pi
//! 0.82.1 (2efa728).
//!
//! Context compaction for long sessions: token estimation, cut-point search,
//! summary generation, and the `CompactionEntry` payload. Pure functions and
//! the `StreamFn`-driven summarization calls live here so the `rpi` main path
//! and the T16 harness share one implementation (design §4.5); session I/O
//! and the agent-session trigger wiring live in `rpi::core::compaction_runner`.
//!
//! Intentional differences:
//! - JS `String.length` counts UTF-16 code units; `chars` here counts Unicode
//!   scalar values. Identical for BMP text (same convention as
//!   `rpi_ai::utils::estimate`, D-003). ADR-0002 §4 forbids any other change
//!   to the estimation algorithm.
//! - `JSON.stringify(args)` becomes `serde_json::to_string` (compact form,
//!   key order preserved). Number formatting can differ for float-valued tool
//!   arguments (D-012 notes the same boundary for unknown entry fields).
//! - Upstream `streamFn(model, context, simpleOptions)` — the rpi `StreamFn`
//!   shape takes `StreamOptions`, which carries `reasoning` instead (see
//!   `rpi_ai::types::StreamOptions`).
//! - `completeSimple` fallback (streamFn-less calls) is not ported: rpi-agent
//!   never talks to providers directly, `StreamFn` is mandatory
//!   (coding-standards §4.2).

pub mod branch_summarization;
pub mod utils;

use futures::StreamExt;
use rpi_ai::types::{
    AssistantContent, AssistantMessage, AssistantRole, CacheRetention, Context, Model, StopReason,
    StreamEvent, StreamOptions, TextContent, Usage, UserContentBlock, UserMessage, UserRole,
};
use rpi_ai::utils::retry::{RetryCallbacks, RetryPolicy, retry_assistant_call};
use rpi_ai::utils::text::content_text_assistant;
use rpi_ai::utils::uuid::uuidv7_now;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::now_millis;
use crate::error::AgentError;
use crate::messages::{AgentMessage, convert_to_llm};
use crate::session::{
    ProjectedSessionEntry, SessionEntry, SessionProjection, build_session_projection,
    session_entry_to_context_messages,
};
use crate::stream_fn::{BoxStream, StreamFn};
use crate::types::ThinkingLevel;
use utils::{
    FileOperations, SUMMARIZATION_SYSTEM_PROMPT, compute_file_lists, create_file_ops,
    extract_file_ops_from_message, format_file_operations, serialize_conversation,
};

pub use rpi_ai::utils::estimate::{ContextUsageEstimate, calculate_context_tokens};

// ============================================================================
// File Operation Tracking
// ============================================================================

/// Details stored in `CompactionEntry.details` for file tracking
/// (`CompactionDetails`, compaction.ts:34-37; camelCase on the wire).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

/// `extractFileOperations` (compaction.ts:42-70): file ops from the previous
/// compaction's `details` (pi-generated only) plus tool calls in messages.
///
/// `pub(crate)` for `harness::agent_harness::prepare_harness_compaction`, the
/// harness-variant preparation (harness/compaction/compaction.ts:640-713).
pub(crate) fn extract_file_operations(
    messages: &[AgentMessage],
    entries: &[SessionEntry],
    prev_compaction_index: Option<usize>,
) -> FileOperations {
    let mut file_ops = create_file_ops();

    if let Some(index) = prev_compaction_index
        && let SessionEntry::Compaction(prev) = &entries[index]
    {
        // fromHook field kept for session file compatibility; only
        // pi-generated compactions carry the default details shape.
        if prev.from_hook != Some(true)
            && let Some(details) = &prev.details
        {
            if let Some(read_files) = details.get("readFiles").and_then(Value::as_array) {
                for f in read_files.iter().filter_map(Value::as_str) {
                    file_ops.read.insert(f.to_owned());
                }
            }
            if let Some(modified) = details.get("modifiedFiles").and_then(Value::as_array) {
                for f in modified.iter().filter_map(Value::as_str) {
                    file_ops.edited.insert(f.to_owned());
                }
            }
        }
    }

    for msg in messages {
        extract_file_ops_from_message(msg, &mut file_ops);
    }

    file_ops
}

// ============================================================================
// Message Extraction
// ============================================================================

/// Result from [`compact`] — the SessionManager adds id/parentId when saving
/// (`CompactionResult`, compaction.ts:88-97). Serialized camelCase for the
/// `compaction_end` event payload (agent-session.ts:157-163).
/// `Deserialize` was added in T15 W1 so the extension host's
/// `session_before_compact` result can carry an extension-provided
/// compaction back across the JSON event boundary (types.ts:1106-1109).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult {
    pub summary: String,
    pub first_kept_entry_id: String,
    pub tokens_before: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_tokens_after: Option<u64>,
    /// Usage from the LLM call(s) that generated this summary, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Implementation-specific data (default: [`CompactionDetails`] JSON).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// `combineUsage` (compaction.ts:99-120).
fn combine_usage(first: &Usage, second: &Usage) -> Usage {
    Usage {
        input: first.input + second.input,
        output: first.output + second.output,
        cache_read: first.cache_read + second.cache_read,
        cache_write: first.cache_write + second.cache_write,
        cache_write1h: match (first.cache_write1h, second.cache_write1h) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
        },
        reasoning: match (first.reasoning, second.reasoning) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
        },
        total_tokens: first.total_tokens + second.total_tokens,
        cost: rpi_ai::types::UsageCost {
            input: first.cost.input + second.cost.input,
            output: first.cost.output + second.cost.output,
            cache_read: first.cost.cache_read + second.cost.cache_read,
            cache_write: first.cost.cache_write + second.cost.cache_write,
            total: first.cost.total + second.cost.total,
        },
    }
}

// ============================================================================
// Types
// ============================================================================

/// `CompactionSettings` (compaction.ts:126-130). camelCase on the wire
/// (settings.json / golden fixtures).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    pub enabled: bool,
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
}

/// `DEFAULT_COMPACTION_SETTINGS` (compaction.ts:132-136).
pub const DEFAULT_COMPACTION_SETTINGS: CompactionSettings = CompactionSettings {
    enabled: true,
    reserve_tokens: 16384,
    keep_recent_tokens: 20000,
};

// ============================================================================
// Token calculation
// ============================================================================

/// `getAssistantUsage` (compaction.ts:154-167): usage of an assistant
/// message, skipping aborted/error/all-zero messages.
fn get_assistant_usage(msg: &AgentMessage) -> Option<&Usage> {
    if let AgentMessage::Assistant(assistant) = msg
        && assistant.stop_reason != StopReason::Aborted
        && assistant.stop_reason != StopReason::Error
        && calculate_context_tokens(&assistant.usage) > 0
    {
        return Some(&assistant.usage);
    }
    None
}

/// `getLastAssistantUsage` (compaction.ts:172-181): last valid assistant
/// usage from session entries.
pub fn get_last_assistant_usage(entries: &[SessionEntry]) -> Option<&Usage> {
    for entry in entries.iter().rev() {
        if let SessionEntry::Message(m) = entry
            && let Some(usage) = get_assistant_usage(&m.message)
        {
            return Some(usage);
        }
    }
    None
}

/// `getLastAssistantUsageInfo` (compaction.ts:190-196).
fn last_assistant_usage_info(messages: &[AgentMessage]) -> Option<(&Usage, usize)> {
    for (i, msg) in messages.iter().enumerate().rev() {
        if let Some(usage) = get_assistant_usage(msg) {
            return Some((usage, i));
        }
    }
    None
}

/// `estimateContextTokens` (compaction.ts:202-230): last valid usage anchor
/// plus `estimateTokens` for the trailing messages.
pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    match last_assistant_usage_info(messages) {
        None => {
            let estimated: u64 = messages.iter().map(estimate_tokens).sum();
            ContextUsageEstimate {
                tokens: estimated,
                usage_tokens: 0,
                trailing_tokens: estimated,
                last_usage_index: None,
            }
        }
        Some((usage, index)) => {
            let usage_tokens = calculate_context_tokens(usage);
            let trailing_tokens: u64 = messages[index + 1..].iter().map(estimate_tokens).sum();
            ContextUsageEstimate {
                tokens: usage_tokens + trailing_tokens,
                usage_tokens,
                trailing_tokens,
                last_usage_index: Some(index),
            }
        }
    }
}

/// `estimateMessagesTokens` (agent-session.ts:284-289): plain
/// `estimateTokens` sum over the context messages — used for
/// `CompactionResult.estimatedTokensAfter` (agent-session.ts:1876/2157).
/// Deliberately NOT the usage-anchored estimate: the pinned upstream sums.
pub fn estimate_messages_tokens(messages: &[AgentMessage]) -> u64 {
    messages.iter().map(estimate_tokens).sum()
}

/// `shouldCompact` (compaction.ts:235-238). The subtraction is signed: with
/// `reserveTokens > contextWindow` any positive usage triggers (JS semantics).
pub fn should_compact(
    context_tokens: u64,
    context_window: u64,
    settings: &CompactionSettings,
) -> bool {
    if !settings.enabled {
        return false;
    }
    (context_tokens as i64) > (context_window as i64 - settings.reserve_tokens as i64)
}

// ============================================================================
// Cut point detection
// ============================================================================

/// `ESTIMATED_IMAGE_CHARS` (compaction.ts:244).
const ESTIMATED_IMAGE_CHARS: usize = 4800;

fn char_len(text: &str) -> usize {
    text.chars().count()
}

fn safe_json_stringify<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[unserializable]".to_owned())
}

/// `estimateTextAndImageContentChars` for user/custom content
/// (compaction.ts:246-260).
fn estimate_user_content_chars(content: &rpi_ai::types::UserContent) -> usize {
    match content {
        rpi_ai::types::UserContent::Text(text) => char_len(text),
        rpi_ai::types::UserContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block {
                UserContentBlock::Text(text) => char_len(&text.text),
                UserContentBlock::Image(_) => ESTIMATED_IMAGE_CHARS,
            })
            .sum(),
    }
}

/// `estimateTokens` — byte-for-byte port of compaction.ts:266-306 (chars/4
/// heuristic; do not "improve", ADR-0002 §4).
pub fn estimate_tokens(message: &AgentMessage) -> u64 {
    let chars: usize = match message {
        // Upstream counts the current prompt/tool state: content + sections +
        // `toolsAdded` declarations (compaction.ts:302-312 @ abe508e1b, the
        // R5 "effective system and tool context" fix).
        AgentMessage::System(system) => {
            let mut chars = match &system.content {
                rpi_ai::types::SystemContent::Text(text) => char_len(text),
                rpi_ai::types::SystemContent::Blocks(blocks) => {
                    blocks.iter().map(|block| char_len(&block.text)).sum()
                }
            };
            if let Some(sections) = &system.sections {
                for section in sections.values() {
                    if let Some(text) = section.as_str() {
                        chars += char_len(text);
                    }
                }
            }
            if let Some(tools) = &system.tools_added {
                chars += char_len(&safe_json_stringify(tools));
            }
            chars
        }
        AgentMessage::User(user) => estimate_user_content_chars(&user.content),
        AgentMessage::Assistant(assistant) => assistant
            .content
            .iter()
            .map(|block| match block {
                rpi_ai::types::AssistantContent::Text(text) => char_len(&text.text),
                rpi_ai::types::AssistantContent::Thinking(thinking) => char_len(&thinking.thinking),
                rpi_ai::types::AssistantContent::ToolCall(call) => {
                    char_len(&call.name) + char_len(&safe_json_stringify(&call.arguments))
                }
            })
            .sum(),
        AgentMessage::Custom(custom) => estimate_user_content_chars(&custom.content),
        AgentMessage::ToolResult(result) => result
            .content
            .iter()
            .map(|block| match block {
                rpi_ai::types::ToolResultContent::Text(text) => char_len(&text.text),
                rpi_ai::types::ToolResultContent::Image(_) => ESTIMATED_IMAGE_CHARS,
            })
            .sum(),
        AgentMessage::BashExecution(bash) => char_len(&bash.command) + char_len(&bash.output),
        AgentMessage::BranchSummary(summary) => char_len(&summary.summary),
        AgentMessage::CompactionSummary(summary) => char_len(&summary.summary),
    };
    (chars as u64).div_ceil(4)
}

/// `isCutPointMessage` (compaction.ts:308-321) — never cut at tool results.
fn is_cut_point_message(message: &AgentMessage) -> bool {
    match message {
        AgentMessage::User(_)
        | AgentMessage::Assistant(_)
        | AgentMessage::BashExecution(_)
        | AgentMessage::Custom(_)
        | AgentMessage::BranchSummary(_)
        | AgentMessage::CompactionSummary(_) => true,
        // #9548: prompt/tool state never anchors a cut.
        AgentMessage::System(_) | AgentMessage::ToolResult(_) => false,
    }
}

/// `isTurnStartMessage` (compaction.ts:323-336).
fn is_turn_start_message(message: &AgentMessage) -> bool {
    match message {
        AgentMessage::User(_)
        | AgentMessage::BashExecution(_)
        | AgentMessage::Custom(_)
        | AgentMessage::BranchSummary(_)
        | AgentMessage::CompactionSummary(_) => true,
        // #9548: a system message is prompt state, not a turn start.
        AgentMessage::Assistant(_) | AgentMessage::ToolResult(_) | AgentMessage::System(_) => false,
    }
}

/// `isTurnStartEntry` (compaction.ts:338-343).
fn is_turn_start_entry(entry: &SessionEntry) -> bool {
    if matches!(entry, SessionEntry::Compaction(_)) {
        return false;
    }
    session_entry_to_context_messages(entry)
        .iter()
        .any(is_turn_start_message)
}

/// `findValidCutPoints` (compaction.ts:351-363).
fn find_valid_cut_points(
    entries: &[SessionEntry],
    start_index: usize,
    end_index: usize,
) -> Vec<usize> {
    let mut cut_points = Vec::new();
    for (i, entry) in entries.iter().enumerate().take(end_index).skip(start_index) {
        if matches!(entry, SessionEntry::Compaction(_)) {
            continue;
        }
        if session_entry_to_context_messages(entry)
            .iter()
            .any(is_cut_point_message)
        {
            cut_points.push(i);
        }
    }
    cut_points
}

/// `findTurnStartIndex` (compaction.ts:369-376).
pub fn find_turn_start_index(
    entries: &[SessionEntry],
    entry_index: usize,
    start_index: usize,
) -> Option<usize> {
    (start_index..=entry_index)
        .rev()
        .find(|&i| is_turn_start_entry(&entries[i]))
}

/// `CutPointResult` (compaction.ts:378-385). `turn_start_index` is `None`
/// when the cut does not split a turn (upstream `-1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CutPointResult {
    /// Index of first entry to keep.
    pub first_kept_entry_index: usize,
    /// Index of the message that starts the turn being split, if splitting.
    pub turn_start_index: Option<usize>,
    /// Whether this cut splits a turn (cut point is not a turn-start message).
    pub is_split_turn: bool,
}

/// `findCutPoint` (compaction.ts:403-461 @ 8bdcd4498): walk backwards from
/// newest, accumulating estimated message sizes; stop at
/// `keepRecentTokens`, snap to the closest valid cut point (falling back to
/// the latest one when trailing tool results alone exceed the budget,
/// #9740), then absorb adjacent metadata entries.
pub fn find_cut_point(
    entries: &[SessionEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPointResult {
    let cut_points = find_valid_cut_points(entries, start_index, end_index);

    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    }

    let mut accumulated_tokens: u64 = 0;
    let mut cut_index = cut_points[0]; // Default: keep from first message

    for i in (start_index..end_index).rev() {
        let message_tokens: u64 = session_entry_to_context_messages(&entries[i])
            .iter()
            .map(estimate_tokens)
            .sum();
        if message_tokens == 0 {
            continue;
        }
        accumulated_tokens += message_tokens;

        if accumulated_tokens >= keep_recent_tokens {
            // Prefer the closest valid cut point at or after this entry. If
            // trailing tool results exceed the budget by themselves, keep
            // their preceding assistant tool call instead of falling back to
            // the first message (#9740, compaction.ts:445-451 @ 8bdcd4498).
            let fallback = cut_points[cut_points.len() - 1]; // non-empty per early return
            cut_index = cut_points
                .iter()
                .copied()
                .find(|&candidate| candidate >= i)
                .unwrap_or(fallback);
            break;
        }
    }

    // Scan backwards from cutIndex to include adjacent metadata entries that
    // do not affect context. Stop at compaction boundaries or
    // context-visible entries.
    while cut_index > start_index {
        let prev_entry = &entries[cut_index - 1];
        if matches!(prev_entry, SessionEntry::Compaction(_))
            || !session_entry_to_context_messages(prev_entry).is_empty()
        {
            break;
        }
        cut_index -= 1;
    }

    let starts_turn = is_turn_start_entry(&entries[cut_index]);
    let turn_start_index = if starts_turn {
        None
    } else {
        find_turn_start_index(entries, cut_index, start_index)
    };

    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
    }
}

// ============================================================================
// Summarization
// ============================================================================

/// `SUMMARIZATION_PROMPT` — byte-exact (compaction.ts:467-498).
pub const SUMMARIZATION_PROMPT: &str = r#"The messages above are a conversation to summarize. Create a structured context checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or "(none)" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

/// `UPDATE_SUMMARIZATION_PROMPT` — byte-exact (compaction.ts:500-537).
pub const UPDATE_SUMMARIZATION_PROMPT: &str = r#"The messages above are NEW conversation messages to incorporate into the existing summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

/// `TURN_PREFIX_SUMMARIZATION_PROMPT` — byte-exact (compaction.ts:942-957
/// @ abe508e1b; #9908 `d192bd6dc`). Conversation content and instructions
/// are separated, and the wording is continuation-oriented so reasoning
/// models (Fable 5.1) do not refuse the split-turn summary.
pub const TURN_PREFIX_SUMMARIZATION_PROMPT: &str = r#"The messages above are earlier context from an ongoing conversation. Later messages are stored separately and do not need to be reconstructed.

Create a concise checkpoint of the user's request and the progress shown above. This checkpoint will be placed before the later messages so the conversation can continue with the necessary context.

## Original Request
[What did the user ask for?]

## Progress So Far
- [Key decisions and work completed in these messages]

## Context Needed to Continue
- [Information from these messages needed to understand the later work]

Only summarize information explicitly present above. Do not infer or recreate later messages."#;

/// Split-turn merge format literal (compaction.ts:881).
pub const SPLIT_TURN_MERGE_SEPARATOR: &str = "\n\n---\n\n**Turn Context (split turn):**\n\n";

/// Placeholder for an empty history in a split turn (compaction.ts:846).
pub const NO_PRIOR_HISTORY: &str = "No prior history.";

/// Arguments that ride along every summarization request (the upstream
/// `SimpleStreamOptions` subset the call sites set).
#[derive(Debug, Clone, Default)]
pub struct SummarizationArgs {
    pub api_key: Option<String>,
    pub headers: Option<rpi_ai::types::ProviderHeaders>,
    pub env: Option<rpi_ai::types::ProviderEnv>,
    pub signal: Option<CancellationToken>,
    pub thinking_level: Option<ThinkingLevel>,
    pub retry: Option<RetryPolicy>,
}

/// `createSummarizationOptions` (compaction.ts:539-553): reasoning models get
/// the session thinking level (unless "off").
fn create_summarization_options(
    model: &Model,
    max_tokens: u64,
    args: &SummarizationArgs,
) -> StreamOptions {
    let reasoning = match &args.thinking_level {
        Some(level) if model.reasoning && *level != ThinkingLevel::Off => Some(*level),
        _ => None,
    };
    StreamOptions {
        max_tokens: Some(max_tokens.min(u32::MAX as u64) as u32),
        reasoning,
        request: rpi_ai::ProviderRequestOptions {
            signal: args.signal.clone(),
            api_key: args.api_key.clone(),
            headers: args.headers.clone(),
            env: args.env.clone(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Consume a `StreamFn` stream to its terminal assistant message
/// (upstream `stream.result()`). A stream that ends without a terminal
/// done/error event violates the `StreamFn` contract; synthesize the same
/// error message `agent_loop.rs` produces for that case.
async fn stream_final_message(
    mut stream: BoxStream<'_, StreamEvent>,
    model: &Model,
) -> AssistantMessage {
    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::Done { message, .. } => return message,
            StreamEvent::Error { error, .. } => return error,
            _ => {}
        }
    }
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        error_message: Some("Stream ended without a terminal done/error event".to_owned()),
        timestamp: now_millis(),
        deferred: None,
        end_turn: None,
        raw_stop_reason: None,
        duration_ms: None,
    }
}

/// Synthesize the aborted summarization result for a cancelled request
/// (#9340/#9777, upstream `throwIfAborted` during the auth wait): the lazy
/// stream's first poll resolves auth, so racing the consumption against the
/// signal here is the rpi shape of "cancellation during authentication is
/// not lost". Aborted messages carry no error message (retry.rs abort
/// normalization shape).
fn aborted_summary_message(model: &Model) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Aborted,
        error_message: None,
        timestamp: now_millis(),
        deferred: None,
        end_turn: None,
        raw_stop_reason: None,
        duration_ms: None,
    }
}

/// `completeSummarization` (compaction.ts:562-581): shared choke point for
/// every compaction/branch-summary summarization call. Summaries are
/// standalone requests: `cacheRetention: "none"` + a fresh uuidv7 routing
/// session id isolate routing and avoid cache writes.
pub async fn complete_summarization(
    model: &Model,
    context: &Context,
    options: &StreamOptions,
    stream_fn: &StreamFn,
    retry: Option<&RetryPolicy>,
    callbacks: Option<&RetryCallbacks>,
) -> AssistantMessage {
    let request_options = StreamOptions {
        cache_retention: Some(CacheRetention::None),
        // Upstream `options.sessionId ?? uuidv7()` (compaction.ts:592): reuse
        // caller-supplied routing when available (routing/cache affinity);
        // callers without a session id — including branch summaries — get a
        // fresh routing id. P2-11: the port previously overwrote the id
        // unconditionally.
        session_id: Some(options.session_id.clone().unwrap_or_else(uuidv7_now)),
        ..options.clone()
    };
    let produce = || async {
        // #9548: the stream function consumes the normalized transcript.
        let transcript = rpi_ai::utils::transcript::normalize_context(context);
        let stream = stream_fn(model.clone(), transcript, request_options.clone());
        match request_options.signal.clone() {
            Some(signal) => tokio::select! {
                // `biased` + stream first: a terminal event that already
                // arrived (adapter-aborted request) wins over a token that
                // fired in the same wakeup.
                biased;
                message = stream_final_message(stream, model) => message,
                () = signal.cancelled() => aborted_summary_message(model),
            },
            None => stream_final_message(stream, model).await,
        }
    };
    retry_assistant_call(produce, retry, request_options.signal.as_ref(), callbacks).await
}

/// `{ text, usage }` result of a summarization call.
#[derive(Debug, Clone)]
pub struct SummaryWithUsage {
    pub text: String,
    pub usage: Usage,
}

/// `getSummarizationFailure` (compaction.ts:545-554): returns an error
/// message when a summarization response cannot safely be persisted. A
/// length stop contains partial text and must not become a session
/// checkpoint (#7048).
pub fn get_summarization_failure(response: &AssistantMessage, label: &str) -> Option<String> {
    if response.stop_reason == StopReason::Error {
        return Some(format!(
            "{label} failed: {}",
            response.error_message.as_deref().unwrap_or("Unknown error")
        ));
    }
    if response.stop_reason == StopReason::Length {
        return Some(format!(
            "{label} failed: generation hit the token cap and the summary is incomplete"
        ));
    }
    None
}

/// `generateSummaryWithUsage` (compaction.ts:622-686). Budget:
/// `maxTokens = min(floor(0.8 * reserveTokens), model.maxTokens)`.
#[allow(clippy::too_many_arguments)]
pub async fn generate_summary_with_usage(
    current_messages: &[AgentMessage],
    model: &Model,
    reserve_tokens: u64,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    stream_fn: &StreamFn,
    args: &SummarizationArgs,
    callbacks: Option<&RetryCallbacks>,
) -> Result<SummaryWithUsage, AgentError> {
    let max_tokens = summary_max_tokens(reserve_tokens, 0.8, model);

    // Use update prompt if we have a previous summary, otherwise initial prompt.
    let mut base_prompt = match previous_summary {
        Some(_) => UPDATE_SUMMARIZATION_PROMPT.to_owned(),
        None => SUMMARIZATION_PROMPT.to_owned(),
    };
    if let Some(instructions) = custom_instructions {
        base_prompt = format!("{base_prompt}\n\nAdditional focus: {instructions}");
    }

    // Serialize conversation to text so the model doesn't try to continue it.
    // convertToLlm first (handles custom types like bashExecution).
    let llm_messages = convert_to_llm(current_messages);
    let conversation_text = serialize_conversation(&llm_messages);

    // Build the prompt with the conversation wrapped in tags.
    let mut prompt_text = format!("<conversation>\n{conversation_text}\n</conversation>\n\n");
    if let Some(previous_summary) = previous_summary {
        prompt_text =
            format!("{prompt_text}<previous-summary>\n{previous_summary}\n</previous-summary>\n\n");
    }
    prompt_text.push_str(&base_prompt);

    let summarization_messages = vec![rpi_ai::types::Message::User(UserMessage {
        role: UserRole::User,
        content: rpi_ai::types::UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
            text: prompt_text,
            text_signature: None,
        })]),
        timestamp: now_millis(),
    })];

    let completion_options = create_summarization_options(model, max_tokens, args);
    let context = Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
        messages: summarization_messages,
        tools: None,
    };

    let response = complete_summarization(
        model,
        &context,
        &completion_options,
        stream_fn,
        args.retry.as_ref(),
        callbacks,
    )
    .await;

    if let Some(failure) = get_summarization_failure(&response, "Summarization") {
        return Err(AgentError::Message(failure));
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContent::ToolCall(_)))
    {
        return Err(AgentError::Message(
            "Summarization attempted to call a tool".to_owned(),
        ));
    }

    Ok(SummaryWithUsage {
        text: content_text_assistant(&response.content, "\n"),
        usage: response.usage,
    })
}

/// `generateSummary` (compaction.ts:587-619) — text-only convenience wrapper
/// (programmatic summarization API, compaction.md).
#[allow(clippy::too_many_arguments)]
pub async fn generate_summary(
    current_messages: &[AgentMessage],
    model: &Model,
    reserve_tokens: u64,
    custom_instructions: Option<&str>,
    previous_summary: Option<&str>,
    stream_fn: &StreamFn,
    args: &SummarizationArgs,
    callbacks: Option<&RetryCallbacks>,
) -> Result<String, AgentError> {
    Ok(generate_summary_with_usage(
        current_messages,
        model,
        reserve_tokens,
        custom_instructions,
        previous_summary,
        stream_fn,
        args,
        callbacks,
    )
    .await?
    .text)
}

/// Shared maxTokens budget: `min(floor(factor * reserveTokens),
/// model.maxTokens > 0 ? model.maxTokens : +Infinity)` (compaction.ts:637-640
/// and :937-940).
fn summary_max_tokens(reserve_tokens: u64, factor: f64, model: &Model) -> u64 {
    let budget = (factor * reserve_tokens as f64).floor() as u64;
    if model.max_tokens > 0 {
        budget.min(model.max_tokens as u64)
    } else {
        budget
    }
}

// ============================================================================
// Compaction Preparation (for extensions)
// ============================================================================

/// `CompactionPreparation` (compaction.ts:692-708). `Serialize` (camelCase)
/// serves the `session_before_compact` extension event payload
/// (types.ts:588).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionPreparation {
    /// Id of first entry to keep.
    pub first_kept_entry_id: String,
    /// Messages that will be summarized and discarded.
    pub messages_to_summarize: Vec<AgentMessage>,
    /// Messages that become the turn prefix summary (when splitting).
    pub turn_prefix_messages: Vec<AgentMessage>,
    /// Whether this is a split turn (cut point in the middle of a turn).
    pub is_split_turn: bool,
    pub tokens_before: u64,
    /// Summary from the previous compaction, for iterative update.
    pub previous_summary: Option<String>,
    /// File operations extracted from `messages_to_summarize`.
    pub file_ops: FileOperations,
    pub settings: CompactionSettings,
}

/// `getMessagesFromProjectedEntryForCompaction` (compaction.ts:98-102 @
/// abe508e1b): compaction boundaries contribute nothing; system messages are
/// prompt state, not conversation.
fn get_messages_from_projected_entry_for_compaction(
    entry: &ProjectedSessionEntry,
) -> Vec<AgentMessage> {
    if matches!(entry.source_entry, SessionEntry::Compaction(_)) {
        return Vec::new();
    }
    entry
        .messages
        .iter()
        .filter(|message| !matches!(message, AgentMessage::System(_)))
        .cloned()
        .collect()
}

/// `isProjectedTurnStart` (compaction.ts:782-785 @ abe508e1b).
fn is_projected_turn_start(entry: &ProjectedSessionEntry) -> bool {
    if matches!(entry.source_entry, SessionEntry::Compaction(_)) {
        return false;
    }
    entry.messages.iter().any(is_turn_start_message)
}

/// `findProjectedTurnStartIndex` (compaction.ts:787-793 @ abe508e1b).
fn find_projected_turn_start_index(
    entries: &[ProjectedSessionEntry],
    entry_index: usize,
    start_index: usize,
) -> Option<usize> {
    (start_index..=entry_index)
        .rev()
        .find(|&i| is_projected_turn_start(&entries[i]))
}

/// `findProjectedCutPoint` (compaction.ts:795-860 @ abe508e1b): the same cut
/// logic as [`find_cut_point`] over projected entries, including the
/// recovery-omission suffix rule that keeps omitted attempts (and their
/// context edits) out of the retained history when they trail the last
/// visible input.
fn find_projected_cut_point(
    entries: &[ProjectedSessionEntry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: u64,
) -> CutPointResult {
    let cut_points: Vec<usize> = (start_index..end_index)
        .filter(|&i| {
            let entry = &entries[i];
            !matches!(entry.source_entry, SessionEntry::Compaction(_))
                && entry.messages.iter().any(is_cut_point_message)
        })
        .collect();
    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: None,
            is_split_turn: false,
        };
    }

    let mut accumulated_tokens: u64 = 0;
    let mut exceeded_budget = false;
    let mut cut_index = cut_points[0];
    for i in (start_index..end_index).rev() {
        let message_tokens: u64 = entries[i].messages.iter().map(estimate_tokens).sum();
        if message_tokens == 0 {
            continue;
        }
        accumulated_tokens += message_tokens;
        if accumulated_tokens >= keep_recent_tokens {
            exceeded_budget = true;
            let fallback = *cut_points.last().unwrap_or(&cut_points[0]);
            cut_index = cut_points
                .iter()
                .copied()
                .find(|&candidate| candidate >= i)
                .unwrap_or(fallback);
            break;
        }
    }

    // A recovery attempt and its omission edits are context-invisible after
    // the last visible input. Advance only for a closed suffix containing an
    // omitted assistant attempt; arbitrary metadata must not move the cut
    // past unsent input (compaction.ts:825-849 @ abe508e1b).
    let suffix = &entries[cut_index + 1..end_index];
    let is_intrinsically_visible = |entry: &ProjectedSessionEntry| {
        !matches!(entry.source_entry, SessionEntry::ContextEdit(_))
            && !session_entry_to_context_messages(&entry.source_entry).is_empty()
    };
    let is_omitted = |entry: &ProjectedSessionEntry| {
        is_intrinsically_visible(entry) && entry.messages.is_empty()
    };
    let omitted_suffix_ids: std::collections::HashSet<&str> = suffix
        .iter()
        .filter(|entry| is_omitted(entry))
        .map(|entry| entry.source_entry.id())
        .collect();
    let has_external_replacement = suffix.iter().any(|entry| {
        matches!(
            &entry.source_entry,
            SessionEntry::ContextEdit(edit)
                if edit.replacement.is_some()
                    && !omitted_suffix_ids.contains(edit.target_id.as_str())
        )
    });
    let is_recovery_omission_suffix =
        exceeded_budget && !has_external_replacement && suffix.iter().any(|entry| {
            matches!(
                &entry.source_entry,
                SessionEntry::Message(message)
                    if matches!(message.message, AgentMessage::Assistant(_)) && is_omitted(entry)
            )
        }) && suffix.iter().all(|entry| {
            !matches!(entry.source_entry, SessionEntry::Compaction(_))
                && (!is_intrinsically_visible(entry) || is_omitted(entry))
        });
    if is_recovery_omission_suffix {
        cut_index += 1;
    }

    while cut_index > start_index {
        let previous = &entries[cut_index - 1];
        if matches!(previous.source_entry, SessionEntry::Compaction(_))
            || !previous.messages.is_empty()
        {
            break;
        }
        cut_index -= 1;
    }

    let starts_turn = is_projected_turn_start(&entries[cut_index]);
    let turn_start_index = if starts_turn {
        None
    } else {
        find_projected_turn_start_index(entries, cut_index, start_index)
    };
    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !starts_turn && turn_start_index.is_some(),
    }
}

/// `prepareCompaction` (compaction.ts:862-929 @ abe508e1b): preparation runs
/// on the canonical projection, so omitted entries never enter the summary
/// and replacements are summarized as edited.
pub fn prepare_compaction(
    path_entries: &[SessionEntry],
    settings: &CompactionSettings,
) -> Option<CompactionPreparation> {
    if matches!(path_entries.last(), Some(SessionEntry::Compaction(_))) {
        return None;
    }

    let projection = build_session_projection(path_entries);
    let projected_entries = &projection.entries;
    let source_entries: Vec<SessionEntry> = projected_entries
        .iter()
        .map(|entry| entry.source_entry.clone())
        .collect();
    // The newest compaction is projected first. Older compaction entries can
    // still occur in its retained raw range, but their projected contribution
    // is empty (compaction.ts:872-878 @ abe508e1b).
    let prev_compaction_index = projected_entries.iter().position(|entry| {
        matches!(entry.source_entry, SessionEntry::Compaction(_)) && !entry.messages.is_empty()
    });

    let mut previous_summary: Option<String> = None;
    let mut boundary_start = 0usize;
    if let Some(index) = prev_compaction_index {
        if let SessionEntry::Compaction(prev) = &projected_entries[index].source_entry {
            previous_summary = Some(prev.summary.clone());
        }
        // The canonical projection has already selected the previous
        // compaction's retained tail.
        boundary_start = index + 1;
    }
    let boundary_end = projected_entries.len();
    let tokens_before = estimate_projected_context_tokens(&projection, path_entries).tokens;
    let cut_point = find_projected_cut_point(
        projected_entries,
        boundary_start,
        boundary_end,
        settings.keep_recent_tokens,
    );

    let first_kept_entry_id = projected_entries
        .get(cut_point.first_kept_entry_index)?
        .source_entry
        .id()
        .to_owned();
    let history_end = if cut_point.is_split_turn {
        cut_point
            .turn_start_index
            .unwrap_or(cut_point.first_kept_entry_index)
    } else {
        cut_point.first_kept_entry_index
    };

    let messages_to_summarize: Vec<AgentMessage> = projected_entries[boundary_start..history_end]
        .iter()
        .flat_map(get_messages_from_projected_entry_for_compaction)
        .collect();
    let turn_prefix_messages: Vec<AgentMessage> = if cut_point.is_split_turn {
        match cut_point.turn_start_index {
            Some(turn_start) => projected_entries[turn_start..cut_point.first_kept_entry_index]
                .iter()
                .flat_map(get_messages_from_projected_entry_for_compaction)
                .collect(),
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };

    if messages_to_summarize.is_empty() && turn_prefix_messages.is_empty() {
        return None;
    }

    let mut file_ops = extract_file_operations(
        &messages_to_summarize,
        &source_entries,
        prev_compaction_index,
    );

    // Also extract file ops from the turn prefix when splitting.
    if cut_point.is_split_turn {
        for msg in &turn_prefix_messages {
            extract_file_ops_from_message(msg, &mut file_ops);
        }
    }

    Some(CompactionPreparation {
        first_kept_entry_id,
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary,
        file_ops,
        settings: *settings,
    })
}

/// `estimateProjectedContextTokens` (compaction.ts:227-270 @ abe508e1b):
/// estimate projected context without trusting usage captured before a later
/// edit or compaction. Falls back to a pure message-size estimate when the
/// usage anchor is invalidated.
pub fn estimate_projected_context_tokens(
    projection: &SessionProjection,
    branch_entries: &[SessionEntry],
) -> ContextUsageEstimate {
    let estimate = estimate_context_tokens(&projection.messages);
    if let Some(last_usage_index) = estimate.last_usage_index {
        let mut projected_message_index = 0usize;
        let mut usage_entry_id: Option<&str> = None;
        for entry in &projection.entries {
            let next_message_index = projected_message_index + entry.messages.len();
            if last_usage_index < next_message_index {
                usage_entry_id = Some(entry.source_entry.id());
                break;
            }
            projected_message_index = next_message_index;
        }

        let usage_entry_index =
            usage_entry_id.and_then(|id| branch_entries.iter().position(|entry| entry.id() == id));
        let latest_invalidating_index = branch_entries.iter().rposition(|entry| {
            matches!(
                entry,
                SessionEntry::ContextEdit(_) | SessionEntry::Compaction(_)
            )
        });
        // JS compares `usageEntryIndex > latestInvalidatingEntryIndex` with -1
        // sentinels: a missing usage entry (-1) never beats a missing
        // invalidating entry (-1), so the fallback still applies.
        let usage_trusted = match latest_invalidating_index {
            None => usage_entry_index.is_some(),
            Some(invalidating) => usage_entry_index
                .map(|index| index > invalidating)
                .unwrap_or(false),
        };
        if usage_trusted {
            return estimate;
        }
    }

    let current_system = {
        let llm_messages: Vec<rpi_ai::types::Message> = projection
            .messages
            .iter()
            .filter_map(|message| match message {
                AgentMessage::System(system) => {
                    Some(rpi_ai::types::Message::System(system.clone()))
                }
                _ => None,
            })
            .collect();
        rpi_ai::utils::transcript::get_current_system_message(&llm_messages)
    };
    let mut tokens = current_system
        .map(|system| estimate_tokens(&AgentMessage::System(system)))
        .unwrap_or(0);
    for message in &projection.messages {
        if !matches!(message, AgentMessage::System(_)) {
            tokens += estimate_tokens(message);
        }
    }
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

// ============================================================================
// Main compaction function
// ============================================================================

/// `compact` (compaction.ts:817-919): generate summaries for compaction using
/// prepared data; split turns merge history + turn prefix summaries.
#[allow(clippy::too_many_arguments)]
pub async fn compact(
    preparation: &CompactionPreparation,
    model: &Model,
    custom_instructions: Option<&str>,
    stream_fn: &StreamFn,
    args: &SummarizationArgs,
    callbacks: Option<&RetryCallbacks>,
) -> Result<CompactionResult, AgentError> {
    let settings = &preparation.settings;

    let summary: String;
    let summary_usage: Usage;

    if preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty() {
        let mut history_text = NO_PRIOR_HISTORY.to_owned();
        let mut history_usage: Option<Usage> = None;
        if !preparation.messages_to_summarize.is_empty() {
            let history_result = generate_summary_with_usage(
                &preparation.messages_to_summarize,
                model,
                settings.reserve_tokens,
                custom_instructions,
                preparation.previous_summary.as_deref(),
                stream_fn,
                args,
                callbacks,
            )
            .await?;
            history_text = history_result.text;
            history_usage = Some(history_result.usage);
        }
        let turn_prefix_result = generate_turn_prefix_summary(
            &preparation.turn_prefix_messages,
            model,
            settings.reserve_tokens,
            stream_fn,
            args,
            callbacks,
        )
        .await?;
        // Merge into a single summary (compaction.ts:881).
        summary = format!(
            "{history_text}{SPLIT_TURN_MERGE_SEPARATOR}{}",
            turn_prefix_result.text
        );
        summary_usage = match &history_usage {
            Some(history) => combine_usage(history, &turn_prefix_result.usage),
            None => turn_prefix_result.usage,
        };
    } else {
        let result = generate_summary_with_usage(
            &preparation.messages_to_summarize,
            model,
            settings.reserve_tokens,
            custom_instructions,
            preparation.previous_summary.as_deref(),
            stream_fn,
            args,
            callbacks,
        )
        .await?;
        summary = result.text;
        summary_usage = result.usage;
    }

    // Compute file lists and append to the summary (compaction.ts:905-906).
    let lists = compute_file_lists(&preparation.file_ops);
    let summary = format!(
        "{summary}{}",
        format_file_operations(&lists.read_files, &lists.modified_files)
    );

    let details = CompactionDetails {
        read_files: lists.read_files,
        modified_files: lists.modified_files,
    };

    Ok(CompactionResult {
        summary,
        first_kept_entry_id: preparation.first_kept_entry_id.clone(),
        tokens_before: preparation.tokens_before,
        estimated_tokens_after: None,
        usage: Some(summary_usage),
        details: Some(serde_json::to_value(&details)?),
    })
}

/// `generateTurnPrefixSummary` (compaction.ts:924-969). Budget:
/// `maxTokens = min(floor(0.5 * reserveTokens), model.maxTokens)`.
async fn generate_turn_prefix_summary(
    messages: &[AgentMessage],
    model: &Model,
    reserve_tokens: u64,
    stream_fn: &StreamFn,
    args: &SummarizationArgs,
    callbacks: Option<&RetryCallbacks>,
) -> Result<SummaryWithUsage, AgentError> {
    let max_tokens = summary_max_tokens(reserve_tokens, 0.5, model);
    let llm_messages = convert_to_llm(messages);
    let conversation_text = serialize_conversation(&llm_messages);
    let prompt_text = format!(
        "# Conversation\n{conversation_text}\n\n# Instructions\n{TURN_PREFIX_SUMMARIZATION_PROMPT}"
    );
    let summarization_messages = vec![rpi_ai::types::Message::User(UserMessage {
        role: UserRole::User,
        content: rpi_ai::types::UserContent::Blocks(vec![UserContentBlock::Text(TextContent {
            text: prompt_text,
            text_signature: None,
        })]),
        timestamp: now_millis(),
    })];

    let context = Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
        messages: summarization_messages,
        tools: None,
    };
    let response = complete_summarization(
        model,
        &context,
        &create_summarization_options(model, max_tokens, args),
        stream_fn,
        args.retry.as_ref(),
        callbacks,
    )
    .await;

    if let Some(failure) = get_summarization_failure(&response, "Turn prefix summarization") {
        return Err(AgentError::Message(failure));
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContent::ToolCall(_)))
    {
        return Err(AgentError::Message(
            "Turn prefix summarization attempted to call a tool".to_owned(),
        ));
    }

    Ok(SummaryWithUsage {
        text: content_text_assistant(&response.content, "\n"),
        usage: response.usage,
    })
}
