//! Agent chat JSON contracts, transcript folding, tool shapes, slash commands,
//! and sync protobuf conversion.

mod catalog;
mod commands;
mod fold;
mod model;
mod proto;
mod tools;
pub use commands::{AGENT_SLASH_COMMANDS, SlashCommand, parse_slash_command};
pub use tools::*;

pub use catalog::*;
pub use fold::fold_chat_event;
pub use model::*;
pub use proto::{conversation_from_proto, conversation_to_proto};

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
