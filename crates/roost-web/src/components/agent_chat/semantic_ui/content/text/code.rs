//! Native `code` blocks: complete source, optional per-line numbers (including
//! non-contiguous `nums` gaps), marked lines, syntax colours and a data-driven
//! preview. Copy always writes the whole source, never the visible window.

use super::super::prop_text;
use super::copy::CopyButton;
use crate::components::ui::{
    ButtonSize, ButtonVariant, Collapsible, CollapsibleContent, CollapsibleTrigger,
};
use crate::syntax_lite::{self, Token, TokenKind};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::Value;
use std::collections::BTreeMap;
use std::ops::Range;

/// The source lines a window can address.
pub(super) fn line_count(node: &OmpUiNode) -> usize {
    prop_text(node, "text").split('\n').count()
}

pub(super) fn render(node: &OmpUiNode, expanded: bool) -> Element {
    view(node, expanded, None)
}

/// Draw only `range` of the source. Parsing, numbering, marks and the copied
/// source stay those of the complete original node.
pub(super) fn render_window(node: &OmpUiNode, range: Range<usize>) -> Element {
    view(node, true, Some(range))
}

fn view(node: &OmpUiNode, expanded: bool, window: Option<Range<usize>>) -> Element {
    let data = prepare(node, expanded, window);
    rsx! {
        CodeView {
            source: data.source,
            line_numbers: data.line_numbers,
            show_numbers: data.show_numbers,
            gaps: data.gaps,
            marks: data.marks,
            preview: data.preview,
            window: data.window,
            path: data.path,
            lang: data.lang,
            grammar: data.grammar,
        }
    }
}

