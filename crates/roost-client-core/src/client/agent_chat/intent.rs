//! UI events that install or discard a fetched transcript.

use roost_protocol::wire::agent_chat::Transcript;

#[derive(Debug, Clone, PartialEq)]
pub enum AgentChatIntent {
    SnapshotRequested {
        conversation_id: String,
        generation: u64,
    },
    SnapshotLoaded {
        conversation_id: String,
        generation: u64,
        seq: u64,
        transcript: Transcript,
    },
    EventsLoaded {
        conversation_id: String,
        batches: Vec<(u64, Vec<roost_protocol::wire::agent_chat::ChatEvent>)>,
        truncated: bool,
    },
    Forget {
        conversation_id: String,
    },
}
