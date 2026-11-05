//! Native structural boxes, spacing and disclosure state rendered with stock UI.
//!
//! Cards and sections follow the TSP shape: an optional `head` (text, or the
//! wire id of the keyed head child), a body of the remaining children, and a
//! controlled `collapsed` flag whose toggle returns to OMP. A disclosure with
//! no `head` prop takes its native head row from the first child, which is how
//! thinking and fallback cards are produced. Preview clamps come only from the
//! wire, never from an invented height.

use crate::components::ui::{
    ButtonSize, ButtonVariant, Collapsible, CollapsibleContent, CollapsibleTrigger, Icon, IconType,
};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiBlob, OmpUiNode};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use super::{body_preview, event, text_value};

pub(super) fn render_box(
    node: &OmpUiNode,
    surface: &str,
    send: EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
    children: impl Iterator<Item = Element>,
) -> Element {
    if !matches!(node.k.as_str(), "card" | "section") {
        let classes = box_classes(node);
        if node.k == "list"
            && let Some(lines) = list_max_lines(node)
        {
            return rsx! {
                div { class: "{classes} overflow-y-auto", style: "max-height:{lines}lh;",
                    for child in children { {child} }
                }
            };
        }
        // Virtual lists (the terminal only paints the visible window) may grow
        // to hundreds of rows; let the browser skip layout/paint of the rows
        // outside the scroll viewport instead of rendering the whole catalog.
        if node.k == "list"
            && node
                .p
                .get("virtual")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            return rsx! {
                div { class: "{classes}",
                    for child in children {
                        div { style: "content-visibility:auto;contain-intrinsic-block-size:1lh;", {child} }
                    }
                }
            };
        }
        return rsx! {
            div { class: "{classes}", for child in children { {child} } }
        };
    }
    let collapsible = node
        .p
        .get("collapsible")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let collapsed = node
        .p
        .get("collapsed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let head_value = node.p.get("head");
    let head_text_value = head_value.map(text_value).unwrap_or_default();
    let head_index = head_value
        .and_then(Value::as_str)
        .and_then(|id| node.c.iter().position(|child| child.id == id))
        .or_else(|| (collapsible && head_text_value.is_empty() && !node.c.is_empty()).then_some(0));
    let head_node = head_index.and_then(|index| node.c.get(index));
    let head_text = if head_index.is_some() {
        String::new()
    } else {
        head_text_value
    };
    let body_children = node
        .c
        .iter()
        .enumerate()
        .filter(|(position, _)| Some(*position) != head_index)
        .map(|(_, child)| child);
    let preview = if node.k == "card" {
        body_preview::Preview::for_card(node)
    } else {
        body_preview::Preview::None
    };
    let body = if collapsible && collapsed {
        body_preview::render_body(body_children, surface, &send, blobs, preview)
    } else {
        body_preview::render_full(body_children, surface, &send, blobs)
    };
    let chevron = if collapsed {
        IconType::ChevronRight
    } else {
        IconType::ChevronDown
    };
    // The disclosure trigger owns only the chevron and, without an extracted
    // head node, the plain label. A head node can carry its own actions, so it
    // is rendered beside the trigger as an independent target rather than
    // inside a button.
    let trigger_label = if head_text.is_empty() {
        node.p
            .get("took")
            .and_then(Value::as_f64)
            .map(|took| format!("Thought for {}", thought_seconds(took)))
            .unwrap_or_else(|| "Details".to_owned())
    } else {
        head_text.clone()
    };
    let trigger_head = rsx! {
        span { class: "flex min-w-0 flex-wrap items-center gap-2",
            Icon { icon: chevron, class: Some("size-3.5 text-muted-foreground".to_owned()) }
            if head_node.is_none() {
                span { class: "min-w-0 break-words text-sm font-medium", "{trigger_label}" }
            } else {
                span { class: "sr-only", "{trigger_label}" }
            }
        }
    };
    let content = if collapsible {
        let sf = surface.to_owned();
        let id = node.id.clone();
        rsx! {
            Collapsible {
                open: !collapsed,
                on_open_change: move |open: bool| send.call(event(
                    "toggle",
                    &sf,
                    &id,
                    json!({ "collapsed": !open }),
                )),
                div { class: "flex min-w-0 items-center gap-2",
                    CollapsibleTrigger {
                        size: ButtonSize::Sm,
                        variant: ButtonVariant::Ghost,
                        class: "h-auto min-w-0 shrink-0 justify-start whitespace-normal px-0 text-left",
                        {trigger_head}
                    }
                    if let Some(child) = head_node {
                        div { class: "min-w-0 flex-1",
                            {super::render_node(child, surface, &send, blobs)}
                        }
                    }
                }
                if collapsed {
                    div { class: "min-w-0", {body} }
                } else {
                    CollapsibleContent { class: "min-w-0", {body} }
                }
            }
        }
    } else {
        rsx! {
            if let Some(child) = head_node {
                div { class: "min-w-0", {super::render_node(child, surface, &send, blobs)} }
            } else if !head_text.is_empty() {
                p { class: "min-w-0 break-words text-sm font-medium", "{head_text}" }
            }
            {body}
        }
    };
    // The terminal paints cards as borderless sections; a tinted panel here
    // would break the transcript's single-line rhythm.
    rsx! { div { class: "flex min-w-0 flex-col gap-2", {content} } }
}

