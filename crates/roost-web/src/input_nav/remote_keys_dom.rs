//! The keydown listener that gives a TV remote the controller's A/B meaning:
//! Back leaves whatever the operator is in, OK on the terminal box opens the
//! key tray. Installed once by the App beside the Gamepad poll, sharing its
//! shell surfaces and router. Bubble phase, so a surface that owns Enter has
//! already had it. Classification is `remote_keys`; the chain is `pad_router`.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::{ReadableExt as _, Signal};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use web_sys::KeyboardEvent;

use crate::input_nav::modality::NavModality;
use crate::input_nav::pad_dom::dispatch_remote_key;
use crate::input_nav::pad_router::PadActionRouter;
use crate::input_nav::pad_shell::ShellPadSurfaces;
use crate::input_nav::remote_keys::classify_remote_key;

/// Removes the keydown listener when dropped (App unmount).
pub struct RemoteKeysGuard {
    listener: Closure<dyn FnMut(KeyboardEvent)>,
}

impl std::fmt::Debug for RemoteKeysGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RemoteKeysGuard")
    }
}

impl Drop for RemoteKeysGuard {
    fn drop(&mut self) {
        if let Some(window) = web_sys::window() {
            let _ = window.remove_event_listener_with_callback(
                "keydown",
                self.listener.as_ref().unchecked_ref(),
            );
        }
    }
}

/// Listen for remote keys for the lifetime of the guard. Inert unless TV mode
/// is on, so a desktop's Enter and browser-back keys keep their meaning.
pub fn install_remote_keys(
    modality: Signal<NavModality>,
    router: Signal<PadActionRouter>,
    surfaces: Rc<RefCell<ShellPadSurfaces>>,
) -> RemoteKeysGuard {
    let listener = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        if event.default_prevented() || !modality.peek().tv_mode_active() {
            return;
        }
        let modified = event.meta_key() || event.ctrl_key() || event.alt_key() || event.shift_key();
        let Some(key) = classify_remote_key(&event.key(), event.key_code(), modified) else {
            return;
        };
        // A pad press already holds the surfaces when its poll synthesizes a
        // key; that key is never a remote press, so skipping it loses nothing.
        let Ok(mut surfaces) = surfaces.try_borrow_mut() else {
            return;
        };
        if dispatch_remote_key(router, &mut *surfaces, key) {
            event.prevent_default();
        }
    });
    if let Some(window) = web_sys::window() {
        let _ =
            window.add_event_listener_with_callback("keydown", listener.as_ref().unchecked_ref());
    }
    tracing::info!(target: "input_nav", "remote keys installed");
    RemoteKeysGuard { listener }
}
