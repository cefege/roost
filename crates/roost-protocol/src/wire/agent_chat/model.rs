//! The agent chat transcript contract shared by the coordinator's harness and
//! the browser: conversations, transcript items, and the events that fold them.

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
    /// `normal` or `plan`.
    #[serde(default = "normal_mode")]
    pub mode: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    /// Whether the advisor reviews this conversation's turns.
    #[serde(default)]
    pub advisor: bool,
    pub run_state: AgentRunState,
    pub error: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
}

fn normal_mode() -> String {
    "normal".to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub items: Vec<TranscriptItem>,
    pub run_state: AgentRunState,
    pub error: Option<String>,
    pub model: Option<ModelRef>,
    pub thinking_level: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
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
        #[serde(default)]
        children: Vec<String>,
    },
    #[serde(rename = "notice")]
    Notice {
        id: String,
        level: String,
        title: String,
        body: String,
    },
    #[serde(rename = "plan")]
    Plan {
        id: String,
        title: String,
        content: String,
        state: String,
    },
    #[serde(rename = "advisory")]
    Advisory {
        id: String,
        severity: String,
        note: String,
        delivered: bool,
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
        mode: Option<String>,
    },
    Usage {
        usage: UsageTotals,
    },
}
