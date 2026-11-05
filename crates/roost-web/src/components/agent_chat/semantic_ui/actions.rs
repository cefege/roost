//! Host-local pointer actions for native TSP nodes.
//!
//! The native terminal runs a node's `copy`/`open` action against its declared
//! `href`; every other action id returns to OMP unchanged. Select and activate
//! report the clicked item's wire ID, and a double press is paired from the
//! pointer stream because Dioxus cannot match the browser's `dblclick` name.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::{Value, json};

use super::event;
use crate::components::hooks::use_copy_clipboard::{copy_text_then, use_copy_clipboard};
use crate::components::ui::{Button, ButtonSize, ButtonVariant, Kbd};

/// How far apart two primary presses may be and still be one double press, ms.
pub(super) const DOUBLE_PRESS_WINDOW_MS: f64 = 500.0;

/// The pointer actions one node inherits from an enclosing list.
///
/// `owner` is the node that declared the action (the list), while the clicked
/// row's own wire id supplies the native `item` key; the two differ for list
/// rows and must stay separate.
#[derive(Clone, Copy, Default, PartialEq)]
pub(super) struct PointerActions<'a> {
    pub owner: Option<&'a str>,
    pub click: Option<&'a str>,
    pub dblclick: Option<&'a str>,
}

impl<'a> PointerActions<'a> {
    pub(super) fn of(node: &'a OmpUiNode) -> Self {
        let actions = node.p.get("actions");
        Self {
            owner: Some(node.id.as_str()),
            click: actions
                .and_then(|actions| actions.get("click"))
                .and_then(Value::as_str)
                .filter(|action| !action.is_empty()),
            dblclick: actions
                .and_then(|actions| actions.get("dblclick"))
                .and_then(Value::as_str)
                .filter(|action| !action.is_empty()),
        }
    }

    /// A list's rows: explicit actions win, and a plain list defaults to the
    /// native select/double-activate pair under the list's own event id.
    pub(super) fn for_list(node: &'a OmpUiNode) -> Self {
        let mut actions = Self::of(node);
        if actions.click.is_none() {
            actions.click = Some("select");
        }
        if actions.dblclick.is_none() {
            actions.dblclick = Some("activate");
        }
        actions
    }
}

/// The actions a list's rows inherit when the row itself declares none.
pub(super) fn click_action(node: &OmpUiNode) -> Option<&str> {
    PointerActions::of(node).click
}

pub(super) fn double_action(node: &OmpUiNode) -> Option<&str> {
    PointerActions::of(node).dblclick
}

