//! Document-level dismissal for an open OMP hover card.
//! Canonical key, press and scroll events close the card without consuming input.
//! The owner drops this subscription when the card closes or unmounts.

#[cfg(target_arch = "wasm32")]
use crate::components::deck::deck_dom::Timeout;
#[cfg(target_arch = "wasm32")]
use std::{cell::RefCell, rc::Rc};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast as _, closure::Closure};

#[cfg(target_arch = "wasm32")]
const EVENTS: [&str; 4] = ["keydown", "click", "wheel", "scroll"];

#[cfg(target_arch = "wasm32")]
pub(super) struct DismissEvents {
    document: web_sys::Document,
    callback: Closure<dyn FnMut(web_sys::Event)>,
}

#[cfg(target_arch = "wasm32")]
impl DismissEvents {
    pub(super) fn install(close: impl FnMut() + 'static) -> Option<Self> {
        let document = web_sys::window()?.document()?;
        let close = Rc::new(RefCell::new(close));
        let pending = Rc::new(RefCell::new(None::<Timeout>));
        let callback = Closure::new(move |event: web_sys::Event| {
            if awaits_browser_activation(&event) {
                return;
            }
            let close = close.clone();
            // Capture still sees stopped events; the next task preserves their
            // target handlers and the browser's link/button activation first.
            *pending.borrow_mut() = Timeout::after(0, move || close.borrow_mut()());
        });
        let options = web_sys::AddEventListenerOptions::new();
        options.set_capture(true);
        options.set_passive(true);
        for event in EVENTS {
            let _ = document.add_event_listener_with_callback_and_add_event_listener_options(
                event,
                callback.as_ref().unchecked_ref(),
                &options,
            );
        }
        Some(Self { document, callback })
    }
}

#[cfg(target_arch = "wasm32")]
impl Drop for DismissEvents {
    fn drop(&mut self) {
        for event in EVENTS {
            let _ = self.document.remove_event_listener_with_callback_and_bool(
                event,
                self.callback.as_ref().unchecked_ref(),
                true,
            );
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn awaits_browser_activation(event: &web_sys::Event) -> bool {
    let Some(keyboard) = event.dyn_ref::<web_sys::KeyboardEvent>() else {
        return false;
    };
    if keyboard.alt_key() || keyboard.ctrl_key() || keyboard.meta_key() {
        return false;
    }
    let Some(target) = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    else {
        return false;
    };
    let Some(control) = target
        .closest("[role=tooltip] button, [role=tooltip] a[href]")
        .ok()
        .flatten()
    else {
        return false;
    };
    keyboard.key() == "Enter" || (keyboard.key() == " " && control.tag_name() == "BUTTON")
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) struct DismissEvents;

#[cfg(not(target_arch = "wasm32"))]
impl DismissEvents {
    pub(super) fn install(_close: impl FnMut() + 'static) -> Option<Self> {
        None
    }
}
