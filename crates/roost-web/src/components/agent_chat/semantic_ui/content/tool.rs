//! One native tool call: the data head, its declared actions and a body whose
//! visible window comes from OMP's preview prop.
//!
//! OMP owns execution, status and collapse. The head's Copy command is the one
//! host-local behaviour (the command never reaches the worker); every other
//! action id is forwarded unchanged. The enclosing tool card is the only
//! disclosure owner, so children never grow an independent collapse control.

use super::super::body_preview::{self, Preview};
use super::super::links::NativeLink;
use super::render_icon;
use super::{duration, event, prop_text, text_value, tone_variant};
use crate::components::hooks::use_copy_clipboard::use_copy_clipboard;
use crate::components::ui::{
    Badge, BadgeSize, BadgeVariant, Button, ButtonSize, ButtonVariant, Card, CardSize, Collapsible,
    CollapsibleContent, CollapsibleTrigger, Kbd, KbdGroup, Spinner, button_class,
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
    let send = *send;
    let inline = node.p.get("frame").and_then(Value::as_str) == Some("inline");
    let collapsible = node
        .p
        .get("collapsible")
        .and_then(Value::as_bool)
        .unwrap_or(!node.c.is_empty());
    let collapsed = node
        .p
        .get("collapsed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let preview = Preview::for_tool(node);
    let intent = prop_text(node, "intent");
    let status = prop_text(node, "status");
    let live = matches!(status.as_str(), "running" | "pending");
    let target = prop_text(node, "target");
    let copy_text = copy_text(node);
    let body = if !collapsible || !collapsed {
        body_preview::render_full(node.c.iter(), surface, &send, blobs)
    } else {
        body_preview::render_body(node.c.iter(), surface, &send, blobs, preview)
    };
    let head = tool_head(node, &status, &target, &intent, live);
    let details = tool_details(node);
    let actions = tool_actions(node, surface, &send, &copy_text);
    let head_title = if live { String::new() } else { intent.clone() };
    let content = rsx! {
        div { class: "flex min-w-0 flex-col gap-2",
            div { class: "flex min-w-0 flex-wrap items-start gap-2", title: "{head_title}",
                if collapsible {
                    CollapsibleTrigger {
                        size: ButtonSize::Sm,
                        variant: ButtonVariant::Ghost,
                        class: "h-auto min-w-0 flex-1 justify-start gap-2 whitespace-normal px-0 text-left",
                        {head}
                    }
                } else {
                    div { class: "flex min-w-0 flex-1 flex-wrap items-center gap-2", {head} }
                }
                div { class: "ml-auto flex shrink-0 flex-wrap items-center gap-1", {actions} }
            }
            {details}
            if collapsible && !collapsed {
                CollapsibleContent { class: "min-w-0", {body} }
            } else {
                {body}
            }
        }
    };
    let framed = if collapsible {
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
                {content}
            }
        }
    } else {
        content
    };
    if inline {
        rsx! { div { class: "flex min-w-0 flex-col gap-2", {framed} } }
    } else {
        rsx! { Card { size: CardSize::Sm, class: Some("min-w-0 p-3".to_owned()), {framed} } }
    }
}

