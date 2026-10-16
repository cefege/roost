//! The agent chat transcript contract shared by the coordinator's harness and
//! the browser: conversations, transcript items, and the events that fold them.

use serde::{Deserialize, Serialize};

pub use super::interaction::{AskOption, AskQuestion, TodoPhase, TodoStatus, TodoTask};
use super::jobs::JobState;

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
    /// First available candidate for the current scope; `model` is the session pin.
    #[serde(default)]
    pub effective_model: Option<ModelRef>,
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

/// The model role a subagent kind's turns draw from (`scout` → `smol`,
/// `reviewer` → `slow`, `task` → `task`); `None` for any other agent name.
/// The agent runtime resolves with it and the composer marks the row with it.
pub fn subagent_role(agent: &str) -> Option<&'static str> {
    match agent {
        "scout" => Some("smol"),
        "reviewer" => Some("slow"),
        "task" => Some("task"),
        _ => None,
    }
}

impl ConversationSummary {
    /// The role this conversation's own turns run on: Architect in plan mode,
    /// its kind's role for a subagent, otherwise Default.
    pub fn running_role(&self) -> &'static str {
        if self.mode == "plan" {
            return "plan";
        }
        self.agent
            .as_deref()
            .and_then(subagent_role)
            .unwrap_or("default")
    }
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
    /// Messages sent while the agent works, waiting for the run's next
    /// boundary (after the current model call or tool round). Each leaves
    /// this list for `items` when the run reads it, so it appears where the
    /// agent took it, never inside the reply it interrupted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued: Vec<QueuedMessage>,
}

/// A user message accepted but not yet read by the running agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedMessage {
    /// The id its user item will have.
    pub id: String,
    pub text: String,
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
        /// Accepted calls wait here until their executor is entered.
        #[serde(default)]
        queued: bool,
        #[serde(default)]
        details_json: String,
        #[serde(default)]
        children: Vec<String>,
    },
    /// A background job that settled. A standalone item, never a tool step, so the
    /// result reads as its own transcript row. `id` is `job_<job_id>`.
    #[serde(rename = "background_job")]
    BackgroundJob {
        id: String,
        job_id: String,
        command: String,
        state: JobState,
        is_error: bool,
        output: String,
        exit_code: Option<i32>,
    },
    /// A message another conversation sent to this one. An arrival, not a reply, so
    /// it renders as its own row. `from` is the sender's name, `from_id` its id.
    #[serde(rename = "peer_message")]
    PeerMessage {
        id: String,
        from_id: String,
        from: String,
        #[serde(default)]
        broadcast: bool,
        text: String,
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
    /// The agent's task list; one per conversation, replaced on every change.
    #[serde(rename = "todo")]
    Todo { id: String, phases: Vec<TodoPhase> },
    /// Questions the agent put to the user; `answers` is `None` until answered.
    #[serde(rename = "ask")]
    Ask {
        id: String,
        questions: Vec<AskQuestion>,
        answers: Option<std::collections::BTreeMap<String, Vec<String>>>,
    },
    /// A `/btw` side question; `answer` and `error` are `None` while it runs.
    #[serde(rename = "btw")]
    Btw {
        id: String,
        question: String,
        answer: Option<String>,
        error: Option<String>,
    },
    #[serde(rename = "goal")]
    Goal { id: String, state: GoalState },
    /// The compact status of an active `/loop`. Ephemeral: upserted while the
    /// loop is enabled, dropped by a transcript reset, and never journaled.
    #[serde(rename = "loop_status")]
    LoopStatus { id: String, status: String },
}
/// Durable state for the session's OMP-compatible `/goal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    BudgetLimited,
    Complete,
    Dropped,
}

impl GoalStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::BudgetLimited => "budget_limited",
            Self::Complete => "complete",
            Self::Dropped => "dropped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalState {
    pub id: String,
    pub objective: String,
    pub status: GoalStatus,
    pub token_budget: Option<u64>,
    pub tokens_used: u64,
    pub time_used_seconds: u64,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub report: Option<String>,
    pub waiting_reason: Option<String>,
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
    ToolStarted {
        item_id: String,
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
    /// A message joined the queue; its `Item` follows when the run reads it.
    Queued {
        message: QueuedMessage,
    },
}

#[cfg(test)]
mod tests {
    use super::{AgentRunState, ConversationSummary};

    fn conversation(mode: &str, agent: Option<&str>) -> ConversationSummary {
        ConversationSummary {
            id: "c".into(),
            title: String::new(),
            worker_fp: String::new(),
            worker_label: String::new(),
            cwd: String::new(),
            model: None,
            effective_model: None,
            thinking_level: None,
            mode: mode.into(),
            parent_id: None,
            agent: agent.map(Into::into),
            advisor: false,
            run_state: AgentRunState::Idle,
            error: None,
            created_ms: 0,
            updated_ms: 0,
        }
    }

    #[test]
    fn plan_mode_then_subagent_kind_then_default_decide_the_running_role() {
        assert_eq!(conversation("plan", Some("scout")).running_role(), "plan");
        assert_eq!(conversation("normal", Some("scout")).running_role(), "smol");
        assert_eq!(
            conversation("normal", Some("reviewer")).running_role(),
            "slow"
        );
        assert_eq!(conversation("normal", Some("task")).running_role(), "task");
        assert_eq!(
            conversation("normal", Some("other")).running_role(),
            "default"
        );
        assert_eq!(conversation("normal", None).running_role(), "default");
    }
}
