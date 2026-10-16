//! Small read-only projections the chat surface uses to mount its controls.
//! Todo and loop state have one visible home beside the composer; neither is
//! rendered as conversation content by the transcript view.

use roost_protocol::wire::agent_chat::{TodoPhase, Transcript, TranscriptItem};

pub(super) fn latest_todo(transcript: &Transcript) -> Option<Vec<TodoPhase>> {
    transcript.items.iter().rev().find_map(|item| match item {
        TranscriptItem::Todo { phases, .. } => Some(phases.clone()),
        _ => None,
    })
}

pub(super) fn loop_status(transcript: &Transcript) -> Option<&str> {
    transcript.items.iter().find_map(|item| match item {
        TranscriptItem::LoopStatus { status, .. } => Some(status.as_str()),
        _ => None,
    })
}
