//! Read-only rendering of a native node's `hovercard` prop tree.
//!
//! A hover card is a `propNode`: a described subtree that carries no wire ids
//! (nothing reconciles or targets it) and no live component. It is drawn with
//! the same text and data renderers the surface uses, but structural kinds stay
//! static and controls are labels: a pointer resting on the card can never
//! drive OMP. The tree is bounded so a pathological prop cannot paint forever.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiBlob, OmpUiNode};
use serde_json::{Map, Value};

use crate::components::deck::deck_dom::Timeout;
use crate::components::ui::field::scoped_element_id;
use crate::components::ui::{HoverCardPresentation, anchored_panel::anchored_placement_style};

use super::{layout, text_value};

const MAX_HOVERCARD_NODES: usize = 256;
const MAX_HOVERCARD_DEPTH: usize = 8;
const MAX_HOVERCARD_CHILDREN: usize = 32;
const HOVER_CARD_OPEN_DELAY_MS: u32 = 350;
const HOVER_CARD_CLOSE_DELAY_MS: u32 = 150;

const CONTENT_KINDS: &[&str] = &[
    "text", "shimmer", "seg", "md", "code", "diff", "ansi", "math", "kv", "table", "tree", "json",
    "badge", "kbd", "icon", "spinner", "elapsed", "progress", "rate", "status", "chart", "meter",
    "effort", "rows",
];

/// A node's declared hover card over its own content.
///
/// The wrapper is a plain anchor (never a focusable trigger around interactive
/// children); dwell, grace and dismissal belong to this owner, and the card is
/// the stock `HoverCardPresentation` placed from the measured anchor.
#[component]
pub(super) fn NativeHoverCard(card: Value, children: Element) -> Element {
    let anchor = use_hook(|| scoped_element_id("omp-hovercard"));
    let mut open = use_signal(|| false);
    let timer = use_hook(|| Rc::new(RefCell::new(None::<Timeout>)));
    let hover_enter = {
        let timer = timer.clone();
        move |_| {
            *timer.borrow_mut() = Timeout::after(HOVER_CARD_OPEN_DELAY_MS, move || open.set(true));
        }
    };
    let hover_leave = {
        let timer = timer.clone();
        move |_| {
            *timer.borrow_mut() =
                Timeout::after(HOVER_CARD_CLOSE_DELAY_MS, move || open.set(false));
        }
    };
    let card_enter = {
        let timer = timer.clone();
        move |_| {
            *timer.borrow_mut() = None;
        }
    };
    let card_leave = {
        let timer = timer.clone();
        move |_| {
            *timer.borrow_mut() =
                Timeout::after(HOVER_CARD_CLOSE_DELAY_MS, move || open.set(false));
        }
    };
    let placement = if open() {
        anchored_placement_style(&anchor, false, false)
            .map(|style| format!("position: fixed; {style}"))
            .unwrap_or_else(|| "position: fixed;".to_owned())
    } else {
        String::new()
    };
    rsx! {
        div {
            id: "{anchor}",
            class: "min-w-0",
            onmouseenter: hover_enter,
            onmouseleave: hover_leave,
            onpointerdown: move |_| {
                *timer.borrow_mut() = None;
                open.set(false);
            },
            {children}
        }
        if open() {
            HoverCardPresentation {
                class: "max-h-96 w-96 overflow-y-auto",
                role: "tooltip",
                style: placement,
                onmouseenter: card_enter,
                onmouseleave: card_leave,
                {render(&card)}
            }
        }
    }
}

/// Draw the bounded, read-only card tree.
fn render(card: &Value) -> Element {
    let send = EventHandler::new(|_: Value| {});
    let blobs = BTreeMap::new();
    let mut budget = MAX_HOVERCARD_NODES;
    render_node(card, 0, &mut budget, &send, &blobs)
}

fn render_node(
    value: &Value,
    depth: usize,
    budget: &mut usize,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    if *budget == 0 || depth > MAX_HOVERCARD_DEPTH {
        return rsx! {};
    }
    *budget -= 1;
    let Some(kind) = value.get("k").and_then(Value::as_str) else {
        return rsx! {};
    };
    let props = value
        .get("p")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if props.get("hidden").and_then(Value::as_bool) == Some(true) {
        return rsx! {};
    }
    let empty = Vec::new();
    let children = value.get("c").and_then(Value::as_array).unwrap_or(&empty);
    if CONTENT_KINDS.contains(&kind) {
        let node = OmpUiNode {
            id: String::new(),
            k: kind.to_owned(),
            p: props.clone(),
            c: Vec::new(),
        };
        if let Some(element) = super::content::render_content(&node, true, "", send, blobs) {
            return element;
        }
    }
    match kind {
        "row" | "col" | "list" | "overlay" | "section" | "card" => {
            let node = OmpUiNode {
                id: String::new(),
                k: kind.to_owned(),
                p: props,
                c: Vec::new(),
            };
            let rendered: Vec<Element> = children
                .iter()
                .take(MAX_HOVERCARD_CHILDREN)
                .map(|child| render_node(child, depth + 1, budget, send, blobs))
                .collect();
            layout::render_window_box(&node, rendered.into_iter())
        }
        "item" => readonly_item(&props),
        "button" => readonly_label(&props, "label", "text"),
        "tool" | "agent" | "checklist" => readonly_label(&props, "title", "name"),
        "image" | "media" => {
            let alt = props
                .get("alt")
                .map(text_value)
                .filter(|alt| !alt.is_empty())
                .unwrap_or_else(|| "Media supplied by OMP".to_owned());
            rsx! { p { class: "text-xs text-muted-foreground", "{alt}" } }
        }
        _ => readonly_label(&props, "text", "label"),
    }
}

fn readonly_item(props: &Map<String, Value>) -> Element {
    let label = value_text(props, "label")
        .or_else(|| value_text(props, "text"))
        .unwrap_or_default();
    let detail = value_text(props, "detail").unwrap_or_default();
    let icon = props.get("icon").and_then(Value::as_str);
    rsx! {
        div { class: "flex min-w-0 flex-wrap items-center gap-2 px-2 py-1 text-sm",
            if let Some(icon) = icon {
                {super::content::render_icon(icon, "sm")}
            }
            span { class: "min-w-0 break-words", "{label}" }
            if !detail.is_empty() {
                span { class: "min-w-0 break-words text-xs text-muted-foreground", "{detail}" }
            }
        }
    }
}

fn readonly_label(props: &Map<String, Value>, first: &str, second: &str) -> Element {
    let text = value_text(props, first)
        .or_else(|| value_text(props, second))
        .unwrap_or_default();
    if text.is_empty() {
        return rsx! {};
    }
    rsx! { p { class: "min-w-0 break-words text-sm", "{text}" } }
}

fn value_text(props: &Map<String, Value>, key: &str) -> Option<String> {
    props
        .get(key)
        .map(text_value)
        .filter(|text| !text.is_empty())
}
