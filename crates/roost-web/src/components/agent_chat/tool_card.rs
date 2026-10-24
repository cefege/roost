//! A compact tool disclosure with its execution state and argument summary.
//! Expanding reveals native output, diffs, child agents and executor details;
//! the transcript-wide expand action opens every disclosure. Tool content is
//! text, never trusted markup. Called by the semantic transcript renderer.

use dioxus::prelude::*;
use libero::{
    components::{Box, Button, Collapse, Flex, Indicator, Paper, Pictogram, Text},
    sx::sx,
};

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
    let mut disclosure_open = use_signal(|| false);
    let mut agents_open = use_signal(|| false);
    let mut details_open = use_signal(|| false);
    let disclosure_id = libero::hooks::use_id().read().clone();
    let details_id = libero::hooks::use_id().read().clone();
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
    let target = argument_summary(&tool_name, &args_json);
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

    let disclosure_visible = expanded || disclosure_open();
    let body = if !disclosure_visible {
        rsx! {}
    } else if tool_name == "bash" {
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
        rsx! { Text { component: "p", size: "xs", color: "muted", "Tool execution failed." } }
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
    let (copy_command_to_clipboard, command_copied) =
        crate::components::hooks::use_copy_clipboard::use_copy_clipboard(None);
    let (copy_output_to_clipboard, output_copied) =
        crate::components::hooks::use_copy_clipboard::use_copy_clipboard(None);
    let card_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .padding("sm")
            .gap("sm")
            .min_width("0")
    });
    let diff_stats_sx = use_hook(|| {
        sx().flex_shrink("0")
            .font_family("monospace")
            .font_size("xs")
            .color("muted")
    });
    let added_sx = use_hook(|| sx().color("success"));
    let removed_sx = use_hook(|| sx().color("error"));
    let meta_label_sx = use_hook(|| sx().font_size("xs").font_weight("500").color("muted"));
    let args_sx = use_hook(|| {
        sx().margin("0")
            .max_height("12rem")
            .overflow("auto")
            .with("overscroll-behavior-y", "auto")
            .white_space("pre-wrap")
            .with("overflow-wrap", "anywhere")
            .font_family("monospace")
            .font_size("xs")
    });
    let intent_sx = use_hook(|| {
        sx().margin("0")
            .with("overflow-wrap", "anywhere")
            .font_size("xs")
            .font_style("italic")
            .color("muted")
    });
    let notes_sx = use_hook(|| {
        sx().margin("0")
            .white_space("pre-wrap")
            .with("overflow-wrap", "anywhere")
            .padding_block("xs")
            .font_family("monospace")
            .font_size("xs")
            .color("muted")
    });
    let facts_sx = use_hook(|| {
        sx().display("flex")
            .flex_wrap("wrap")
            .gap("sm")
            .margin("0")
            .padding("0")
            .with("list-style", "none")
            .font_size("xs")
            .color("muted")
    });
    let details_sx = use_hook(|| {
        sx().margin("0")
            .white_space("pre-wrap")
            .with("overflow-wrap", "anywhere")
            .font_family("monospace")
            .font_size("xs")
    });
    let copy_actions_sx = use_hook(|| sx().display("flex").flex_wrap("wrap").gap("sm"));
    rsx! {
        Paper {
            sx: card_sx,
            "data-state": state,
            "data-tool": tool_name.clone(),
            Button {
                variant: "standard",
                color: "muted",
                size: "sm",
                aria_expanded: disclosure_visible,
                aria_controls: disclosure_id.clone(),
                onclick: move |_| disclosure_open.toggle(),
                Flex { direction: "row", align: "center", gap: "sm",
                    Pictogram { icon }
                    Text { size: "sm", "{verb}" }
                    if !target.is_empty() {
                        code { title: target.clone(), "{target}" }
                    }
                    if diff_stats.added > 0 || diff_stats.removed > 0 {
                        Box { component: "span", sx: diff_stats_sx,
                            "["
                            Box { component: "span", sx: added_sx, "+{diff_stats.added}" }
                            "/"
                            Box { component: "span", sx: removed_sx, "−{diff_stats.removed}" }
                            "]"
                        }
                    }
                    Flex { direction: "row", align: "center", gap: "xs",
                        if state == "running" {
                            Indicator { color: "info", processing: true, "aria-label": "Running" }
                        } else {
                            Indicator {
                                color: match status_name(state) {
                                    "ok" => "success",
                                    "running" => "info",
                                    "error" => "error",
                                    _ => "warning",
                                },
                                "aria-label": state_text,
                            }
                        }
                        Text { size: "xs", "{state_text}" }
                    }
                    Pictogram {
                        icon: if disclosure_visible { pictogram_icons_lucide::chevron_up::outlined } else { pictogram_icons_lucide::chevron_down::outlined },
                    }
                }
            }
            Collapse { id: disclosure_id, open: disclosure_visible,
                Flex { direction: "column", gap: "sm",
                    if !args_json.is_empty() && args_json != "{}" {
                        Text { component: "span", sx: meta_label_sx.clone(), "Input" }
                        Box { component: "pre", sx: args_sx, "{args_json}" }
                    }
                    if let Some(text) = intent {
                        Text { component: "p", sx: intent_sx, "{text}" }
                    }
                    Text { component: "span", sx: meta_label_sx.clone(), if running { "Live output" } else { "Result" } }
                    {body}
                    if tool_name == "edit" && !diff_text.is_empty() && !notes.is_empty() {
                        Box { component: "pre", sx: notes_sx, "{notes}" }
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
                        Button {
                            variant: "standard",
                            color: "muted",
                            size: "sm",
                            aria_expanded: details_open(),
                            aria_controls: details_id.clone(),
                            onclick: move |_| details_open.toggle(),
                            "Details"
                        }
                        Collapse { id: details_id, open: details_open(),
                            Flex { direction: "column", gap: "sm",
                                if !facts.is_empty() {
                                    Box { component: "ul", sx: facts_sx,
                                        for fact in facts { Box { component: "li", "{fact}" } }
                                    }
                                }
                                if !details_json.is_empty() && details_json != "{}" {
                                    Box { component: "pre", sx: details_sx, "{details_json}" }
                                }
                                Flex { direction: "row", sx: copy_actions_sx,
                                    if copy_command {
                                        Button { variant: "standard", color: "primary", onclick: move |_| copy_command_to_clipboard(&command_text), if command_copied() { "Copied" } else { "Copy command" } }
                                    }
                                    if !output.is_empty() {
                                        Button { variant: "standard", color: "primary", onclick: move |_| copy_output_to_clipboard(&output_text), if output_copied() { "Copied" } else { "Copy output" } }
                                    }
                                }
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
