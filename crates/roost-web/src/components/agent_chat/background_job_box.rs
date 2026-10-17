//! A settled background job as its own transcript row: a compact tool box with
//! the job id, state, command, folded output and exit code. Rendered by
//! `transcript`; the job never becomes a child of a tool box.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::JobState;

use super::output_window::OutputWindow;
use super::tool_view::{state_label, status_name, tool_state};
use crate::components::ui::{Icon, IconType, StatusDot};

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
        div { class: "flex min-w-0 flex-col gap-1 rounded-md border border-border p-2 data-[state=error]:border-destructive/40", "data-state": state_name, "data-tool": "bash",
            div { class: "flex min-w-0 items-center gap-2 text-sm text-muted-foreground",
                span { class: "inline-flex flex-none items-center gap-1", Icon { icon: IconType::Terminal, class: "size-4" } }
                span { class: "flex-none font-medium text-foreground", "Background job" }
                span { class: "min-w-0 flex-1 truncate font-mono text-xs text-foreground", "{job_id}" }
                span { class: "ml-auto inline-flex flex-none items-center gap-1 text-xs text-muted-foreground",
                    StatusDot { status: status_name(state_name).to_string(), title: Some(label.to_string()) }
                    span { "{label}" }
                }
            }
            div { class: "flex min-w-0 flex-col px-3 pt-2 font-mono text-xs text-foreground",
                span { class: "whitespace-pre-wrap break-words",
                    span { class: "text-primary", "$ " }
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
                p { class: "m-0 px-3 pt-1 text-xs text-muted-foreground", "Exit: {code}" }
            }
        }
    }
}
