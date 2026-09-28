//! The DOM half of the md primitives: re-asserting a controlled checkbox, the
//! dialog's focus moves, and the two browser readings `Select` places its
//! listbox with. Called by `switch.rs`, `checkbox.rs`, `dialog.rs` and
//! `select.rs`; every rule these apply lives in a target-independent sibling
//! (`focus_scope`, `select_navigation`, `select_placement`).
//!
//! Each function has a native arm because the components are compiled for the
//! native target too. There is no document there, so a native arm answers "no
//! element" or does nothing — the native target paints nothing to move focus in.

use dioxus::prelude::*;

use super::focus_scope::FocusEdge;
#[cfg(target_arch = "wasm32")]
use super::focus_scope::{FOCUS_TRAP_ATTRIBUTE, FOCUSABLE_SELECTOR, is_in_tab_order};

/// The element that held focus when a dialog opened, so closing it can hand
/// focus back. Uninhabited natively, where nothing is ever focused.
#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub struct FocusOpener(web_sys::HtmlElement);

/// The element that held focus when a dialog opened.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub enum FocusOpener {}

/// Put a checkbox back to the state its caller rendered.
///
/// The browser toggles the element before the handler runs; if the caller then
/// declines the change, nothing re-renders and the element would keep showing
/// the value that was refused.
#[cfg(target_arch = "wasm32")]
pub fn restore_checked(event: &FormEvent, checked: bool) {
    use dioxus::web::WebEventExt as _;
    use wasm_bindgen::JsCast as _;

    let input = event
        .try_as_web_event()
        .and_then(|web_event| web_event.target())
        .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok());
    if let Some(input) = input {
        input.set_checked(checked);
    }
}

/// Put a checkbox back to the state its caller rendered.
#[cfg(not(target_arch = "wasm32"))]
pub fn restore_checked(_event: &FormEvent, _checked: bool) {}

/// The element focus is on now, if it is an element that can be re-focused.
#[cfg(target_arch = "wasm32")]
pub fn focused_element() -> Option<FocusOpener> {
    use wasm_bindgen::JsCast as _;

    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element())
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
        .map(FocusOpener)
}

/// The element focus is on now.
#[cfg(not(target_arch = "wasm32"))]
pub fn focused_element() -> Option<FocusOpener> {
    None
}

/// Hand focus back to the opener, without scrolling, if it is still in the
/// document. A detached opener (the row that opened the dialog was deleted by
/// it) is skipped rather than focused into nowhere.
#[cfg(target_arch = "wasm32")]
pub fn restore_focus(opener: &FocusOpener) {
    if opener.0.is_connected() {
        focus_without_scrolling(&opener.0);
    }
}

/// Hand focus back to the opener.
#[cfg(not(target_arch = "wasm32"))]
pub fn restore_focus(opener: &FocusOpener) {
    match *opener {}
}

/// Focus one end of the container's tabbable elements, or the container itself
/// when it has none — the mount auto-focus and the sentinel wrap both land here.
#[cfg(target_arch = "wasm32")]
pub fn focus_edge(container: &MountedData, edge: FocusEdge) {
    use dioxus::web::WebEventExt as _;
    use wasm_bindgen::JsCast as _;

    let Some(container) = container.try_as_web_event() else {
        return;
    };
    let tabbables = tabbable_elements(&container);
    let target = match edge {
        FocusEdge::First => tabbables.first(),
        FocusEdge::Last => tabbables.last(),
    };
    match target {
        Some(element) => focus_without_scrolling(element),
        None => {
            if let Ok(container) = container.dyn_into::<web_sys::HtmlElement>() {
                focus_without_scrolling(&container);
            }
        }
    }
}

/// Focus one end of the container's tabbable elements.
#[cfg(not(target_arch = "wasm32"))]
pub fn focus_edge(_container: &MountedData, _edge: FocusEdge) {}

/// Whether focus arrived at a sentinel from the container's first tabbable
/// element — the one signal that says the reader is tabbing backwards.
#[cfg(target_arch = "wasm32")]
pub fn came_from_first_tabbable(event: &FocusEvent, container: &MountedData) -> bool {
    use dioxus::web::WebEventExt as _;
    use wasm_bindgen::JsCast as _;

    let Some(container) = container.try_as_web_event() else {
        return false;
    };
    let related = event
        .try_as_web_event()
        .and_then(|focus| focus.related_target())
        .and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok());
    match (related, tabbable_elements(&container).first()) {
        (Some(related), Some(first)) => related == *first,
        _ => false,
    }
}

/// Whether focus arrived at a sentinel from the first tabbable element.
#[cfg(not(target_arch = "wasm32"))]
pub fn came_from_first_tabbable(_event: &FocusEvent, _container: &MountedData) -> bool {
    false
}

/// The viewport's height in CSS pixels, for flipping a listbox that would run
/// off the bottom of the screen.
#[cfg(target_arch = "wasm32")]
pub fn viewport_height() -> Option<f64> {
    web_sys::window()
        .and_then(|window| window.inner_height().ok())
        .and_then(|height| height.as_f64())
}

/// The viewport's height; unknown natively.
#[cfg(not(target_arch = "wasm32"))]
pub fn viewport_height() -> Option<f64> {
    None
}

/// When a key was pressed, in the event's own milliseconds, for the listbox's
/// type-ahead window.
#[cfg(target_arch = "wasm32")]
pub fn key_event_time_ms(event: &KeyboardEvent) -> f64 {
    use dioxus::web::WebEventExt as _;

    event
        .try_as_web_event()
        .map_or(0.0, |keyboard| keyboard.time_stamp())
}

/// When a key was pressed; natively there is no event clock.
#[cfg(not(target_arch = "wasm32"))]
pub fn key_event_time_ms(_event: &KeyboardEvent) -> f64 {
    0.0
}

/// The container's tabbable descendants in document order, sentinels excluded.
#[cfg(target_arch = "wasm32")]
fn tabbable_elements(container: &web_sys::Element) -> Vec<web_sys::HtmlElement> {
    use wasm_bindgen::JsCast as _;

    let Ok(nodes) = container.query_selector_all(FOCUSABLE_SELECTOR) else {
        return Vec::new();
    };
    (0..nodes.length())
        .filter_map(|index| nodes.item(index))
        .filter_map(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        .filter(|element| {
            !element.has_attribute(FOCUS_TRAP_ATTRIBUTE)
                && is_in_tab_order(element.get_attribute("tabindex").as_deref())
                && element.get_client_rects().length() > 0
        })
        .collect()
}

/// `element.focus({ preventScroll: true })`.
///
/// Called through `Reflect` because the options dictionary is the one piece of
/// the focus API this crate's `web-sys` feature set does not name.
#[cfg(target_arch = "wasm32")]
fn focus_without_scrolling(element: &web_sys::HtmlElement) {
    use wasm_bindgen::{JsCast as _, JsValue};

    let options = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&options, &JsValue::from_str("preventScroll"), &JsValue::TRUE);
    let focus = js_sys::Reflect::get(element, &JsValue::from_str("focus"))
        .ok()
        .and_then(|focus| focus.dyn_into::<js_sys::Function>().ok());
    match focus {
        Some(focus) => {
            let _ = focus.call1(element, &options);
        }
        None => {
            let _ = element.focus();
        }
    }
}
