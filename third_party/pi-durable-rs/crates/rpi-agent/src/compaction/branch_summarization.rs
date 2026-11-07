//! Port of `packages/coding-agent/src/core/compaction/branch-summarization.ts`
//! @ pi 0.82.1 (2efa728).
//!
//! Branch summarization for tree navigation: when navigating to a different
//! point in the session tree, generate a summary of the branch being left so
//! context isn't lost.
//!
//! Intentional difference: upstream reads entries through the session
//! manager (`getBranch` / `getEntry`). Here `collect_entries_for_branch_summary`
//! takes the two root-first branch paths as slices — the same data the
//! session-manager walk produces, without the I/O (the caller, e.g.
//! `rpi::core`, owns the session store). Semantics are identical: the
//! parent-id walk from the old leaf visits exactly the `old_path` suffix
//! after the common ancestor.

use rpi_ai::types::{AssistantContent, Model, StopReason, StreamOptions, Usage};
use rpi_ai::utils::retry::RetryCallbacks;
use rpi_ai::utils::text::content_text_assistant;

use super::utils::{
    FileOperations, SUMMARIZATION_SYSTEM_PROMPT, compute_file_lists, create_file_ops,
    extract_file_ops_from_message, format_file_operations, serialize_conversation,
};
use super::{
    SummarizationArgs, complete_summarization, estimate_tokens, get_summarization_failure,
};
use crate::messages::{AgentMessage, convert_to_llm};
use crate::session::{
    SessionEntry, create_branch_summary_message, create_compaction_summary_message,
    create_custom_message,
};
use crate::stream_fn::StreamFn;

// ============================================================================
// Types
// ============================================================================

/// `BranchSummaryResult` (branch-summarization.ts:34-41).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BranchSummaryResult {
    pub summary: Option<String>,
    pub usage: Option<Usage>,
    pub read_files: Option<Vec<String>>,
    pub modified_files: Option<Vec<String>>,
    pub aborted: Option<bool>,
    pub error: Option<String>,
}

/// Details stored in `BranchSummaryEntry.details` for file tracking
/// (`BranchSummaryDetails`, branch-summarization.ts:44-47; camelCase on the
/// wire).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}

/// `BranchPreparation` (branch-summarization.ts:51-58).
#[derive(Debug, Clone, Default)]
pub struct BranchPreparation {
    /// Messages extracted for summarization, in chronological order.
    pub messages: Vec<AgentMessage>,
    /// File operations extracted from tool calls.
    pub file_ops: FileOperations,
    /// Total estimated tokens in messages.
    pub total_tokens: u64,
}

/// `CollectEntriesResult` (branch-summarization.ts:60-65).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CollectEntriesResult {
    /// Entries to summarize, in chronological order.
    pub entries: Vec<SessionEntry>,
    /// Common ancestor between old and new position, if any.
    pub common_ancestor_id: Option<String>,
}

/// `BranchSummarySettings` (settings-manager.ts:17-20 /
/// `getBranchSummarySettings` :789-794). All-optional upstream; the Rust form
/// holds the resolved values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchSummarySettings {
    /// Tokens reserved for prompt + LLM response (settings-manager.ts:20).
    pub reserve_tokens: u64,
    /// When true, skips the "Summarize branch?" prompt and defaults to no
    /// summary (consumed by the navigation caller, not by this module).
    pub skip_prompt: bool,
}

impl Default for BranchSummarySettings {
    fn default() -> Self {
        Self {
            reserve_tokens: DEFAULT_BRANCH_RESERVE_TOKENS,
            skip_prompt: false,
        }
    }
}

