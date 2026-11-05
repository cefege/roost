//! Native `ansi` output: the shared SGR line engine keeps terminal colours,
//! weights and line structure while dropping every control sequence.

use super::super::prop_text;
use super::copy::CopyButton;
use crate::components::agent_chat::output_lines::{styled_lines, window};
use crate::components::ui::{
    ButtonSize, ButtonVariant, Card, CardContent, CardSize, Collapsible, CollapsibleContent,
    CollapsibleTrigger,
};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::Value;
use std::ops::Range;

/// The styled rows a window can address.
pub(super) fn line_count(node: &OmpUiNode) -> usize {
    styled_lines(&prop_text(node, "text"), false).len()
}

pub(super) fn render(node: &OmpUiNode, expanded: bool) -> Element {
    view(node, expanded, None)
}

/// Draw only `range` of the styled rows. The complete source is parsed first,
/// so a tail keeps the SGR style that earlier lines established, and Copy still
/// writes every line.
pub(super) fn render_window(node: &OmpUiNode, range: Range<usize>) -> Element {
    view(node, true, Some(range))
}

fn view(node: &OmpUiNode, expanded: bool, window: Option<Range<usize>>) -> Element {
    let data = prepare(node, expanded, window);
    rsx! {
        AnsiView {
            lines: data.lines,
            plain: data.plain,
            rows: data.rows,
            from_head: data.from_head,
            window: data.window,
        }
    }
}

fn prepare(node: &OmpUiNode, expanded: bool, window: Option<Range<usize>>) -> AnsiViewProps {
    let source = prop_text(node, "text");
    let follow = node
        .p
        .get("follow")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let lines = styled_lines(&source, false);
    let rows = if expanded || window.is_some() {
        lines.len()
    } else {
        node.p
            .get("preview")
            .and_then(|value| value.get("lines"))
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(lines.len())
    };
    let plain = lines
        .iter()
        .map(|line| {
            line.runs
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    AnsiViewProps {
        lines,
        plain,
        rows,
        from_head: !follow,
        window,
    }
}

#[derive(Props, Clone, PartialEq)]
struct AnsiViewProps {
    lines: Vec<crate::components::agent_chat::output_lines::OutputLine>,
    plain: String,
    rows: usize,
    from_head: bool,
    window: Option<Range<usize>>,
}

#[component]
fn AnsiView(props: AnsiViewProps) -> Element {
    let mut open = use_signal(|| false);
    let total = props.lines.len();
    let (shown, hidden, previewable) = match props.window.as_ref() {
        Some(range) => {
            let start = range.start.min(total);
            let end = range.end.min(total).max(start);
            (&props.lines[start..end], 0, false)
        }
        None => {
            let (shown, hidden) = window(&props.lines, props.rows, props.from_head, open());
            (shown, hidden, total > props.rows)
        }
    };
    rsx! {
        Card { size: CardSize::Sm,
            CardContent { class: "flex min-w-0 flex-col gap-2",
                div { class: "flex min-w-0 items-center justify-between gap-2",
                    span { class: "font-mono text-xs text-muted-foreground", "output" }
                    CopyButton { text: props.plain.clone() }
                }
                if previewable && !props.from_head {
                    { output_hint(hidden, props.rows, total) }
                }
                if previewable && !props.from_head {
                    Collapsible {
                        open: open(),
                        on_open_change: move |next: bool| open.set(next),
                        CollapsibleTrigger {
                            variant: ButtonVariant::Link,
                            size: ButtonSize::Sm,
                            class: "justify-start px-0",
                            if open() { "Show fewer lines" } else { "Show all {total} lines" }
                        }
                        CollapsibleContent {
                            if !open() {
                                for line in props.lines.iter().take(total - props.rows) {
                                    { output_line(line) }
                                }
                            }
                        }
                    }
                }
                pre { class: "m-0 overflow-x-auto bg-terminal p-3 font-mono text-xs leading-5 text-terminal-foreground",
                    for line in shown.iter() {
                        { output_line(line) }
                    }
                }
                if previewable && props.from_head {
                    Collapsible {
                        open: open(),
                        on_open_change: move |next: bool| open.set(next),
                        CollapsibleTrigger {
                            variant: ButtonVariant::Link,
                            size: ButtonSize::Sm,
                            class: "justify-start px-0",
                            if open() { "Show fewer lines" } else { "Show all {total} lines" }
                        }
                        CollapsibleContent {
                            if !open() {
                                for line in props.lines.iter().skip(props.rows) {
                                    { output_line(line) }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn output_hint(hidden: usize, shown: usize, total: usize) -> Element {
    if hidden == 0 {
        return rsx! {};
    }
    let text = format!("… ({hidden} earlier lines, showing {shown} of {total})");
    rsx! { p { class: "m-0 text-xs text-muted-foreground", "{text}" } }
}

fn output_line(line: &crate::components::agent_chat::output_lines::OutputLine) -> Element {
    rsx! {
        div { class: "min-w-0 whitespace-pre-wrap break-all",
            for run in &line.runs {
                span { class: run.style.classes(), "{run.text}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::prepare;
    use crate::components::agent_chat::output_lines::styled_lines;
    use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
    use serde_json::json;

    fn ansi_node(text: &str) -> OmpUiNode {
        OmpUiNode {
            id: "ansi".into(),
            k: "ansi".into(),
            p: json!({ "text": text, "preview": { "lines": 1 } })
                .as_object()
                .unwrap()
                .clone(),
            c: Vec::new(),
        }
    }

    #[test]
    fn a_tail_window_keeps_the_sgr_style_established_by_earlier_lines() {
        let lines = styled_lines("\u{1b}[31mred1\nred2\nred3\u{1b}[0m", false);
        let tail = &lines[2..];
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].runs[0].text, "red3");
        assert_eq!(tail[0].runs[0].style.color, Some(1));
    }

    #[test]
    fn window_copy_source_is_the_complete_original_text() {
        let node = ansi_node("one\ntwo\nthree");
        let props = prepare(&node, true, Some(1..2));
        assert_eq!(props.plain, "one\ntwo\nthree");
        assert_eq!(props.window, Some(1..2));
        assert_eq!(props.lines.len(), 3);
    }
}
