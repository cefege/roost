//! One tool call as a step in the agent's timeline: a verb ("Ran", "Read",
//! "Edited"), what it acted on, and its state, expanding to the output. Output
//! is text in a `pre`, never markup. Rendered by `agent_chat::transcript`
//! inside a step group.

use dioxus::prelude::*;

use crate::components::md::{Button, ButtonVariant, Icon, IconSize};

/// Lines shown before "Show all" on a long output.
const COLLAPSED_OUTPUT_LINES: usize = 20;

#[component]
pub fn ToolCard(
    tool_name: String,
    args_json: String,
    output: String,
    is_error: bool,
    running: bool,
) -> Element {
    let mut show_all = use_signal(|| false);
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
        "grep" | "find" | "ls" | "glob" => ("search", "Searching", "Searched"),
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
    let key = match tool_name {
        "bash" => "command",
        "read" | "write" | "edit" | "ls" => "path",
        "grep" | "find" | "glob" => "pattern",
        _ => return compact_arguments(&parsed),
    };
    parsed
        .get(key)
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
