//! Port of `packages/ai/src/utils/estimate.ts` @ `abe508e1b` (v1.1.0,
//! `27075fe07`).
//!
//! chars/3.5 heuristic token estimation with usage-anchor trailing
//! estimation. Do not "improve" — the constants are pinned behavior
//! (ADR-0002 §4).
//!
//! Intentional differences: JS `String.length` counts UTF-16 code units;
//! `chars` here counts `char`s. For BMP text they are identical; for astral
//! characters the JS estimate is up to 2x higher per character. This matches
//! the D-003 faux-provider decision (chars/4, BMP-equivalent).

use crate::types::{
    AssistantContent, ImageContent, Message, TextContent, Tool, ToolResultContent,
    TranscriptContext, Usage, UserContent, UserContentBlock,
};

/// #10497 (27075fe07): 3.5 characters per token, leaving more room when
/// calculating output limits. `ceil(chars * 2 / 7)`, exact integer
/// arithmetic.
const CHARS_NUMERATOR: u64 = 2;
const CHARS_DENOMINATOR: u64 = 7;
const ESTIMATED_IMAGE_CHARS: usize = 4800;

/// `Math.ceil(chars / 3.5)` for a non-negative integer count.
fn chars_to_tokens(chars: usize) -> u64 {
    (chars as u64 * CHARS_NUMERATOR).div_ceil(CHARS_DENOMINATOR)
}

/// `calculateContextTokens`: `totalTokens` when set, else the component sum.
pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage.input + usage.output + usage.cache_read + usage.cache_write
    }
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

fn safe_json_stringify<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[unserializable]".to_owned())
}

/// Estimated total context tokens plus the usage-anchor breakdown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    /// Estimated total context tokens.
    pub tokens: u64,
    /// Tokens reported by the most recent applicable assistant usage block.
    pub usage_tokens: u64,
    /// Estimated tokens after the most recent applicable assistant usage block.
    pub trailing_tokens: u64,
    /// Index of the applicable message that provided usage, or `None`.
    pub last_usage_index: Option<usize>,
}

fn estimate_text_and_image_chars_text(text: &str) -> usize {
    char_len(text)
}

fn estimate_block_chars(text: &TextContent) -> usize {
    char_len(&text.text)
}

fn image_chars(_image: &ImageContent) -> usize {
    ESTIMATED_IMAGE_CHARS
}

pub fn estimate_text_tokens(text: &str) -> u64 {
    chars_to_tokens(char_len(text))
}

pub fn estimate_message_tokens(message: &Message) -> u64 {
    // System messages count their rendered text plus both tool lists
    // (#9548, estimate.ts:49-56).
    if let Message::System(system) = message {
        let mut tokens = estimate_text_tokens(&crate::utils::text::get_system_message_text(system));
        tokens += estimate_tools_tokens(system.tools_added.as_deref());
        tokens += estimate_tool_reference_tokens(system.tools_removed.as_deref());
        return tokens;
    }
    let chars: usize = match message {
        // Handled by the early return above.
        Message::System(_) => 0,
        Message::User(user) => match &user.content {
            UserContent::Text(text) => estimate_text_and_image_chars_text(text),
            UserContent::Blocks(blocks) => blocks
                .iter()
                .map(|block| match block {
                    UserContentBlock::Text(text) => estimate_block_chars(text),
                    UserContentBlock::Image(image) => image_chars(image),
                })
                .sum(),
        },
        Message::ToolResult(result) => result
            .content
            .iter()
            .map(|block| match block {
                ToolResultContent::Text(text) => estimate_block_chars(text),
                ToolResultContent::Image(image) => image_chars(image),
            })
            .sum(),
        Message::Assistant(assistant) => assistant
            .content
            .iter()
            .map(|block| match block {
                AssistantContent::Text(text) => char_len(&text.text),
                AssistantContent::Thinking(thinking) => char_len(&thinking.thinking),
                AssistantContent::ToolCall(call) => {
                    char_len(&call.name) + char_len(&safe_json_stringify(&call.arguments))
                }
            })
            .sum(),
    };
    chars_to_tokens(chars)
}

fn last_assistant_usage_info(messages: &[Message]) -> Option<(&Usage, usize)> {
    let mut latest_prefix_timestamp = i64::MIN;
    let mut usage_info: Option<(&Usage, usize)> = None;

    for (i, message) in messages.iter().enumerate() {
        if let Message::Assistant(assistant) = message {
            // A newer prefix message was inserted after this response (for
            // example, a compaction summary), so its usage cannot describe
            // the current prefix.
            let usage_applies_to_prefix = assistant.timestamp >= latest_prefix_timestamp;
            if usage_applies_to_prefix
                && assistant.stop_reason != crate::types::StopReason::Aborted
                && assistant.stop_reason != crate::types::StopReason::Error
                && calculate_context_tokens(&assistant.usage) > 0
            {
                usage_info = Some((&assistant.usage, i));
            }
        }
        latest_prefix_timestamp = latest_prefix_timestamp.max(message_timestamp(message));
    }

    usage_info
}

fn message_timestamp(message: &Message) -> i64 {
    match message {
        Message::System(m) => m.timestamp,
        Message::User(m) => m.timestamp,
        Message::Assistant(m) => m.timestamp,
        Message::ToolResult(m) => m.timestamp,
    }
}

fn estimate_messages(messages: &[Message]) -> ContextUsageEstimate {
    if let Some((usage, index)) = last_assistant_usage_info(messages) {
        let usage_tokens = calculate_context_tokens(usage);
        let trailing_tokens: u64 = messages[index + 1..]
            .iter()
            .map(estimate_message_tokens)
            .sum();
        return ContextUsageEstimate {
            tokens: usage_tokens + trailing_tokens,
            usage_tokens,
            trailing_tokens,
            last_usage_index: Some(index),
        };
    }

    let tokens: u64 = messages.iter().map(estimate_message_tokens).sum();
    ContextUsageEstimate {
        tokens,
        usage_tokens: 0,
        trailing_tokens: tokens,
        last_usage_index: None,
    }
}

fn estimate_tools_tokens(tools: Option<&[Tool]>) -> u64 {
    match tools {
        Some(tools) if !tools.is_empty() => estimate_text_tokens(&safe_json_stringify(&tools)),
        _ => 0,
    }
}

fn estimate_tool_reference_tokens(tools: Option<&[crate::types::ToolReference]>) -> u64 {
    match tools {
        Some(tools) if !tools.is_empty() => estimate_text_tokens(&safe_json_stringify(&tools)),
        _ => 0,
    }
}

/// `estimateContextTokens` for a bare message slice (upstream array overload).
pub fn estimate_messages_tokens(messages: &[Message]) -> ContextUsageEstimate {
    estimate_messages(messages)
}

/// `estimateContextTokens` for a normalized [`TranscriptContext`]
/// (post-#9548: the prompt and tools ride the system messages, so the
/// message-only estimate already covers them; the old `Context` prefix /
/// `addedToolNames` adjustments are gone with the fields themselves).
pub fn estimate_context_tokens(context: &TranscriptContext) -> ContextUsageEstimate {
    estimate_messages(&context.messages)
}
