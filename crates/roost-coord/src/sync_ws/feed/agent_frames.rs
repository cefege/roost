//! Sync frames for the built-in agent conversation and transcript buses.
//!
//! Kept beside the generic bus adapters because these wire values carry their
//! own durable conversation identity and transcript sequence.

use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{AgentChatEventsFrame, AgentConversationFrame, FirehoseFrame};

use crate::events::bus_messages::{AgentChatEventsUpdate, AgentConversationUpdate};
use crate::sync_ws::feed::FeedFrame;

/// One agent conversation update as an install-wide Sync frame.
pub fn agent_conversation_frame(update: &AgentConversationUpdate) -> FeedFrame {
    FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::AgentConversation(Box::new(AgentConversationFrame {
            conversation_id: update.conversation_id.clone(),
            removed: update.removed,
            conversation: update
                .conversation
                .as_ref()
                .map(roost_protocol::wire::agent_chat::conversation_to_proto)
                .into(),
            host_connected: update.host_connected.unwrap_or_default(),
            ..Default::default()
        }))),
        ..Default::default()
    })
}

/// One ordered agent transcript batch as a Sync frame.
pub fn agent_chat_events_frame(update: &AgentChatEventsUpdate) -> FeedFrame {
    FeedFrame::of(FirehoseFrame {
        frame: Some(Frame::AgentChatEvents(Box::new(AgentChatEventsFrame {
            conversation_id: update.conversation_id.clone(),
            seq: update.seq,
            events_json: update.events_json.clone(),
            ..Default::default()
        }))),
        ..Default::default()
    })
}
