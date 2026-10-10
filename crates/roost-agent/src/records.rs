//! The durable harness records: a conversation row, its append-only entry
//! journal, and the global agent settings. The coordinator persists them
//! through `AgentStore`; `projection` turns entries into transcript items.

use std::collections::BTreeMap;

use roost_llm::{AssistantBlock, Usage};
use roost_protocol::wire::agent_chat::{AgentRunState, ModelRef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Normal,
    Plan,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Plan => "plan",
        }
    }

    pub fn from_wire(value: &str) -> Self {
        if value == "plan" {
            Self::Plan
        } else {
            Self::Normal
        }
    }
}

/// A model role: a named slot a selector is configured for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Default,
    Smol,
    Slow,
    Plan,
    Task,
    Tiny,
    Judge,
    Advisor,
}

impl Role {
    pub const ALL: [Self; 8] = [
        Self::Default,
        Self::Smol,
        Self::Slow,
        Self::Plan,
        Self::Task,
        Self::Tiny,
        Self::Judge,
        Self::Advisor,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Smol => "smol",
            Self::Slow => "slow",
            Self::Plan => "plan",
            Self::Task => "task",
            Self::Tiny => "tiny",
            Self::Judge => "judge",
            Self::Advisor => "advisor",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationRecord {
    pub id: String,
    pub title: String,
    pub worker_fp: String,
    pub worker_label: String,
    pub worker_os: String,
    pub cwd: String,
    pub model: Option<ModelRef>,
    /// `off|minimal|low|medium|high|xhigh|max` or `auto`.
    pub thinking_level: String,
    pub mode: Mode,
    pub pre_plan_model: Option<ModelRef>,
    pub parent_id: Option<String>,
    /// The bundled subagent kind (`scout`, `task`, `reviewer`) of a child.
    pub agent: Option<String>,
    /// Per-conversation advisor override; `None` follows the global setting.
    pub advisor: Option<bool>,
    pub run_state: AgentRunState,
    pub error: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// One journal entry. The journal is append-only: a plan proposal or an
/// advisory whose state changes is appended again under the same `item_id`,
/// and the latest copy wins in both the transcript and the model history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    User {
        text: String,
    },
    Assistant {
        item_id: String,
        blocks: Vec<AssistantBlock>,
        usage: Usage,
        model: Option<ModelRef>,
    },
    ToolResult {
        call_id: String,
        tool_name: String,
        text: String,
        is_error: bool,
        details_json: String,
    },
    Compaction {
        summary: String,
        first_kept_seq: u64,
    },
    ModeChange {
        mode: Mode,
        plan_title: Option<String>,
    },
    PlanProposal {
        item_id: String,
        title: String,
        content: String,
        state: PlanState,
    },
    Notice {
        level: String,
        title: String,
        body: String,
    },
    Advisory {
        item_id: String,
        severity: AdvisorySeverity,
        note: String,
        delivered: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    Proposed,
    Approved,
    Refined,
}

impl PlanState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Approved => "approved",
            Self::Refined => "refined",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdvisorySeverity {
    Nit,
    Concern,
    Blocker,
}

impl AdvisorySeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nit => "nit",
            Self::Concern => "concern",
            Self::Blocker => "blocker",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "nit" => Some(Self::Nit),
            "concern" => Some(Self::Concern),
            "blocker" => Some(Self::Blocker),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSettings {
    #[serde(default)]
    pub model_roles: BTreeMap<Role, String>,
    #[serde(default)]
    pub default_model: Option<ModelRef>,
    #[serde(default)]
    pub advisor_enabled: bool,
}

pub fn model_ref(provider: &str, model_id: &str) -> ModelRef {
    ModelRef {
        provider: provider.to_owned(),
        model_id: model_id.to_owned(),
    }
}
