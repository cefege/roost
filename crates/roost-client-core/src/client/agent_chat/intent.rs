//! UI events that install or discard a fetched transcript.

use roost_protocol::wire::agent_chat::Transcript;

#[derive(Debug, Clone, PartialEq)]
pub enum AgentChatIntent {
    SnapshotLoaded {
        conversation_id: String,
        seq: u64,
        transcript: Transcript,
    },
    Forget {
        conversation_id: String,
    },
}
