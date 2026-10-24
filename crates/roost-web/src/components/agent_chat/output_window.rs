//! The folded terminal output a tool box shows: OMP's head or tail preview of
//! styled output, the hidden-line hint and the expand control. Stateless; the
//! caller owns the expansion switch and receives the requested state.

use super::output_lines::{styled_lines, window};
use dioxus::prelude::*;
use libero::{
    components::{Box, Button, Paper, Text},
    sx::sx,
};

/// The line printed where folded output hides lines. A tail preview names how
/// much it shows of the whole, since the reader is looking at the latest lines.
pub(super) fn window_hint(shown: usize, hidden: usize, total: usize, from_head: bool) -> String {
    if from_head {
        format!("… {hidden} more lines")
    } else {
        format!("… ({hidden} earlier lines, showing {shown} of {total})")
    }
}

/// `output` as a folded terminal window of at most `rows` lines. `from_head`
/// keeps the first lines; `open` shows all of them. `on_expand` receives the
/// state the reader asked for.
#[component]
pub fn OutputWindow(
    output: String,
    diff: bool,
    rows: usize,
    from_head: bool,
    open: bool,
    failed: bool,
    on_expand: EventHandler<bool>,
) -> Element {
    let lines = styled_lines(&output, diff);
    let (window_lines, hidden) = window(&lines, rows, from_head, open);
    let window_lines = if output.is_empty() {
        &[][..]
    } else {
        window_lines
    };
    let hint = window_hint(window_lines.len(), hidden, lines.len(), from_head);
    let can_collapse = open && lines.len() > rows;
    let error = failed.then_some("true");
    let terminal_surface = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("xs")
            .border_radius("md")
            .padding_top("sm")
            .padding_bottom("sm")
            .background("var(--term-bg)")
            .color("var(--term-fg)")
    });
    let hint_style = use_hook(|| sx().padding_left("md").padding_right("md"));
    let output_sx = use_hook(|| {
        sx().margin("0")
            .overflow_x("auto")
            .with("overscroll-behavior-y", "auto")
            .font_family("monospace")
            .font_size("xs")
            .line_height("1.25rem")
            .selector(
                "& [data-tone='added']",
                sx().background_color("color-mix(in srgb, var(--primary) 10%, transparent)"),
            )
            .selector(
                "& [data-tone='removed']",
                sx().background_color("color-mix(in srgb, var(--destructive) 10%, transparent)"),
            )
            .selector(
                "& [data-tone='header']",
                sx().background_color("var(--muted)")
                    .color("var(--muted-foreground)"),
            )
    });
    let output_line_sx = use_hook(|| {
        sx().white_space("pre-wrap")
            .with("overflow-wrap", "anywhere")
            .padding_inline("md")
    });
    rsx! {
        Paper { sx: terminal_surface,
            if hidden > 0 && !from_head {
                Text { sx: hint_style.clone(), size: "xs", color: "muted", "{hint}" }
            }
            Box { component: "pre", sx: output_sx, "data-error": error,
                for line in window_lines.iter() {
                    Box { component: "div", sx: output_line_sx.clone(), "data-tone": line.tone.attribute(),
                        for run in line.runs.iter() {
                            Box { component: "span", sx: run.style.sx(), "{run.text}" }
                        }
                    }
                }
            }
            if hidden > 0 && from_head {
                Text { sx: hint_style, size: "xs", color: "muted", "{hint}" }
            }
        }
        if hidden > 0 {
            Button { variant: "standard", color: "primary", size: "sm",
                onclick: move |_| on_expand.call(true), "Show all" }
        }
        if can_collapse {
            Button { variant: "standard", color: "primary", size: "sm",
                onclick: move |_| on_expand.call(false), "Collapse" }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::window_hint;

    #[test]
    fn a_tail_hint_names_the_hidden_and_shown_counts() {
        assert_eq!(
            window_hint(10, 15, 25, false),
            "… (15 earlier lines, showing 10 of 25)"
        );
        assert_eq!(window_hint(3, 4, 7, true), "… 4 more lines");
    }
}
