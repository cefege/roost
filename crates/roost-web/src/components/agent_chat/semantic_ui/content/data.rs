//! Leaf renderers for simple semantic data kinds.
//! Structured tables, trees, charts, and meters live in their own sibling modules.
use super::super::elapsed_clock::{NativeElapsed, PropElapsed};
use super::{prop_text, render_spans, text_value, tone_variant};
use crate::components::agent_chat::output_lines::{render_run, styled_rows};
use crate::components::ui::{Badge, Icon, IconType, Kbd, Progress, Spinner, StatusDot};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::Value;

#[path = "status.rs"]
mod status;

pub(super) fn render(node: &OmpUiNode, surface: &str) -> Element {
    match node.k.as_str() {
        "kv" | "table" => super::data_table::render(node),
        "tree" | "json" => super::data_table::render(node),
        "chart" => super::data_chart::render(node),
        "meter" | "effort" => super::data_metrics::render(node),
        "badge" => badge(node),
        "kbd" => kbd(node),
        "icon" => icon(node),
        "spinner" => spinner(node),
        "elapsed" => elapsed(node, surface),
        "progress" => progress(node),
        "rate" => rate(node),
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
        "chev-up" | "chevron-up" => IconType::ChevronUp,
        "chev-l" | "chevron-left" => IconType::ChevronLeft,
        "check" => IconType::Check,
        "copy" => IconType::Copy,
        "x" | "close" => IconType::X,
        "type" => IconType::Type,
        "corner-down-right" => IconType::CornerDownRight,
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
        "appearance" => IconType::Palette,
        "model" => IconType::Bot,
        "interaction" | "keyboard" => IconType::Keyboard,
        "context" => IconType::Layers,
        "memory" => IconType::Brain,
        "files" => IconType::FolderOpen,
        "shell" => IconType::Terminal,
        "tools" => IconType::Wrench,
        "tasks" => IconType::ListChecks,
        "providers" => IconType::Cloud,
        "plugins" => IconType::Package,
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
            for key in keys.into_iter().flatten() { Kbd { "{super::super::key_hint::keycap(&text_value(key))}" } }
        }
    }
}

fn spinner(node: &OmpUiNode) -> Element {
    let label = node.p.get("label");
    rsx! {
        span { class: "inline-flex items-center gap-2",
            Spinner {}
            {render_spans(label, "")}
        }
    }
}

fn elapsed(node: &OmpUiNode, surface: &str) -> Element {
    let format = node
        .p
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or("short")
        .to_owned();
    if node.id.is_empty() {
        let number = |key: &str| {
            node.p
                .get(key)
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite())
        };
        return match number("age") {
            Some(age) => rsx! { PropElapsed { age, stopped: number("stopped"), format } },
            None => {
                rsx! { span { role: "alert", class: "text-destructive", "OMP timer unavailable" } }
            }
        };
    }
    rsx! { NativeElapsed { surface: surface.to_owned(), node: node.id.clone(), format } }
}

fn progress(node: &OmpUiNode) -> Element {
    let value = node.p.get("value").and_then(Value::as_f64);
    let native_label = node.p.get("label");
    let label = native_label
        .map(text_value)
        .filter(|label| !label.is_empty())
        .or_else(|| {
            node.p
                .get("title")
                .map(text_value)
                .filter(|title| !title.is_empty())
        })
        .unwrap_or_else(|| "Progress".to_owned());
    let tone = node.p.get("tone").and_then(Value::as_str);
    rsx! {
        div { class: "flex min-w-0 flex-col gap-1",
            {render_spans(native_label, "")}
            div { class: "flex min-w-0 items-center gap-2",
                if let Some(tone) = tone {
                    span { role: "img", "aria-label": "{tone}",
                        StatusDot { status: progress_status(tone).to_owned(), title: Some(tone.to_owned()) }
                    }
                }
                Progress { value: value.map(|value| value.clamp(0.0, 1.0)), label, class: Some("w-full".into()) }
            }
        }
    }
}

fn progress_status(tone: &str) -> &'static str {
    match tone {
        "success" => "ok",
        "warning" => "warn",
        "error" => "error",
        "info" => "info",
        "accent" => "running",
        _ => "idle",
    }
}

fn rate(node: &OmpUiNode) -> Element {
    let value = node.p.get("value").and_then(Value::as_f64).unwrap_or(0.0);
    let unit = prop_text(node, "unit");
    rsx! { span { class: "text-sm tabular-nums", "{value:.1} {unit}" } }
}

pub(super) fn render_status(node: &OmpUiNode, children: Vec<Element>) -> Element {
    status::render(node, children)
}

fn rows(node: &OmpUiNode) -> Element {
    let rows = node.p.get("lines").and_then(Value::as_array);
    let lines = styled_rows(rows.into_iter().flatten().filter_map(Value::as_str));
    rsx! {
        pre { class: "m-0 max-w-full overflow-x-auto bg-terminal font-mono text-xs text-terminal-foreground",
            for line in &lines {
                div { class: "min-h-lh whitespace-pre",
                    for run in &line.runs { {render_run(run)} }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::progress_status;

    #[test]
    fn progress_keeps_ranked_find_tones_distinct() {
        assert_eq!(progress_status("success"), "ok");
        assert_eq!(progress_status("warning"), "warn");
        assert_eq!(progress_status("muted"), "idle");
        assert_eq!(progress_status("error"), "error");
    }
}
