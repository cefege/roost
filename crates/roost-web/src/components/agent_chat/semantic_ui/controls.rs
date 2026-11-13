//! Native buttons and list rows.
//!
//! A button keeps OMP's `TspButtonProps` semantics: stock variant/size, the
//! spinner while busy, `aria-pressed` and keycaps while toggled, and a disabled
//! reason as the tooltip. Its declared action runs locally for `copy`/`open`
//! with an href and otherwise returns to OMP as the native event kind it names.
//! A list row reports select/activate with its own item key.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::Value;

use crate::components::ui::{Button, ButtonSize, ButtonVariant, Kbd, Spinner, button_class};

use super::actions::{self, DoubleClickRegion, NativeCopyButton};
use super::content::render_spans;
use super::links::NativeLink;
use super::text_value;

pub(super) fn render_button(node: &OmpUiNode, surface: &str, send: EventHandler<Value>) -> Element {
    let id = node.id.clone();
    let sf = surface.to_owned();
    let action = actions::click_action(node).unwrap_or("click").to_owned();
    let href = actions::href(node);
    let text = node
        .p
        .get("text")
        .or_else(|| node.p.get("label"))
        .map(text_value)
        .unwrap_or_default();
    let icon = node
        .p
        .get("icon")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let busy = node.p.get("busy").and_then(Value::as_bool) == Some(true);
    let disabled_value = node.p.get("disabled");
    let disabled = busy || disabled_value.is_some_and(|value| !value.is_null() && value != false);
    let reason = disabled_value
        .and_then(Value::as_str)
        .or_else(|| node.p.get("title").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned();
    let aria = node
        .p
        .get("aria")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let pressed = node.p.get("on").and_then(Value::as_bool) == Some(true);
    let base_variant = match node.p.get("variant").and_then(Value::as_str) {
        Some("primary") => ButtonVariant::Default,
        Some("quiet") => ButtonVariant::Ghost,
        Some("danger") => ButtonVariant::Destructive,
        _ => ButtonVariant::Outline,
    };
    // A pressed toggle keeps its stock recipe and gains the secondary surface,
    // the same treatment the picker's selected chips use.
    let variant =
        if pressed && matches!(base_variant, ButtonVariant::Ghost | ButtonVariant::Outline) {
            ButtonVariant::Secondary
        } else {
            base_variant
        };
    let size = match node.p.get("size").and_then(Value::as_str) {
        Some("sm") => ButtonSize::Sm,
        _ => ButtonSize::Default,
    };
    let keys: Vec<String> = node
        .p
        .get("keys")
        .and_then(Value::as_array)
        .map(|keys| {
            keys.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let aria_label = if !aria.is_empty() {
        Some(aria)
    } else if text.is_empty() && !reason.is_empty() {
        Some(reason.clone())
    } else {
        None
    };
    let title = (!reason.is_empty()).then_some(reason.clone());
    let pressed_attr = pressed.then_some("true".to_owned());
    if disabled {
        return rsx! {
            Button {
                size,
                variant,
                disabled: true,
                title,
                aria_label,
                aria_pressed: pressed_attr,
                class: Some("pointer-events-auto".into()),
                if busy {
                    Spinner { class: Some("size-3.5".into()) }
                } else if let Some(icon) = icon.as_deref() {
                    {super::content::render_icon(icon, "sm")}
                }
                if !text.is_empty() { "{text}" }
                for key in &keys {
                    Kbd { "{super::key_hint::keycap(key)}" }
                }
            }
        };
    }
    match (action.as_str(), href.as_deref()) {
        ("copy", Some(value)) => rsx! {
            NativeCopyButton {
                value: value.to_owned(),
                label: text,
                aria_label,
                icon,
                keys,
                size,
                variant,
                disabled: false,
                reason: reason.clone(),
                pressed,
            }
        },
        ("open", Some(target)) => {
            let label = if text.is_empty() {
                if reason.is_empty() {
                    "Open".to_owned()
                } else {
                    reason.clone()
                }
            } else {
                text.clone()
            };
            rsx! {
                div {
                    class: "contents",
                    onclick: move |event: MouseEvent| event.stop_propagation(),
                    NativeLink {
                        href: target.to_owned(),
                        report_refusal: true,
                        class: Some(button_class(variant, size, Some("pointer-events-auto"))),
                        title: Some(label.clone()),
                        aria_label: aria_label.clone(),
                        aria_pressed: pressed_attr.clone(),
                        if let Some(icon) = icon.as_deref() {
                            {super::content::render_icon(icon, "sm")}
                        }
                        if !text.is_empty() { span { "{text}" } }
                        if text.is_empty() && !label.is_empty() {
                            span { class: "sr-only", "{label}" }
                        }
                        for key in &keys {
                            Kbd { "{super::key_hint::keycap(key)}" }
                        }
                    }
                }
            }
        }
        _ => {
            let action_event = action.clone();
            let target = href.clone();
            rsx! {
                Button {
                    size,
                    variant,
                    title,
                    aria_label,
                    aria_pressed: pressed_attr,
                    class: Some("pointer-events-auto".into()),
                    onclick: move |event: MouseEvent| {
                        event.stop_propagation();
                        actions::dispatch_click(
                            send,
                            &sf,
                            &id,
                            &action_event,
                            &id,
                            target.as_deref(),
                            false,
                        );
                    },
                    if let Some(icon) = icon.as_deref() {
                        {super::content::render_icon(icon, "sm")}
                    }
                    if !text.is_empty() { "{text}" }
                    for key in &keys {
                        Kbd { "{super::key_hint::keycap(key)}" }
                    }
                }
            }
        }
    }
}

/// The row's leading glyph, named by `ItemProps.icon`.
fn item_icon(node: &OmpUiNode) -> Element {
    match node.p.get("icon").and_then(Value::as_str) {
        Some(icon) if !icon.is_empty() => {
            rsx! { span { class: "shrink-0", {super::content::render_icon(icon, "sm")} } }
        }
        _ => rsx! {},
    }
}

/// Key hints (`ItemProps.hint`) the TUI prints beside the row: a status letter or a key name.
fn item_hint(node: &OmpUiNode) -> Element {
    let hints: Vec<String> = node
        .p
        .get("hint")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(text_value)
        .filter(|hint| !hint.is_empty())
        .collect();
    if hints.is_empty() {
        return rsx! {};
    }
    rsx! { span { class: "flex shrink-0 gap-1", for hint in hints { Kbd { "{hint}" } } } }
}

pub(super) fn render_item<'a>(
    node: &'a OmpUiNode,
    surface: &str,
    send: EventHandler<Value>,
    inherited: Option<actions::PointerActions<'a>>,
) -> Element {
    let own = actions::PointerActions::of(node);
    let click = own.click.or(inherited.and_then(|actions| actions.click));
    let dblclick = own
        .dblclick
        .or(inherited.and_then(|actions| actions.dblclick));
    // An inherited action belongs to the list; the clicked row only supplies
    // the native item key.
    let click_owner = if own.click.is_some() {
        own.owner
    } else {
        inherited.and_then(|actions| actions.owner)
    };
    let dbl_owner = if own.dblclick.is_some() {
        own.owner
    } else {
        inherited.and_then(|actions| actions.owner)
    };
    let disabled = node.p.get("busy").and_then(Value::as_bool) == Some(true)
        || node
            .p
            .get("disabled")
            .is_some_and(|value| !value.is_null() && value != false);
    let tone_muted = node.p.get("tone").and_then(Value::as_str) == Some("muted");
    let label = node.p.get("label").or_else(|| node.p.get("text"));
    let value = node.p.get("value");
    let detail = node.p.get("detail");
    let item_href = actions::href(node);
    let marked_selected = node.p.get("mark").and_then(Value::as_str) == Some("pick");
    let selected = inherited
        .filter(|actions| actions.owner.is_some())
        .map(|actions| actions.selected == Some(node.id.as_str()))
        .unwrap_or(marked_selected);
    // A selection the terminal does not mark with a glyph shows as the accent
    // row, the same role a focused CommandItem uses.
    let highlighted = selected && !marked_selected;
    let row_class = if highlighted {
        "pointer-events-auto h-auto w-full flex-wrap justify-start whitespace-normal text-left bg-accent text-accent-foreground"
    } else {
        "pointer-events-auto h-auto w-full flex-wrap justify-start whitespace-normal text-left"
    };
    let row = if let Some(action) = click {
        let action = action.to_owned();
        let sf = surface.to_owned();
        let owner = click_owner.unwrap_or(node.id.as_str()).to_owned();
        let clicked = node.id.clone();
        let target = item_href.clone();
        let row_body = rsx! {
            {item_icon(node)}
            span { class: "min-w-0 flex-1", {render_spans(label, if tone_muted { "text-muted-foreground" } else { "" })} }
            {item_hint(node)}
            if let Some(value) = value {
                span { class: "ml-auto shrink-0 tabular-nums", {render_spans(Some(value), "whitespace-pre font-mono text-xs")} }
            }
            if let Some(detail) = detail {
                span { class: "w-full basis-full text-xs", {render_spans(Some(detail), "min-w-0 break-words")} }
            }
        };
        if action == "open"
            && !disabled
            && let Some(target) = target.as_ref()
        {
            rsx! {
                NativeLink {
                    href: target.clone(),
                    report_refusal: true,
                    class: Some(button_class(
                        ButtonVariant::Ghost,
                        ButtonSize::Sm,
                        Some(row_class),
                    )),
                    {row_body}
                }
            }
        } else {
            rsx! {
                Button {
                    class: Some(row_class.into()),
                    size: ButtonSize::Sm,
                    variant: ButtonVariant::Ghost,
                    disabled,
                    onclick: move |event: MouseEvent| {
                        event.stop_propagation();
                        actions::dispatch_click(
                            send,
                            &sf,
                            &owner,
                            &action,
                            &clicked,
                            target.as_deref(),
                            false,
                        );
                    },
                    {row_body}
                }
            }
        }
    } else {
        rsx! {
            div {
                class: if highlighted { "flex w-full min-w-0 flex-col rounded-md bg-accent px-2 py-1 text-accent-foreground" } else { "flex w-full min-w-0 flex-col px-2 py-1" },
                div { class: "flex w-full min-w-0 items-center gap-2 text-sm",
                    {item_icon(node)}
                    span { class: "min-w-0 flex-1", {render_spans(label, if tone_muted { "text-muted-foreground" } else { "" })} }
                    {item_hint(node)}
                    if let Some(value) = value {
                        span { class: "ml-auto shrink-0 tabular-nums", {render_spans(Some(value), "whitespace-pre font-mono text-xs")} }
                    }
                }
                if let Some(detail) = detail {
                    span { class: "text-xs", {render_spans(Some(detail), "min-w-0 break-words")} }
                }
            }
        }
    };
    let body = if !disabled && let Some(action) = dblclick {
        let action = action.to_owned();
        let sf = surface.to_owned();
        let owner = dbl_owner.unwrap_or(node.id.as_str()).to_owned();
        let clicked = node.id.clone();
        let target = item_href;
        rsx! {
            DoubleClickRegion {
                on_double: EventHandler::new(move |()| {
                    actions::dispatch_double(
                        send,
                        &sf,
                        &owner,
                        &action,
                        &clicked,
                        target.as_deref(),
                        false,
                    );
                }),
                {row}
            }
        }
    } else {
        row
    };
    rsx! {
        div {
            class: "contents",
            "data-omp-item": node.id.as_str(),
            "data-omp-item-selected": if selected { "true" } else { "false" },
            {body}
        }
    }
}
