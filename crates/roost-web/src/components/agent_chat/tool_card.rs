//! One tool call as a compact step: intent, target and truthful lifecycle.
//! Arguments, output and structured executor facts remain reachable on demand;
//! all host-provided content is rendered as text, never trusted markup.

use std::borrow::Cow;

use dioxus::prelude::*;

use crate::components::md::{Button, ButtonVariant, Icon, IconSize};
use crate::pump::use_store;
use crate::router_state::use_navigate;

const PREVIEW_LINES: usize = 10;
const PREVIEW_CHARS: usize = 2_000;

#[component]
pub fn ToolCard(
    tool_name: String,
    args_json: String,
    output: String,
    is_error: bool,
    running: bool,
    queued: bool,
    details_json: String,
    #[props(default)] child_ids: Vec<String>,
) -> Element {
    let pump = use_store();
    let _revision = pump.revision()();
    let navigate = use_navigate();
    let child_links: Vec<(String, String, String, String)> = {
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
                            child.run_state.as_str().to_owned(),
                        )
                    })
            })
            .collect()
    };
    let target = argument_summary(&tool_name, &args_json);
    let visible_output = strip_ansi(&output);
    let recent = visible_output.trim_end_matches('\n');
    let line_start = recent
        .rmatch_indices('\n')
        .nth(PREVIEW_LINES - 1)
        .map_or(0, |(offset, _)| offset + 1);
    let recent = &recent[line_start..];
    let character_start = recent
        .char_indices()
        .rev()
        .nth(PREVIEW_CHARS - 1)
        .map_or(0, |(offset, _)| offset);
    let preview = &recent[character_start..];
    let preview_trimmed = line_start > 0 || character_start > 0;
    let details = serde_json::from_str::<serde_json::Value>(&details_json).ok();
    let facts = detail_facts(details.as_ref());
    let timed_out = details.as_ref().is_some_and(|value| {
        value.get("timed_out").and_then(serde_json::Value::as_bool) == Some(true)
    });
    let cancelled = details.as_ref().is_some_and(|value| {
        value.get("cancelled").and_then(serde_json::Value::as_bool) == Some(true)
    });
    let outcome_unknown = details.as_ref().is_some_and(|value| {
        value
            .get("outcome_unknown")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    });
    let failed = is_error && !cancelled && !timed_out;
    let state = if queued {
        "waiting"
    } else if running {
        "running"
    } else if cancelled {
        "cancelled"
    } else if timed_out {
        "timeout"
    } else if failed {
        "error"
    } else {
        "done"
    };
    let state_label = match state {
        "waiting" => "Waiting",
        "running" => "Running",
        "cancelled" => "Cancelled",
        "timeout" => "Timed out",
        "error" => "Error",
        _ => "Done",
    };
    let (icon, verb) = presentation(&tool_name, queued, running, cancelled, timed_out);
    let has_output = !visible_output.is_empty();
    let command = argument_summary("bash", &args_json);
    let copy_command = tool_name == "bash" && !command.is_empty();
    let error_preview = if visible_output.is_empty() {
        "Tool execution failed."
    } else {
        preview
    };
    rsx! {
        div { class: "agent-chat__step", "data-state": state,
            details { class: "agent-chat__step-disclosure",
            summary { class: "agent-chat__step-summary",
                span { class: "agent-chat__step-icon", Icon { name: icon, size: IconSize::Sm } }
                span { class: "agent-chat__step-verb", "{verb}" }
                if !target.is_empty() { code { class: "agent-chat__step-target", title: target.clone(), "{target}" } }
                span { class: "agent-chat__step-state", if running && !queued {
                    span { class: "agent-chat__spinner", role: "status", "aria-label": "Running" }
                } else if failed {
                    Icon { name: "error", size: IconSize::Sm }
                } else if cancelled {
                    Icon { name: "cancel", size: IconSize::Sm }
                } else if timed_out {
                    Icon { name: "warning", size: IconSize::Sm }
                } else if queued {
                    Icon { name: "schedule", size: IconSize::Sm }
                }
                    span { "{state_label}" }
                }
                span { class: "agent-chat__step-chevron", Icon { name: "expand_more", size: IconSize::Sm } }
            }
            div { class: "agent-chat__step-body",
                if queued { p { class: "agent-chat__step-empty", "Waiting for an executor to start this call." } }
                if !facts.is_empty() {
                    ul { class: "agent-chat__step-facts", for fact in facts { li { "{fact}" } } }
                }
                if !details_json.is_empty() && details_json != "{}" {
                    details { class: "agent-chat__step-detail-disclosure",
                        summary { "Execution details" }
                        pre { class: "agent-chat__step-metadata", "{details_json}" }
                    }
                }
                if !args_json.is_empty() && args_json != "{}" {
                    details { class: "agent-chat__step-detail-disclosure",
                        summary { "Arguments" }
                        pre { class: "agent-chat__step-metadata", "{args_json}" }
                    }
                }
                if !running && has_output {
                    pre { class: "agent-chat__step-output", "{visible_output}" }
                } else if !has_output && !queued {
                    p { class: "agent-chat__step-empty", if running { "Waiting for output…" } else { "No output was returned." } }
                }
                if copy_command || !output.is_empty() {
                    div { class: "agent-chat__step-actions",
                        if copy_command {
                            Button { variant: ButtonVariant::Link, onclick: { let command = command.clone(); move |_: MouseEvent| { crate::components::notifications::clipboard::copy_text(&command); } }, "Copy command" }
                        }
                        if !output.is_empty() {
                            Button { variant: ButtonVariant::Link, onclick: { let text = output.clone(); move |_: MouseEvent| { crate::components::notifications::clipboard::copy_text(&text); } }, "Copy output" }
                        }
                    }
                }
                if !child_links.is_empty() {
                    nav { class: "agent-chat__task-children", aria_label: "Subagent conversations",
                        for (child_id, agent, title, status) in child_links {
                            a {
                                key: "{child_id}",
                                href: crate::routes::agent_href(&child_id),
                                onclick: move |event| {
                                    if event.modifiers().is_empty() {
                                        event.prevent_default();
                                        navigate.call(crate::routes::agent_href(&child_id));
                                    }
                                },
                                "{agent}: {title} — {status}"
                            }
                        }
                    }
                }
            }
        }
            if running && has_output {
                div { class: "agent-chat__step-live-preview", aria_label: "Recent live tool output",
                    pre { class: "agent-chat__step-output", "{preview}" }
                    if preview_trimmed { span { class: "agent-chat__step-fact", "Latest output preview · up to {PREVIEW_LINES} lines / {PREVIEW_CHARS} characters." } }
                }
            }
            if timed_out {
                p { class: "agent-chat__step-warning", role: "status", "Execution timed out." }
            }
            if failed {
                p { class: "agent-chat__step-error", role: "alert", "{error_preview}" }
                if preview_trimmed {
                    span { class: "agent-chat__step-fact", "Latest error preview · up to {PREVIEW_LINES} lines / {PREVIEW_CHARS} characters. Open details for the full transported output." }
                }
            }
            if outcome_unknown {
                p { class: "agent-chat__step-warning", role: "status", "Final outcome unknown. Inspect current state before repeating the call." }
            }
        }
    }
}
fn presentation(
    tool_name: &str,
    queued: bool,
    running: bool,
    cancelled: bool,
    timed_out: bool,
) -> (&'static str, String) {
    let (icon, active, finished) = match tool_name {
        "bash" => ("terminal", "Running", "Ran"),
        "read" => ("description", "Reading", "Read"),
        "edit" => ("edit", "Editing", "Edited"),
        "write" => ("edit_document", "Writing", "Wrote"),
        "grep" | "find" | "glob" | "lsp" => ("search", "Searching", "Searched"),
        "task" => ("account_tree", "Starting", "Started"),
        "propose_plan" => ("assignment", "Drafting", "Drafted"),
        _ => ("build", tool_name, tool_name),
    };
    (
        icon,
        if cancelled {
            "Cancelled".into()
        } else if timed_out {
            "Timed out".into()
        } else if queued {
            "Waiting".into()
        } else if running {
            active.into()
        } else {
            finished.into()
        },
    )
}