fn prepare(node: &OmpUiNode, expanded: bool, window: Option<Range<usize>>) -> CodeViewProps {
    let source = prop_text(node, "text");
    let total = source.split('\n').count();
    let numbers = node.p.get("nums").and_then(Value::as_array);
    let start = node.p.get("start").and_then(Value::as_u64).unwrap_or(1);
    let numbered = node
        .p
        .get("numbers")
        .and_then(Value::as_bool)
        .unwrap_or(numbers.is_some() || node.p.contains_key("start"));
    let nums = numbers.filter(|nums| nums.len() == total).and_then(|nums| {
        let values: Option<Vec<u64>> = nums.iter().map(Value::as_u64).collect();
        let values = values?;
        values
            .windows(2)
            .all(|pair| pair[0] < pair[1])
            .then_some(values)
    });
    let gaps = nums.is_some();
    let line_numbers = nums.unwrap_or_else(|| {
        (0..total)
            .map(|idx| start.saturating_add(idx as u64))
            .collect()
    });
    let marks: BTreeMap<u64, &'static str> = node
        .p
        .get("marks")
        .and_then(Value::as_array)
        .map(|marks| {
            marks
                .iter()
                .filter_map(|mark| {
                    Some((
                        mark.get("line").and_then(Value::as_u64)?,
                        mark_class(mark.get("tone").and_then(Value::as_str)),
                    ))
                })
                .filter(|(_, class)| !class.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let preview = if expanded || window.is_some() {
        None
    } else {
        node.p
            .get("preview")
            .and_then(|value| value.get("lines"))
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .map(|count| count.min(total))
    };
    let path = prop_text(node, "path");
    let lang = prop_text(node, "lang");
    let grammar = if lang.is_empty() {
        syntax_lite::ext_from_basename(&path)
    } else {
        extension_for(&lang)
    };
    CodeViewProps {
        source,
        line_numbers,
        show_numbers: numbered,
        gaps,
        marks,
        preview,
        window,
        path,
        lang,
        grammar,
    }
}

#[derive(Props, Clone, PartialEq)]
struct CodeViewProps {
    source: String,
    line_numbers: Vec<u64>,
    show_numbers: bool,
    gaps: bool,
    marks: BTreeMap<u64, &'static str>,
    preview: Option<usize>,
    window: Option<Range<usize>>,
    path: String,
    lang: String,
    grammar: String,
}

#[component]
fn CodeView(props: CodeViewProps) -> Element {
    let mut open = use_signal(|| false);
    let lines: Vec<&str> = props.source.split('\n').collect();
    let total = lines.len();
    let tokens = syntax_lite::should_highlight(&props.grammar)
        .then(|| syntax_lite::tokenize_lines(lines.iter().copied()));
    let (visible_start, visible_end, footer) = match props.window.as_ref() {
        Some(range) => {
            let start = range.start.min(total);
            let end = range.end.min(total).max(start);
            (start, end, false)
        }
        None => {
            let visible = if open() {
                total
            } else {
                props.preview.unwrap_or(total)
            };
            (0, visible, visible < total)
        }
    };
    let label = if !props.lang.is_empty() {
        props.lang.clone()
    } else if !props.path.is_empty() {
        props.path.clone()
    } else {
        "code".to_owned()
    };
    rsx! {
        div { class: "flex min-w-0 flex-col rounded-none border border-border",
            div { class: "flex min-w-0 items-center justify-between gap-2 border-b border-border px-2 py-1",
                span { class: "min-w-0 truncate font-mono text-xs text-muted-foreground", "{label}" }
                CopyButton { text: props.source.clone() }
            }
            pre { class: "m-0 overflow-x-auto bg-terminal p-2 font-mono text-xs leading-5 text-terminal-foreground",
                    for (offset, line) in lines[visible_start..visible_end].iter().enumerate() {
                        { line_row(
                            visible_start + offset,
                            line,
                            props.show_numbers,
                            props.show_numbers.then_some(props.line_numbers.get(visible_start + offset).copied()).flatten(),
                            props.show_numbers && props.gaps && visible_start + offset > 0
                                && gap_before(&props.line_numbers, visible_start + offset),
                            props.line_numbers
                                .get(visible_start + offset)
                                .and_then(|number| props.marks.get(number))
                                .copied(),
                            tokens.as_ref().and_then(|rows| rows.get(visible_start + offset)).map(Vec::as_slice),
                        ) }
                    }
                }
                if footer {
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
                            pre { class: "m-0 overflow-x-auto bg-terminal p-2 font-mono text-xs leading-5 text-terminal-foreground",
                                for (idx, line) in lines.iter().enumerate().skip(visible_end) {
                                    { line_row(
                                        idx,
                                        line,
                                        props.show_numbers,
                                        props.show_numbers.then_some(props.line_numbers.get(idx).copied()).flatten(),
                                        props.show_numbers && props.gaps && idx > 0 && gap_before(&props.line_numbers, idx),
                                        props.line_numbers.get(idx).and_then(|number| props.marks.get(number)).copied(),
                                        tokens.as_ref().and_then(|rows| rows.get(idx)).map(Vec::as_slice),
                                    ) }
                                }
                            }
                        }
                    }
                }
        }
    }
}

fn line_row(
    idx: usize,
    line: &str,
    show_numbers: bool,
    number: Option<u64>,
    gap: bool,
    mark: Option<&'static str>,
    tokens: Option<&[Token]>,
) -> Element {
    let row_class = format!("flex min-w-0 gap-3 {}", mark.unwrap_or(""));
    let line_number = number.unwrap_or((idx + 1) as u64);
    rsx! {
        if gap {
            div { class: "flex min-w-0 gap-3 text-muted-foreground", "aria-hidden": "true",
                span { class: "w-10 flex-none select-none text-right", "…" }
                span { "…" }
            }
        }
        div {
            class: "{row_class}",
            "data-line": "{line_number}",
            if show_numbers {
                if let Some(number) = number {
                    span { class: "w-10 flex-none select-none text-right text-muted-foreground", "{number}" }
                } else {
                    span { class: "w-10 flex-none select-none text-right text-muted-foreground", "" }
                }
            }
            span { class: "min-w-0 whitespace-pre",
                if let Some(tokens) = tokens {
                    for token in tokens {
                        if token.kind == TokenKind::Plain {
                            "{token.text}"
                        } else {
                            span { class: crate::components::agent_chat::markdown::token_class(token.kind), "{token.text}" }
                        }
                    }
                } else {
                    "{line}"
                }
            }
        }
    }
}

fn gap_before(numbers: &[u64], idx: usize) -> bool {
    numbers
        .get(idx.wrapping_sub(1))
        .zip(numbers.get(idx))
        .is_some_and(|(before, after)| *after != before.saturating_add(1))
}

fn mark_class(tone: Option<&str>) -> &'static str {
    match tone {
        Some("success") => "bg-success/10",
        Some("error") => "bg-destructive/10",
        Some("warning") => "bg-warning/10",
        Some("info") => "bg-info/10",
        Some("accent" | "user") => "bg-accent",
        Some("muted" | "pending") => "bg-muted",
        _ => "",
    }
}

fn extension_for(lang: &str) -> String {
    match lang.to_ascii_lowercase().as_str() {
        "rust" => "rs".to_owned(),
        "typescript" | "tsx" => "ts".to_owned(),
        "javascript" | "jsx" => "js".to_owned(),
        "python" => "py".to_owned(),
        "golang" => "go".to_owned(),
        "shell" | "console" | "bash" | "zsh" | "sh" => "sh".to_owned(),
        "json" => "js".to_owned(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{line_count, prepare};
    use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
    use serde_json::json;

    fn code_node(text: &str) -> OmpUiNode {
        OmpUiNode {
            id: "code".into(),
            k: "code".into(),
            p: json!({ "text": text, "preview": { "lines": 1 } })
                .as_object()
                .unwrap()
                .clone(),
            c: Vec::new(),
        }
    }

    #[test]
    fn window_keeps_full_copy_source_and_original_line_numbers() {
        let node = code_node("one\ntwo\nthree");
        assert_eq!(line_count(&node), 3);
        let props = prepare(&node, false, Some(1..3));
        assert_eq!(props.source, "one\ntwo\nthree");
        assert_eq!(props.window, Some(1..3));
        assert_eq!(props.preview, None);
        assert_eq!(props.line_numbers, vec![1, 2, 3]);
    }
}
