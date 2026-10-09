//! A live tool call and its bounded output view.
//! Output is rendered as text in a `pre`, never interpreted as markup.

use dioxus::prelude::*;

use crate::components::md::{Button, ButtonVariant, Card, CardVariant, ProgressBar};

#[component]
pub fn ToolCard(
    tool_name: String,
    args_json: String,
    output: String,
    is_error: bool,
    running: bool,
) -> Element {
    let mut show_all = use_signal(|| false);
    let summary = argument_summary(&tool_name, &args_json);
    let lines: Vec<&str> = output.lines().collect();
    let collapsed = !show_all() && lines.len() > 20;
    let visible = if collapsed {
        lines[lines.len() - 20..].join("\n")
    } else {
        output.clone()
    };
    let variant = if is_error {
        CardVariant::Outlined
    } else {
        CardVariant::Elevated
    };
    rsx! {
        Card { variant, class: Some(if is_error { "agent-chat__tool agent-chat__tool--error".to_string() } else { "agent-chat__tool".to_string() }),
            div { class: "agent-chat__tool-heading",
                strong { {tool_name} }
                if !summary.is_empty() { code { {summary} } }
            }
            if running { ProgressBar { value: None, label: "Tool running".to_string() } }
            if !output.is_empty() {
                pre { class: "agent-chat__tool-output", {visible} }
                if collapsed {
                    Button { variant: ButtonVariant::Link, onclick: move |_| show_all.set(true), "Show all" }
                }
            }
        }
    }
}

fn argument_summary(tool_name: &str, args_json: &str) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(args_json) {
        Ok(value) => value,
        Err(_) => return args_json.chars().take(80).collect(),
    };
    let key = match tool_name {
        "bash" => "command",
        "read" | "write" | "edit" => "path",
        _ => return args_json.chars().take(80).collect(),
    };
    parsed
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| args_json.chars().take(80).collect())
}
