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
                    Kbd { "{key}" }
                }
            }
        };
    }
    match (action.as_str(), href.as_deref()) {
        ("copy", Some(value)) => rsx! {
            NativeCopyButton {
                value: value.to_owned(),
                label: text,
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
                        class: Some(button_class(variant, size, Some("pointer-events-auto"))),
                        title: Some(label.clone()),
                        if let Some(icon) = icon.as_deref() {
                            {super::content::render_icon(icon, "sm")}
                        }
                        if !text.is_empty() { span { "{text}" } }
                        if text.is_empty() && !label.is_empty() {
                            span { class: "sr-only", "{label}" }
                        }
                        for key in &keys {
                            Kbd { "{key}" }
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
                        Kbd { "{key}" }
                    }
                }
            }
        }
    }
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
    let disabled = node
        .p
        .get("disabled")
        .is_some_and(|value| !value.is_null() && value != false);
    let text = node
        .p
        .get("label")
        .or_else(|| node.p.get("text"))
        .map(text_value)
        .unwrap_or_default();
    let item_href = actions::href(node);
    let row = if let Some(action) = click {
        let action = action.to_owned();
        let sf = surface.to_owned();
        let owner = click_owner.unwrap_or(node.id.as_str()).to_owned();
        let clicked = node.id.clone();
        let target = item_href.clone();
        rsx! {
            Button {
                class: Some(
                    "pointer-events-auto h-auto w-full justify-start whitespace-normal text-left"
                        .into(),
                ),
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
                "{text}"
            }
        }
    } else {
        rsx! {
            div {
                class: "flex w-full min-w-0 flex-wrap items-center gap-2 px-2 py-1 text-sm",
                "{text}"
            }
        }
    };
    if disabled {
        return row;
    }
    let Some(action) = dblclick else {
        return row;
    };
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
}
