//! Which tools a conversation offers the model: worker tools from
//! roost-protocol, harness tools the coordinator executes itself, and the
//! mode-specific filters for subagents.

use roost_llm::ToolSpec;
use roost_protocol::wire::agent_chat::{
    TOOL_BASH, TOOL_EDIT, TOOL_GLOB, TOOL_GREP, TOOL_LSP, TOOL_READ, TOOL_WRITE, worker_tool_specs,
};
use serde_json::json;

use crate::goal_mode::TOOL_GOAL;
use crate::prompts;
use crate::records::{ConversationRecord, Mode};

pub const TOOL_TASK: &str = "task";
pub const TOOL_FIND: &str = "find";
pub const TOOL_PROPOSE_PLAN: &str = "propose_plan";
pub const TOOL_YIELD: &str = "yield";
pub const TOOL_TODO: &str = "todo";
pub const TOOL_ASK: &str = "ask";
pub const TOOL_FETCH: &str = "fetch";
pub const TOOL_WEB_SEARCH: &str = "web_search";

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
        TOOL_READ | TOOL_GREP | TOOL_GLOB | TOOL_LSP | TOOL_FIND | TOOL_FETCH | TOOL_WEB_SEARCH
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
    // `edit` and `write` exist only in the non-read-only list, and no
    // read-only agent is offered them, so one list covers every kind.
    let mut specs: Vec<ToolSpec> = worker_tool_specs(read_only_lsp)
        .into_iter()
        .filter(|spec| worker_names.contains(&spec.name.as_str()))
        .map(|spec| ToolSpec {
            name: spec.name,
            description: spec.description,
            parameters: spec.parameters,
        })
        .collect();
    if find_available {
        specs.push(find_spec());
    }
    specs.push(crate::web_tools::fetch_spec());
    specs.push(crate::web_tools::web_search_spec());
    let may_delegate = match kind {
        None => true,
        Some(SubagentKind::Task) => depth < MAX_DEPTH,
        Some(SubagentKind::Scout | SubagentKind::Reviewer) => false,
    };
    if may_delegate {
        specs.push(task_spec(plan_mode));
    }
    if kind != Some(SubagentKind::Scout) {
        specs.push(todo_spec());
    }
    if kind.is_none() {
        specs.push(ask_spec());
    }
    if kind.is_some() {
        specs.push(yield_spec());
    } else if plan_mode {
        specs.push(propose_plan_spec());
    } else {
        specs.push(goal_spec());
    }
    let specs = keep_first_tool_names(specs);
    Toolset {
        specs,
        lsp_read_only: read_only_lsp,
    }
}

fn keep_first_tool_names(specs: Vec<ToolSpec>) -> Vec<ToolSpec> {
    let mut unique = Vec::with_capacity(specs.len());
    for spec in specs {
        if unique
            .iter()
            .any(|existing: &ToolSpec| existing.name == spec.name)
        {
            tracing::warn!(tool = %spec.name, "dropping duplicate agent tool name");
        } else {
            unique.push(spec);
        }
    }
    unique
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

pub fn todo_spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_TODO.into(),
        description: prompts::TODO_TOOL.into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "op": {"type": "string", "enum": ["init", "start", "done", "rm", "drop", "block", "unblock", "append", "view"]},
                "list": {"type": "array", "items": {"type": "object", "properties": {
                    "phase": {"type": "string"},
                    "items": {"type": "array", "items": {"type": "string"}, "minItems": 1}
                }, "required": ["phase", "items"], "additionalProperties": false}},
                "task": {"type": "string", "description": "Verbatim task content"},
                "phase": {"type": "string"},
                "items": {"type": "array", "items": {"type": "string"}},
                "reason": {"type": "string"}
            },
            "required": ["op"],
            "additionalProperties": false
        }),
    }
}

pub fn ask_spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_ASK.into(),
        description: prompts::ASK_TOOL.into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "questions": {"type": "array", "minItems": 1, "items": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "question": {"type": "string"},
                        "options": {"type": "array", "minItems": 2, "maxItems": 5, "items": {
                            "type": "object", "properties": {
                                "label": {"type": "string"},
                                "description": {"type": "string"}
                            }, "required": ["label"], "additionalProperties": false
                        }},
                        "multi": {"type": "boolean"},
                        "recommended": {"type": "integer", "minimum": 0}
                    },
                    "required": ["question", "options"],
                    "additionalProperties": false
                }}
            },
            "required": ["questions"],
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
pub fn goal_spec() -> ToolSpec {
    ToolSpec {
        name: TOOL_GOAL.into(),
        description: prompts::GOAL_TOOL.into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "op": {"type": "string", "enum": ["create", "get", "complete", "resume", "drop"]},
                "objective": {"type": "string"},
                "token_budget": {"type": "integer", "minimum": 1}
            },
            "required": ["op"],
            "additionalProperties": false
        }),
    }
}
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn merged_tool_specs_keep_first_definition_for_duplicate_names() {
        let specs = vec![
            ToolSpec {
                name: "read".into(),
                description: "built-in definition".into(),
                parameters: json!({"type": "object"}),
            },
            ToolSpec {
                name: "read".into(),
                description: "colliding definition".into(),
                parameters: json!({"type": "string"}),
            },
            ToolSpec {
                name: "mcp__relay_read".into(),
                description: "distinct relay tool".into(),
                parameters: json!({"type": "object"}),
            },
        ];
        let merged = keep_first_tool_names(specs);

        assert_eq!(
            merged
                .iter()
                .map(|spec| spec.name.as_str())
                .collect::<Vec<_>>(),
            ["read", "mcp__relay_read"]
        );
        assert_eq!(merged[0].description, "built-in definition");
    }
}
