//! Which tools a conversation offers the model: the worker tools from
//! roost-protocol, filtered by mode and subagent kind, plus the harness tools
//! the coordinator executes itself (`task`, `find`, `propose_plan`, `yield`).

use roost_llm::ToolSpec;
use roost_protocol::wire::agent_chat::{
    TOOL_BASH, TOOL_EDIT, TOOL_GLOB, TOOL_GREP, TOOL_LSP, TOOL_READ, TOOL_WRITE, worker_tool_specs,
};
use serde_json::json;

use crate::prompts;
use crate::records::{ConversationRecord, Mode};

pub const TOOL_TASK: &str = "task";
pub const TOOL_FIND: &str = "find";
pub const TOOL_PROPOSE_PLAN: &str = "propose_plan";
pub const TOOL_YIELD: &str = "yield";

/// Subagent nesting limit: a depth-2 child may not spawn further children.
pub const MAX_DEPTH: u32 = 2;

/// LSP actions that only read.
pub const LSP_READ_ACTIONS: [&str; 5] = [
    "diagnostics",
    "definition",
    "references",
    "hover",
    "symbols",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentKind {
    Scout,
    Task,
    Reviewer,
}

impl SubagentKind {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "scout" => Some(Self::Scout),
            "task" => Some(Self::Task),
            "reviewer" => Some(Self::Reviewer),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scout => "scout",
            Self::Task => "task",
            Self::Reviewer => "reviewer",
        }
    }

    pub fn prompt(self) -> &'static str {
        match self {
            Self::Scout => prompts::AGENT_SCOUT,
            Self::Task => prompts::AGENT_TASK,
            Self::Reviewer => prompts::AGENT_REVIEWER,
        }
    }
}

/// The model-facing tools of one call and which side executes each.
#[derive(Debug, Clone, Default)]
pub struct Toolset {
    pub specs: Vec<ToolSpec>,
    /// Whether `lsp` is limited to `LSP_READ_ACTIONS`.
    pub lsp_read_only: bool,
}

impl Toolset {
    pub fn names(&self) -> Vec<String> {
        self.specs.iter().map(|spec| spec.name.clone()).collect()
    }

    pub fn offers(&self, name: &str) -> bool {
        self.specs.iter().any(|spec| spec.name == name)
    }
}

/// Whether a tool only reads, so a round of such calls may run concurrently.
pub fn is_read_only(name: &str) -> bool {
    matches!(
        name,
        TOOL_READ | TOOL_GREP | TOOL_GLOB | TOOL_LSP | TOOL_FIND
    )
}

/// The tools for `record` at nesting `depth` (0 = a top-level conversation).
pub fn toolset_for(record: &ConversationRecord, depth: u32, find_available: bool) -> Toolset {
    let kind = record.agent.as_deref().and_then(SubagentKind::from_name);
    let plan_mode = record.mode == Mode::Plan;
    let read_only_lsp = plan_mode || kind == Some(SubagentKind::Scout);
    let worker_names: &[&str] = match kind {
        Some(SubagentKind::Scout) => &[TOOL_READ, TOOL_GREP, TOOL_GLOB, TOOL_LSP],
        Some(SubagentKind::Reviewer) => &[TOOL_READ, TOOL_GREP, TOOL_GLOB, TOOL_LSP, TOOL_BASH],
        Some(SubagentKind::Task) | None if plan_mode => {
            &[TOOL_READ, TOOL_GREP, TOOL_GLOB, TOOL_LSP, TOOL_BASH]
        }
        Some(SubagentKind::Task) | None => &[
            TOOL_READ, TOOL_GREP, TOOL_GLOB, TOOL_LSP, TOOL_BASH, TOOL_EDIT, TOOL_WRITE,
        ],
    };
    let mut specs: Vec<ToolSpec> = worker_tool_specs(read_only_lsp)
        .into_iter()
        .chain(
            worker_tool_specs(false)
                .into_iter()
                .filter(|spec| spec.name == TOOL_EDIT || spec.name == TOOL_WRITE),
        )
        .filter(|spec| worker_names.contains(&spec.name.as_str()))
        .map(|spec| ToolSpec {
            name: spec.name,
            description: spec.description,
            parameters: spec.parameters,
        })
        .collect();
    specs.dedup_by(|left, right| left.name == right.name);
    if find_available {
        specs.push(find_spec());
    }
    let may_delegate = match kind {
        None => true,
        Some(SubagentKind::Task) => depth < MAX_DEPTH,
        Some(SubagentKind::Scout | SubagentKind::Reviewer) => false,
    };
    if may_delegate {
        specs.push(task_spec(plan_mode));
    }
    if kind.is_some() {
        specs.push(yield_spec());
    } else if plan_mode {
        specs.push(propose_plan_spec());
    }
    Toolset {
        specs,
        lsp_read_only: read_only_lsp,
    }
}

pub fn task_spec(scouts_only: bool) -> ToolSpec {
    let agents: &[&str] = if scouts_only {
        &["scout"]
    } else {
        &["scout", "task", "reviewer"]
    };
    ToolSpec {
        name: TOOL_TASK.into(),
        description: prompts::TASK_TOOL.into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "context": {"type": "string", "description": "Shared background for every task"},
                "tasks": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string"},
                            "agent": {"type": "string", "enum": agents},
                            "task": {"type": "string"}
                        },
                        "required": ["task"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["tasks"],
            "additionalProperties": false
        }),
    }
}

pub fn find_spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_FIND.into(),
        description: prompts::FIND_TOOL.into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "path": {"type": "string"}
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    }
}

pub fn propose_plan_spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_PROPOSE_PLAN.into(),
        description: prompts::PROPOSE_PLAN_TOOL.into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "plan": {"type": "string"}
            },
            "required": ["title", "plan"],
            "additionalProperties": false
        }),
    }
}

pub fn yield_spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_YIELD.into(),
        description: prompts::YIELD_TOOL.into(),
        parameters: json!({
            "type": "object",
            "properties": {"result": {"type": "string"}},
            "required": ["result"],
            "additionalProperties": false
        }),
    }
}
