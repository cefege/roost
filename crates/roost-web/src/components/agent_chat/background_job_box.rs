//! A settled background job as its own transcript row: a compact tool box with
//! the job id, state, command, folded output and exit code. Rendered by
//! `transcript`; the job never becomes a child of a tool box.

use dioxus::prelude::*;
use libero::{
    components::{Flex, Indicator, Paper, Pictogram, Text},
    sx::sx,
};
use roost_protocol::wire::agent_chat::JobState;

use super::output_window::OutputWindow;
use super::tool_view::{state_label, status_name, tool_state};

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
    let card = use_hook(|| sx().gap("xs").padding("sm").min_width("0"));
    let header = use_hook(|| sx().gap("sm").align_items("center").color("muted"));
    let command_style = use_hook(|| {
        sx().padding_left("md")
            .padding_top("sm")
            .font_family("monospace")
    });
    let state_style = use_hook(|| sx().gap("xs").align_items("center"));
    rsx! {
        Paper { sx: card, "data-state": state_name, "data-tool": "bash",
            Flex { direction: "column", gap: "sm",
                Flex { sx: header,
                    Pictogram { icon: pictogram_icons_lucide::terminal::outlined }
                    Text { size: "sm", "Background job" }
                    Text { size: "xs", color: "primary", "{job_id}" }
                    Flex { sx: state_style,
                        Indicator {
                            color: match status_name(state_name) {
                                "ok" => "success",
                                "running" => "info",
                                "error" => "error",
                                _ => "warning",
                            },
                            "aria-label": label,
                        }
                        Text { size: "xs", "{label}" }
                    }
                }
                Flex { direction: "column", sx: command_style,
                    Text { size: "xs", color: "primary", "$ {command}" }
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
                    Text { size: "xs", color: "muted", "Exit: {code}" }
                }
            }
        }
    }
}
