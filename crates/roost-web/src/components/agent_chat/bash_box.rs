//! A bash call's box body: the command as OMP's dim `$` lines, the folded
//! output window, and the executor footer. Pure presentation over the call's
//! JSON; the footer facts are computed in `bash_footer`.

use dioxus::prelude::*;

use super::bash_footer::{COMMAND_PREVIEW_ROWS, bash_footer_facts};
use super::output_lines::window;
use super::output_window::OutputWindow;

use libero::{
    components::{Box, Flex, Text},
    sx::sx,
};
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
    let command_sx = use_hook(|| {
        sx().min_width("0")
            .padding_inline("md")
            .padding_top("sm")
            .font_family("monospace")
            .font_size("xs")
            .color("var(--foreground)")
    });
    let command_line_sx = use_hook(|| {
        sx().white_space("pre-wrap")
            .with("overflow-wrap", "anywhere")
    });
    let hint_sx = use_hook(|| {
        sx().margin("0")
            .padding_inline("md")
            .font_size("xs")
            .color("muted")
    });
    let footer_sx = use_hook(|| {
        sx().margin("0")
            .padding_inline("md")
            .padding_top("xs")
            .font_size("xs")
            .color("muted")
    });
    let prompt_sx = use_hook(|| sx().color("primary"));
    rsx! {
        Flex { direction: "column", sx: command_sx,
            if hidden_command > 0 {
                Text { component: "p", sx: hint_sx, "{hint}" }
            }
            for (offset, line) in shown_command.iter().enumerate() {
                Box { component: "div", sx: command_line_sx.clone(), key: "{offset}",
                    if first_shown_index + offset == 0 {
                        Text { component: "span", sx: prompt_sx.clone(), "$ " }
                    }
                    "{line}"
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
            Text { component: "p", sx: footer_sx, "{footer}" }
        }
    }
}
