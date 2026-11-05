//! Shared native node bounds, selection marks and pointer action targets.
//! Every rendered node gets a real wrapper carrying its wire id and surface, so
//! scroll effects can find it; stretched stock buttons stay siblings of their
//! content, so a model's effort control remains independently clickable without
//! invalid nested buttons. Context menus and hover cards are stock overlays.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::Value;

use crate::components::ui::{Button, ButtonVariant, Item, ItemSize, ItemVariant};

use super::actions::{self, CopyOverlay, DoubleClickRegion};
use super::hovercard::NativeHoverCard;
use super::links::NativeLink;
use super::node_menu::NativeContextMenu;
use super::text_value;

pub(super) fn decorate(
    node: &OmpUiNode,
    surface: &str,
    send: EventHandler<Value>,
    body: Element,
) -> Element {
    let style = super::layout::common_style(&node.p);
    let title = node
        .p
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let aria = node.p.get("aria").and_then(Value::as_str).unwrap_or(title);
    let role = node
        .p
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mark = node
        .p
        .get("mark")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let click = actions::click_action(node);
    // A list's actions apply to its rows (threaded as inherited actions), and a
    // row's own double press is handled by the row renderer; neither may also
    // wrap the whole container.
    let dblclick =
        actions::double_action(node).filter(|_| !matches!(node.k.as_str(), "list" | "item"));
    let menu = actions::menu_actions(node);
    let href = actions::href(node);
    let collapsed = node
        .p
        .get("collapsed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let owns_controls = matches!(
        node.k.as_str(),
        "button"
            | "item"
            | "input"
            | "editor"
            | "picker"
            | "prefs"
            | "tabs"
            | "tool"
            | "agent"
            | "checklist"
            | "list"
    ) || (matches!(node.k.as_str(), "card" | "section")
        && node.p.get("collapsible").and_then(Value::as_bool) == Some(true));
    let label = if aria.is_empty() {
        node.p
            .get("text")
            .or_else(|| node.p.get("label"))
            .map(text_value)
            .unwrap_or_default()
    } else {
        aria.to_owned()
    };
    let mut positioned = false;
    let body = if let Some(act) = click.filter(|_| !owns_controls) {
        positioned = true;
        let overlay = match (act, href.as_deref()) {
            ("open", Some(target)) => {
                let label = overlay_label(&label, act);
                rsx! {
                    NativeLink {
                        href: target.to_owned(),
                        class: Some(
                            "pointer-events-auto absolute inset-0 rounded-md focus-visible:ring-2 focus-visible:ring-ring/50"
                                .into(),
                        ),
                        title: Some(label.clone()),
                        span { class: "sr-only", "{label}" }
                    }
                }
            }
            ("copy", Some(text)) => {
                let label = overlay_label(&label, act);
                rsx! { CopyOverlay { text: text.to_owned(), label } }
            }
            _ => {
                let act = act.to_owned();
                let sf = surface.to_owned();
                let id = node.id.clone();
                let target = href.clone();
                rsx! {
                    Button {
                        variant: ButtonVariant::Ghost,
                        class: Some(
                            "pointer-events-auto absolute inset-0 h-full w-full opacity-0 focus-visible:opacity-100"
                                .into(),
                        ),
                        aria_label: overlay_label(&label, &act),
                        onclick: move |event: MouseEvent| {
                            event.stop_propagation();
                            actions::dispatch_click(
                                send,
                                &sf,
                                &id,
                                &act,
                                &id,
                                target.as_deref(),
                                collapsed,
                            );
                        },
                    }
                }
            }
        };
        rsx! {
            {overlay}
            div { class: "relative min-w-0 pointer-events-none", {body} }
        }
    } else {
        body
    };
    let body = if let Some(act) = dblclick {
        let act = act.to_owned();
        let sf = surface.to_owned();
        let id = node.id.clone();
        let target = href.clone();
        rsx! {
            DoubleClickRegion {
                on_double: EventHandler::new(move |()| {
                    actions::dispatch_double(
                        send,
                        &sf,
                        &id,
                        &act,
                        &id,
                        target.as_deref(),
                        collapsed,
                    );
                }),
                {body}
            }
        }
    } else {
        body
    };
    let body = if menu.is_empty() {
        body
    } else {
        rsx! {
            NativeContextMenu {
                actions: menu,
                surface: surface.to_owned(),
                id: node.id.clone(),
                href: href.clone(),
                collapsed,
                send,
                {body}
            }
        }
    };
    let body = if let Some(card) = node.p.get("hovercard") {
        rsx! { NativeHoverCard { card: card.clone(), {body} } }
    } else {
        body
    };
    let body = if mark == "pick" {
        rsx! {
            Item { variant: ItemVariant::Muted, size: ItemSize::Sm, class: Some("w-full".into()), {body} }
        }
    } else {
        body
    };
    let opacity = if mark == "drop" {
        "opacity-50"
    } else {
        "opacity-100"
    };
    let layout = if node.k == "spacer" { "shrink-0" } else { "" };
    // Native editors occupy the line's flexible space without declaring
    // `grow`; the outer wrapper must do it, because an inner `flex-1` cannot
    // widen a shrink-wrapped wrapper. An explicit grow prop still wins.
    let fill = if matches!(node.k.as_str(), "input" | "editor") && !node.p.contains_key("grow") {
        "flex-1"
    } else {
        ""
    };
    let position = if positioned { "relative" } else { "" };
    let interactive = if owns_controls {
        "pointer-events-auto"
    } else {
        ""
    };
    rsx! {
        div {
            class: "min-w-0 {fill} {layout} {position} {interactive} {opacity}",
            style,
            title,
            "data-omp-node": node.id.as_str(),
            "data-omp-surface": surface,
            "data-omp-role": role,
            "data-omp-mark": mark,
            {body}
        }
    }
}

fn overlay_label(label: &str, act: &str) -> String {
    if label.is_empty() {
        act.to_owned()
    } else {
        label.to_owned()
    }
}
