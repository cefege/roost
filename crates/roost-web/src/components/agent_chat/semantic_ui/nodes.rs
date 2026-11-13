//! Native node dispatch and existing controller event addresses.
//! Layout boxes are rendered separately from data-bearing widgets and editors.
//! Every control uses the same OMP-owned node and surface IDs; the wrapper that
//! carries those IDs is added by [`super::chrome`], and this module reports the
//! node's declared action as the native event kind it names.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiBlob, OmpUiNode};
use serde_json::{Value, json};

use crate::components::ui::{Separator, Tabs, TabsList, TabsTrigger, TabsVariant};

use super::actions::PointerActions;
use super::content::{render_content, render_spans};
use super::controls::{render_button, render_item};
use super::editors::{edit_node, media_node};
use super::{picker, prefs, text_value};

pub(super) fn render_node(
    node: &OmpUiNode,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    render_node_with_focus(node, surface, send, blobs, None)
}

pub(super) fn render_node_with_focus(
    node: &OmpUiNode,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
    focused_node: Option<&str>,
) -> Element {
    try_render_node(node, surface, *send, blobs, focused_node, false, None)
        .unwrap_or_else(|| rsx! {})
}

pub(super) fn render_node_expanded(
    node: &OmpUiNode,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    try_render_node(node, surface, *send, blobs, None, true, None).unwrap_or_else(|| rsx! {})
}

pub(super) fn render_node_window(
    node: &OmpUiNode,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
    range: std::ops::Range<usize>,
) -> Element {
    if node.p.get("hidden").and_then(Value::as_bool) == Some(true) {
        return rsx! {};
    }
    if super::line_count(node).is_some() {
        let body = super::content::render_window(node, range);
        super::chrome::decorate(node, surface, *send, body)
    } else {
        render_node_expanded(node, surface, send, blobs)
    }
}

fn try_render_node<'a>(
    node: &'a OmpUiNode,
    surface: &str,
    send: EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
    focused_node: Option<&str>,
    expanded: bool,
    inherited: Option<PointerActions<'a>>,
) -> Option<Element> {
    if node.p.get("hidden").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    if matches!(
        node.k.as_str(),
        "col" | "row" | "section" | "card" | "list" | "overlay"
    ) {
        let inherited = if node.k == "list" {
            PointerActions::for_list(node)
        } else {
            inherited.unwrap_or_default()
        };
        let mut children = node
            .c
            .iter()
            .filter_map(|child| {
                try_render_node(
                    child,
                    surface,
                    send,
                    blobs,
                    focused_node,
                    expanded,
                    Some(inherited),
                )
            })
            .peekable();
        // An empty list shows its `empty` message in place of rows, as the terminal does.
        if node.k == "list"
            && children.peek().is_none()
            && let Some(empty) = node.p.get("empty").filter(|value| !value.is_null())
        {
            let label = text_value(empty);
            let body = rsx! { p { class: "text-sm text-muted-foreground", "{label}" } };
            return Some(super::chrome::decorate(node, surface, send, body));
        }
        if matches!(node.k.as_str(), "col" | "row" | "list" | "overlay")
            && children.peek().is_none()
            && !super::layout::reserves_layout_space(&node.p)
        {
            return None;
        }
        let body = super::layout::render_box(node, surface, send, blobs, children);
        return Some(super::chrome::decorate(node, surface, send, body));
    }
    let body = if let Some(element) = render_content(node, expanded, surface, &send, blobs) {
        element
    } else {
        match node.k.as_str() {
            "rule" => render_rule(node),
            "spacer" => super::layout::spacer(node),
            "image" | "media" => media_node(node, surface, blobs),
            "button" => render_button(node, surface, send),
            "item" => render_item(node, surface, send, inherited),
            "tabs" => render_tabs(node, surface, &send),
            "input" | "editor" => {
                edit_node(node, surface, &send, focused_node == Some(node.id.as_str()))
            }
            "picker" => picker::render_picker(node, surface, &send, blobs),
            "prefs" => prefs::render_prefs(node, surface, &send, blobs),
            "toast" => {
                let text = node
                    .p
                    .get("text")
                    .or_else(|| node.p.get("label"))
                    .map(text_value)
                    .unwrap_or_default();
                rsx! {p{role:"status",class:"min-w-0 whitespace-pre-wrap wrap-anywhere text-sm","{text}"}}
            }
            _ => {
                let kind = &node.k;
                rsx! {p{role:"alert","data-omp-render-error":"unsupported-kind",class:"text-sm text-destructive","Unsupported OMP component: {kind}"}}
            }
        }
    };
    Some(super::chrome::decorate(node, surface, send, body))
}

fn render_tabs(node: &OmpUiNode, surface: &str, send: &EventHandler<Value>) -> Element {
    let send = *send;
    let items = node.p.get("items").and_then(Value::as_array);
    let active = node
        .p
        .get("active")
        .and_then(Value::as_str)
        .or_else(|| {
            items
                .and_then(|items| items.first())
                .and_then(|item| item.get("id"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default()
        .to_owned();
    let id = node.id.clone();
    let sf = surface.to_owned();
    let variant =
        if node.k == "seg" || node.p.get("variant").and_then(Value::as_str) == Some("segmented") {
            TabsVariant::Default
        } else {
            TabsVariant::Line
        };
    rsx! {
        Tabs {
            value: active.clone(),
            on_value_change: move |value| send.call(super::event("select", &sf, &id, json!({"item": value}))),
            TabsList {
                aria_label: Some("Options".into()),
                variant,
                for item in items.into_iter().flatten() {
                    {
                        let value = item.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
                        let label = item.get("label").map(text_value).unwrap_or_else(|| value.clone());
                        let selected = active == value;
                        rsx! { TabsTrigger {
                            value: value.clone(),
                            has_panel: false,
                            "data-omp-item": value,
                            "data-omp-item-selected": if selected { "true" } else { "false" },
                            "{label}"
                        } }
                    }
                }
            }
        }
    }
}

/// A rule with a `label` leads into its caption the way the terminal draws it
/// (a short rule, then the label); an unlabelled rule spans the row.
pub(super) fn render_rule(node: &OmpUiNode) -> Element {
    match node.p.get("label").filter(|label| !label.is_null()) {
        None => rsx! { Separator { decorative: true } },
        Some(label) => rsx! {
            div { class: "flex items-center gap-2",
                Separator { decorative: true, class: "w-10 shrink-0" }
                span { class: "min-w-0 break-words", {render_spans(Some(label), "")} }
            }
        },
    }
}
