//! The context menu a native node's `actions.menu` declares.
//!
//! Menu items run the same host-local `copy`/`open` semantics as a click: a
//! validated http(s) URL opens a new tab with no opener, a worker file
//! navigates through the authenticated viewer, and an unsafe target does
//! nothing. Every other id returns to OMP unchanged. The panel is the stock
//! controlled `ContextMenu`.

use dioxus::prelude::*;
use serde_json::Value;

use super::actions::{send_action, send_item_action, send_toggle};
use super::links::{NativeTarget, resolve_native_target};
use super::media::MediaContext;
use crate::components::hooks::use_copy_clipboard::copy_text_then;
use crate::components::ui::{ContextMenu, ContextMenuContent, ContextMenuItem, ContextMenuTrigger};
use crate::pump::use_store;
use crate::router_state::use_navigate;
use crate::terminal_href::worker_os as worker_operating_system;

/// A node's declared context-menu actions over the stock controlled menu.
#[component]
#[allow(clippy::too_many_arguments)]
pub(super) fn NativeContextMenu(
    actions: Vec<String>,
    surface: String,
    id: String,
    #[props(default)] href: Option<String>,
    #[props(default)] collapsed: bool,
    send: EventHandler<Value>,
    children: Element,
) -> Element {
    let mut open = use_signal(|| false);
    let context = use_context::<Signal<MediaContext>>().read().clone();
    let pump = use_store();
    let navigate = use_navigate();
    let worker_os = {
        let core = pump.core();
        let core = core.borrow();
        worker_operating_system(core.store(), &context.worker_fp).map(str::to_owned)
    };
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
                        let item_worker_os = worker_os.clone();
                        let item_context = context.clone();
                        rsx! {
                            ContextMenuItem {
                                onclick: move |event: MouseEvent| {
                                    event.stop_propagation();
                                    run_menu_action(
                                        &action_id,
                                        item_href.as_deref(),
                                        &item_context,
                                        item_worker_os.as_deref(),
                                        navigate,
                                        send,
                                        &item_surface,
                                        &item_node,
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

#[allow(clippy::too_many_arguments)]
fn run_menu_action(
    act: &str,
    href: Option<&str>,
    context: &MediaContext,
    worker_os: Option<&str>,
    navigate: EventHandler<String>,
    send: EventHandler<Value>,
    surface: &str,
    id: &str,
    collapsed: bool,
) {
    match act {
        "copy" => match href {
            Some(text) => copy_text_then(text, |_| {}),
            None => send_action(send, surface, id, act),
        },
        "open" => match href {
            Some(target) => {
                match resolve_native_target(target, &context.worker_fp, &context.cwd, worker_os) {
                    NativeTarget::External { href, .. } => open_browser(&href),
                    NativeTarget::WorkerFile { href } => navigate.call(href),
                    NativeTarget::Inert => {}
                }
            }
            None => send_action(send, surface, id, act),
        },
        "toggle" => send_toggle(send, surface, id, !collapsed),
        "select" => send_item_action(send, surface, id, "select", id),
        "activate" => send_item_action(send, surface, id, "activate", id),
        _ => send_action(send, surface, id, act),
    }
}

#[cfg(target_arch = "wasm32")]
fn open_browser(href: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(Some(opened)) = window.open_with_url_and_target("about:blank", "_blank") else {
        return;
    };
    let _ = js_sys::Reflect::set(&opened, &"opener".into(), &wasm_bindgen::JsValue::NULL);
    let _ = opened.location().set_href(href);
}

#[cfg(not(target_arch = "wasm32"))]
fn open_browser(_href: &str) {}
