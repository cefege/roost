//! Native checklists and subagent rows.
//!
//! OMP owns phase, item and agent state; this file presents it and forwards the
//! one disclosure event each row owns. Tool cards live in `tool.rs`; the head
//! here shows only source-authored facts, never raw schema fields.

use super::{duration, event, prop_text, render_children, text_value};
use crate::components::ui::{
    Badge, BadgeSize, BadgeVariant, ButtonSize, ButtonVariant, Card, CardSize, Collapsible,
    CollapsibleContent, CollapsibleTrigger, Icon, IconType, Progress,
};
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiBlob, OmpUiNode};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) fn render(
    node: &OmpUiNode,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    match node.k.as_str() {
        "tool" => super::tool::render(node, surface, send, blobs),
        "checklist" => checklist(node, surface, send),
        _ => agent(node, surface, send, blobs),
    }
}

fn checklist(node: &OmpUiNode, surface: &str, send: &EventHandler<Value>) -> Element {
    let mode = prop_text(node, "mode");
    let note = prop_text(node, "note");
    let phases = node.p.get("phases").and_then(Value::as_array);
    let flat = matches!(mode.as_str(), "reminder" | "hud");
    let note_class = if mode == "reminder" {
        "text-xs text-warning"
    } else {
        "text-xs text-muted-foreground"
    };
    rsx! {
        section { class: "flex min-w-0 flex-col gap-2",
            if !note.is_empty() {
                span { class: "{note_class}", "{note}" }
            }
            for phase in phases.into_iter().flatten() {
                {phase_block(phase, &mode, flat, node, surface, send)}
            }
        }
    }
}

