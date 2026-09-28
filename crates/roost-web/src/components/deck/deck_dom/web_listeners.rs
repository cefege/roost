//! The browser arm of `deck_dom`'s event listeners: a tab drag's window
//! listeners, the deck's chord listener and its touch listeners. Owners
//! remove their listeners on drop; `remove` detaches early without dropping a
//! closure that may be running.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

use super::DeckTouch;
use super::web::element;
use crate::platform::browser_platform::{ShortcutKey, shortcut_key};

type Listener = Closure<dyn FnMut(web_sys::Event)>;

/// Listeners on one target, removed on `remove` or drop.
pub struct Listeners {
    target: web_sys::EventTarget,
    attached: RefCell<Vec<(&'static str, bool, Listener)>>,
}

impl Listeners {
    fn on(target: web_sys::EventTarget) -> Self {
        Self { target, attached: RefCell::new(Vec::new()) }
    }

    fn add(&self, name: &'static str, capture: bool, passive: bool, listener: Listener) {
        let options = web_sys::AddEventListenerOptions::new();
        options.set_capture(capture);
        options.set_passive(passive);
        let _ = self.target.add_event_listener_with_callback_and_add_event_listener_options(
            name,
            listener.as_ref().unchecked_ref(),
            &options,
        );
        self.attached.borrow_mut().push((name, capture, listener));
    }

    /// Detach every listener. Idempotent; the closures stay alive until drop.
    pub fn remove(&self) {
        for (name, capture, listener) in self.attached.borrow().iter() {
            let _ = self.target.remove_event_listener_with_callback_and_bool(
                name,
                listener.as_ref().unchecked_ref(),
                *capture,
            );
        }
    }

    /// A tab drag's window listeners: move, release, cancel, in client px.
    pub fn window_pointer_drag(
        mut on_move: impl FnMut(f64, f64) + 'static,
        mut on_up: impl FnMut(f64, f64) + 'static,
        mut on_cancel: impl FnMut() + 'static,
    ) -> Option<Self> {
        let listeners = Self::on(web_sys::window()?.into());
        let point = |event: &web_sys::Event| {
            event.dyn_ref::<web_sys::PointerEvent>().map(|pointer| (f64::from(pointer.client_x()), f64::from(pointer.client_y())))
        };
        listeners.add("pointermove", false, true, Closure::new(move |event: web_sys::Event| {
            if let Some((x, y)) = point(&event) {
                on_move(x, y);
            }
        }));
        listeners.add("pointerup", false, true, Closure::new(move |event: web_sys::Event| {
            if let Some((x, y)) = point(&event) {
                on_up(x, y);
            }
        }));
        listeners.add("pointercancel", false, true, Closure::new(move |_| on_cancel()));
        Some(listeners)
    }

    /// The deck's document-level chord listener, in the capture phase; a
    /// press `on_key` claims is consumed.
    pub fn document_keys(mut on_key: impl FnMut(&ShortcutKey) -> bool + 'static) -> Option<Self> {
        let listeners = Self::on(web_sys::window()?.document()?.into());
        listeners.add("keydown", true, false, Closure::new(move |event: web_sys::Event| {
            let Some(key) = event.dyn_ref::<web_sys::KeyboardEvent>() else { return };
            if key.default_prevented() {
                return;
            }
            if on_key(&shortcut_key(key)) {
                key.prevent_default();
                key.stop_propagation();
            }
        }));
        Some(listeners)
    }

    /// The deck's touch listeners, capture phase; a move `on_touch` claims
    /// is consumed so the terminal does not also scroll.
    pub fn deck_touches(deck: &MountedData, on_touch: impl FnMut(DeckTouch) -> bool + 'static) -> Option<Self> {
        let listeners = Self::on(element(deck)?.into());
        let on_touch = Rc::new(RefCell::new(on_touch));
        let now = || web_sys::window().and_then(|window| window.performance()).map_or(0.0, |clock| clock.now());
        let first = |event: &web_sys::TouchEvent| event.touches().get(0).map(|touch| (f64::from(touch.client_x()), f64::from(touch.client_y())));
        let start = Rc::clone(&on_touch);
        listeners.add("touchstart", true, true, Closure::new(move |event: web_sys::Event| {
            let Some(touch) = event.dyn_ref::<web_sys::TouchEvent>() else { return };
            let (x, y) = first(touch).unwrap_or_default();
            (&mut *start.borrow_mut())(DeckTouch::Start { x, y, touches: touch.touches().length(), at_ms: now() });
        }));
        let moving = Rc::clone(&on_touch);
        listeners.add("touchmove", true, false, Closure::new(move |event: web_sys::Event| {
            let Some((x, y)) = event.dyn_ref::<web_sys::TouchEvent>().and_then(first) else { return };
            if (&mut *moving.borrow_mut())(DeckTouch::Move { x, y, at_ms: now() }) {
                event.prevent_default();
                event.stop_propagation();
            }
        }));
        for name in ["touchend", "touchcancel"] {
            let ending = Rc::clone(&on_touch);
            listeners.add(name, true, true, Closure::new(move |_| {
                (&mut *ending.borrow_mut())(DeckTouch::End { at_ms: now() });
            }));
        }
        Some(listeners)
    }
}

impl Drop for Listeners {
    fn drop(&mut self) {
        self.remove();
    }
}

impl std::fmt::Debug for Listeners {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Listeners").field("attached", &self.attached.borrow().len()).finish()
    }
}