/// The head verb, state, elapsed time and source badges. The primary argument
/// stays in [`tool_details`] so it is rendered once and can be a real link.
fn tool_head(node: &OmpUiNode, status: &str, target: &str, intent: &str, live: bool) -> Element {
    let title = match prop_text(node, "title") {
        title if !title.is_empty() => title,
        _ => prop_text(node, "name"),
    };
    let exit = node
        .p
        .get("exit")
        .and_then(Value::as_i64)
        .filter(|code| *code != 0);
    let age = node.p.get("age").and_then(Value::as_f64).map(duration);
    let took = node.p.get("took").and_then(Value::as_f64).map(duration);
    let note = prop_text(node, "note");
    let show_note = !note.is_empty() && note != status;
    let badges = node.p.get("badges").and_then(Value::as_array);
    // The native head shows the intent as the working row while live, and as
    // its tooltip afterwards; an intent that repeats the target is already
    // visible and is not repeated here.
    let show_intent = live && !intent.is_empty() && intent != target;
    rsx! {
        div { class: "flex min-w-0 flex-wrap items-center gap-2",
            strong { class: "min-w-0 break-words text-sm", "{title}" }
            {status_chip(status)}
            if let Some(exit) = exit {
                Badge { variant: BadgeVariant::Destructive, size: BadgeSize::Sm, "exit {exit}" }
            }
            if let Some(age) = age {
                span { class: "text-xs text-muted-foreground", title: "Elapsed",
                    time { "{age}" }
                    if status == "running" || status == "pending" { span { class: "sr-only", " elapsed, still running" } }
                }
            }
            if let Some(took) = took {
                span { class: "text-xs text-muted-foreground", title: "Duration",
                    "Took "
                    time { "{took}" }
                }
            }
            for badge in badges.into_iter().flatten() {
                {
                    let text = badge.get("text").map(text_value).unwrap_or_else(|| text_value(badge));
                    let variant = tone_variant(badge.get("tone").and_then(Value::as_str));
                    let title = badge.get("title").and_then(Value::as_str).unwrap_or_default().to_owned();
                    rsx! { span { title: "{title}", Badge { variant, size: BadgeSize::Sm, "{text}" } } }
                }
            }
            if show_note {
                Badge { variant: BadgeVariant::Muted, size: BadgeSize::Sm, "{note}" }
            }
            if show_intent {
                span { class: "min-w-0 break-words text-sm text-muted-foreground", "{intent}" }
            }
        }
    }
}

fn status_chip(status: &str) -> Element {
    match status {
        "" => rsx! {},
        "running" | "pending" => rsx! {
            span { class: "inline-flex items-center gap-1 text-xs text-muted-foreground",
                Spinner { class: Some("size-3".to_owned()) }
                span { class: "sr-only", "{status}" }
            }
        },
        "done" => rsx! {
            span { class: "inline-flex items-center text-success", title: "Done",
                {render_icon("check", "size-3.5")}
                span { class: "sr-only", "done" }
            }
        },
        "error" => {
            rsx! { Badge { variant: BadgeVariant::Destructive, size: BadgeSize::Sm, "error" } }
        }
        "cancelled" => {
            rsx! { Badge { variant: BadgeVariant::Muted, size: BadgeSize::Sm, "cancelled" } }
        }
        other => rsx! { Badge { variant: BadgeVariant::Muted, size: BadgeSize::Sm, "{other}" } },
    }
}

/// The primary argument shown once, followed by the quiet facts OMP authored.
fn tool_details(node: &OmpUiNode) -> Element {
    let target = prop_text(node, "target");
    let kind = prop_text(node, "targetKind");
    let href = prop_text(node, "href");
    let meta = node.p.get("meta").and_then(Value::as_array);
    if target.is_empty() && meta.is_none() {
        return rsx! {};
    }
    let target_line = (!target.is_empty()).then(|| target_text(&target, &kind, &href));
    rsx! {
        div { class: "flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1",
            if let Some(target_line) = target_line { {target_line} }
            for (idx, fact) in meta.into_iter().flatten().enumerate() {
                if idx > 0 { span { class: "text-xs text-muted-foreground", "·" } }
                span { class: "text-xs text-muted-foreground", "{text_value(fact)}" }
            }
        }
    }
}

fn target_text(target: &str, kind: &str, href: &str) -> Element {
    let class = match kind {
        "command" => "min-w-0 break-all font-mono text-xs text-foreground",
        "path" => "min-w-0 break-all font-mono text-xs",
        "pattern" | "query" => "min-w-0 break-all font-mono text-xs text-primary",
        _ => "min-w-0 break-words text-sm text-muted-foreground",
    };
    let label = match kind {
        "pattern" | "query" => format!("\"{target}\""),
        _ => target.to_owned(),
    };
    let body = if kind == "path" {
        let split = target.rfind('/').map_or(0, |idx| idx + 1);
        let (directory, name) = target.split_at(split);
        rsx! {
            if !directory.is_empty() { span { class: "text-muted-foreground", "{directory}" } }
            span { class: "font-semibold", if name.is_empty() { "{target}" } else { "{name}" } }
        }
    } else {
        rsx! { "{label}" }
    };
    if href.is_empty() {
        rsx! { span { class: "{class}", title: "{target}", {body} } }
    } else {
        rsx! {
            NativeLink { href: href.to_owned(), class: Some(class.to_owned()), title: Some(target.to_owned()),
                {body}
            }
        }
    }
}

