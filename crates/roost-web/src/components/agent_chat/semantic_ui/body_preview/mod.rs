//! Native body clamps for disclosure nodes.
//!
//! OMP names the visible line count and whether the clamp keeps the head or the
//! tail. This file selects borrowed node/window views only: row-addressable
//! leaves keep their complete source in props and are drawn through a line
//! range, so ANSI parsing and Copy always see the original output. Structural
//! boxes are rendered with the selected children, never re-reading their
//! original child list.

use std::ops::Range;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiBlob, OmpUiNode};
use serde_json::Value;
use std::collections::BTreeMap;

/// `DEFAULT_TERMINAL_PREVIEW_LINES`: the clamp a native tool card applies when
/// OMP sends no preview, and what `auto` resolves to.
const DEFAULT_PREVIEW_LINES: usize = 10;

/// How a disclosure body is clamped while collapsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Preview {
    /// No body is drawn while collapsed.
    None,
    /// The body stays whole and each child's own preview clamps it.
    Children,
    /// The first `n` drawn lines.
    Lines(usize),
    /// The last `n` drawn lines (terminal output).
    Tail(usize),
}

impl Preview {
    /// The tool card's clamp: `inline` frames default to no body, a frame
    /// defaults to the native preview size. A declared value always wins.
    pub(super) fn for_tool(node: &OmpUiNode) -> Self {
        let declared = Self::declared(node);
        if node.p.get("frame").and_then(Value::as_str) == Some("inline") {
            declared.unwrap_or(Self::None)
        } else {
            declared.unwrap_or(Self::Lines(DEFAULT_PREVIEW_LINES))
        }
    }

    /// A generic card defaults to the native preview size; `none` and
    /// `children` keep their own meaning.
    pub(super) fn for_card(node: &OmpUiNode) -> Self {
        Self::declared(node).unwrap_or(Self::Lines(DEFAULT_PREVIEW_LINES))
    }

    fn declared(node: &OmpUiNode) -> Option<Self> {
        match node.p.get("preview")? {
            Value::String(text) if text == "none" => Some(Self::None),
            Value::String(text) if text == "children" => Some(Self::Children),
            Value::String(text) if text == "auto" => Some(Self::Lines(DEFAULT_PREVIEW_LINES)),
            Value::Object(map) => map
                .get("lines")
                .and_then(Value::as_u64)
                .map(|lines| Self::Lines(usize::try_from(lines).unwrap_or(usize::MAX)))
                .or_else(|| {
                    map.get("tail")
                        .and_then(Value::as_u64)
                        .map(|tail| Self::Tail(usize::try_from(tail).unwrap_or(usize::MAX)))
                }),
            _ => None,
        }
    }
}

/// A collapsed disclosure body under `preview`.
pub(super) fn render_body<'a>(
    children: impl DoubleEndedIterator<Item = &'a OmpUiNode>,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
    preview: Preview,
) -> Element {
    match preview {
        Preview::None => rsx! {},
        Preview::Children => render_children(children, surface, send, blobs, false),
        Preview::Lines(lines) => render_window(children, lines, false, surface, send, blobs),
        Preview::Tail(lines) => render_window(children, lines, true, surface, send, blobs),
    }
}

/// An open (or non-collapsible) disclosure: the complete body, with leaf
/// previews disabled so the enclosing disclosure owns the only clamp.
pub(super) fn render_full<'a>(
    children: impl DoubleEndedIterator<Item = &'a OmpUiNode>,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    render_children(children, surface, send, blobs, true)
}

/// A borrowed view of one selected child.
#[derive(Debug, PartialEq)]
pub(super) enum WindowNode<'a> {
    /// The whole node fits the line budget.
    Full(&'a OmpUiNode),
    /// A row-addressable text leaf drawn through `render_node_window`.
    Text {
        node: &'a OmpUiNode,
        range: Range<usize>,
    },
    /// A structural box whose selected children replace its original list.
    Structure {
        node: &'a OmpUiNode,
        children: Vec<WindowNode<'a>>,
    },
}

pub(super) fn render_with<'a>(
    children: impl DoubleEndedIterator<Item = &'a OmpUiNode>,
    preview: Preview,
    mut render: impl FnMut(&WindowNode<'a>, bool) -> Element,
) -> Element {
    let (selected, expanded, frame) = match preview {
        Preview::None => return rsx! {},
        Preview::Children => (children.map(WindowNode::Full).collect(), false, None),
        Preview::Lines(lines) | Preview::Tail(lines) => {
            let tail = matches!(preview, Preview::Tail(_));
            let mut budget = lines;
            let selected = select_nodes(children, &mut budget, tail);
            (selected, true, Some((lines, tail)))
        }
    };
    let body = rsx! { for node in &selected { {render(node, expanded)} } };
    if let Some((lines, tail)) = frame {
        window_frame(body, lines, tail)
    } else {
        body
    }
}

