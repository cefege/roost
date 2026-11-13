//! The context menu a native node's `actions.menu` declares.
//!
//! Menu items run the same host-local `copy`/`open` semantics as a click: a
//! validated http(s) URL opens a new tab with no opener, a worker file
//! navigates through the authenticated viewer, and an unsafe target reports a
//! refusal. Missing local payloads return to OMP. The panel is the stock
//! controlled `ContextMenu`.

use dioxus::prelude::*;
use serde_json::Value;

use super::actions::dispatch_click;
use crate::components::ui::{ContextMenu, ContextMenuContent, ContextMenuItem, ContextMenuTrigger};

/// A node's declared context-menu actions over the stock controlled menu.
#[component]
#[allow(clippy::too_many_arguments)]
pub(super) fn NativeContextMenu(
    actions: Vec<String>,
    surface: String,
    id: String,
    #[props(default)] href: Option<String>,
    #[props(default)] collapsed: bool,
    #[props(default)] disabled: bool,
    send: EventHandler<Value>,
    children: Element,
) -> Element {
    let mut open = use_signal(|| false);
    if disabled {
        return children;
    }
    rsx! {
        ContextMenu {
            open: open(),
            on_open_change: move |next: bool| open.set(next),
            aria_label: Some("Actions".to_owned()),
            ContextMenuTrigger { {children} }
            ContextMenuContent {
                for action in actions {
                    {
                        let label = action_label(&action);
                        let action_id = action.clone();
                        let item_href = href.clone();
                        let item_surface = surface.clone();
                        let item_node = id.clone();
                        rsx! {
                            ContextMenuItem {
                                onclick: move |event: MouseEvent| {
                                    event.stop_propagation();
                                    dispatch_click(
                                        send,
                                        &item_surface,
                                        &item_node,
                                        &action_id,
                                        &item_node,
                                        item_href.as_deref(),
                                        collapsed,
                                    );
                                },
                                "{label}"
                            }
                        }
                    }
                }
            }
        }
    }
}

fn action_label(action: &str) -> String {
    let mut characters = action.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    let first = first.to_uppercase().collect::<String>();
    let rest: String = characters
        .map(|character| match character {
            '-' | '_' => ' ',
            other => other,
        })
        .collect();
    format!("{first}{rest}")
}
