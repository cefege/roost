//! Browser focus and visibility tracking for terminal notification delivery.
//! Listeners update the attention signal and are removed when its scope ends.
//! Clipboard and notification presenters use the same foreground test.

use std::rc::Rc;

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast as _;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::closure::Closure;
#[cfg(target_arch = "wasm32")]
use web_sys::{Event, EventTarget};

use crate::platform::visibility::page_visible;

/// Whether this tab is foregrounded and its window owns input focus, as a
/// signal a render can subscribe to.
pub fn use_page_attention() -> Signal<bool> {
    // `Rc` because a hook slot is `Clone`-bound, and a guard that owns DOM
    // listeners is not: the slot keeps the registration for the life of the
    // scope, which is exactly the lifetime a listener needs.
    use_hook(|| Rc::new(PageAttention::install())).read
}

/// Whether the page is foregrounded AND the window owns input focus — the
/// same gate a card checks before it treats a session as already looked at.
pub(in crate::components::notifications) fn attended_now() -> bool {
    page_visible() && window_focused()
}

/// Whether this window owns input focus. A browser that will not answer counts
/// as focused: acknowledging is the direction to lose, and a reader whose
/// browser declines the question must still get the acknowledgement their view
/// earned.
#[cfg(target_arch = "wasm32")]
fn window_focused() -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .is_none_or(|document| document.has_focus().unwrap_or(true))
}

/// No window to ask, so a native render is looking at its page.
#[cfg(not(target_arch = "wasm32"))]
fn window_focused() -> bool {
    true
}

/// One installed listener: its target, its event name, and the closure the
/// browser holds a reference to.
#[cfg(target_arch = "wasm32")]
type AttentionListener = (EventTarget, &'static str, Closure<dyn FnMut(Event)>);

/// The attention signal plus the listeners that move it.
///
/// Visibility and focus are EDGES, not values: what is owed is owed on the way
/// BACK, and nothing re-renders unless a listener writes the new answer. Each
/// listener is held here and taken back off when the scope that installed it
/// ends, so a remount cannot stack a second set on the same document.
struct PageAttention {
    read: Signal<bool>,
    #[cfg(target_arch = "wasm32")]
    listeners: Vec<AttentionListener>,
}

impl Drop for PageAttention {
    fn drop(&mut self) {
        #[cfg(target_arch = "wasm32")]
        for (target, kind, listener) in self.listeners.drain(..) {
            let _ =
                target.remove_event_listener_with_callback(kind, listener.as_ref().unchecked_ref());
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl PageAttention {
    fn install() -> Self {
        // `mut` because every listener below writes it, and `Signal::set` takes
        // `&mut self` on the captured copy.
        let mut read = Signal::new(attended_now());
        let mut listeners = Vec::new();
        let (Some(window), Some(document)) = (
            web_sys::window().map(|window| window.unchecked_into::<EventTarget>()),
            web_sys::window()
                .and_then(|window| window.document())
                .map(|document| document.unchecked_into::<EventTarget>()),
        ) else {
            return Self {
                read,
                listeners: Vec::new(),
            };
        };
        for (target, kind) in [
            (window.clone(), "focus"),
            (window, "blur"),
            (document, "visibilitychange"),
        ] {
            let listener = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
                read.set(attended_now());
            });
            if target
                .add_event_listener_with_callback(kind, listener.as_ref().unchecked_ref())
                .is_err()
            {
                tracing::warn!(
                    target: "notifications",
                    event = kind,
                    "page attention listener refused"
                );
                continue;
            }
            listeners.push((target, kind, listener));
        }
        Self { read, listeners }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl PageAttention {
    fn install() -> Self {
        Self {
            read: Signal::new(attended_now()),
        }
    }
}
