//! Agent chat JSON contracts, transcript folding, tool shapes, slash commands,
//! and sync protobuf conversion.
mod image_payload;
pub use image_payload::{
    AgentChatImagePayloadTooLarge, MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES,
    validate_agent_chat_image_payload,
};

mod commands;
mod event;
mod fold;
pub use event::ChatEvent;
mod interaction;
mod interaction_validation;
mod jobs;
mod model;
pub use interaction_validation::{
    OmpInteractionValidationError, validate_omp_interaction_response,
};
mod omp;
pub use omp::*;
mod proto;
pub mod rpi;
pub mod semantic_ui;
pub use rpi::{RpiCommandPayload, RpiCommandResult, RpiErrorCode};
mod tools;
mod usage;
/// An attachment admitted with a user submission.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentChatAttachment {
    pub filename: String,
    pub mime_type: String,
    #[serde(default)]
    pub data: Vec<u8>,
    #[serde(default)]
    pub path: String,
}
pub use commands::{AGENT_SLASH_COMMANDS, SlashArgument, SlashCommand, parse_slash_command};
pub use jobs::*;
pub use tools::*;

pub use fold::fold_chat_event;
pub use model::*;
pub use proto::{conversation_from_proto, conversation_to_proto};
pub use usage::UsageTotals;

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
