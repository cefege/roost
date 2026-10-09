//! JSON contract values shared by the agent host, coordinator and browser.
//! These types keep the host stream's tags and nullable values explicit.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunState {
    Idle,
    Running,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub id: String,
    pub title: String,
    pub worker_fp: String,
    pub worker_label: String,
    pub cwd: String,
    pub model: Option<ModelRef>,
    pub thinking_level: Option<String>,
    pub run_state: AgentRunState,
    pub error: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub items: Vec<TranscriptItem>,
    pub run_state: AgentRunState,
    pub error: Option<String>,
    pub model: Option<ModelRef>,
    pub thinking_level: Option<String>,
    pub usage: UsageTotals,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TranscriptItem {
    #[serde(rename = "user")]
    User { id: String, text: String },
    #[serde(rename = "assistant")]
    Assistant {
        id: String,
        blocks: Vec<TranscriptBlock>,
        streaming: bool,
        error: Option<String>,
    },
    #[serde(rename = "tool")]
    Tool {
        id: String,
        call_id: String,
        tool_name: String,
        args_json: String,
        output: String,
        is_error: bool,
        running: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TranscriptBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { text: String },
    #[serde(rename = "tool_call")]
    ToolCall {
        call_id: String,
        tool_name: String,
        args_json: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    Reset {
        transcript: Transcript,
    },
    Item {
        item: TranscriptItem,
    },
    TextDelta {
        item_id: String,
        block: usize,
        delta: String,
    },
    ThinkingDelta {
        item_id: String,
        block: usize,
        delta: String,
    },
    BlockSet {
        item_id: String,
        block: usize,
        value: TranscriptBlock,
    },
    ToolOutput {
        item_id: String,
        trim_start: Option<u64>,
        append: Option<String>,
        set: Option<String>,
    },
    RunState {
        run_state: AgentRunState,
        error: Option<String>,
    },
    Agent {
        model: Option<ModelRef>,
        thinking_level: Option<String>,
    },
    Usage {
        usage: UsageTotals,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostStreamLine {
    Hello {
        protocol: u32,
    },
    Conversations {
        conversations: Vec<ConversationSummary>,
    },
    Conversation {
        conversation: ConversationSummary,
    },
    ConversationRemoved {
        id: String,
    },
    Chat {
        conversation_id: String,
        events: Vec<ChatEvent>,
    },
    Ping,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelsCatalog {
    pub models: Vec<ModelEntry>,
    pub providers: Vec<ProviderEntry>,
    pub thinking_levels: Vec<String>,
    pub default_model: Option<ModelRef>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelEntry {
    pub provider: String,
    pub model_id: String,
    pub name: String,
    pub reasoning: bool,
    pub available: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub id: String,
    pub name: String,
    pub configured: bool,
    pub credential: Option<CredentialKind>,
    pub supports_oauth: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    Oauth,
    ApiKey,
    Env,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginState {
    pub state: LoginStatus,
    pub prompt: Option<LoginPrompt>,
    pub notices: Vec<LoginNotice>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatus {
    Waiting,
    Prompt,
    Done,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginPrompt {
    pub id: String,
    #[serde(rename = "type")]
    pub prompt_type: LoginPromptType,
    pub message: String,
    pub options: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginPromptType {
    Text,
    Secret,
    Select,
    ManualCode,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginNotice {
    #[serde(rename = "type")]
    pub notice_type: LoginNoticeType,
    pub message: String,
    pub url: Option<String>,
    pub code: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginNoticeType {
    Info,
    AuthUrl,
    DeviceCode,
    Progress,
}
