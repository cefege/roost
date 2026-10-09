//! Agent-host JSON contracts, transcript folding, and sync protobuf conversion.

mod fold;
mod model;
mod proto;
mod tunnel_args;

pub use fold::fold_chat_event;
pub use model::*;
pub use proto::{conversation_from_proto, conversation_to_proto};
pub use tunnel_args::validated_daemon_args;

impl AgentRunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Failed => "failed",
        }
    }
    pub fn from_wire(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "failed" => Self::Failed,
            _ => Self::Idle,
        }
    }
}
