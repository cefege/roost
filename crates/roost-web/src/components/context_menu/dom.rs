//! Browser navigation for product pointer command menus.
//! Focus starts only after Libero places the popover; no deferred focus retries.
//! Escape and outside presses belong exclusively to Libero's dismissal owner.

use super::{PointerMenuKeyAction, pointer_menu_key_action};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use web_sys::{Element, HtmlElement, KeyboardEvent, NodeList};

const ENABLED_ITEMS: &str = "[role=\"menuitem\"]:not(:disabled):not([aria-disabled=\"true\"])";

pub fn focus_pointer_menu(menu_id: &str) {
    let Some(item) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(menu_id))
        .filter(|menu| menu.is_connected())
        .and_then(|menu| menu.query_selector(ENABLED_ITEMS).ok().flatten())
        .and_then(|item| item.dyn_into::<HtmlElement>().ok())
    else {
        return;
    };
    let _ = item.focus();
}

pub fn run_pointer_menu_key(
    event: &KeyboardEvent,
    menu: &Element,
    on_tab: impl FnOnce() + 'static,
) {
    let Ok(items) = menu.query_selector_all(ENABLED_ITEMS) else {
        return;
    };
    let active = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element());
    let current = active.and_then(|active| {
        (0..items.length()).position(|index| {
            items
                .item(index)
                .and_then(|node| node.dyn_into::<Element>().ok())
                .as_ref()
                == Some(&active)
        })
    });
    match pointer_menu_key_action(&event.key(), current, items.length() as usize) {
        PointerMenuKeyAction::Tab => {
            let deferred = Closure::once(move |_: wasm_bindgen::JsValue| on_tab());
            let _ = js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED).then(&deferred);
            deferred.forget();
        }
        PointerMenuKeyAction::Focus(index) => {
            event.prevent_default();
            event.stop_propagation();
            if let Some(item) = menu_item(&items, index) {
                let _ = item.focus();
            }
        }
        PointerMenuKeyAction::Activate(index) => {
            event.prevent_default();
            event.stop_propagation();
            if let Some(item) = menu_item(&items, index) {
                item.click();
            }
        }
        PointerMenuKeyAction::Ignore => {}
    }
}

fn menu_item(items: &NodeList, index: usize) -> Option<HtmlElement> {
    items.item(index as u32)?.dyn_into::<HtmlElement>().ok()
}
