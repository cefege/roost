//! In-place application of host chat events to a transcript.
//! Invalid item and block references are ignored rather than panicking.

use super::model::{AgentRunState, ChatEvent, Transcript, TranscriptBlock, TranscriptItem};

pub fn fold_chat_event(transcript: &mut Transcript, event: &ChatEvent) {
    match event {
        ChatEvent::Reset {
            transcript: replacement,
        } => *transcript = replacement.clone(),
        // A user message is emitted once, when the run reads it, and belongs
        // after everything already shown; one that was queued leaves the queue.
        ChatEvent::Item {
            item: item @ TranscriptItem::User { id, .. },
        } => {
            transcript.queued.retain(|queued| queued.id != *id);
            transcript.items.retain(|old| old.id() != id);
            transcript.items.push(item.clone());
        }
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
        ChatEvent::Queued { message } => {
            transcript.items.retain(|item| item.id() != message.id);
            if !transcript
                .queued
                .iter()
                .any(|queued| queued.id == message.id)
            {
                transcript.queued.push(message.clone());
            }
        }
        ChatEvent::QueueRemoved { submission_id } => {
            transcript
                .queued
                .retain(|queued| queued.id != *submission_id);
            transcript.items.retain(|item| item.id() != submission_id);
        }
        ChatEvent::QueueDelivered { submission_ids } => {
            for id in submission_ids {
                if let Some(position) = transcript
                    .queued
                    .iter()
                    .position(|message| message.id == *id)
                {
                    let message = transcript.queued.remove(position);
                    if !transcript.items.iter().any(|item| item.id() == id) {
                        transcript.items.push(TranscriptItem::User {
                            id: message.id,
                            text: message.text,
                        });
                    }
                }
            }
        }
        ChatEvent::ScheduleSet { schedule } => {
            if let Some(existing) = transcript
                .schedules
                .iter_mut()
                .find(|row| row.id == schedule.id)
            {
                *existing = schedule.clone();
            } else {
                transcript.schedules.push(schedule.clone());
            }
        }
        ChatEvent::ScheduleRemoved { id } => {
            transcript.schedules.retain(|row| row.id != *id);
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
        ChatEvent::ToolStarted { item_id } => {
            if let Some(TranscriptItem::Tool { queued, .. }) = item_mut(transcript, item_id) {
                *queued = false;
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
            | Self::Advisory { id, .. }
            | Self::Todo { id, .. }
            | Self::Goal { id, .. }
            | Self::LoopStatus { id, .. }
            | Self::Ask { id, .. }
            | Self::BackgroundJob { id, .. }
            | Self::PeerMessage { id, .. }
            | Self::Btw { id, .. } => id,
        }
    }
}
#[allow(dead_code)]
fn _assert_closed_run_state(_: AgentRunState) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::agent_chat::ScheduledMessage;

    fn transcript() -> Transcript {
        Transcript {
            items: Vec::new(),
            run_state: AgentRunState::Idle,
            error: None,
            model: None,
            thinking_level: None,
            mode: None,
            usage: Default::default(),
            queued: Vec::new(),
            schedules: Vec::new(),
        }
    }

    fn schedule(id: &str, due_ms: i64) -> ScheduledMessage {
        ScheduledMessage {
            id: id.to_owned(),
            text: "ping".to_owned(),
            due_ms,
            every_ms: None,
            fired: 0,
        }
    }

    #[test]
    fn schedule_set_inserts_a_new_row() {
        let mut transcript = transcript();
        fold_chat_event(
            &mut transcript,
            &ChatEvent::ScheduleSet {
                schedule: schedule("s-1", 100),
            },
        );
        assert_eq!(transcript.schedules, vec![schedule("s-1", 100)]);
    }

    #[test]
    fn schedule_set_upserts_in_place_by_id() {
        let mut transcript = transcript();
        fold_chat_event(
            &mut transcript,
            &ChatEvent::ScheduleSet {
                schedule: schedule("s-1", 100),
            },
        );
        fold_chat_event(
            &mut transcript,
            &ChatEvent::ScheduleSet {
                schedule: schedule("s-1", 200),
            },
        );
        assert_eq!(transcript.schedules, vec![schedule("s-1", 200)]);
    }

    #[test]
    fn schedule_removed_drops_the_row() {
        let mut transcript = transcript();
        fold_chat_event(
            &mut transcript,
            &ChatEvent::ScheduleSet {
                schedule: schedule("s-1", 100),
            },
        );
        fold_chat_event(
            &mut transcript,
            &ChatEvent::ScheduleRemoved {
                id: "s-1".to_owned(),
            },
        );
        assert!(transcript.schedules.is_empty());
    }

    #[test]
    fn schedule_removed_leaves_other_rows_untouched() {
        let mut transcript = transcript();
        fold_chat_event(
            &mut transcript,
            &ChatEvent::ScheduleSet {
                schedule: schedule("s-1", 100),
            },
        );
        fold_chat_event(
            &mut transcript,
            &ChatEvent::ScheduleSet {
                schedule: schedule("s-2", 200),
            },
        );
        fold_chat_event(
            &mut transcript,
            &ChatEvent::ScheduleRemoved {
                id: "s-1".to_owned(),
            },
        );
        assert_eq!(transcript.schedules, vec![schedule("s-2", 200)]);
    }
}
