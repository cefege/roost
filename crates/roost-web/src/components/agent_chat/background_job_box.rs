//! A settled background job as its own transcript row: a compact tool box with
//! the job id, state, command, folded output and exit code. Rendered by
//! `transcript`; the job never becomes a child of a tool box.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::JobState;

use super::output_window::OutputWindow;
use super::tool_view::{state_label, status_name, tool_state};
use crate::components::md::{Icon, IconSize, StatusDot};

/// Output rows a finished job shows before folding; the tail is what a reader
/// of a settled job wants, as with bash.
const BACKGROUND_JOB_ROWS: usize = 10;

#[component]
pub fn BackgroundJobBox(
    job_id: String,
    command: String,
    state: JobState,
    is_error: bool,
    output: String,
    exit_code: Option<i32>,
) -> Element {
    let mut output_open = use_signal(|| false);
    let state_name = tool_state(false, false, false, false, is_error, Some(state.as_str()));
    let label = state_label(state_name);
    rsx! {
        div { class: "agent-chat__step agent-chat__job", "data-state": state_name, "data-tool": "bash",
            div { class: "agent-chat__step-head",
                span { class: "agent-chat__step-icon", Icon { name: "terminal", size: IconSize::Sm } }
                span { class: "agent-chat__step-verb", "Background job" }
                span { class: "agent-chat__step-target", "{job_id}" }
                span { class: "agent-chat__step-state",
                    StatusDot { status: status_name(state_name).to_string(), title: label.to_string() }
                    span { "{label}" }
                }
            }
            div { class: "agent-chat__bash-command",
                span { class: "agent-chat__bash-line",
                    span { class: "agent-chat__bash-prompt", "$ " }
                    "{command}"
                }
            }
            if !output.is_empty() {
                OutputWindow {
                    output: output.clone(),
                    diff: false,
                    rows: BACKGROUND_JOB_ROWS,
                    from_head: false,
                    open: output_open(),
                    failed: is_error,
                    on_expand: move |value: bool| output_open.set(value),
                }
            }
            if let Some(code) = exit_code.filter(|code| *code != 0) {
                p { class: "agent-chat__bash-footer", "Exit: {code}" }
            }
        }
    }
}
