//! The picker's `window` keydown listener: it gathers the facts
//! `browse::keys::picker_key` decides on and runs the answer. The listener is
//! the only part that touches the document, so the decision stays a pure
//! function a test can reach.
//!
//! Installed for the life of `browse::picker::BrowsePicker`. Ports the listener
//! `apps/web/src/components/browse/browsePickerKeys.ts` installs.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::components::browse::keys::{PickerKey, PickerKeyContext};

#[cfg(target_arch = "wasm32")]
use crate::components::browse::dom;
#[cfg(target_arch = "wasm32")]
use crate::components::browse::keys::picker_key;
#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;

/// The listener, for the life of the page.
pub struct PickerKeysGuard {
    #[cfg(target_arch = "wasm32")]
    listener: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::KeyboardEvent)>,
}

impl std::fmt::Debug for PickerKeysGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PickerKeysGuard")
    }
}

impl Drop for PickerKeysGuard {
    fn drop(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::JsCast as _;
            if let Some(window) = web_sys::window() {
                let _ = window.remove_event_listener_with_callback(
                    "keydown",
                    self.listener.as_ref().unchecked_ref(),
                );
            }
        }
    }
}

/// Install the listener. `facts` re-reads the store on every press, so a key
/// never acts on a value the page has already moved past.
pub fn install_picker_keys(
    facts: impl Fn() -> PickerKeyContext + 'static,
    on_key: impl FnMut(PickerKey) + 'static,
) -> std::rc::Rc<PickerKeysGuard> {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast as _;
        use wasm_bindgen::closure::Closure;

        let on_key = RefCell::new(on_key);
        let listener = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
            move |event: web_sys::KeyboardEvent| {
                let context = facts();
                let action = picker_key(
                    &event.key(),
                    event.alt_key(),
                    &PickerKeyContext {
                        inside_results: dom::press_path_contains(dom::RESULTS_ID, &event),
                        ..context
                    },
                );
                if matches!(action, PickerKey::Ignore) {
                    return;
                }
                event.prevent_default();
                if let Ok(mut on_key) = on_key.try_borrow_mut() {
                    on_key(action);
                }
            },
        );
        if let Some(window) = web_sys::window() {
            let _ = window
                .add_event_listener_with_callback("keydown", listener.as_ref().unchecked_ref());
        }
        tracing::debug!(target: "browse", "picker keys installed");
        return Rc::new(PickerKeysGuard { listener });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (facts, on_key);
        Rc::new(PickerKeysGuard {})
    }
}

/// Keep the guard alive for as long as the scope that installed it.
pub fn use_picker_keys(
    facts: impl Fn() -> PickerKeyContext + 'static,
    on_key: impl FnMut(PickerKey) + 'static,
) {
    use_hook(|| install_picker_keys(facts, on_key));
}
