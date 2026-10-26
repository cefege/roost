//! Native agent workspace inspection data shared by the coordinator and web client.
//! Values describe persisted runtime state and the effective tool surface only.
//! Credentials, provider secrets and synthetic scheduler identifiers are excluded.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ConversationSummary, GoalState, QueuedMessage, ScheduledMessage, UsageTotals};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInspection {
    pub conversation: ConversationSummary,
    pub context: ContextInspection,
    pub tools: Vec<InspectedTool>,
    pub activity: Vec<AgentActivity>,
    #[serde(default)]
    pub read_only: bool,
    pub goal: Option<GoalState>,
    #[serde(default)]
    pub inbox: Vec<QueuedMessage>,
    #[serde(default)]
    pub schedules: Vec<ScheduledMessage>,
    pub usage: UsageTotals,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextInspection {
    pub context_tokens: u64,
    pub context_window_tokens: Option<u64>,
    pub compaction_trigger_tokens: Option<u64>,
    pub compaction_headroom_tokens: u64,
    pub compaction_keep_tokens: u64,
    pub last_compacted_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InspectedTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub restriction: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentActivity {
    pub kind: String,
    pub label: String,
    pub state: String,
    pub detail: Option<String>,
}
