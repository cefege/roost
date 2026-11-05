//! Text-bearing TSP nodes retain their complete source and styled spans.
//! Markdown, code, diffs, terminal output and TeX each keep their native
//! semantics; links and files resolve through the authenticated surface link.
use super::prop_text;
use crate::components::ui::{Markdown, MarkdownSize};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::Value;
use std::ops::Range;

mod ansi;
mod code;
mod copy;
mod diff;
mod math;
mod spans;

pub(super) fn render(node: &OmpUiNode, expanded: bool) -> Element {
    match node.k.as_str() {
        "seg" => spans::render(
            node.p.get("spans"),
            node.p
                .get("icon")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            "whitespace-pre-wrap break-words",
        ),
        "md" => markdown(node),
        "code" => code::render(node, expanded),
        "diff" => diff::render(node),
        "ansi" => ansi::render(node, expanded),
        "math" => math::render(node),
        "shimmer" => spans::render(
            node.p.get("spans").or_else(|| node.p.get("text")),
            "",
            "whitespace-pre-wrap break-words motion-safe:animate-pulse",
        ),
        "text" => text(node),
        _ => spans::render(
            node.p
                .get("spans")
                .or_else(|| node.p.get("text"))
                .or_else(|| node.p.get("label")),
            "",
            "whitespace-pre-wrap break-words",
        ),
    }
}

/// Rows a disclosure can address without rewriting the node. Code and ANSI
/// report source-line counts; every other text leaf is rendered whole.
pub(super) fn line_count(node: &OmpUiNode) -> Option<usize> {
    match node.k.as_str() {
        "code" => Some(code::line_count(node)),
        "ansi" => Some(ansi::line_count(node)),
        _ => None,
    }
}

/// Draw only `range` of a row-addressable leaf. The node's complete source
/// still backs Copy, numbering, marks and SGR state; a non-row leaf ignores
/// the range and renders whole.
pub(super) fn render_window(node: &OmpUiNode, range: Range<usize>) -> Element {
    match node.k.as_str() {
        "code" => code::render_window(node, range),
        "ansi" => ansi::render_window(node, range),
        _ => render(node, true),
    }
}

fn markdown(node: &OmpUiNode) -> Element {
    let source = prop_text(node, "text");
    let html = spans::markdown_with_marks(&source, node.p.get("marks"));
    let streaming = node
        .p
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    rsx! {
        div { class: "min-w-0",
            if streaming { span { class: "sr-only", "Response is streaming" } }
            Markdown { html, size: MarkdownSize::Sm }
        }
    }
}

/// A text node honours the native wrap and line-clamp props; `truncate`
/// narrows the box, the source itself stays selectable in full.
fn text(node: &OmpUiNode) -> Element {
    let wrap = node.p.get("wrap").and_then(Value::as_str).unwrap_or("word");
    let text_layout = match wrap {
        "none" => "whitespace-pre",
        "char" => "whitespace-pre-wrap break-all",
        _ => "whitespace-pre-wrap break-words",
    };
    let truncate = node.p.get("truncate").and_then(Value::as_str);
    let width = match truncate {
        Some("end") => "truncate",
        Some(_) => "max-w-full overflow-hidden text-ellipsis whitespace-nowrap",
        None => "",
    };
    let lines = node
        .p
        .get("lines")
        .and_then(Value::as_u64)
        .and_then(|lines| usize::try_from(lines).ok());
    let clamp = lines
        .map(|lines| format!("display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:{lines};overflow:hidden;"));
    let value = node.p.get("spans").or_else(|| node.p.get("text"));
    rsx! {
        div {
            class: "min-w-0 text-sm {text_layout} {width}",
            style: clamp.as_deref(),
            {spans::render(value, "", text_layout)}
        }
    }
}