fn phase_block(
    phase: &Value,
    mode: &str,
    flat: bool,
    node: &OmpUiNode,
    surface: &str,
    send: &EventHandler<Value>,
) -> Element {
    let id = phase
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let title = phase.get("title").map(text_value).unwrap_or_default();
    let items = phase.get("items").and_then(Value::as_array);
    let collapsed = phase
        .get("collapsed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let total = items.map_or(0, Vec::len);
    let closed = items
        .into_iter()
        .flatten()
        .filter(|item| matches!(item_status(item), "done" | "dropped"))
        .count();
    let visible: Vec<&Value> = items
        .into_iter()
        .flatten()
        .filter(|item| mode != "reminder" || !matches!(item_status(item), "done" | "dropped"))
        .collect();
    let list = rsx! {
        ul { class: "flex min-w-0 flex-col gap-1",
            for item in &visible { {checklist_item(item)} }
        }
    };
    let heading = if title.is_empty() {
        rsx! {}
    } else {
        rsx! {
            div { class: "flex min-w-0 items-center gap-2",
                strong { class: "min-w-0 break-words text-sm font-medium", "{title}" }
                if total > 0 {
                    span { class: "text-xs text-muted-foreground", "{closed}/{total}" }
                }
            }
        }
    };
    if flat || !collapsed || title.is_empty() {
        return rsx! { div { class: "flex min-w-0 flex-col gap-1", {heading} {list} } };
    }
    let sf = surface.to_owned();
    let node_id = node.id.clone();
    let phase_id = id.clone();
    let toggle_send = *send;
    rsx! {
        Collapsible {
            open: false,
            on_open_change: move |open: bool| toggle_send.call(event(
                "toggle",
                &sf,
                &node_id,
                json!({ "collapsed": !open, "key": phase_id }),
            )),
            CollapsibleTrigger {
                size: ButtonSize::Sm,
                variant: ButtonVariant::Ghost,
                class: "h-auto min-w-0 justify-start whitespace-normal px-0 text-left",
                {heading}
            }
            CollapsibleContent { class: "min-w-0", {list} }
        }
    }
}

fn checklist_item(item: &Value) -> Element {
    let text = item.get("text").map(text_value).unwrap_or_default();
    let status = item_status(item);
    let note = item.get("note").map(text_value).unwrap_or_default();
    let text_class = match status {
        "done" | "dropped" => "min-w-0 break-words text-sm text-muted-foreground line-through",
        "active" => "min-w-0 break-words text-sm font-medium text-foreground",
        _ => "min-w-0 break-words text-sm text-muted-foreground",
    };
    let note_class = if status == "blocked" {
        "text-xs text-warning"
    } else {
        "text-xs text-muted-foreground"
    };
    rsx! {
        li { class: "flex min-w-0 items-start gap-2",
            span { class: "mt-0.5 shrink-0", {checklist_glyph(status)} }
            div { class: "flex min-w-0 flex-1 flex-col gap-0.5",
                span { class: "{text_class}", "{text}" }
                span { class: "sr-only", "{status}" }
                if !note.is_empty() { p { class: "{note_class}", "{note}" } }
            }
        }
    }
}

fn checklist_glyph(status: &str) -> Element {
    let (icon, class) = match status {
        "done" => (IconType::Check, "size-3.5 text-success"),
        "active" => (IconType::Circle, "size-3.5 text-primary"),
        "blocked" => (IconType::TriangleAlert, "size-3.5 text-warning"),
        "dropped" => (IconType::Minus, "size-3.5 text-destructive"),
        _ => (IconType::Circle, "size-3.5 text-muted-foreground"),
    };
    rsx! { Icon { icon, class: Some(class.to_owned()) } }
}

fn item_status(item: &Value) -> &str {
    item.get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending")
}

fn agent(
    node: &OmpUiNode,
    surface: &str,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Element {
    let name = match prop_text(node, "name") {
        name if !name.is_empty() => name,
        _ => "Agent".to_owned(),
    };
    let kind = prop_text(node, "agent");
    let task = prop_text(node, "task");
    let status = prop_text(node, "status");
    let model = prop_text(node, "model");
    let thinking = prop_text(node, "thinking");
    let collapsed = node
        .p
        .get("collapsed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let collapsible = node
        .p
        .get("collapsible")
        .and_then(Value::as_bool)
        .unwrap_or(!node.c.is_empty());
    let stats = node.p.get("stats");
    let done = stats
        .and_then(|stats| stats.get("done"))
        .and_then(Value::as_f64);
    let current_tool = node.p.get("tool").filter(|value| !value.is_null());
    let retry = node.p.get("retry").filter(|value| !value.is_null());
    let badges = node.p.get("badges").and_then(Value::as_array);
    let stats_line = agent_stats(stats);
    let tool_line = current_tool.map(|tool| {
        let name = tool.get("name").and_then(Value::as_str).unwrap_or("tool");
        let intent = tool.get("intent").map(text_value).unwrap_or_default();
        let age = tool.get("age").and_then(Value::as_f64).map(duration);
        (name.to_owned(), intent, age)
    });
    let retry_line = retry.map(|retry| {
        let attempt = retry.get("attempt").map(text_value).unwrap_or_default();
        let max = retry.get("max").map(text_value).unwrap_or_default();
        let delay = retry.get("delay").map(text_value).unwrap_or_default();
        let error = retry.get("error").and_then(Value::as_str).unwrap_or("");
        (attempt, max, delay, error.to_owned())
    });
    let head = rsx! {
        div { class: "flex min-w-0 flex-wrap items-center gap-2",
            strong { class: "min-w-0 break-words text-sm", "{name}" }
            {agent_status(&status)}
            if !kind.is_empty() { Badge { variant: BadgeVariant::Muted, size: BadgeSize::Sm, "{kind}" } }
            if !model.is_empty() { Badge { variant: BadgeVariant::Outline, size: BadgeSize::Sm, "{model}" } }
            if !thinking.is_empty() {
                {thinking_badge(&thinking)}
            }
            for badge in badges.into_iter().flatten() {
                {
                    let text = badge.get("text").map(text_value).unwrap_or_default();
                    let variant = super::tone_variant(badge.get("tone").and_then(Value::as_str));
                    rsx! { Badge { variant, size: BadgeSize::Sm, "{text}" } }
                }
            }
        }
    };
    let body = rsx! {
        div { class: "flex min-w-0 flex-col gap-2",
            if collapsible {
                CollapsibleTrigger {
                    size: ButtonSize::Sm,
                    variant: ButtonVariant::Ghost,
                    class: "h-auto min-w-0 justify-start whitespace-normal px-0 text-left",
                    {head}
                }
            } else {
                {head}
            }
            if !stats_line.is_empty() {
                p { class: "text-xs text-muted-foreground", "{stats_line}" }
            }
            if !task.is_empty() { p { class: "min-w-0 break-words text-sm", "{task}" } }
            if let Some((tool, intent, age)) = tool_line {
                p { class: "text-sm text-muted-foreground",
                    "Running {tool}"
                    if !intent.is_empty() { span { ": {intent}" } }
                    if let Some(age) = age { span { " · {age}" } }
                }
            }
            if let Some((attempt, max, delay, error)) = retry_line {
                p { class: "text-xs text-warning",
                    "Retry {attempt}/{max} in {delay} ms"
                    if !error.is_empty() { span { " · {error}" } }
                }
            }
            if let Some(done) = done {
                Progress { value: Some(done), label: "Agent completion".to_owned() }
            }
            if collapsible {
                CollapsibleContent { class: "min-w-0",
                    div { class: "mt-2 flex flex-col gap-2", {render_children(node, surface, send, blobs)} }
                }
            }
        }
    };
    let framed = if collapsible {
        let sf = surface.to_owned();
        let id = node.id.clone();
        let toggle_send = *send;
        rsx! {
            Collapsible {
                open: !collapsed,
                on_open_change: move |open: bool| toggle_send.call(event(
                    "toggle",
                    &sf,
                    &id,
                    json!({ "collapsed": !open }),
                )),
                {body}
            }
        }
    } else {
        body
    };
    rsx! { Card { size: CardSize::Sm, class: Some("min-w-0 p-3".to_owned()), {framed} } }
}

fn agent_status(status: &str) -> Element {
    match status {
        "" => rsx! {},
        "pending" => {
            rsx! { Badge { variant: BadgeVariant::Muted, size: BadgeSize::Sm, "pending" } }
        }
        "running" => rsx! { Badge { variant: BadgeVariant::Info, size: BadgeSize::Sm, "running" } },
        "done" => rsx! { Badge { variant: BadgeVariant::Success, size: BadgeSize::Sm, "done" } },
        "failed" => {
            rsx! { Badge { variant: BadgeVariant::Destructive, size: BadgeSize::Sm, "failed" } }
        }
        "aborted" => {
            rsx! { Badge { variant: BadgeVariant::Warning, size: BadgeSize::Sm, "aborted" } }
        }
        "idle" => rsx! { Badge { variant: BadgeVariant::Muted, size: BadgeSize::Sm, "idle" } },
        "parked" => rsx! { Badge { variant: BadgeVariant::Muted, size: BadgeSize::Sm, "parked" } },
        other => rsx! { Badge { variant: BadgeVariant::Muted, size: BadgeSize::Sm, "{other}" } },
    }
}

/// The wire's `thinkingLow` token becomes the level a reader recognises; the
/// dot-native terminal shows only a colour, so the web keeps the level in a
/// small outline badge instead of echoing the token.
fn thinking_badge(token: &str) -> Element {
    let level = token
        .strip_prefix("thinking")
        .filter(|level| !level.is_empty())
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| token.to_ascii_lowercase());
    rsx! {
        span { title: "Thinking level: {level}",
            Badge { variant: BadgeVariant::Outline, size: BadgeSize::Sm, "{level}" }
        }
    }
}

fn agent_stats(stats: Option<&Value>) -> String {
    let Some(stats) = stats else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for (key, label) in [("tools", "tools"), ("requests", "requests")] {
        if let Some(value) = stats.get(key).filter(|value| !value.is_null()) {
            parts.push(format!("{} {label}", text_value(value)));
        }
    }
    if let Some(label) = stats.get("contextLabel").and_then(Value::as_str) {
        parts.push(format!("context {label}"));
    } else if let Some(value) = stats.get("context").and_then(Value::as_f64) {
        parts.push(format!("context {:.0}%", value * 100.0));
    } else if let Some(value) = stats.get("tokens").filter(|value| !value.is_null()) {
        parts.push(format!("{} tokens", text_value(value)));
    }
    if let Some(value) = stats.get("cost").and_then(Value::as_f64) {
        parts.push(format!("${value:.2}"));
    }
    if let Some(value) = stats.get("took").and_then(Value::as_f64) {
        parts.push(format!("took {}", duration(value)));
    } else if let Some(value) = stats.get("age").and_then(Value::as_f64) {
        parts.push(format!("running {}", duration(value)));
    }
    parts.join(" · ")
}
