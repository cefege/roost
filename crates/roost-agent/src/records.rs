//! The durable harness records: a conversation row, its append-only entry
//! journal, and the global agent settings. The coordinator persists them
//! through `AgentStore`; `projection` turns entries into transcript items.

use std::collections::BTreeMap;

use roost_protocol::wire::agent_chat::{AgentRunState, FolderModelRoles, ModelPresets, ModelRef};
use serde::{Deserialize, Serialize};

mod journal;

pub use journal::{AssistantMetadata, Entry, QueuedInputState};

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
    #[serde(default)]
    pub read_only: bool,
    pub run_state: AgentRunState,
    pub error: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
    /// The stored row's compare-and-set revision; 0 before the first save.
    #[serde(default)]
    pub revision: u64,
}

impl ConversationRecord {
    /// The one owner of the parent link: peer lookups, cascades and the
    /// fleet's child listings all ask through this.
    pub(crate) fn is_child_of(&self, parent_id: &str) -> bool {
        self.parent_id.as_deref() == Some(parent_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    Proposed,
    Approved,
    Refined,
    Rejected,
}

impl PlanState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Approved => "approved",
            Self::Refined => "refined",
            Self::Rejected => "rejected",
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
