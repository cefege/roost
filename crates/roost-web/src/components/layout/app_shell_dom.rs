//! The browser half of `AppShell`: the `--roost-main-left` document property,
//! the sidebar-toggle chord and page-show resize reset on `window`, and moving
//! focus to the rail before the sidebar collapses. wasm32 only. Ports the
//! effects and `onMount` listeners of
//! `apps/web/src/components/layout/AppShell.tsx`.

use dioxus::prelude::*;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

use super::activity_bar::SESSIONS_ITEM_ID;
use super::shell_style::main_left_offset;
use super::sidebar_region::SIDEBAR_REGION_ID;
use crate::motion::resize_drag::use_resize_drag;
use crate::platform::browser_platform::{PlatformShortcut, browser_platform, matches_platform_shortcut, shortcut_key};
use crate::pump::Pump;

fn document() -> Option<web_sys::Document> {
    web_sys::window()?.document()
}

/// Keep `--roost-main-left` on `<html>` in step with the grid; removed on unmount.
pub(super) fn use_main_left(compact: bool, collapsed: bool, sidebar_width: u32) {
    let offset = main_left_offset(compact, collapsed, sidebar_width);
    let mut written = use_signal(String::new);
    if *written.peek() != offset {
        if let Some(root) = document().and_then(|document| document.document_element())
            && let Some(root) = root.dyn_ref::<web_sys::HtmlElement>()
        {
            let _ = root.style().set_property("--roost-main-left", &offset);
        }
        written.set(offset);
    }
    use_drop(|| {
        if let Some(root) = document().and_then(|document| document.document_element())
            && let Some(root) = root.dyn_ref::<web_sys::HtmlElement>()
        {
            let _ = root.style().remove_property("--roost-main-left");
        }
    });
}

struct ShellListeners {
    keydown: Closure<dyn FnMut(web_sys::KeyboardEvent)>,
    pageshow: Closure<dyn FnMut()>,
}

/// The toggle-sidebar chord and the page-show reset, for the shell's lifetime.
pub(super) fn use_shell_listeners(pump: Pump, compact: bool) {
    let mut compact_now = use_signal(|| compact);
    if *compact_now.peek() != compact {
        compact_now.set(compact);
    }
    let drag = use_resize_drag();
    let listeners = use_hook(move || {
        let keydown = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |event: web_sys::KeyboardEvent| {
            let key = shortcut_key(&event);
            if event.default_prevented()
                || !matches_platform_shortcut(&key, PlatformShortcut::ToggleSidebar, browser_platform())
            {
                return;
            }
            event.prevent_default();
            super::app_shell::toggle_desktop_sidebar(&pump, *compact_now.peek());
        });
        let pageshow = Closure::<dyn FnMut()>::new(move || drag.reset());
        if let Some(window) = web_sys::window() {
            let _ = window.add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref());
            let _ = window.add_event_listener_with_callback("pageshow", pageshow.as_ref().unchecked_ref());
        }
        std::rc::Rc::new(ShellListeners { keydown, pageshow })
    });
    use_drop(move || {
        if let Some(window) = web_sys::window() {
            let _ = window.remove_event_listener_with_callback("keydown", listeners.keydown.as_ref().unchecked_ref());
            let _ = window.remove_event_listener_with_callback("pageshow", listeners.pageshow.as_ref().unchecked_ref());
        }
    });
}

fn sessions_item() -> Option<web_sys::HtmlElement> {
    document()?.get_element_by_id(SESSIONS_ITEM_ID)?.dyn_into().ok()
}

/// When the desktop sidebar is about to collapse with focus inside it, move
/// focus to the rail's Sessions item first; `true` when it did.
pub(super) fn focus_rail_before_collapse(compact: bool, collapsed: bool) -> bool {
    if compact || collapsed {
        return false;
    }
    let Some(document) = document() else { return false };
    let inside = match (document.get_element_by_id(SIDEBAR_REGION_ID), document.active_element()) {
        (Some(region), Some(active)) => region.contains(Some(&active)),
        _ => false,
    };
    if !inside {
        return false;
    }
    if let Some(item) = sessions_item() {
        let _ = item.focus();
    }
    true
}

/// Re-assert the rail's focus after the collapse re-rendered, on the next
/// frame's task — the region turning `inert` can drop it again.
pub(super) fn refocus_rail_after_collapse() {
    if let Some(item) = sessions_item() {
        let _ = item.focus();
    }
    let Some(window) = web_sys::window() else { return };
    let later = Closure::once_into_js(move || {
        let task = Closure::once_into_js(|| {
            if let Some(item) = sessions_item() {
                let _ = item.focus();
            }
        });
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(task.unchecked_ref(), 0);
        }
    });
    let _ = window.request_animation_frame(later.unchecked_ref());
}
