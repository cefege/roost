//! Ported from oh-my-pi packages/coding-agent/src/prompts/ (MIT), adapted to Roost's tools.
//! The embedded prompt templates and their `{{var}}` rendering, plus the
//! system prompt assembled from them for a conversation and its tool set.

use crate::records::ConversationRecord;

pub const SYSTEM: &str = include_str!("prompts/system.md");
pub const PLAN_MODE: &str = include_str!("prompts/plan_mode.md");
pub const PLAN_APPROVED: &str = include_str!("prompts/plan_approved.md");
pub const PROPOSE_PLAN_TOOL: &str = include_str!("prompts/propose_plan_tool.md");
pub const SUBAGENT: &str = include_str!("prompts/subagent.md");
pub const AGENT_SCOUT: &str = include_str!("prompts/agent_scout.md");
pub const AGENT_TASK: &str = include_str!("prompts/agent_task.md");
pub const AGENT_REVIEWER: &str = include_str!("prompts/agent_reviewer.md");
pub const TASK_TOOL: &str = include_str!("prompts/task_tool.md");
pub const YIELD_TOOL: &str = include_str!("prompts/yield_tool.md");
pub const YIELD_REMINDER: &str = include_str!("prompts/yield_reminder.md");
pub const UNEXPECTED_STOP: &str = include_str!("prompts/unexpected_stop.md");
pub const COMPACTION: &str = include_str!("prompts/compaction.md");
pub const FIND_TOOL: &str = include_str!("prompts/find_tool.md");

/// Replaces each `{{name}}` with its value. Unknown placeholders stay as-is
/// so a missing variable is visible in the rendered prompt.
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut rendered = template.to_owned();
    for (name, value) in vars {
        rendered = rendered.replace(&format!("{{{{{name}}}}}"), value);
    }
    rendered
}

/// What a subagent's system prompt adds over the primary one.
#[derive(Debug, Clone, Copy)]
pub struct SubagentPrompt<'a> {
    pub agent_prompt: &'a str,
    pub context: &'a str,
}

/// The system prompt blocks for one model call.
pub fn system_blocks(
    record: &ConversationRecord,
    tool_names: &[String],
    context_files: &str,
    subagent: Option<SubagentPrompt<'_>>,
) -> Vec<String> {
    let has = |name: &str| tool_names.iter().any(|tool| tool == name);
    let mut policy = Vec::new();
    if has("read") {
        policy.push("- File/directory reads: `read` (directory lists entries).");
    }
    if has("edit") {
        policy.push("- Surgical edits: `edit`, anchored on the `[PATH#TAG]` header and `N:` line numbers of your latest `read`/`grep`.");
    }
    if has("write") {
        policy.push("- Create/overwrite: `write`.");
    }
    if has("lsp") {
        policy.push("- Language server available: MUST use `lsp` for definitions, references, hover, symbols, diagnostics, rename. NEVER text-search for code intelligence.");
    }
    if has("find") {
        policy.push("- Unknown behavior/location: descriptive `find` FIRST; NEVER guess `grep`/`glob` targets.");
    }
    if has("grep") {
        policy.push("- Regex/literal search: `grep`, NEVER shell `grep`/`rg`/`awk`.");
    }
    if has("glob") {
        policy.push("- File structure/names: `glob`, NEVER `ls **/*.ext`/`fd`.");
    }
    if has("bash") {
        policy.push("- `bash`: real binaries/short fact pipelines (builds, tests, git, counts), NEVER specialized-tool work.");
    }
    let mut tool_policy = policy.join("\n");
    if has("edit") {
        tool_policy.push_str("\n<critical>\nNEVER use `sed`|`perl`|`python` via `bash` to issue individual edits; MUST use `edit`.\n</critical>");
    }
    let delegation = if has("task") {
        "\n# Delegation\n- Map unknown code via `task` with `scout` agents, not reading file after file yourself.\n- Fan genuine independent slices out in one `task` call; supply full requirements, subagents lack this conversation.\n- User says `parallel` → MUST use `task` subagents.\n"
    } else {
        ""
    };
    let mut blocks = vec![render(
        SYSTEM,
        &[
            ("worker_label", &record.worker_label),
            ("worker_os", &record.worker_os),
            ("cwd", &record.cwd),
            ("tool_policy", &tool_policy),
            ("delegation", delegation),
        ],
    )];
    if let Some(subagent) = subagent {
        blocks.push(render(
            SUBAGENT,
            &[
                ("agent_prompt", subagent.agent_prompt),
                ("context", subagent.context),
            ],
        ));
    }
    if record.mode == crate::records::Mode::Plan {
        blocks.push(PLAN_MODE.to_owned());
    }
    if !context_files.trim().is_empty() {
        blocks.push(format!(
            "<context_files>\n{context_files}\n</context_files>"
        ));
    }
    blocks
}
