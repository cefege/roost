//! Small read-only projections the chat surface uses to mount its controls.

use roost_protocol::wire::agent_chat::{Transcript, TranscriptItem};

/// Whether the newest visible item is an assistant message still receiving
/// text. Docked goal/status records are not transcript progress indicators.
pub(super) fn assistant_is_streaming(transcript: &Transcript) -> bool {
    transcript
        .items
        .iter()
        .rev()
        .find_map(|item| match item {
            TranscriptItem::Goal { .. }
            | TranscriptItem::Todo { .. }
            | TranscriptItem::LoopStatus { .. } => None,
            TranscriptItem::Assistant { streaming, .. } => Some(*streaming),
            _ => Some(false),
        })
        .unwrap_or(false)
}