fn argument_summary(tool_name: &str, args_json: &str) -> String {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(args_json) else {
        return String::new();
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
            return [parsed.get("action"), parsed.get("file")]
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
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

fn compact_arguments(arguments: &serde_json::Value) -> String {
    arguments
        .as_object()
        .map(|object| {
            object
                .values()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
                .chars()
                .take(100)
                .collect()
        })
        .unwrap_or_default()
}

fn detail_facts(details: Option<&serde_json::Value>) -> Vec<String> {
    let Some(details) = details.and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    let mut facts = Vec::new();
    if let Some(code) = details.get("exit_code").and_then(serde_json::Value::as_i64) {
        facts.push(format!("Exit code {code}"));
    }
    if let Some(ms) = details
        .get("duration_ms")
        .and_then(serde_json::Value::as_u64)
    {
        facts.push(format!("Execution time: {ms} ms"));
    }
    if details
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        facts.push("Output was truncated; some output was not transported.".into());
    }
    if let Some(path) = details
        .get("artifact_path")
        .and_then(serde_json::Value::as_str)
    {
        let complete = details
            .get("artifact_complete")
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        facts.push(if complete {
            format!("Complete output artifact: {path}")
        } else {
            format!("Earlier output artifact: {path} (incomplete; full output was not transported)")
        });
    }
    facts
}

/// Remove terminal control sequences without interpreting their styling.
fn strip_ansi(text: &str) -> Cow<'_, str> {
    if !text.contains('\u{1b}') {
        return Cow::Borrowed(text);
    }
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            clean.push(ch);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            Some(_) | None => {}
        }
    }
    Cow::Owned(clean)
}
