//! The folded terminal output a tool box shows: OMP's head or tail preview of
//! styled output, the hidden-line hint and the expand control. Stateless; the
//! caller owns the expansion switch and receives the requested state.

use dioxus::prelude::*;

use super::markdown::TERMINAL_GROUND_STYLE;
use super::output_lines::{styled_lines, window};
use crate::components::md::{Button, ButtonSize, ButtonVariant, Surface, SurfaceRadius};

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
    rsx! {
        Surface { radius: SurfaceRadius::Md, style: TERMINAL_GROUND_STYLE.to_string(),
            class: "agent-chat__step-output-ground",
            if hidden > 0 && !from_head {
                p { class: "agent-chat__step-hint", "{hint}" }
            }
            pre { class: "agent-chat__step-output", "data-error": error,
                for line in window_lines.iter() {
                    div { class: "agent-chat__out-line", "data-tone": line.tone.attribute(),
                        for run in line.runs.iter() {
                            span { class: run.style.classes(), "{run.text}" }
                        }
                    }
                }
            }
            if hidden > 0 && from_head {
                p { class: "agent-chat__step-hint", "{hint}" }
            }
        }
        if hidden > 0 {
            Button { variant: ButtonVariant::Link, size: ButtonSize::Sm, class: "agent-chat__step-more",
                onclick: move |_| on_expand.call(true), "Show all" }
        }
        if can_collapse {
            Button { variant: ButtonVariant::Link, size: ButtonSize::Sm, class: "agent-chat__step-more",
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
