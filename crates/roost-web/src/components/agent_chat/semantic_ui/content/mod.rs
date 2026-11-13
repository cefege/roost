//! Data-first renderers for semantic TSP transcript nodes.
//! The parent owns interactive settings and editor controls; this module owns
//! content nodes and sends their declared actions to OMP unchanged.
mod activity;
mod data;
mod data_chart;
mod data_metrics;
mod data_table;
mod data_tone;
mod text;
mod tool;

pub(super) use text::{
    editor_style_classes, editor_style_supported, render_editor_run, render_spans,
};

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
    if node.p.get("hidden").and_then(Value::as_bool) == Some(true) {
        return Some(rsx! {});
    }
    let element = match node.k.as_str() {
        "tool" | "checklist" | "agent" => activity::render(node, surface, send, blobs),
        "status" => data::render_status(
            node,
            node.c
                .iter()
                .map(|child| super::render_node(child, surface, send, blobs))
                .collect(),
        ),
        "seg" => segment(node, expanded, render_children(node, surface, send, blobs)),
        _ => return render_leaf(node, expanded, surface),
    };
    Some(element)
}

pub(super) fn render_readonly(
    node: &OmpUiNode,
    expanded: bool,
    surface: &str,
    children: Element,
) -> Option<Element> {
    if node.p.get("hidden").and_then(Value::as_bool) == Some(true) {
        return Some(rsx! {});
    }
    Some(match node.k.as_str() {
        "tool" | "checklist" | "agent" => activity::render_readonly(node, surface, children),
        "seg" => segment(node, expanded, children),
        _ => return render_leaf(node, expanded, surface),
    })
}

/// A status strip measures each segment, so read-only callers hand over one element per child.
pub(super) fn render_readonly_status(node: &OmpUiNode, children: Vec<Element>) -> Element {
    data::render_status(node, children)
}

fn render_leaf(node: &OmpUiNode, expanded: bool, surface: &str) -> Option<Element> {
    Some(match node.k.as_str() {
        "text" | "shimmer" | "md" | "code" | "diff" | "ansi" | "math" => {
            text::render(node, expanded)
        }
        "kv" | "table" | "tree" | "json" | "badge" | "kbd" | "icon" | "spinner" | "elapsed"
        | "progress" | "rate" | "chart" | "meter" | "effort" | "rows" => {
            data::render(node, surface)
        }
        _ => return None,
    })
}

fn segment(node: &OmpUiNode, expanded: bool, children: Element) -> Element {
    rsx! {
        span { class: "inline-flex min-w-0 items-center gap-1",
            {text::render(node, expanded)}
            {children}
        }
    }
}

pub(super) fn line_count(node: &OmpUiNode) -> Option<usize> {
    text::line_count(node)
}

pub(super) fn render_window(node: &OmpUiNode, range: std::ops::Range<usize>) -> Element {
    text::render_window(node, range)
}

pub(super) fn text_is_empty(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.is_empty(),
        Some(Value::Array(spans)) => spans.iter().all(|span| {
            span.get("t")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        }),
        _ => false,
    }
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
    if !milliseconds.is_finite() || milliseconds <= 0.0 {
        return "0ms".to_owned();
    }
    if milliseconds < 1_000.0 {
        return format!("{:.0}ms", milliseconds.floor());
    }
    if milliseconds < 60_000.0 {
        let tenths = (milliseconds / 100.0).round();
        return if tenths < 600.0 {
            format!("{:.1}s", tenths / 10.0)
        } else {
            "1m".to_owned()
        };
    }
    let (unit, unit_ms, remainder, remainder_ms) = if milliseconds < 3_600_000.0 {
        ("m", 60_000.0, "s", 1_000.0)
    } else if milliseconds < 86_400_000.0 {
        ("h", 3_600_000.0, "m", 60_000.0)
    } else {
        ("d", 86_400_000.0, "h", 3_600_000.0)
    };
    let count = (milliseconds / unit_ms).floor();
    let rest = ((milliseconds % unit_ms) / remainder_ms).floor();
    if rest > 0.0 {
        format!("{count:.0}{unit}{rest:.0}{remainder}")
    } else {
        format!("{count:.0}{unit}")
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

#[cfg(test)]
mod tests {
    use super::duration;

    #[test]
    fn elapsed_labels_preserve_native_unit_boundaries_and_second_precision() {
        for (milliseconds, expected) in [
            (f64::NAN, "0ms"),
            (-1.0, "0ms"),
            (0.0, "0ms"),
            (999.9, "999ms"),
            (1_000.0, "1.0s"),
            (6_000.0, "6.0s"),
            (59_949.0, "59.9s"),
            (59_950.0, "1m"),
            (60_000.0, "1m"),
            (61_999.0, "1m1s"),
            (3_600_000.0, "1h"),
            (3_660_000.0, "1h1m"),
            (86_400_000.0, "1d"),
            (90_000_000.0, "1d1h"),
        ] {
            assert_eq!(duration(milliseconds), expected);
        }
    }
}
