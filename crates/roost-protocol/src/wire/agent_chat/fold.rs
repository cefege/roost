//! In-place application of host chat events to a transcript.
//! Invalid item and block references are ignored rather than panicking.

use super::model::{AgentRunState, ChatEvent, Transcript, TranscriptBlock, TranscriptItem};

pub fn fold_chat_event(transcript: &mut Transcript, event: &ChatEvent) {
    match event {
        ChatEvent::Reset {
            transcript: replacement,
        } => *transcript = replacement.clone(),
        ChatEvent::Item { item } => {
            if let Some(existing) = transcript
                .items
                .iter_mut()
                .find(|old| old.id() == item.id())
            {
                *existing = item.clone();
            } else {
                transcript.items.push(item.clone());
            }
        }
        ChatEvent::TextDelta {
            item_id,
            block,
            delta,
        } => append_block(transcript, item_id, *block, delta, false),
        ChatEvent::ThinkingDelta {
            item_id,
            block,
            delta,
        } => append_block(transcript, item_id, *block, delta, true),
        ChatEvent::BlockSet {
            item_id,
            block,
            value,
        } => {
            if let Some(TranscriptItem::Assistant { blocks, .. }) = item_mut(transcript, item_id) {
                if *block < blocks.len() {
                    blocks[*block] = value.clone();
                } else if *block == blocks.len() {
                    blocks.push(value.clone());
                } else {
                    tracing::debug!(item_id, block, "ignoring out-of-range agent block");
                }
            }
        }
        ChatEvent::ToolOutput {
            item_id,
            trim_start,
            append,
            set,
        } => {
            if let Some(TranscriptItem::Tool { output, .. }) = item_mut(transcript, item_id) {
                if let Some(replacement) = set {
                    *output = replacement.clone();
                } else {
                    if let Some(count) = trim_start {
                        let boundary = output
                            .char_indices()
                            .map(|(idx, _)| idx)
                            .chain(std::iter::once(output.len()))
                            .nth(usize::try_from(*count).unwrap_or(usize::MAX))
                            .unwrap_or(output.len());
                        output.drain(..boundary);
                    }
                    if let Some(suffix) = append {
                        output.push_str(suffix);
                    }
                }
            }
        }
        ChatEvent::RunState { run_state, error } => {
            transcript.run_state = *run_state;
            transcript.error = error.clone();
        }
        ChatEvent::Agent {
            model,
            thinking_level,
            mode,
        } => {
            transcript.model = model.clone();
            transcript.thinking_level = thinking_level.clone();
            transcript.mode = mode.clone();
        }
        ChatEvent::Usage { usage } => transcript.usage = usage.clone(),
    }
}

fn append_block(
    transcript: &mut Transcript,
    item_id: &str,
    block: usize,
    delta: &str,
    thinking: bool,
) {
    if let Some(TranscriptItem::Assistant { blocks, .. }) = item_mut(transcript, item_id) {
        if block == blocks.len() {
            blocks.push(if thinking {
                TranscriptBlock::Thinking {
                    text: String::new(),
                }
            } else {
                TranscriptBlock::Text {
                    text: String::new(),
                }
            });
        }
        match blocks.get_mut(block) {
            Some(TranscriptBlock::Text { text }) | Some(TranscriptBlock::Thinking { text }) => {
                text.push_str(delta)
            }
            Some(TranscriptBlock::ToolCall { .. }) => {
                tracing::debug!(item_id, block, "ignoring text delta for tool-call block")
            }
            None => tracing::debug!(item_id, block, "ignoring out-of-range agent block"),
        }
    }
}
fn item_mut<'a>(transcript: &'a mut Transcript, id: &str) -> Option<&'a mut TranscriptItem> {
    let item = transcript.items.iter_mut().find(|item| item.id() == id);
    if item.is_none() {
        tracing::debug!(item_id = id, "ignoring agent event for unknown item");
    }
    item
}
impl TranscriptItem {
    fn id(&self) -> &str {
        match self {
            Self::User { id, .. }
            | Self::Assistant { id, .. }
            | Self::Tool { id, .. }
            | Self::Notice { id, .. }
            | Self::Plan { id, .. }
            | Self::Advisory { id, .. } => id,
        }
    }
}
#[allow(dead_code)]
fn _assert_closed_run_state(_: AgentRunState) {}