fn render_window<'a>(
    children: impl DoubleEndedIterator<Item = &'a OmpUiNode>,
    lines: usize,
    tail: bool,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    let mut budget = lines;
    let selected = select_nodes(children, &mut budget, tail);
    // Rich leaves have no addressable source rows; keep their complete source
    // but bound the collapsed paint just as strictly as a code/ANSI window.
    window_frame(
        render_selected(&selected, surface, send, blobs),
        lines,
        tail,
    )
}

fn window_frame(body: Element, lines: usize, tail: bool) -> Element {
    let alignment = if tail { "justify-end" } else { "justify-start" };
    rsx! {
        div { class:"flex min-w-0 flex-col overflow-hidden {alignment}",style:"max-height:{lines}lh;",
            div { class:"min-w-0 shrink-0", {body} }
        }
    }
}

fn select_children<'a>(
    children: &'a [OmpUiNode],
    budget: &mut usize,
    tail: bool,
) -> Vec<WindowNode<'a>> {
    select_nodes(children.iter(), budget, tail)
}

fn select_nodes<'a>(
    children: impl DoubleEndedIterator<Item = &'a OmpUiNode>,
    budget: &mut usize,
    tail: bool,
) -> Vec<WindowNode<'a>> {
    let mut selected = Vec::new();
    if tail {
        for child in children.rev() {
            if *budget == 0 {
                break;
            }
            if let Some(node) = select_node(child, budget, tail) {
                selected.push(node);
            }
        }
        selected.reverse();
    } else {
        for child in children {
            if *budget == 0 {
                break;
            }
            if let Some(node) = select_node(child, budget, tail) {
                selected.push(node);
            }
        }
    }
    selected
}

fn select_node<'a>(node: &'a OmpUiNode, budget: &mut usize, tail: bool) -> Option<WindowNode<'a>> {
    if node.p.get("hidden").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    if *budget == 0 {
        return None;
    }
    if let Some(total) = super::line_count(node) {
        if total <= *budget {
            *budget -= total;
            return Some(WindowNode::Full(node));
        }
        let start = if tail { total - *budget } else { 0 };
        let end = if tail { total } else { *budget };
        *budget = 0;
        return Some(WindowNode::Text {
            node,
            range: start..end,
        });
    }
    if node.k == "row" {
        *budget -= 1;
        return Some(WindowNode::Full(node));
    }
    if matches!(
        node.k.as_str(),
        "col" | "section" | "card" | "list" | "overlay"
    ) {
        let head_line =
            matches!(node.k.as_str(), "section" | "card") && node.p.contains_key("head");
        if head_line && !tail {
            if *budget == 0 {
                return None;
            }
            *budget -= 1;
        }
        let children = select_children(&node.c, budget, tail);
        let tail_head = head_line && tail && *budget > 0;
        if tail_head {
            *budget -= 1;
        }
        if children.is_empty() && !(head_line && (!tail || tail_head)) {
            return None;
        }
        return Some(WindowNode::Structure { node, children });
    }
    // A non-row-addressable leaf draws whole; it still occupies one drawn line.
    *budget -= 1;
    Some(WindowNode::Full(node))
}

fn render_selected(
    selected: &[WindowNode<'_>],
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    rsx! {
        for node in selected {
            {render_window_node(node, surface, send, blobs)}
        }
    }
}

fn render_window_node(
    node: &WindowNode<'_>,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    match node {
        WindowNode::Full(node) => super::render_node_expanded(node, surface, send, blobs),
        WindowNode::Text { node, range } => {
            super::render_node_window(node, surface, send, blobs, range.clone())
        }
        WindowNode::Structure { node, children } => {
            let elements: Vec<Element> = children
                .iter()
                .map(|child| render_window_node(child, surface, send, blobs))
                .collect();
            let body = super::layout::render_window_box(node, elements.into_iter());
            super::chrome::decorate(node, surface, *send, body)
        }
    }
}

fn render_children<'a>(
    children: impl Iterator<Item = &'a OmpUiNode>,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
    expanded: bool,
) -> Element {
    rsx! {
        for child in children {
            {render_child(child, surface, send, blobs, expanded)}
        }
    }
}

fn render_child(
    node: &OmpUiNode,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
    expanded: bool,
) -> Element {
    if expanded {
        super::render_node_expanded(node, surface, send, blobs)
    } else {
        super::render_node(node, surface, send, blobs)
    }
}

#[cfg(test)]
mod tests;
