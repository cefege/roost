//! Leaf renderers for simple semantic data kinds.
//! Structured tables, trees, charts, and meters live in their own sibling modules.
use super::{duration, prop_text, text_value, tone_variant};
use crate::components::ui::{Badge, Icon, IconType, Kbd, Progress, Spinner, StatusDot};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::Value;

pub(super) fn render(node: &OmpUiNode) -> Element {
    match node.k.as_str() {
        "kv" | "table" => super::data_table::render(node),
        "tree" | "json" => super::data_table::render(node),
        "chart" => super::data_chart::render(node),
        "meter" | "effort" => super::data_metrics::render(node),
        "badge" => badge(node),
        "kbd" => kbd(node),
        "icon" => icon(node),
        "spinner" => spinner(node),
        "elapsed" => elapsed(node),
        "progress" => progress(node),
        "rate" => rate(node),
        "status" => status(node),
        "rows" => rows(node),
        _ => rsx! { span {} },
    }
}

fn badge(node: &OmpUiNode) -> Element {
    let text = prop_text(node, "text");
    let variant = tone_variant(node.p.get("tone").and_then(Value::as_str));
    rsx! { Badge { variant, "{text}" } }
}

pub(super) fn icon_element(name: &str, size: &str) -> Element {
    let icon = match name {
        "chev-r" | "chevron-right" => IconType::ChevronRight,
        "chev" | "chevron-down" => IconType::ChevronDown,
        "check" => IconType::Check,
        "copy" => IconType::Copy,
        "link" => IconType::Link,
        "warn" | "alert" => IconType::TriangleAlert,
        "history" => IconType::History,
        "search" => IconType::Search,
        "bash" | "terminal" => IconType::Terminal,
        "read" | "file" => IconType::FileText,
        "task" => IconType::ListTree,
        "wait" | "clock" => IconType::Clock,
        "job" | "jobs" => IconType::Briefcase,
        "folder" => IconType::Folder,
        "folder-open" => IconType::FolderOpen,
        "users" => IconType::Users,
        "lightbulb" => IconType::Lightbulb,
        "branch" | "git-branch" => IconType::GitBranch,
        "context.speculation" => IconType::Sparkles,
        "context.compaction" => IconType::Archive,
        _ => IconType::CircleHelp,
    };
    rsx! { Icon { icon, class: Some(size.to_owned()) } }
}

fn icon(node: &OmpUiNode) -> Element {
    let name = prop_text(node, "name");
    let class = match node.p.get("tone").and_then(Value::as_str) {
        Some("success") => "text-success",
        Some("warning") => "text-warning",
        Some("error") => "text-destructive",
        Some("info") => "text-info",
        Some("accent") => "text-primary",
        _ => "text-muted-foreground",
    };
    let rendered = icon_element(&name, "size-4");
    rsx! { span { class: "{class}", title: "{name}", {rendered} } }
}

fn kbd(node: &OmpUiNode) -> Element {
    let keys = node.p.get("keys").and_then(Value::as_array);
    rsx! {
        crate::components::ui::KbdGroup {
            for key in keys.into_iter().flatten() { Kbd { "{text_value(key)}" } }
        }
    }
}

fn spinner(node: &OmpUiNode) -> Element {
    let label = prop_text(node, "label");
    rsx! {
        span { class: "inline-flex items-center gap-2",
            Spinner {}
            if !label.is_empty() { span { class: "text-sm", "{label}" } }
        }
    }
}

fn elapsed(node: &OmpUiNode) -> Element {
    let age = node.p.get("age").and_then(Value::as_f64).unwrap_or(0.0);
    let stopped = node.p.get("stopped").and_then(Value::as_f64);
    let milliseconds = stopped.unwrap_or(age);
    let format = node
        .p
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("short");
    let label = if format == "clock" {
        let seconds = (milliseconds.max(0.0) / 1000.0).floor() as u64;
        format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        )
    } else {
        duration(milliseconds)
    };
    rsx! {
        time {
            "{label}"
            if stopped.is_none() { span { class: "sr-only", " elapsed and counting" } }
        }
    }
}

fn progress(node: &OmpUiNode) -> Element {
    let value = node.p.get("value").and_then(Value::as_f64);
    let label = match prop_text(node, "label") {
        label if label.is_empty() => "Progress".to_owned(),
        label => label,
    };
    if let Some(value) = value {
        rsx! { Progress { value: Some(value.clamp(0.0, 1.0)), label, class: Some("w-full".into()) } }
    } else {
        rsx! {
            div { class: "flex items-center gap-2",
                Progress { value: None, label: label.clone(), class: Some("w-full".into()) }
                span { class: "text-xs text-muted-foreground", "Indeterminate" }
            }
        }
    }
}

fn rate(node: &OmpUiNode) -> Element {
    let value = node.p.get("value").and_then(Value::as_f64).unwrap_or(0.0);
    let unit = prop_text(node, "unit");
    rsx! { span { class: "text-sm tabular-nums", "{value} {unit}" } }
}

fn status(node: &OmpUiNode) -> Element {
    let text = node
        .p
        .get("spans")
        .or_else(|| node.p.get("text"))
        .map(text_value)
        .unwrap_or_default();
    let label = if text.is_empty() {
        prop_text(node, "label")
    } else {
        text
    };
    let status = node
        .p
        .get("tone")
        .and_then(Value::as_str)
        .unwrap_or("muted")
        .to_owned();
    let transparent = node
        .p
        .get("transparent")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    rsx! {
        span { class: "inline-flex items-center gap-2",
            StatusDot { status, hollow: transparent, title: None }
            span { class: "text-sm", "{label}" }
        }
    }
}

fn rows(node: &OmpUiNode) -> Element {
    let lines = node.p.get("lines").and_then(Value::as_array);
    rsx! {
        div { class: "flex flex-col gap-1",
            for line in lines.into_iter().flatten() {
                p { class: "whitespace-pre-wrap text-sm", "{text_value(line)}" }
            }
        }
    }
}
