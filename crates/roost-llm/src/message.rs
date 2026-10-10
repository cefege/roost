//! The provider-neutral chat wire model: one request shape every provider
//! client translates from, and one stream-event shape every provider client
//! translates into. Thinking blocks carry provider data the provider requires back.

use serde::{Deserialize, Serialize};

use crate::catalog::ModelInfo;

/// One model call.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub model: ModelInfo,
    pub system: Vec<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    /// One of `off|minimal|low|medium|high|xhigh|max`; `auto` is resolved upstream.
    pub thinking: String,
    /// Stable per conversation: prompt-cache affinity and Codex session id.
    pub session_id: String,
    pub max_tokens: Option<u64>,
}

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User {
        content: Vec<UserContent>,
    },
    Assistant {
        blocks: Vec<AssistantBlock>,
    },
    ToolResult {
        call_id: String,
        tool_name: String,
        text: String,
        is_error: bool,
    },
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Message::User {
            content: vec![UserContent::Text { text: text.into() }],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContent {
    Text { text: String },
    Image { mime: String, base64: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantBlock {
    Text {
        text: String,
    },
    /// `provider_data` round-trips Anthropic `signature` / `redacted_thinking`
    /// and Codex encrypted reasoning: providers reject a resent thinking block
    /// without it.
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_data: Option<serde_json::Value>,
    },
    ToolCall {
        call_id: String,
        name: String,
        args_json: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Text,
    Thinking,
    ToolCall,
}

/// Token counts for one model call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Error(String),
}

/// One event of a streamed model response. `index` is the assistant block
/// index within this response; every block ends with exactly one `BlockEnd`.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    BlockStart {
        index: usize,
        kind: BlockKind,
    },
    TextDelta {
        index: usize,
        text: String,
    },
    ThinkingDelta {
        index: usize,
        text: String,
    },
    ToolCallStart {
        index: usize,
        call_id: String,
        name: String,
    },
    ToolArgsDelta {
        index: usize,
        json: String,
    },
    BlockEnd {
        index: usize,
        block: AssistantBlock,
    },
    Usage(Usage),
    Stop(StopReason),
}
