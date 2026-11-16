//! The picker's document-capture key listener and scope-owned action callback.
//! `browse::picker` mounts it; teardown removes it. Local navigation runs before
//! the window's directional-focus fallback. DOM facts feed `keys::picker_key`;
//! actions execute inside the picker's Dioxus runtime and scope.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::components::browse::keys::{PickerKey, PickerKeyContext};

#[cfg(target_arch = "wasm32")]
use crate::components::browse::dom;
#[cfg(target_arch = "wasm32")]
use crate::components::browse::keys::picker_key;
#[cfg(target_arch = "wasm32")]
use crate::components::ui::dom::focus_by_id;
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
            if let Some(document) = web_sys::window().and_then(|window| window.document()) {
                let _ = document.remove_event_listener_with_callback_and_bool(
                    "keydown",
                    self.listener.as_ref().unchecked_ref(),
                    true,
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
        // Native key callbacks need the owner scope when they spawn picker tasks.
        let on_key = EventHandler::new(move |action| {
            if let Ok(mut on_key) = on_key.try_borrow_mut() {
                on_key(action);
            }
        });
        let typeahead = RefCell::new((String::new(), 0.0_f64));
        let listener = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
            move |event: web_sys::KeyboardEvent| {
                let context = facts();
                if event.key().eq_ignore_ascii_case("l")
                    && (event.ctrl_key() || event.meta_key())
                    && !context.dialog_open
                    && !event.default_prevented()
                {
                    event.prevent_default();
                    focus_by_id("browse-path-input");
                    return;
                }
                let target = event
                    .target()
                    .and_then(|target| target.dyn_into::<web_sys::Element>().ok());
                let independent_control = target.as_ref().is_some_and(|element| {
                    element
                        .closest("a, button, input, textarea, select, [contenteditable='true']")
                        .ok()
                        .flatten()
                        .is_some()
                        && element
                            .closest("[data-browse-folder]")
                            .ok()
                            .flatten()
                            .is_none()
                });
                if independent_control || event.is_composing() {
                    return;
                }
                let context = PickerKeyContext {
                    inside_results: dom::press_path_contains(dom::RESULTS_ID, &event),
                    default_prevented: event.default_prevented(),
                    ..context
                };
                let action = if event.key() == "Enter"
                    && (event.ctrl_key() || event.meta_key())
                    && context.scoped
                    && context.inside_results
                    && !context.dialog_open
                    && !context.default_prevented
                {
                    PickerKey::OpenHere
                } else if context.inside_results
                    && context.scoped
                    && !context.dialog_open
                    && !context.default_prevented
                    && !event.ctrl_key()
                    && !event.meta_key()
                    && (event.key() == "/" || event.key() == "~")
                {
                    PickerKey::EditPath(event.key())
                } else if context.inside_results
                    && context.scoped
                    && !context.dialog_open
                    && !context.default_prevented
                    && !event.ctrl_key()
                    && !event.meta_key()
                    && !event.alt_key()
                    && event.key().chars().count() == 1
                    && event.key() != " "
                {
                    let mut typed = typeahead.borrow_mut();
                    if event.time_stamp() - typed.1 > 1000.0 {
                        typed.0.clear();
                    }
                    typed.0.push_str(&event.key().to_lowercase());
                    typed.1 = event.time_stamp();
                    PickerKey::Typeahead(typed.0.clone())
                } else {
                    picker_key(&event.key(), event.alt_key(), &context)
                };
                if matches!(action, PickerKey::Ignore) {
                    return;
                }
                let action = match action {
                    PickerKey::Move(delta)
                        if matches!(event.key().as_str(), "ArrowUp" | "ArrowDown") =>
                    {
                        PickerKey::Move(delta * dom::folder_row_stride())
                    }
                    action => action,
                };
                event.prevent_default();
                on_key.call(action);
            },
        );
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            let _ = document.add_event_listener_with_callback_and_bool(
                "keydown",
                listener.as_ref().unchecked_ref(),
                true,
            );
        }
        tracing::debug!(target: "browse", "picker keys installed");
        Rc::new(PickerKeysGuard { listener })
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
