//! The browser arm of `deck_dom`'s event listeners: a tab drag's window
//! listeners, the deck's chord listener and its touch listeners, plus the
//! per-touch listeners on a target the terminal renderer may detach. Owners
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
type TouchSink = Rc<RefCell<dyn FnMut(DeckTouch) -> bool>>;

/// Listeners on one target, removed on `remove` or drop.
pub struct Listeners {
    target: web_sys::EventTarget,
    attached: RefCell<Vec<(&'static str, bool, Listener)>>,
}

impl Listeners {
    fn on(target: web_sys::EventTarget) -> Self {
        Self {
            target,
            attached: RefCell::new(Vec::new()),
        }
    }

    fn add(&self, name: &'static str, capture: bool, passive: bool, listener: Listener) {
        let options = web_sys::AddEventListenerOptions::new();
        options.set_capture(capture);
        options.set_passive(passive);
        let _ = self
            .target
            .add_event_listener_with_callback_and_add_event_listener_options(
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
        pointer_id: i32,
        mut on_move: impl FnMut(f64, f64) + 'static,
        mut on_up: impl FnMut(f64, f64) + 'static,
        on_cancel: impl FnMut() + 'static,
    ) -> Option<Self> {
        let listeners = Self::on(web_sys::window()?.into());
        let point = move |event: &web_sys::Event| {
            event
                .dyn_ref::<web_sys::PointerEvent>()
                .filter(|pointer| pointer.pointer_id() == pointer_id)
                .map(|pointer| (f64::from(pointer.client_x()), f64::from(pointer.client_y())))
        };
        let on_cancel = Rc::new(RefCell::new(on_cancel));
        let cancel_pointer = Rc::clone(&on_cancel);
        listeners.add(
            "pointermove",
            false,
            true,
            Closure::new(move |event: web_sys::Event| {
                if let Some((x, y)) = point(&event) {
                    on_move(x, y);
                }
            }),
        );
        listeners.add(
            "pointerup",
            false,
            true,
            Closure::new(move |event: web_sys::Event| {
                if let Some((x, y)) = point(&event) {
                    on_up(x, y);
                }
            }),
        );
        listeners.add(
            "pointercancel",
            false,
            true,
            Closure::new(move |event: web_sys::Event| {
                if point(&event).is_some() {
                    cancel_pointer.borrow_mut()();
                }
            }),
        );
        listeners.add(
            "blur",
            false,
            true,
            Closure::new(move |_| on_cancel.borrow_mut()()),
        );
        Some(listeners)
    }

    /// The deck's document-level chord listener, in the capture phase; a
    /// press `on_key` claims is consumed.
    pub fn document_keys(mut on_key: impl FnMut(&ShortcutKey) -> bool + 'static) -> Option<Self> {
        let listeners = Self::on(web_sys::window()?.document()?.into());
        listeners.add(
            "keydown",
            true,
            false,
            Closure::new(move |event: web_sys::Event| {
                let Some(key) = event.dyn_ref::<web_sys::KeyboardEvent>() else {
                    return;
                };
                if key.default_prevented() {
                    return;
                }
                if on_key(&shortcut_key(key)) {
                    key.prevent_default();
                    key.stop_propagation();
                }
            }),
        );
        Some(listeners)
    }

    /// The deck's touch listeners, capture phase; a move `on_touch` claims
    /// is consumed so the terminal does not also scroll. Each touch is also
    /// followed on its own target: the terminal renderer replaces a row
    /// element whose text changed, and a touch whose target has left the
    /// document is dispatched to that detached node alone, so the deck would
    /// see neither the drag's later moves nor its release.
    pub fn deck_touches(
        deck: &MountedData,
        on_touch: impl FnMut(DeckTouch) -> bool + 'static,
    ) -> Option<Self> {
        let deck_element = element(deck)?;
        let deck_node: web_sys::Node = deck_element.clone().into();
        let listeners = Self::on(deck_element.into());
        let on_touch: TouchSink = Rc::new(RefCell::new(on_touch));
        // Replaced at every touchstart, from the deck's own listener, so a
        // follower is never dropped while one of its closures is running.
        let follower: RefCell<Option<Listeners>> = RefCell::new(None);
        let start = Rc::clone(&on_touch);
        listeners.add(
            "touchstart",
            true,
            true,
            Closure::new(move |event: web_sys::Event| {
                let Some(touch) = event.dyn_ref::<web_sys::TouchEvent>() else {
                    return;
                };
                *follower.borrow_mut() = event
                    .target()
                    .and_then(|target| Self::follow_touch_target(target, &deck_node, &start));
                let (x, y) = first_touch(touch).unwrap_or_default();
                start.borrow_mut()(DeckTouch::Start {
                    x,
                    y,
                    touches: touch.touches().length(),
                    at_ms: now_ms(),
                });
            }),
        );
        let moving = Rc::clone(&on_touch);
        listeners.add(
            "touchmove",
            true,
            false,
            Closure::new(move |event: web_sys::Event| deliver_touch_move(&moving, &event)),
        );
        let ending = Rc::clone(&on_touch);
        listeners.add(
            "touchend",
            true,
            true,
            Closure::new(move |_| {
                ending.borrow_mut()(DeckTouch::End { at_ms: now_ms() });
            }),
        );
        listeners.add(
            "touchcancel",
            true,
            true,
            Closure::new(move |_| {
                on_touch.borrow_mut()(DeckTouch::Cancel);
            }),
        );
        Some(listeners)
    }

    /// Listeners on one touch's target that deliver its moves and its end
    /// only once the deck no longer contains it; while it does, the deck's
    /// capture listeners already saw the event.
    fn follow_touch_target(
        target: web_sys::EventTarget,
        deck: &web_sys::Node,
        on_touch: &TouchSink,
    ) -> Option<Self> {
        let on_touch = Rc::clone(on_touch);
        Self::follow_detached_touches(target, deck, move |event| match event.type_().as_str() {
            "touchmove" => deliver_touch_move(&on_touch, &event),
            "touchend" => {
                on_touch.borrow_mut()(DeckTouch::End { at_ms: now_ms() });
            }
            "touchcancel" => {
                on_touch.borrow_mut()(DeckTouch::Cancel);
            }
            _ => {}
        })
    }

    /// Follow an origin detached from its capture owner. Keep this follower
    /// until the next touchstart, never drop it from its own event callback.
    pub fn follow_detached_touches(
        target: web_sys::EventTarget,
        owner: &web_sys::Node,
        on_event: impl FnMut(web_sys::Event) + 'static,
    ) -> Option<Self> {
        let target_node = target.dyn_ref::<web_sys::Node>()?.clone();
        let follower = Self::on(target);
        let on_event = Rc::new(RefCell::new(on_event));
        for name in ["touchmove", "touchend", "touchcancel"] {
            let owner = owner.clone();
            let target_node = target_node.clone();
            let on_event = Rc::clone(&on_event);
            follower.add(
                name,
                false,
                name != "touchmove",
                Closure::new(move |event: web_sys::Event| {
                    if !owner.contains(Some(&target_node)) {
                        on_event.borrow_mut()(event);
                    }
                }),
            );
        }
        Some(follower)
    }
}

impl Drop for Listeners {
    fn drop(&mut self) {
        self.remove();
    }
}

impl std::fmt::Debug for Listeners {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Listeners")
            .field("attached", &self.attached.borrow().len())
            .finish()
    }
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |clock| clock.now())
}

fn first_touch(event: &web_sys::TouchEvent) -> Option<(f64, f64)> {
    event
        .touches()
        .get(0)
        .map(|touch| (f64::from(touch.client_x()), f64::from(touch.client_y())))
}

/// A touch move for the swipe; one it claims is consumed so the terminal
/// does not also scroll it.
fn deliver_touch_move(on_touch: &TouchSink, event: &web_sys::Event) {
    let Some(touch) = event.dyn_ref::<web_sys::TouchEvent>() else {
        return;
    };
    if touch.touches().length() != 1 {
        on_touch.borrow_mut()(DeckTouch::Cancel);
        return;
    }
    let Some((x, y)) = first_touch(touch) else {
        return;
    };
    if on_touch.borrow_mut()(DeckTouch::Move {
        x,
        y,
        at_ms: now_ms(),
    }) {
        event.prevent_default();
        event.stop_propagation();
    }
}
