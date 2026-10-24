//! A bash call's box body: the command as OMP's dim `$` lines, the folded
//! output window, and the executor footer. Pure presentation over the call's
//! JSON; the footer facts are computed in `bash_footer`.

use dioxus::prelude::*;

use super::bash_footer::{COMMAND_PREVIEW_ROWS, bash_footer_facts};
use super::output_lines::window;
use super::output_window::OutputWindow;

/// Rows of output a bash box shows before folding, matching OMP's tail window.
const BASH_OUTPUT_ROWS: usize = 10;

#[component]
pub fn BashBody(
    command: String,
    output: String,
    details_json: String,
    args_json: String,
    is_error: bool,
    open: bool,
    on_expand: EventHandler<bool>,
) -> Element {
    let details: Option<serde_json::Value> = serde_json::from_str(&details_json).ok();
    let args: Option<serde_json::Value> = serde_json::from_str(&args_json).ok();
    let facts = bash_footer_facts(details.as_ref(), args.as_ref(), is_error);
    let footer = facts.join(" | ");

    let command_lines: Vec<&str> = command.lines().collect();
    let (shown_command, hidden_command) = window(&command_lines, COMMAND_PREVIEW_ROWS, false, open);
    let first_shown_index = hidden_command;
    let hint = format!("… {hidden_command} earlier lines");
    rsx! {
        div { class: "flex min-w-0 flex-col px-3 pt-2 font-mono text-xs text-foreground",
            if hidden_command > 0 {
                p { class: "m-0 px-3 text-xs text-muted-foreground", "{hint}" }
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
        if !footer.is_empty() {
            p { class: "m-0 px-3 pt-1 text-xs text-muted-foreground", "{footer}" }
        }
    }
}
