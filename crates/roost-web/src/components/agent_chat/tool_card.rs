//! One tool call as OMP's transcript draws its box: a head with the tool's
//! neutral icon, verb, target and diff stats, a status indicator for the call's
//! state, the intent the model declared, then the body the tool calls for. The
//! bodies live in their own modules; this file dispatches and owns the details
//! disclosure. All host-provided content is rendered as text, never as markup.

use dioxus::prelude::*;

use super::agent_alert::{AgentAlert, AlertTone};
use super::bash_box::BashBody;
use super::diff_model::{diff_stats, parse_unified, unexplained_output};
use super::diff_view::DiffBody;
use super::output_lines::reads_as_diff;
use super::output_window::OutputWindow;
use super::task_rows::TaskRows;
use super::tool_view::{
    ERROR_ROWS, PreviewEdge, argument_summary, detail_facts, intent_for, presentation,
    preview_window, state_label, status_name, tool_state,
};
use super::write_view::WriteBody;
use crate::components::ui::{Button, ButtonVariant, Icon, Spinner, StatusDot};

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
    /// The transcript-wide "expand tool output" switch.
    #[props(default)]
    expanded: bool,
) -> Element {
    let mut body_open = use_signal(|| false);
    let mut agents_open = use_signal(|| false);
    let details = serde_json::from_str::<serde_json::Value>(&details_json).ok();
    let flag = |name: &str| {
        details
            .as_ref()
            .and_then(|value| value.get(name))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    let timed_out = flag("timed_out");
    let cancelled = flag("cancelled");
    let outcome_unknown = flag("outcome_unknown");
    let failed = is_error && !cancelled && !timed_out;
    let background = details
        .as_ref()
        .and_then(|value| value.pointer("/async/state"))
        .and_then(serde_json::Value::as_str);
    let state = tool_state(queued, running, cancelled, timed_out, failed, background);

    let open = expanded || body_open();
    let agents_expanded = expanded || agents_open();
    let (icon, verb) = presentation(&tool_name, running);
    let target = if tool_name == "bash" {
        String::new()
    } else {
        argument_summary(&tool_name, &args_json)
    };
    let intent = intent_for(&args_json);
    let facts = detail_facts(details.as_ref());
    let command = string_field(&args_json, "command");
    let content = string_field(&args_json, "content");
    let diff_text = string_field(&details_json, "diff");
    let diff_stats = diff_stats(&parse_unified(&diff_text));
    let notes = unexplained_output(&output, &diff_text);
    let (preview_rows, preview_edge) = if failed {
        (ERROR_ROWS, PreviewEdge::Tail)
    } else {
        preview_window(&tool_name)
    };
    let from_head = preview_edge == PreviewEdge::Head;
    let on_body_expand = EventHandler::new(move |value: bool| body_open.set(value));
    let on_agents_expand = EventHandler::new(move |value: bool| agents_open.set(value));

    let body = if tool_name == "bash" {
        rsx! {
            BashBody {
                command: command.clone(),
                output: output.clone(),
                details_json: details_json.clone(),
                args_json: args_json.clone(),
                is_error: failed,
                open,
                on_expand: on_body_expand,
            }
        }
    } else if tool_name == "edit" && !diff_text.is_empty() {
        rsx! { DiffBody { diff: diff_text.clone(), open, on_expand: on_body_expand } }
    } else if tool_name == "write" && !failed && !content.is_empty() {
        rsx! { WriteBody { content: content.clone(), open, on_expand: on_body_expand } }
    } else if failed && output.is_empty() {
        rsx! { p { class: "m-0 text-xs text-muted-foreground", "Tool execution failed." } }
    } else if output.is_empty() {
        rsx! {}
    } else {
        rsx! {
            OutputWindow {
                output: output.clone(),
                diff: reads_as_diff(&tool_name, &output),
                rows: preview_rows,
                from_head,
                open,
                failed,
                on_expand: on_body_expand,
            }
        }
    };

    let copy_command = tool_name == "bash" && !command.is_empty();
    let has_details = !facts.is_empty()
        || (!details_json.is_empty() && details_json != "{}")
        || (!args_json.is_empty() && args_json != "{}")
        || copy_command
        || !output.is_empty();
    let state_text = state_label(state);

    let command_text = command.clone();
    let output_text = output.clone();
    rsx! {
        div { class: "flex min-w-0 flex-col gap-1 rounded-md border border-border p-2 data-[state=error]:border-destructive/40", "data-state": state, "data-tool": tool_name.clone(),
            div { class: "flex min-w-0 items-center gap-2 text-sm text-muted-foreground",
                span { class: "inline-flex flex-none items-center gap-1", Icon { icon, class: "size-4" } }
                span { class: "flex-none font-medium text-foreground", "{verb}" }
                if !target.is_empty() {
                    code { class: target_class(&tool_name), title: target.clone(), "{target}" }
                }
                if diff_stats.added > 0 || diff_stats.removed > 0 {
                    span { class: "flex-none font-mono text-xs text-muted-foreground",
                        "["
                        span { class: "text-[var(--status-ok)]", "+{diff_stats.added}" }
                        "/"
                        span { class: "text-[var(--status-err)]", "−{diff_stats.removed}" }
                        "]"
                    }
                }
                span { class: state_text_class(state),
                    if state == "running" {
                        span { class: "inline-flex", role: "status", "aria-label": "Running", Spinner { class: "size-3" } }
                    } else {
                        StatusDot { status: status_name(state).to_string(), title: Some(state_text.to_string()) }
                        span { "{state_text}" }
                    }
                }
            }
            if let Some(text) = intent {
                p { class: "m-0 break-words text-xs italic text-muted-foreground", "{text}" }
            }
            {body}
            if tool_name == "edit" && !diff_text.is_empty() && !notes.is_empty() {
                pre { class: "m-0 whitespace-pre-wrap break-words py-1 font-mono text-xs text-muted-foreground", "{notes}" }
            }
            if timed_out {
                AgentAlert { tone: AlertTone::Warning, p { "Execution timed out." } }
            }
            if outcome_unknown {
                AgentAlert { tone: AlertTone::Warning,
                    p { "Final outcome unknown. Inspect current state before repeating the call." }
                }
            }
            if !child_ids.is_empty() {
                TaskRows { child_ids: child_ids.clone(), open: agents_expanded, on_expand: on_agents_expand }
            }
            if has_details {
                details { class: "group",
                    summary { class: "cursor-pointer py-1 text-xs text-muted-foreground", "Details" }
                    div { class: "flex flex-col gap-2 pb-2",
                        if !facts.is_empty() {
                            ul { class: "m-0 flex list-none flex-wrap gap-x-4 gap-y-2 p-0 text-xs text-muted-foreground", for fact in facts { li { "{fact}" } } }
                        }
                        if !args_json.is_empty() && args_json != "{}" {
                            pre { class: "m-0 whitespace-pre-wrap break-words font-mono text-xs", "{args_json}" }
                        }
                        if !details_json.is_empty() && details_json != "{}" {
                            pre { class: "m-0 whitespace-pre-wrap break-words font-mono text-xs", "{details_json}" }
                        }
                        div { class: "flex flex-wrap gap-2",
                            if copy_command {
                                Button { variant: ButtonVariant::Link, onclick: move |_| { crate::components::notifications::clipboard::copy_text(&command_text); }, "Copy command" }
                            }
                            if !output.is_empty() {
                                Button { variant: ButtonVariant::Link, onclick: move |_| { crate::components::notifications::clipboard::copy_text(&output_text); }, "Copy output" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A string field of a JSON object, or empty when the JSON or the field is absent.
fn string_field(json: &str, key: &str) -> String {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|value| {
            value
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

/// The target's ink: a search pattern takes the accent, a path the full foreground.
fn target_class(tool_name: &str) -> &'static str {
    match tool_name {
        "grep" | "glob" | "find" => "min-w-0 flex-1 truncate font-mono text-xs text-primary",
        _ => "min-w-0 flex-1 truncate font-mono text-xs text-foreground",
    }
}

/// The state word's ink: errors and timeouts draw in their status colour.
fn state_text_class(state: &str) -> &'static str {
    match state {
        "error" => "ml-auto inline-flex flex-none items-center gap-1 text-xs text-destructive",
        "timeout" => "ml-auto inline-flex flex-none items-center gap-1 text-xs text-[var(--status-warn)]",
        _ => "ml-auto inline-flex flex-none items-center gap-1 text-xs text-muted-foreground",
    }
}