/// `GenerateBranchSummaryOptions` (branch-summarization.ts:67-90). `args`
/// carries api key / headers / env / signal / retry; `thinking_level` is
/// intentionally unused — the upstream branch request options set only
/// `maxTokens: min(4096, model.maxTokens)` (branch-summarization.ts:345, cap
/// introduced by e44d75c20 / #8845), never `reasoning`. (P2-14: this
/// comment previously said 2048 — the pre-#8845 value.)
pub struct GenerateBranchSummaryOptions<'a> {
    pub model: &'a Model,
    pub stream_fn: &'a StreamFn,
    pub args: &'a SummarizationArgs,
    pub custom_instructions: Option<&'a str>,
    /// If true, `custom_instructions` replaces the default prompt instead of
    /// being appended.
    pub replace_instructions: bool,
    /// Tokens reserved when selecting branch history (default 16384)
    /// (branch-summarization.ts:82 @ e44d75c20).
    pub reserve_tokens: u64,
    pub callbacks: Option<&'a RetryCallbacks>,
}

/// Default `reserveTokens` for branch summaries
/// (branch-summarization.ts:305, settings-manager.ts:791).
pub const DEFAULT_BRANCH_RESERVE_TOKENS: u64 = 16384;

// ============================================================================
// Entry Collection
// ============================================================================

/// Collect entries to summarize when navigating from one position to another
/// (`collectEntriesForBranchSummary`, branch-summarization.ts:108-146).
///
/// `old_path` / `target_path` are the root-first branches (as `getBranch`
/// returns) ending at the old leaf and the target entry. Does NOT stop at
/// compaction boundaries — those are included and their summaries become
/// context.
pub fn collect_entries_for_branch_summary(
    old_path: &[SessionEntry],
    old_leaf_id: Option<&str>,
    target_path: &[SessionEntry],
) -> CollectEntriesResult {
    // If no old position, nothing to summarize.
    let Some(old_leaf_id) = old_leaf_id else {
        return CollectEntriesResult::default();
    };

    // Find the common ancestor: `target_path` is root-first, so iterate
    // backwards to find the deepest node also on the old path.
    let common_ancestor_id = target_path
        .iter()
        .rev()
        .map(|entry| entry.id())
        .find(|id| old_path.iter().any(|e| e.id() == *id))
        .map(str::to_owned);

    // Entries from the old leaf back to (excluding) the common ancestor.
    // Upstream walks parent ids via `getEntry`; on a consistent tree that
    // chain is exactly the `old_path` suffix after the ancestor.
    let start = match &common_ancestor_id {
        Some(ancestor) => old_path
            .iter()
            .position(|entry| entry.id() == ancestor)
            .map(|idx| idx + 1)
            .unwrap_or(0),
        None => 0,
    };
    let mut entries: Vec<SessionEntry> = old_path[start..].to_vec();

    // Defensive: if the caller's `old_leaf_id` disagrees with the path tail,
    // upstream's walk would start at that leaf instead; trim the suffix so
    // the last collected entry is the requested leaf.
    while let Some(last) = entries.last() {
        if last.id() == old_leaf_id {
            break;
        }
        entries.pop();
    }

    CollectEntriesResult {
        entries,
        common_ancestor_id,
    }
}

// ============================================================================
// Entry to Message Conversion
// ============================================================================

/// Extract `AgentMessage` from a session entry (`getMessageFromEntry`,
/// branch-summarization.ts:156-180). Similar to the compaction variant but
/// also handles `compaction` entries, and skips tool results (context is in
/// the assistant's tool call).
fn get_message_from_entry(entry: &SessionEntry) -> Option<AgentMessage> {
    match entry {
        SessionEntry::Message(m) => {
            if matches!(m.message, AgentMessage::ToolResult(_)) {
                return None;
            }
            Some(m.message.clone())
        }
        SessionEntry::CustomMessage(c) => Some(AgentMessage::Custom(create_custom_message(
            &c.custom_type,
            c.content.clone(),
            c.display,
            c.details.clone(),
            &c.timestamp,
        ))),
        SessionEntry::BranchSummary(b) => Some(AgentMessage::BranchSummary(
            create_branch_summary_message(&b.summary, &b.from_id, &b.timestamp),
        )),
        SessionEntry::Compaction(c) => Some(AgentMessage::CompactionSummary(
            create_compaction_summary_message(&c.summary, c.tokens_before, &c.timestamp),
        )),
        // `context_edit` is applied when the projection runs, not when
        // entries are converted for summarization.
        SessionEntry::ThinkingLevelChange(_)
        | SessionEntry::ModelChange(_)
        | SessionEntry::ActiveToolsChange(_)
        | SessionEntry::Usage(_)
        | SessionEntry::Custom(_)
        | SessionEntry::ContextEdit(_)
        | SessionEntry::Label(_)
        | SessionEntry::SessionInfo(_)
        | SessionEntry::Leaf(_) => None,
    }
}

