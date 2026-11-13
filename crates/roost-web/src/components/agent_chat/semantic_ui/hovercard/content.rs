//! Read-only prop trees using the surface's existing content renderers.
//! Prop nodes have no controller IDs; native actions remain labels, not targets.
//! OMP's children, styled runs and disclosure previews retain their source order.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiBlob, OmpUiNode};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

use super::super::{
    body_preview::{self, Preview, WindowNode},
    content as native_content, editors, layout,
};

pub(super) fn render(card: &Value, surface: &str, blobs: &BTreeMap<String, OmpUiBlob>) -> Element {
    match decode(card) {
        Ok(node) => render_node(&node, false, surface, blobs),
        Err(message) => {
            rsx! { p { role: "alert", "data-omp-render-error": "invalid-hover-card", class: "text-sm text-destructive", "Invalid OMP hover card: {message}" } }
        }
    }
}

fn decode(value: &Value) -> Result<OmpUiNode, &'static str> {
    let kind = value
        .get("k")
        .and_then(Value::as_str)
        .ok_or("missing component kind")?;
    let props = match value.get("p") {
        Some(Value::Object(props)) => props.clone(),
        None => Map::new(),
        _ => return Err("component props are not an object"),
    };
    let children = match value.get("c") {
        Some(Value::Array(children)) => {
            children.iter().map(decode).collect::<Result<Vec<_>, _>>()?
        }
        None => Vec::new(),
        _ => return Err("component children are not an array"),
    };
    Ok(OmpUiNode {
        id: String::new(),
        k: kind.to_owned(),
        p: props,
        c: children,
    })
}

fn render_node(
    node: &OmpUiNode,
    expanded: bool,
    surface: &str,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    if node.p.get("hidden").and_then(Value::as_bool) == Some(true) {
        return rsx! {};
    }
    match node.k.as_str() {
        "row" | "col" | "list" | "overlay" | "section" | "card" => {
            render_structure(node, expanded, surface, blobs)
        }
        "tool" => {
            let collapsed = node.p.get("collapsed").and_then(Value::as_bool) == Some(true);
            let collapsible = node
                .p
                .get("collapsible")
                .and_then(Value::as_bool)
                .unwrap_or(!node.c.is_empty());
            let body = if collapsed && collapsible {
                body_preview::render_with(
                    node.c.iter(),
                    Preview::for_tool(node),
                    |view, expanded| render_window(view, expanded, surface, blobs),
                )
            } else {
                render_children(node, true, surface, blobs)
            };
            native_content::render_readonly(node, true, surface, body)
                .unwrap_or_else(|| unsupported(node))
        }
        "status" => native_content::render_readonly_status(
            node,
            node.c
                .iter()
                .map(|child| render_node(child, expanded, surface, blobs))
                .collect(),
        ),
        "agent" | "checklist" | "seg" => {
            let hidden_body = node.k == "checklist"
                || (node.k == "agent"
                    && node.p.get("collapsed").and_then(Value::as_bool) == Some(true)
                    && node
                        .p
                        .get("collapsible")
                        .and_then(Value::as_bool)
                        .unwrap_or(!node.c.is_empty()));
            let body = if hidden_body {
                rsx! {}
            } else {
                render_children(node, expanded, surface, blobs)
            };
            native_content::render_readonly(node, expanded, surface, body)
                .unwrap_or_else(|| unsupported(node))
        }
        "item" => readonly_item(node, surface, blobs),
        "button" => {
            rsx! { span { class: "text-sm", {native_content::render_spans(node.p.get("label"), "")} } }
        }
        "rule" => super::super::nodes::render_rule(node),
        "spacer" => layout::spacer(node),
        "image" | "media" => editors::media_node(node, surface, blobs),
        "toast" => {
            rsx! { p { class: "text-sm", {native_content::render_spans(node.p.get("text"), "")} {native_content::render_spans(node.p.get("sub"), "text-muted-foreground")} } }
        }
        _ => native_content::render_readonly(node, expanded, surface, rsx! {})
            .unwrap_or_else(|| unsupported(node)),
    }
}

fn render_structure(
    node: &OmpUiNode,
    expanded: bool,
    surface: &str,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    if !matches!(node.k.as_str(), "card" | "section") {
        return layout::render_window_box(
            node,
            node.c
                .iter()
                .map(|child| render_node(child, expanded, surface, blobs)),
        );
    }
    let collapsible = node.p.get("collapsible").and_then(Value::as_bool) == Some(true);
    let head_index = layout::disclosure_head_index(
        node,
        collapsible,
        native_content::text_is_empty(node.p.get("head")),
    );
    let children = node
        .c
        .iter()
        .enumerate()
        .filter_map(|(index, child)| (Some(index) != head_index).then_some(child));
    let body = if collapsible && node.p.get("collapsed").and_then(Value::as_bool) == Some(true) {
        let preview = if node.k == "card" {
            Preview::for_card(node)
        } else {
            Preview::None
        };
        body_preview::render_with(children, preview, |view, expanded| {
            render_window(view, expanded, surface, blobs)
        })
    } else {
        rsx! { for child in children { {render_node(child, true, surface, blobs)} } }
    };
    let head = head_index
        .and_then(|index| node.c.get(index))
        .map(|head| render_node(head, false, surface, blobs));
    layout::render_window_box(node, head.into_iter().chain(std::iter::once(body)))
}

fn render_children(
    node: &OmpUiNode,
    expanded: bool,
    surface: &str,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    rsx! { for child in &node.c { {render_node(child, expanded, surface, blobs)} } }
}

fn render_window(
    view: &WindowNode<'_>,
    expanded: bool,
    surface: &str,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    match view {
        WindowNode::Full(node) => render_node(node, expanded, surface, blobs),
        WindowNode::Text { node, range } => native_content::render_window(node, range.clone()),
        WindowNode::Structure { node, children } => layout::render_window_box(
            node,
            children
                .iter()
                .map(|child| render_window(child, expanded, surface, blobs)),
        ),
    }
}

fn readonly_item(node: &OmpUiNode, surface: &str, blobs: &BTreeMap<String, OmpUiBlob>) -> Element {
    let icon = node.p.get("icon").and_then(Value::as_str);
    rsx! {
        div { class: "flex min-w-0 flex-col gap-1 px-2 py-1 text-sm",
            div { class: "flex min-w-0 flex-wrap items-center gap-2",
                if let Some(icon) = icon { {native_content::render_icon(icon, "size-4")} }
                {native_content::render_spans(node.p.get("label"), "min-w-0 break-words")}
                {native_content::render_spans(node.p.get("detail"), "min-w-0 break-words text-xs text-muted-foreground")}
            }
            {render_children(node, false, surface, blobs)}
        }
    }
}

fn unsupported(node: &OmpUiNode) -> Element {
    let kind = &node.k;
    rsx! { p { role: "alert", "data-omp-render-error": "unsupported-kind", class: "text-sm text-destructive", "Unsupported read-only OMP hover-card component: {kind}" } }
}

#[cfg(test)]
#[path = "content_tests.rs"]
mod tests;