/// Declared actions plus the implicit Copy command. `copy` and a linked `open`
/// are host-local; every other id returns to OMP unchanged.
fn tool_actions(
    node: &OmpUiNode,
    surface: &str,
    send: &EventHandler<Value>,
    copy_text: &str,
) -> Element {
    let action_send = *send;
    let id = node.id.clone();
    let sf = surface.to_owned();
    let href = prop_text(node, "href");
    let actions = node.p.get("tools").and_then(Value::as_array);
    let has_copy = actions
        .into_iter()
        .flatten()
        .any(|action| action.get("id").and_then(Value::as_str) == Some("copy"));
    rsx! {
        for action in actions.into_iter().flatten() {
            {
                let action_id = action.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
                let label = action.get("label").and_then(Value::as_str).unwrap_or_default().to_owned();
                let disabled_value = action.get("disabled");
                let disabled = disabled_value.is_some_and(|value| value == true || value.is_string());
                let reason = disabled_value.and_then(Value::as_str).unwrap_or_default().to_owned();
                let keys = action.get("keys").and_then(Value::as_array);
                let pressed = action.get("on").and_then(Value::as_bool) == Some(true);
                let variant = action_variant(action);
                let action_event = event("action", &sf, &id, json!({ "act": action_id.clone() }));
                rsx! {
                    div { class: "flex items-center gap-1",
                        if action_id == "copy" {
                            CopyCommand { label: label, command: copy_text.to_owned(), disabled, reason: reason.clone() }
                        } else if action_id == "open" && !href.is_empty() && !disabled {
                            NativeLink {
                                href: href.clone(),
                                class: Some(button_class(ButtonVariant::Outline, ButtonSize::Sm, None)),
                                title: Some(label.clone()),
                                "{label}"
                            }
                        } else {
                            Button {
                                size: ButtonSize::Sm,
                                variant,
                                disabled,
                                title: reason.clone(),
                                aria_pressed: pressed.then_some("true".to_owned()),
                                aria_label: label.clone(),
                                onclick: move |_| action_send.call(action_event.clone()),
                                "{label}"
                            }
                        }
                        if pressed { Badge { variant: BadgeVariant::Accent, size: BadgeSize::Sm, "on" } }
                        if let Some(keys) = keys {
                            KbdGroup { for key in keys { Kbd { "{text_value(key)}" } } }
                        }
                        if !reason.is_empty() {
                            span { class: "text-xs text-muted-foreground", "{reason}" }
                        }
                    }
                }
            }
        }
        if !has_copy && !copy_text.is_empty() {
            CopyCommand { label: "Copy".to_owned(), command: copy_text.to_owned(), disabled: false, reason: String::new() }
        }
    }
}

/// The head's Copy command, host-local by contract: it writes the complete
/// `command` prop (never the clamped display) through the one clipboard hook.
#[component]
fn CopyCommand(label: String, command: String, disabled: bool, reason: String) -> Element {
    let (copy, copied) = use_copy_clipboard(None);
    let value = command;
    rsx! {
        Button {
            size: ButtonSize::Sm,
            variant: ButtonVariant::Outline,
            disabled: disabled || value.is_empty(),
            title: reason,
            aria_label: "Copy command".to_owned(),
            onclick: move |_| copy(&value),
            if copied() {
                "Copied"
            } else {
                {render_icon("copy", "size-3.5")}
                if label.is_empty() { "Copy" } else { "{label}" }
            }
        }
    }
}

/// The command the head copies: the explicit prop, or a `command` target that
/// sets it implicitly.
fn copy_text(node: &OmpUiNode) -> String {
    let command = prop_text(node, "command");
    if command.is_empty() && prop_text(node, "targetKind") == "command" {
        prop_text(node, "target")
    } else {
        command
    }
}

fn action_variant(action: &Value) -> ButtonVariant {
    if action.get("danger").and_then(Value::as_bool) == Some(true) {
        ButtonVariant::Destructive
    } else if action.get("primary").and_then(Value::as_bool) == Some(true) {
        ButtonVariant::Default
    } else {
        ButtonVariant::Outline
    }
}
