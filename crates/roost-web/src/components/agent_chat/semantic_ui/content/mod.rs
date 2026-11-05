//! Data-first renderers for semantic TSP transcript nodes.
//! The parent owns interactive settings and editor controls; this module owns
//! content nodes and sends their declared actions to OMP unchanged.
mod activity;
mod data;
mod data_chart;
mod data_metrics;
mod data_table;
mod text;
mod tool;

pub(super) use text::render_spans;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiBlob, OmpUiNode};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) fn render_content(
    node: &OmpUiNode,
    expanded: bool,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Option<Element> {
    let hidden = node.p.get("hidden").and_then(Value::as_bool) == Some(true);
    let element = match node.k.as_str() {
        "tool" | "checklist" | "agent" => {
            if hidden {
                rsx! {}
            } else {
                activity::render(node, surface, send, blobs)
            }
        }
        "text" | "shimmer" | "seg" | "md" | "code" | "diff" | "ansi" | "math" => {
            if hidden {
                rsx! {}
            } else {
                text::render(node, expanded)
            }
        }
        "kv" | "table" | "tree" | "json" | "badge" | "kbd" | "icon" | "spinner" | "elapsed"
        | "progress" | "rate" | "status" | "chart" | "meter" | "effort" | "rows" => {
            if hidden {
                rsx! {}
            } else {
                data::render(node)
            }
        }
        _ => return None,
    };
    Some(element)
}

pub(super) fn line_count(node: &OmpUiNode) -> Option<usize> {
    text::line_count(node)
}

pub(super) fn render_window(node: &OmpUiNode, range: std::ops::Range<usize>) -> Element {
    text::render_window(node, range)
}

pub(super) fn render_icon(name: &str, size: &str) -> Element {
    data::icon_element(name, size)
}

pub(super) fn text_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(spans) => spans
            .iter()
            .filter_map(|span| span.get("t").and_then(Value::as_str))
            .collect(),
        Value::Null => String::new(),
        _ => value.to_string(),
    }
}

pub(super) fn prop_text(node: &OmpUiNode, key: &str) -> String {
    node.p.get(key).map(text_value).unwrap_or_default()
}

pub(super) fn event(verb: &str, surface: &str, id: &str, fields: Value) -> Value {
    let mut event = match fields {
        Value::Object(object) => object,
        _ => serde_json::Map::new(),
    };
    event.insert("ev".into(), json!(verb));
    event.insert("sf".into(), json!(surface));
    event.insert("id".into(), json!(id));
    json!({"method":"event","params":{"event":event}})
}

pub(super) fn render_children(
    node: &OmpUiNode,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    rsx! { for child in &node.c { {super::render_node(child, surface, send, blobs)} } }
}

pub(super) fn duration(milliseconds: f64) -> String {
    let seconds = milliseconds.max(0.0) / 1000.0;
    if seconds >= 60.0 {
        format!("{}m {:.0}s", (seconds / 60.0).floor(), seconds % 60.0)
    } else {
        format!("{seconds:.1}s")
    }
}

pub(super) fn tone_variant(tone: Option<&str>) -> crate::components::ui::BadgeVariant {
    use crate::components::ui::BadgeVariant;
    match tone {
        Some("success") => BadgeVariant::Success,
        Some("error") => BadgeVariant::Destructive,
        Some("warning") => BadgeVariant::Warning,
        Some("info") => BadgeVariant::Info,
        Some("accent") => BadgeVariant::Accent,
        _ => BadgeVariant::Muted,
    }
}
