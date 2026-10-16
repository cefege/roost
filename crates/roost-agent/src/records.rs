//! The durable harness records: a conversation row, its append-only entry
//! journal, and the global agent settings. The coordinator persists them
//! through `AgentStore`; `projection` turns entries into transcript items.

use std::collections::BTreeMap;

use roost_llm::{AssistantBlock, StopReason, Usage};
use roost_protocol::wire::agent_chat::{
    AgentRunState, AskQuestion, FolderModelRoles, GoalState, JobState, ModelPresets, ModelRef,
    TodoPhase,
};
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
    Web,
}

impl Role {
    pub const ALL: [Self; 9] = [
        Self::Default,
        Self::Smol,
        Self::Slow,
        Self::Plan,
        Self::Task,
        Self::Tiny,
        Self::Judge,
        Self::Advisor,
        Self::Web,
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
            Self::Web => "web",
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
    /// The stored row's compare-and-set revision; 0 before the first save.
    #[serde(default)]
    pub revision: u64,
}

/// One journal entry. The journal is append-only: a plan proposal or an
/// advisory whose state changes is appended again under the same `item_id`,
/// and the latest copy wins in both the transcript and the model history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    User {
        text: String,
        /// An identical user steer must not be charged as a committed repeat.
        #[serde(default)]
        loop_repeat: bool,
    },
    Assistant {
        item_id: String,
        blocks: Vec<AssistantBlock>,
        usage: Usage,
        model: Option<ModelRef>,
        #[serde(default)]
        metadata: Option<AssistantMetadata>,
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
    /// A settled background bash job, delivered back into the conversation as a
    /// follow-up. `state` is `Completed`, `Failed` (including lost to a worker
    /// disconnect) or `Cancelled`.
    BackgroundJob {
        job_id: String,
        command: String,
        state: JobState,
        is_error: bool,
        output: String,
        exit_code: Option<i32>,
    },
    /// The agent's task list after a `todo` call; the latest copy is current.
    Todo { phases: Vec<TodoPhase> },
    /// Questions the `ask` tool put to the user; re-appended with `answers`.
    Ask {
        item_id: String,
        questions: Vec<AskQuestion>,
        answers: Option<BTreeMap<String, Vec<String>>>,
    },
    /// A `/btw` side question and its answer. Never sent to the model.
    Btw {
        item_id: String,
        question: String,
        answer: Option<String>,
        error: Option<String>,
    },
    /// Every earlier entry with `seq >= to_seq` is undone: hidden from the
    /// transcript and the model history.
    Rewind { to_seq: u64 },
    /// Tokens spent outside the conversation's own turns (advisor reviews,
    /// compaction, `/btw`, judgments). Counted in usage totals only.
    ExtraUsage {
        provider: String,
        model_id: String,
        usage: Usage,
    },
    /// A subagent's shared `task` context, written once at creation.
    SubagentContext { context: String },
    /// Durable intent for supervising accepted work across restarts.
    /// `through_seq` is the last journal sequence included in this run.
    RunIntent {
        active: bool,
        through_seq: u64,
        aborted: bool,
    },
    /// The durable `/loop` snapshot; the latest entry is authoritative.
    LoopState { state_json: Option<String> },
    /// The latest durable goal snapshot; each transition appends a replacement.
    Goal { state: GoalState },
    /// Hidden prompt provenance for one autonomous goal continuation.
    GoalContinuation {
        goal_id: String,
        #[serde(default)]
        budget_wrap_up: bool,
        #[serde(default)]
        input_through_seq: u64,
    },
    /// The last completed goal work unit, used to detect repeated activity.
    GoalActivity {
        goal_id: String,
        fingerprint: String,
    },
}

/// Durable request and run state for one assistant response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantMetadata {
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub stop_reason: Option<StopReason>,
    /// The last accepted submission represented in the model request.
    #[serde(default)]
    pub input_through_seq: u64,
    #[serde(default)]
    pub continued: bool,
    #[serde(default)]
    pub yield_reminders: u32,
    /// The eligible goal when the provider request was admitted, even if it is paused later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
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
    #[serde(default)]
    pub searxng_url: Option<String>,
    #[serde(default)]
    pub folder_model_roles: Vec<FolderModelRoles>,
    /// Named sets of all-folders roles: preset → role name → selector chain.
    #[serde(default)]
    pub model_presets: ModelPresets,
}

impl AgentSettings {
    /// The settings one conversation resolves against: the row for this exact
    /// worker and directory replaces the global value of each role it names.
    /// Blank values and unknown role names are skipped, so a row can only
    /// override a role it actually sets.
    pub fn for_folder(&self, worker_fp: &str, cwd: &str) -> AgentSettings {
        let mut effective = AgentSettings {
            model_roles: self.model_roles.clone(),
            default_model: self.default_model.clone(),
            advisor_enabled: self.advisor_enabled,
            searxng_url: self.searxng_url.clone(),
            folder_model_roles: Vec::new(),
            model_presets: ModelPresets::new(),
        };
        let Some(row) = self
            .folder_model_roles
            .iter()
            .find(|row| row.worker_fp == worker_fp && row.cwd == cwd)
        else {
            return effective;
        };
        for (name, chain) in &row.model_roles {
            let Some(role) = Role::from_name(name) else {
                continue;
            };
            if chain.trim().is_empty() {
                continue;
            }
            effective.model_roles.insert(role, chain.clone());
        }
        effective
    }
}

pub fn model_ref(provider: &str, model_id: &str) -> ModelRef {
    ModelRef {
        provider: provider.to_owned(),
        model_id: model_id.to_owned(),
    }
}
