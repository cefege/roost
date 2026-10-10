//! One tool call as a step in the agent's timeline: a verb ("Ran", "Read",
//! "Edited"), what it acted on, and its state, expanding to the output. Output
//! is text in a `pre`, never markup. Rendered by `agent_chat::transcript`
//! inside a step group.

use dioxus::prelude::*;

use crate::components::md::{Button, ButtonVariant, Icon, IconSize};
use crate::pump::use_store;
use crate::router_state::use_navigate;

/// Lines shown before "Show all" on a long output.
const COLLAPSED_OUTPUT_LINES: usize = 20;

#[component]
pub fn ToolCard(
    tool_name: String,
    args_json: String,
    output: String,
    is_error: bool,
    running: bool,
    #[props(default)] child_ids: Vec<String>,
) -> Element {
    let mut show_all = use_signal(|| false);
    let pump = use_store();
    let _revision = pump.revision()();
    let navigate = use_navigate();
    let child_links: Vec<(String, String, String)> = {
        let core = pump.core();
        let core = core.borrow();
        child_ids
            .iter()
            .filter_map(|child_id| {
                core.store()
                    .agent_chat
                    .conversations
                    .get(child_id)
                    .map(|child| {
                        (
                            child_id.clone(),
                            child
                                .agent
                                .clone()
                                .unwrap_or_else(|| "Subagent".to_string()),
                            child.title.clone(),
                        )
                    })
            })
            .collect()
    };
    let target = argument_summary(&tool_name, &args_json);
    let lines: Vec<&str> = output.lines().collect();
    let hidden_lines = lines.len().saturating_sub(COLLAPSED_OUTPUT_LINES);
    let collapsed = !show_all() && hidden_lines > 0;
    let visible = if collapsed {
        lines[hidden_lines..].join("\n")
    } else {
        output.clone()
    };
    let (icon, verb) = presentation(&tool_name, running);
    let state = if running {
        "running"
    } else if is_error {
        "error"
    } else {
        "done"
    };
    rsx! {
        details { class: "agent-chat__step", "data-state": state,
            summary { class: "agent-chat__step-summary",
                span { class: "agent-chat__step-icon", Icon { name: icon, size: IconSize::Sm } }
                span { class: "agent-chat__step-verb", "{verb}" }
                if !target.is_empty() {
                    code { class: "agent-chat__step-target", title: target.clone(), "{target}" }
                }
                span { class: "agent-chat__step-state",
                    if running {
                        span { class: "agent-chat__spinner", role: "status", "aria-label": "Running" }
                    } else if is_error {
                        Icon { name: "error", size: IconSize::Sm }
                    }
                }
                span { class: "agent-chat__step-chevron", Icon { name: "expand_more", size: IconSize::Sm } }
            }
            div { class: "agent-chat__step-body",
                if output.is_empty() {
                    p { class: "agent-chat__step-empty", if running { "Waiting for output…" } else { "No output." } }
                } else {
                    if collapsed {
                        Button {
                            variant: ButtonVariant::Link,
                            class: "agent-chat__step-more",
                            onclick: move |_| show_all.set(true),
                            "Show {hidden_lines} earlier lines"
                        }
                    }
                    pre { class: "agent-chat__step-output", "{visible}" }
                }
            }
                if !child_links.is_empty() {
                    nav { class: "agent-chat__task-children", aria_label: "Subagent conversations",
                        for (child_id, agent, title) in child_links {
                            a {
                                key: "{child_id}",
                                href: crate::routes::agent_href(&child_id),
                                onclick: move |event| {
                                    if event.modifiers().is_empty() {
                                        event.prevent_default();
                                        navigate.call(crate::routes::agent_href(&child_id));
                                    }
                                },
                                "{agent}: {title}"
                            }
                        }
                    }
                }
        }
    }
}

/// The step's icon and verb: what the tool did, in the tense of its state.
fn presentation(tool_name: &str, running: bool) -> (&'static str, String) {
    let (icon, active, finished) = match tool_name {
        "bash" => ("terminal", "Running", "Ran"),
        "read" => ("description", "Reading", "Read"),
        "edit" => ("edit", "Editing", "Edited"),
        "write" => ("edit_document", "Writing", "Wrote"),
        "grep" | "find" | "glob" | "lsp" => ("search", "Searching", "Searched"),
        "task" => ("account_tree", "Starting", "Started"),
        "propose_plan" => ("assignment", "Drafting", "Drafted"),
        _ => ("build", "", ""),
    };
    if active.is_empty() {
        return (icon, tool_name.to_owned());
    }
    (icon, if running { active } else { finished }.to_owned())
}

fn argument_summary(tool_name: &str, args_json: &str) -> String {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(args_json) else {
        return args_json.chars().take(80).collect();
    };
    if tool_name == "task" {
        let count = parsed
            .get("tasks")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        return format!("{count} task{}", if count == 1 { "" } else { "s" });
    }
    let value = match tool_name {
        "bash" => parsed.get("command"),
        "read" | "write" | "edit" | "ls" => parsed.get("path"),
        "grep" | "glob" => parsed.get("pattern"),
        "find" => parsed.get("query").or_else(|| parsed.get("pattern")),
        "lsp" => {
            let action = parsed
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let file = parsed
                .get("file")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            return [action, file]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
        }
        "propose_plan" => parsed.get("title"),
        _ => None,
    };
    value
        .and_then(serde_json::Value::as_str)
        .map(|value| value.lines().next().unwrap_or_default().to_owned())
        .unwrap_or_else(|| compact_arguments(&parsed))
}

/// An unknown tool's arguments on one line: its string fields, comma-joined.
fn compact_arguments(arguments: &serde_json::Value) -> String {
    let Some(object) = arguments.as_object() else {
        return String::new();
    };
    object
        .values()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>()
        .join(", ")
        .chars()
        .take(80)
        .collect()
}
