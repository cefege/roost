//! A bash call's box body: the command as OMP's dim `$` lines and its folded
//! output window. Output stays text; the executor's redundant detail footer is
//! intentionally absent.

use dioxus::prelude::*;

use super::output_lines::window;
use super::output_window::OutputWindow;

/// Rows of the command a bash box shows before folding.
const COMMAND_PREVIEW_ROWS: usize = 6;

/// Rows of output a bash box shows before folding, matching OMP's tail window.
const BASH_OUTPUT_ROWS: usize = 10;

#[component]
pub fn BashBody(
    command: String,
    output: String,
    is_error: bool,
    open: bool,
    on_expand: EventHandler<bool>,
) -> Element {
    let command_lines: Vec<&str> = command.lines().collect();
    let (shown_command, hidden_command) = window(&command_lines, COMMAND_PREVIEW_ROWS, false, open);
    let first_shown_index = hidden_command;
    let hint = format!("… {hidden_command} earlier lines");
    rsx! {
        div { class: "flex min-w-0 flex-col px-2 pt-1 font-mono text-xs text-foreground",
            if hidden_command > 0 {
                p { class: "m-0 px-2 text-xs text-muted-foreground", "{hint}" }
            }
            for (offset, line) in shown_command.iter().enumerate() {
                div { class: "whitespace-pre-wrap break-words", key: "{offset}",
                    if first_shown_index + offset == 0 {
                        span { class: "text-primary", "$ " }
                    }
                    span { "{line}" }
                }
            }
        }
        if !output.is_empty() {
            OutputWindow {
                output: output,
                diff: false,
                rows: BASH_OUTPUT_ROWS,
                from_head: false,
                open: open,
                failed: is_error,
                on_expand: on_expand,
            }
        }
    }
}
