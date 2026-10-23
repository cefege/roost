//! Context compaction: when the last call's tokens approach the model's
//! window, summarise everything before the newest ~20 000 tokens with the
//! conversation's model and record a `Compaction` entry. `/compact` forces it.

use roost_llm::{AssistantBlock, ChatRequest, Message, ModelInfo};
use tokio_util::sync::CancellationToken;

use crate::compaction_text::{estimate_tokens, render_for_summary};
use crate::error::AgentError;
use crate::history::active_window;
use crate::model_call::{Silent, call_chain};
use crate::prompts;
use crate::records::{ConversationRecord, Entry};
use crate::runtime::AgentRuntime;

/// Headroom kept below the context window before compaction triggers.
pub(crate) const HEADROOM_TOKENS: u64 = 16_384;
/// Recent history kept verbatim after a compaction.
pub(crate) const KEEP_TOKENS: u64 = 20_000;

pub(crate) fn latest_compaction_usage_tokens(entries: &[(u64, Entry)]) -> Option<u64> {
    let (_, window) = active_window(entries);
    window.iter().rev().find_map(|(_, entry)| match entry {
        Entry::Assistant { usage, .. } if usage.input + usage.output > 0 => {
            Some(usage.input + usage.cache_read + usage.cache_write + usage.output)
        }
        _ => None,
    })
}

pub(crate) fn compaction_trigger_tokens(context_window: u64) -> u64 {
    context_window.saturating_sub(HEADROOM_TOKENS)
}

/// `models` is the conversation's candidate chain; its head sets the window.
pub(crate) async fn compact_if_needed(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    models: &[ModelInfo],
    cancel: &CancellationToken,
) -> Result<(), AgentError> {
    let Some(head) = models.first() else {
        return Err(AgentError::FailedPrecondition(
            "no model is available; connect a provider under Settings → Models".into(),
        ));
    };
    let entries =
        crate::journal::effective_entries(&runtime.inner.store.entries(&record.id).await?);
    let last_tokens = latest_compaction_usage_tokens(&entries);
    let limit = compaction_trigger_tokens(head.context_window);
    if last_tokens.is_some_and(|tokens| head.context_window > 0 && tokens > limit) {
        tracing::info!(conversation_id = %record.id, ?last_tokens, limit, "auto-compacting conversation");
        let summary_models = crate::turn::side_task_models(runtime, record).await?;
        compact(runtime, record, &summary_models, None, cancel).await?;
    }
    Ok(())
}

/// Compacts now. Returns whether anything was summarised.
pub(crate) async fn compact(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    models: &[ModelInfo],
    instructions: Option<&str>,
    cancel: &CancellationToken,
) -> Result<bool, AgentError> {
    let entries =
        crate::journal::effective_entries(&runtime.inner.store.entries(&record.id).await?);
    let (previous_summary, window) = active_window(&entries);
    let Some(split) = split_point(window) else {
        return Ok(false);
    };
    let first_kept_seq = window[split].0;
    let mut conversation = String::new();
    if let Some(summary) = previous_summary {
        conversation.push_str(&format!("[earlier summary]\n{summary}\n"));
    }
    conversation.push_str(&render_for_summary(&window[..split]));
    let extra = instructions
        .filter(|text| !text.trim().is_empty())
        .map(|text| format!("Additional instructions: {text}"))
        .unwrap_or_default();
    let prompt = prompts::render(
        prompts::COMPACTION,
        &[("instructions", &extra), ("conversation", &conversation)],
    );
    let attempts: Vec<ChatRequest> = models
        .iter()
        .map(|model| ChatRequest {
            model: model.clone(),
            system: vec!["You summarise coding-agent conversations precisely.".into()],
            messages: vec![Message::user_text(prompt.clone())],
            tools: Vec::new(),
            thinking: model.clamp_thinking_level("off").to_owned(),
            session_id: record.id.clone(),
            max_tokens: Some(8192),
        })
        .collect();
    let (used, result) = call_chain(runtime, &record.id, &attempts, cancel, &mut Silent).await;
    let output = result?;
    let answered = &attempts[used].model;
    runtime
        .add_extra_usage(&record.id, &answered.provider, &answered.id, &output.usage)
        .await;
    let summary = output
        .blocks
        .iter()
        .filter_map(|block| match block {
            AssistantBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if summary.trim().is_empty() {
        return Err(AgentError::Model(
            "compaction produced an empty summary".into(),
        ));
    }
    runtime
        .append(
            &record.id,
            Entry::Compaction {
                summary,
                first_kept_seq,
            },
        )
        .await?;
    runtime.advisor_reset(&record.id);
    tracing::info!(conversation_id = %record.id, first_kept_seq, "conversation compacted");
    Ok(true)
}

/// The index of the first kept entry: the newest `KEEP_TOKENS` stay, and the
/// cut moves forward to a user or assistant message so no tool result is
/// separated from its call.
fn split_point(window: &[(u64, Entry)]) -> Option<usize> {
    let mut kept_tokens = 0;
    let mut boundary = window.len();
    for (position, (_, entry)) in window.iter().enumerate().rev() {
        kept_tokens += estimate_tokens(entry);
        boundary = position;
        if kept_tokens >= KEEP_TOKENS {
            break;
        }
    }
    let cut = (boundary..window.len())
        .find(|position| {
            matches!(
                window[*position].1,
                Entry::User { .. } | Entry::Assistant { .. }
            )
        })
        .unwrap_or(window.len());
    (cut > 0 && cut < window.len()).then_some(cut)
}

#[cfg(test)]
mod tests {
    use super::{compaction_trigger_tokens, latest_compaction_usage_tokens};
    use crate::records::Entry;
    use roost_llm::Usage;

    #[test]
    fn automatic_compaction_usage_includes_output_tokens() {
        let assistant = Entry::Assistant {
            item_id: "assistant".into(),
            blocks: Vec::new(),
            usage: Usage {
                input: 100,
                output: 25,
                cache_read: 10,
                cache_write: 5,
            },
            model: None,
            metadata: None,
        };
        assert_eq!(latest_compaction_usage_tokens(&[(1, assistant)]), Some(140));
    }

    #[test]
    fn compaction_threshold_matches_native_headroom() {
        assert_eq!(compaction_trigger_tokens(32_000), 15_616);
    }
}