/// The node's declared context-menu actions, in OMP's order.
pub(super) fn menu_actions(node: &OmpUiNode) -> Vec<String> {
    node.p
        .get("actions")
        .and_then(|actions| actions.get("menu"))
        .and_then(Value::as_array)
        .map(|actions| {
            actions
                .iter()
                .filter_map(Value::as_str)
                .filter(|action| !action.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The local payload of a `copy`/`open` action.
pub(super) fn href(node: &OmpUiNode) -> Option<String> {
    node.p
        .get("href")
        .and_then(Value::as_str)
        .filter(|href| !href.is_empty())
        .map(str::to_owned)
}

pub(super) fn send_action(send: EventHandler<Value>, surface: &str, id: &str, act: &str) {
    send.call(event("action", surface, id, json!({ "act": act })));
}

pub(super) fn send_toggle(send: EventHandler<Value>, surface: &str, id: &str, collapsed: bool) {
    send.call(event(
        "toggle",
        surface,
        id,
        json!({ "collapsed": collapsed }),
    ));
}

pub(super) fn send_selection(
    send: EventHandler<Value>,
    surface: &str,
    id: &str,
    verb: &str,
    item: &str,
) {
    send.call(event(verb, surface, id, json!({ "item": item })));
}

/// Address the owning list and preserve the clicked wire ID for OMP's decoder.
pub(super) fn send_item_action(
    send: EventHandler<Value>,
    surface: &str,
    owner_id: &str,
    verb: &str,
    clicked_id: &str,
) {
    send_selection(send, surface, owner_id, verb, clicked_id);
}

/// Run a click action: local copy targets stay here, the rest return to OMP.
///
/// An `open` target is rendered as a real `NativeLink` by the caller so the
/// browser owns activation and an unsafe scheme stays inert; reaching this path
/// means the target has no local payload and is forwarded unchanged.
pub(super) fn dispatch_click(
    send: EventHandler<Value>,
    surface: &str,
    owner_id: &str,
    act: &str,
    clicked_id: &str,
    href: Option<&str>,
    collapsed: bool,
) {
    match act {
        "copy" => match href {
            Some(text) => copy_text_then(text, |_| {}),
            None => send_action(send, surface, owner_id, act),
        },
        "toggle" => send_toggle(send, surface, owner_id, !collapsed),
        "select" => send_item_action(send, surface, owner_id, "select", clicked_id),
        "activate" => send_item_action(send, surface, owner_id, "activate", clicked_id),
        _ => send_action(send, surface, owner_id, act),
    }
}

/// Run a double-press action. Copy still works locally; open stays with the
/// single-click link because a double press has no anchor to activate.
pub(super) fn dispatch_double(
    send: EventHandler<Value>,
    surface: &str,
    owner_id: &str,
    act: &str,
    clicked_id: &str,
    href: Option<&str>,
    collapsed: bool,
) {
    match act {
        "copy" => match href {
            Some(text) => copy_text_then(text, |_| {}),
            None => send_action(send, surface, owner_id, act),
        },
        "open" => {}
        "toggle" => send_toggle(send, surface, owner_id, !collapsed),
        "select" => send_item_action(send, surface, owner_id, "select", clicked_id),
        "activate" => send_item_action(send, surface, owner_id, "activate", clicked_id),
        _ => send_action(send, surface, owner_id, act),
    }
}

/// A press memory that answers whether two presses are one double press.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct DoublePress {
    last: Option<f64>,
}

impl DoublePress {
    /// Record a press and answer whether it completed a pair.
    pub(super) fn press(&mut self, now_ms: f64) -> bool {
        let paired = self
            .last
            .is_some_and(|last| (now_ms - last).abs() <= DOUBLE_PRESS_WINDOW_MS);
        self.last = if paired { None } else { Some(now_ms) };
        paired
    }
}

#[cfg(target_arch = "wasm32")]
fn now_ms() -> f64 {
    js_sys::Date::now()
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> f64 {
    0.0
}

#[component]
pub(super) fn DoubleClickRegion(on_double: EventHandler<()>, children: Element) -> Element {
    let mut presses = use_signal(DoublePress::default);
    rsx! {
        div {
            class: "contents",
            onpointerdown: move |event: PointerEvent| {
                if event.trigger_button()
                    != Some(dioxus::html::input_data::MouseButton::Primary)
                {
                    return;
                }
                let paired = {
                    let mut presses = presses.write();
                    presses.press(now_ms())
                };
                if paired {
                    on_double.call(());
                }
            },
            {children}
        }
    }
}

/// The invisible hit target that copies a node's declared `href`.
#[component]
pub(super) fn CopyOverlay(text: String, label: String) -> Element {
    let (copy, _copied) = use_copy_clipboard(None);
    let value = text;
    rsx! {
        Button {
            variant: ButtonVariant::Ghost,
            class: Some(
                "pointer-events-auto absolute inset-0 h-full w-full opacity-0 focus-visible:opacity-100"
                    .into(),
            ),
            aria_label: label,
            onclick: move |event: MouseEvent| {
                event.stop_propagation();
                copy(&value);
            },
        }
    }
}

/// A declared button that copies its `href` instead of reporting the action.
#[component]
#[allow(clippy::too_many_arguments)]
pub(super) fn NativeCopyButton(
    value: String,
    label: String,
    #[props(default)] icon: Option<String>,
    #[props(default)] keys: Vec<String>,
    #[props(default)] size: ButtonSize,
    #[props(default)] variant: ButtonVariant,
    #[props(default)] disabled: bool,
    #[props(default)] reason: String,
    #[props(default)] pressed: bool,
) -> Element {
    let (copy, copied) = use_copy_clipboard(None);
    let text = value;
    let aria = if label.is_empty() {
        reason.clone()
    } else {
        label.clone()
    };
    let aria_label = (!aria.is_empty()).then_some(aria);
    let title = (!reason.is_empty()).then_some(reason);
    rsx! {
        Button {
            size,
            variant,
            disabled,
            title,
            aria_label,
            aria_pressed: pressed.then_some("true".to_owned()),
            class: Some("pointer-events-auto".into()),
            onclick: move |event: MouseEvent| {
                event.stop_propagation();
                copy(&text);
            },
            if let Some(icon) = icon {
                {super::content::render_icon(&icon, "sm")}
            }
            if copied() {
                "Copied"
            } else if !label.is_empty() {
                "{label}"
            }
            for key in keys {
                Kbd { "{key}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn two_presses_inside_the_window_are_one_double_press() {
        let mut press = DoublePress::default();
        assert!(!press.press(0.0));
        assert!(press.press(DOUBLE_PRESS_WINDOW_MS / 2.0));
        assert!(!press.press(DOUBLE_PRESS_WINDOW_MS * 2.0));
    }

    fn list_node(actions: Option<Value>) -> OmpUiNode {
        let mut props = serde_json::Map::new();
        if let Some(actions) = actions {
            props.insert("actions".into(), actions);
        }
        OmpUiNode {
            id: "cmp.list".into(),
            k: "list".into(),
            p: props,
            c: Vec::new(),
        }
    }

    #[test]
    fn a_plain_list_gets_native_select_and_activate_under_its_own_id() {
        let node = list_node(None);
        let actions = PointerActions::for_list(&node);
        assert_eq!(actions.owner, Some("cmp.list"));
        assert_eq!(actions.click, Some("select"));
        assert_eq!(actions.dblclick, Some("activate"));
    }

    #[test]
    fn a_list_override_wins_and_the_missing_default_still_fills() {
        let node = list_node(Some(json!({ "click": "open" })));
        let actions = PointerActions::for_list(&node);
        assert_eq!(actions.click, Some("open"));
        assert_eq!(actions.dblclick, Some("activate"));
        let node = list_node(Some(json!({ "click": "activate", "dblclick": "open" })));
        let both = PointerActions::for_list(&node);
        assert_eq!(both.click, Some("activate"));
        assert_eq!(both.dblclick, Some("open"));
    }
}