/// Prepare entries for summarization with a token budget
/// (`prepareBranchEntries`, branch-summarization.ts:195-247).
///
/// Walks entries from NEWEST to OLDEST, adding messages until the budget is
/// hit, so the most recent context survives when the branch is too long.
/// `token_budget == 0` means no limit.
pub fn prepare_branch_entries(entries: &[SessionEntry], token_budget: u64) -> BranchPreparation {
    let mut messages: Vec<AgentMessage> = Vec::new();
    let mut file_ops = create_file_ops();
    let mut total_tokens: u64 = 0;

    // First pass: collect file ops from ALL entries (even if they don't fit
    // the token budget) — cumulative file tracking from nested branch
    // summaries. Only pi-generated summaries (`fromHook != true`).
    for entry in entries {
        if let SessionEntry::BranchSummary(b) = entry {
            if b.from_hook == Some(true) {
                continue;
            }
            if let Some(details) = &b.details
                && let Ok(details) = serde_json::from_value::<BranchSummaryDetails>(details.clone())
            {
                for f in details.read_files {
                    file_ops.read.insert(f);
                }
                // Modified files go into edited (upstream comment says
                // "both edited and written" but the code adds to `edited`
                // only — the code wins, branch-summarization.ts:209-214).
                for f in details.modified_files {
                    file_ops.edited.insert(f);
                }
            }
        }
    }

    // Second pass: walk from newest to oldest, adding messages until the
    // token budget.
    for entry in entries.iter().rev() {
        let Some(message) = get_message_from_entry(entry) else {
            continue;
        };

        // Extract file ops from assistant messages (tool calls).
        extract_file_ops_from_message(&message, &mut file_ops);

        let tokens = estimate_tokens(&message);

        // Check budget before adding.
        if token_budget > 0 && total_tokens + tokens > token_budget {
            // If this is a summary entry, try to fit it anyway — important
            // context.
            if matches!(
                entry,
                SessionEntry::Compaction(_) | SessionEntry::BranchSummary(_)
            ) && (total_tokens as f64) < token_budget as f64 * 0.9
            {
                messages.insert(0, message);
                total_tokens += tokens;
            }
            // Stop — we've hit the budget.
            break;
        }

        messages.insert(0, message);
        total_tokens += tokens;
    }

    BranchPreparation {
        messages,
        file_ops,
        total_tokens,
    }
}

// ============================================================================
// Summary Generation
// ============================================================================

/// `BRANCH_SUMMARY_PREAMBLE` (branch-summarization.ts:253-256). Byte-exact;
/// includes the trailing blank line.
pub const BRANCH_SUMMARY_PREAMBLE: &str = "The user explored a different conversation branch before returning here.\nSummary of that exploration:\n\n";

/// `BRANCH_SUMMARY_PROMPT` (branch-summarization.ts:258-285). Byte-exact; the
/// golden prompt fixtures compare against this string.
pub const BRANCH_SUMMARY_PROMPT: &str = r#"Create a structured summary of this conversation branch for context when returning later.

Use this EXACT format:

## Goal
[What was the user trying to accomplish in this branch?]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned]
- [Or "(none)" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Work that was started but not finished]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [What should happen next to continue this work]

Keep each section concise. Preserve exact file paths, function names, and error messages."#;

