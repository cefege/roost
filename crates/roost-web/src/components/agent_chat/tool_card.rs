//! One tool call as OMP's transcript shows it: a head line with what ran and
//! its state in OMP's status colours, then the output itself in its terminal
//! colours (`output_lines`), folded to the tool's preview window
//! (`tool_view::preview_window`) and opened by the transcript-wide toggle or
//! the card's own "Show all". Arguments and executor facts stay one click
//! away. All host-provided content is rendered as text, never trusted markup.

use dioxus::prelude::*;

use super::agent_alert::{AgentAlert, AlertTone};
use super::markdown::TERMINAL_GROUND_STYLE;
use super::output_lines::{reads_as_diff, styled_lines, window};
use super::tool_view::{
    ERROR_ROWS, PreviewEdge, argument_summary, detail_facts, presentation, preview_window,
};
use crate::components::md::{
    Button, ButtonSize, ButtonVariant, Icon, IconSize, List, ListRow, Surface, SurfaceRadius,
};
use crate::pump::use_store;
use crate::router_state::use_navigate;

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
    let mut show_all = use_signal(|| false);
    let target = argument_summary(&tool_name, &args_json);
    let lines = styled_lines(&output, reads_as_diff(&tool_name, &output));
    let details = serde_json::from_str::<serde_json::Value>(&details_json).ok();
    let facts = detail_facts(details.as_ref());
    let flag = |name: &str| {
        details
            .as_ref()
            .is_some_and(|value| value.get(name).and_then(serde_json::Value::as_bool) == Some(true))
    };
    let timed_out = flag("timed_out");
    let cancelled = flag("cancelled");
    let outcome_unknown = flag("outcome_unknown");
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
        _ => "",
    };
    let (icon, verb) = presentation(&tool_name, queued, running, cancelled, timed_out);
    let open = expanded || show_all();
    let (rows, edge) = if failed {
        (ERROR_ROWS, PreviewEdge::Tail)
    } else {
        preview_window(&tool_name)
    };
    let (shown, hidden) = window(&lines, rows, edge == PreviewEdge::Head, open);
    let shown = if lines.len() == 1 && lines.first().is_some_and(|line| line.runs.is_empty()) {
        &[][..]
    } else {
        shown
    };
    let command = argument_summary("bash", &args_json);
    let copy_command = tool_name == "bash" && !command.is_empty();
    let has_details = !facts.is_empty()
        || (!details_json.is_empty() && details_json != "{}")
        || (!args_json.is_empty() && args_json != "{}")
        || copy_command
        || !output.is_empty();
    rsx! {
        div { class: "agent-chat__step", "data-state": state, "data-tool": tool_name.clone(),
            div { class: "agent-chat__step-head",
                span { class: "agent-chat__step-icon", Icon { name: icon, size: IconSize::Sm } }
                span { class: "agent-chat__step-verb", "{verb}" }
                if !target.is_empty() { code { class: "agent-chat__step-target", title: target.clone(), "{target}" } }
                span { class: "agent-chat__step-state",
                    if running && !queued {
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
                    if !state_label.is_empty() { span { "{state_label}" } }
                }
            }
            if !shown.is_empty() {
                Surface { radius: SurfaceRadius::Md, style: TERMINAL_GROUND_STYLE.to_string(),
                    class: "agent-chat__step-output-ground",
                    if hidden > 0 && edge == PreviewEdge::Tail {
                        Button { variant: ButtonVariant::Link, size: ButtonSize::Sm,
                            class: "agent-chat__step-more",
                            onclick: move |_| show_all.set(true),
                            "… {hidden} earlier lines"
                        }
                    }
                    pre { class: "agent-chat__step-output", "data-error": failed.then_some("true"),
                        for line in shown.iter() {
                            div { class: "agent-chat__out-line", "data-tone": line.tone.attribute(),
                                for run in line.runs.iter() {
                                    span { class: run.style.classes(), "{run.text}" }
                                }
                            }
                        }
                    }
                    if hidden > 0 && edge == PreviewEdge::Head {
                        Button { variant: ButtonVariant::Link, size: ButtonSize::Sm,
                            class: "agent-chat__step-more",
                            onclick: move |_| show_all.set(true),
                            "… {hidden} more lines"
                        }
                    }
                }
            } else if failed {
                p { class: "agent-chat__step-empty", "Tool execution failed." }
            }
            if show_all() && !expanded {
                Button { variant: ButtonVariant::Link, size: ButtonSize::Sm, class: "agent-chat__step-more",
                    onclick: move |_| show_all.set(false),
                    "Fold output"
                }
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
                ChildLinks { child_ids }
            }
            if has_details {
                details { class: "agent-chat__step-detail-disclosure",
                    summary { "Details" }
                    div { class: "agent-chat__step-body",
                        if !facts.is_empty() {
                            ul { class: "agent-chat__step-facts", for fact in facts { li { "{fact}" } } }
                        }
                        if !args_json.is_empty() && args_json != "{}" {
                            pre { class: "agent-chat__step-pre", "{args_json}" }
                        }
                        if !details_json.is_empty() && details_json != "{}" {
                            pre { class: "agent-chat__step-pre", "{details_json}" }
                        }
                        div { class: "agent-chat__step-actions",
                            if copy_command {
                                Button { variant: ButtonVariant::Link, onclick: { let command = command.clone(); move |_: MouseEvent| { crate::components::notifications::clipboard::copy_text(&command); } }, "Copy command" }
                            }
                            if !output.is_empty() {
                                Button { variant: ButtonVariant::Link, onclick: { let text = output.clone(); move |_: MouseEvent| { crate::components::notifications::clipboard::copy_text(&text); } }, "Copy output" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The conversations a `task` call started, with their live state. Only this
/// part reads the store, so ordinary tool cards do not re-render on every
/// streamed delta.
#[component]
fn ChildLinks(child_ids: Vec<String>) -> Element {
    let pump = use_store();
    let _revision = pump.revision()();
    let navigate = use_navigate();
    let links: Vec<(String, String, String, String)> = {
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
    if links.is_empty() {
        return rsx! {};
    }
    rsx! {
        nav { class: "agent-chat__task-children", aria_label: "Subagent conversations",
            List {
                for (child_id, agent, title, status) in links {
                    ListRow {
                        key: "{child_id}",
                        dense: true,
                        class: "agent-chat__task-child",
                        href: crate::routes::agent_href(&child_id),
                        on_navigate: navigate,
                        headline: rsx! { "{agent}: {title}" },
                        support: rsx! { "{status}" },
                    }
                }
            }
        }
    }
}