/// A structural box inside a line window. The selected children replace the
/// original list; nested cards and sections flatten to borderless sections, as
/// the native terminal does inside a tool frame.
pub(super) fn render_window_box(
    node: &OmpUiNode,
    children: impl Iterator<Item = Element>,
) -> Element {
    if matches!(node.k.as_str(), "card" | "section") {
        let head = if head_refers_to_child(node) {
            String::new()
        } else {
            node.p.get("head").map(text_value).unwrap_or_default()
        };
        return rsx! {
            div { class: "flex min-w-0 flex-col gap-2",
                if !head.is_empty() {
                    p { class: "min-w-0 break-words text-sm font-medium", "{head}" }
                }
                for child in children { {child} }
            }
        };
    }
    let classes = box_classes(node);
    rsx! { div { class: "{classes}", for child in children { {child} } } }
}

fn head_refers_to_child(node: &OmpUiNode) -> bool {
    node.p
        .get("head")
        .and_then(Value::as_str)
        .is_some_and(|id| node.c.iter().any(|child| child.id == id))
}

fn list_max_lines(node: &OmpUiNode) -> Option<u64> {
    match node.p.get("max")? {
        Value::Number(lines) => lines.as_u64(),
        Value::Object(bounds) => bounds.get("lines").and_then(Value::as_u64),
        _ => None,
    }
}

fn box_classes(node: &OmpUiNode) -> String {
    let gap = match node.p.get("gap").and_then(Value::as_str).unwrap_or("none") {
        "xs" => "gap-1",
        "sm" => "gap-2",
        "md" => "gap-4",
        "lg" => "gap-6",
        _ => "gap-0",
    };
    let align = match node
        .p
        .get("align")
        .and_then(Value::as_str)
        .unwrap_or("stretch")
    {
        "start" => "items-start",
        "center" => "items-center",
        "end" => "items-end",
        "baseline" => "items-baseline",
        _ => "items-stretch",
    };
    let justify = match node.p.get("justify").and_then(Value::as_str) {
        Some("end") => "justify-end",
        Some("between") => "justify-between",
        _ => "justify-start",
    };
    let direction = if node.k == "row" {
        "flex-row"
    } else {
        "flex-col"
    };
    let wrap = if node.p.get("wrap").and_then(Value::as_bool) == Some(true) {
        "flex-wrap"
    } else {
        "flex-nowrap"
    };
    format!("flex min-w-0 {direction} {gap} {align} {justify} {wrap}")
}

/// Native thinking head duration, as `thoughtLabel` writes it.
fn thought_seconds(milliseconds: f64) -> String {
    let seconds = milliseconds.max(0.0) / 1000.0;
    if seconds >= 60.0 {
        format!("{}m {:.0}s", (seconds / 60.0).floor(), seconds % 60.0)
    } else {
        format!("{}s", (seconds.round() as u64).max(1))
    }
}

pub(super) fn spacer(node: &OmpUiNode) -> Element {
    let class = match node.p.get("size").and_then(Value::as_str).unwrap_or("sm") {
        "none" => "h-0 w-0",
        "xs" => "h-1 w-1",
        "md" => "h-4 w-4",
        "lg" => "h-6 w-6",
        _ => "h-2 w-2",
    };
    rsx! { div { class: "shrink-0 {class}", aria_hidden: "true" } }
}

pub(super) fn common_style(props: &serde_json::Map<String, Value>) -> String {
    let mut style = String::new();
    for (key, css) in [("grow", "flex-grow"), ("shrink", "flex-shrink")] {
        if let Some(value) = props
            .get(key)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 0.0)
        {
            use std::fmt::Write;
            let _ = write!(style, "{css}:{value};");
        }
    }
    if let Some(basis) = props.get("basis") {
        match basis.as_str() {
            Some("auto") => style.push_str("flex-basis:auto;"),
            Some("content") => style.push_str("flex-basis:content;"),
            _ => {
                if let Some(value) = basis
                    .as_f64()
                    .filter(|value| value.is_finite() && *value >= 0.0)
                {
                    use std::fmt::Write;
                    let _ = write!(style, "flex-basis:{}%;", value * 100.0);
                }
            }
        }
    }
    for (bound, prefix) in [("min", "min"), ("max", "max")] {
        for (axis, css) in [("w", "width"), ("h", "height")] {
            if let Some(extent) = props
                .get(bound)
                .and_then(|value| value.get(axis))
                .and_then(extent_css)
            {
                use std::fmt::Write;
                let _ = write!(style, "{prefix}-{css}:{extent};");
            }
        }
    }
    style
}

fn extent_css(value: &Value) -> Option<String> {
    if let Some(fraction) = value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0)
    {
        return Some(format!("{}%", fraction * 100.0));
    }
    let text = value.as_str()?;
    let (number, unit) = if let Some(number) = text.strip_suffix("lines") {
        (number, "lh")
    } else {
        (text.strip_suffix("ch")?, "ch")
    };
    let number = number
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)?;
    Some(format!("{number}{unit}"))
}