/// Generate a summary of abandoned branch entries (`generateBranchSummary`,
/// branch-summarization.ts:293-376).
pub async fn generate_branch_summary(
    entries: &[SessionEntry],
    options: &GenerateBranchSummaryOptions<'_>,
) -> BranchSummaryResult {
    let model = options.model;
    let args = options.args;

    // Token budget = context window minus reserved space for prompt +
    // response.
    let context_window = if model.context_window > 0 {
        model.context_window as u64
    } else {
        128_000
    };
    let token_budget = context_window.saturating_sub(options.reserve_tokens);

    let BranchPreparation {
        messages, file_ops, ..
    } = prepare_branch_entries(entries, token_budget);

    if messages.is_empty() {
        return BranchSummaryResult {
            summary: Some("No content to summarize".to_owned()),
            ..Default::default()
        };
    }

    // Transform to LLM-compatible messages, then serialize to text —
    // serialization prevents the model from treating it as a conversation to
    // continue.
    let llm_messages = convert_to_llm(&messages);
    let conversation_text = serialize_conversation(&llm_messages);

    // Build prompt.
    let instructions = match (options.replace_instructions, options.custom_instructions) {
        (true, Some(custom)) => custom.to_owned(),
        (_, Some(custom)) => format!("{BRANCH_SUMMARY_PROMPT}\n\nAdditional focus: {custom}"),
        (_, None) => BRANCH_SUMMARY_PROMPT.to_owned(),
    };
    let prompt_text =
        format!("<conversation>\n{conversation_text}\n</conversation>\n\n{instructions}");

    let summarization_messages = vec![rpi_ai::types::Message::User(rpi_ai::types::UserMessage {
        role: rpi_ai::types::UserRole::User,
        content: rpi_ai::types::UserContent::Blocks(vec![rpi_ai::types::UserContentBlock::Text(
            rpi_ai::types::TextContent {
                text: prompt_text,
                text_signature: None,
            },
        )]),
        timestamp: crate::agent_loop::now_millis(),
    })];

    // Call the LLM via the session stream function; transient stream drops
    // are retried through `completeSummarization` (branch-summarization.ts:345-351).
    let context = rpi_ai::types::Context {
        system_prompt: Some(SUMMARIZATION_SYSTEM_PROMPT.to_owned()),
        messages: summarization_messages,
        tools: None,
    };
    let request_options = StreamOptions {
        // Output cap (e44d75c20, #8845): min(4096, model.maxTokens) with
        // maxTokens == 0 meaning no limit (upstream Infinity branch).
        max_tokens: Some(if model.max_tokens > 0 {
            4096u64.min(u64::from(model.max_tokens)) as u32
        } else {
            4096
        }),
        request: rpi_ai::ProviderRequestOptions {
            signal: args.signal.clone(),
            api_key: args.api_key.clone(),
            headers: args.headers.clone(),
            env: args.env.clone(),
            ..Default::default()
        },
        ..Default::default()
    };
    let response = complete_summarization(
        model,
        &context,
        &request_options,
        options.stream_fn,
        args.retry.as_ref(),
        options.callbacks,
    )
    .await;

    // Check if aborted or errored.
    if response.stop_reason == StopReason::Aborted {
        return BranchSummaryResult {
            aborted: Some(true),
            ..Default::default()
        };
    }
    if let Some(failure) = get_summarization_failure(&response, "Branch summarization") {
        return BranchSummaryResult {
            error: Some(failure),
            ..Default::default()
        };
    }
    if response
        .content
        .iter()
        .any(|block| matches!(block, AssistantContent::ToolCall(_)))
    {
        return BranchSummaryResult {
            error: Some("Branch summarization attempted to call a tool".to_owned()),
            ..Default::default()
        };
    }

    let mut summary = content_text_assistant(&response.content, "\n");

    // Prepend preamble to provide context about the branch summary.
    summary = format!("{BRANCH_SUMMARY_PREAMBLE}{summary}");

    // Compute file lists and append to the summary.
    let lists = compute_file_lists(&file_ops);
    summary.push_str(&format_file_operations(
        &lists.read_files,
        &lists.modified_files,
    ));

    BranchSummaryResult {
        summary: Some(if summary.is_empty() {
            "No summary generated".to_owned()
        } else {
            summary
        }),
        usage: Some(response.usage),
        read_files: Some(lists.read_files),
        modified_files: Some(lists.modified_files),
        ..Default::default()
    }
}
