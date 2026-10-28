//! Agent chat JSON contracts, transcript folding, tool shapes, slash commands,
//! and sync protobuf conversion.
mod image_payload;
pub use image_payload::{
    AgentChatImagePayloadTooLarge, MAX_AGENT_CHAT_IMAGE_PAYLOAD_BYTES,
    validate_agent_chat_image_payload,
};

mod catalog;
mod commands;
mod fold;
mod inspection;
mod interaction;
mod jobs;
mod model;
mod model_order;
mod model_presets;
pub mod model_search;
mod proto;
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
/// Original user content returned when a turn is rewound for replacement.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RewoundUserMessage {
    pub text: String,
    pub attachments: Vec<AgentChatAttachment>,
}
pub use commands::{AGENT_SLASH_COMMANDS, SlashArgument, SlashCommand, parse_slash_command};
pub use jobs::*;
pub use tools::*;

pub use catalog::*;
pub use fold::fold_chat_event;
pub use inspection::{AgentActivity, AgentInspection, ContextInspection, InspectedTool};
pub use model::*;
pub use model_order::{
    compare_models_newest_first, compare_newest_first, model_family, model_version,
};
pub use model_presets::{
    BUILTIN_PRESETS, ModelPresets, PRESET_NAME_HINT, active_preset, apply_preset,
    is_builtin_preset, presets_with_builtins, valid_preset_name,
};
pub use model_search::{fuzzy_score, model_search_text, rank_matches};
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
