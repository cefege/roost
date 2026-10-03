//! The browser half of the floating-menu primitives: measuring a trigger,
//! focusing a menu edge after it mounts, running a menu key, and the document
//! listeners behind outside-click/Escape dismissal. wasm32 only; the decisions
//! are `super::{anchored_menu_pos, menu_key_action}` and the focus request is
//! `super::MenuFocusRequest`. Ports the DOM half of
//! `apps/web/src/components/contextMenuPrimitives.tsx`.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::EventHandler;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use web_sys::{Element, HtmlElement, KeyboardEvent};

use super::{
    AnchoredMenuPos, MenuFocusEdge, MenuFocusRequest, MenuKeyAction, anchored_menu_pos,
    menu_key_action,
};

const ENABLED_ITEMS: &str = "[role=\"menuitem\"]:not(:disabled)";
const FOCUS_ATTEMPTS: u32 = 4;

/// Where a menu anchored under `trigger` sits.
pub fn anchored_menu_position(trigger: &Element) -> AnchoredMenuPos {
    let rect = trigger.get_bounding_client_rect();
    let width = web_sys::window()
        .and_then(|window| window.inner_width().ok())
        .and_then(|value| value.as_f64())
        .unwrap_or(rect.right());
    anchored_menu_pos(rect.right(), rect.bottom(), width)
}

fn enabled_items(menu: &Element) -> Vec<HtmlElement> {
    let Ok(nodes) = menu.query_selector_all(ENABLED_ITEMS) else {
        return Vec::new();
    };
    (0..nodes.length())
        .filter_map(|index| nodes.item(index)?.dyn_into::<HtmlElement>().ok())
        .collect()
}

fn active_index(items: &[HtmlElement]) -> Option<usize> {
    let active = web_sys::window()?.document()?.active_element()?;
    items.iter().position(|item| {
        let item: &Element = item.as_ref();
        item == &active
    })
}

/// The self-replacing attempt: a closure the cell keeps so each retry can
/// hand the browser its own successor.
type StepCell = Rc<RefCell<Option<Closure<dyn FnMut()>>>>;

/// Focus the menu `menu_id`'s first or last enabled item once it has mounted.
///
/// The first attempt is a microtask, as in v2: the opening key's render runs
/// in an earlier microtask of the same task, so the item is focused before
/// the next key arrives and that key reaches the menu, not the trigger. A
/// portal that misses it is retried by frame.
pub fn focus_menu_edge(menu_id: &str, edge: MenuFocusEdge) -> MenuFocusRequest {
    let request = MenuFocusRequest::new(menu_id, edge);
    let Some(window) = web_sys::window() else {
        return request;
    };
    let pending = request.clone();
    let mut attempts = 0_u32;
    let step: StepCell = Rc::new(RefCell::new(None));
    let next = Rc::clone(&step);
    *step.borrow_mut() = Some(Closure::new(move || {
        attempt_menu_focus(&pending);
        attempts += 1;
        if !pending.is_pending() || attempts >= FOCUS_ATTEMPTS {
            pending.cancel();
            next.borrow_mut().take();
            return;
        }
        if let (Some(window), Some(callback)) = (web_sys::window(), next.borrow().as_ref()) {
            let _ = window.request_animation_frame(callback.as_ref().unchecked_ref());
        }
    }));
    if let Some(callback) = step.borrow().as_ref() {
        window.queue_microtask(callback.as_ref().unchecked_ref());
    }
    request
}

/// Attempt `request` now: focus its edge item when the request is pending and
/// the menu is in the document, settling the request once focus is there. A
/// menu's `onmounted` calls this so focus lands in the render that inserted it.
pub fn attempt_menu_focus(request: &MenuFocusRequest) {
    if !request.is_pending() {
        return;
    }
    let Some(menu) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(request.menu_id()))
        .filter(|menu| menu.is_connected())
    else {
        return;
    };
    let items = enabled_items(&menu);
    let index = match request.edge() {
        MenuFocusEdge::First => 0,
        MenuFocusEdge::Last => items.len().saturating_sub(1),
    };
    let Some(target) = items.get(index) else {
        return;
    };
    let _ = target.focus();
    if active_index(&items) == Some(index) {
        request.cancel();
    }
}

/// Apply one key to the menu `menu`: roving focus, activation, Escape and Tab.
pub fn run_menu_key(
    event: &KeyboardEvent,
    menu: &Element,
    on_escape: impl FnOnce(),
    on_tab: impl FnOnce() + 'static,
) {
    let items = enabled_items(menu);
    match menu_key_action(&event.key(), active_index(&items), items.len()) {
        MenuKeyAction::Escape => {
            event.prevent_default();
            event.stop_propagation();
            on_escape();
        }
        MenuKeyAction::Tab => {
            // Native sequential focus leaves the programmatic item first.
            let deferred = Closure::once(move |_: wasm_bindgen::JsValue| on_tab());
            let _ = js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED).then(&deferred);
            deferred.forget();
        }
        MenuKeyAction::Focus(index) => {
            event.prevent_default();
            event.stop_propagation();
            if let Some(item) = items.get(index) {
                let _ = item.focus();
            }
        }
        MenuKeyAction::Activate(index) => {
            event.prevent_default();
            event.stop_propagation();
            if let Some(item) = items.get(index) {
                item.click();
            }
        }
        MenuKeyAction::Ignore => {}
    }
}

/// The document listeners one open menu holds.
pub struct DismissListeners {
    click: Closure<dyn FnMut(web_sys::MouseEvent)>,
    keydown: Closure<dyn FnMut(KeyboardEvent)>,
}

impl DismissListeners {
    /// Listen on the document until `remove`.
    pub fn install(
        on_close: EventHandler<()>,
        on_escape: Option<EventHandler<()>>,
        within_ids: Vec<String>,
    ) -> Self {
        let click =
            Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |event: web_sys::MouseEvent| {
                let target = event
                    .target()
                    .and_then(|target| target.dyn_into::<web_sys::Node>().ok());
                let document = web_sys::window().and_then(|window| window.document());
                let inside = match (target, document) {
                    (Some(target), Some(document)) => within_ids.iter().any(|id| {
                        document
                            .get_element_by_id(id)
                            .is_some_and(|element| element.contains(Some(&target)))
                    }),
                    _ => false,
                };
                if !inside {
                    on_close.call(());
                }
            });
        let keydown = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            if event.key() == "Escape" {
                on_escape.unwrap_or(on_close).call(());
            }
        });
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            let _ =
                document.add_event_listener_with_callback("click", click.as_ref().unchecked_ref());
            let _ = document
                .add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref());
        }
        Self { click, keydown }
    }

    /// Stop listening.
    pub fn remove(&self) {
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            let _ = document
                .remove_event_listener_with_callback("click", self.click.as_ref().unchecked_ref());
            let _ = document.remove_event_listener_with_callback(
                "keydown",
                self.keydown.as_ref().unchecked_ref(),
            );
        }
    }
}
