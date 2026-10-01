//! The palette's list keys: arrows move the cursor, Enter opens the row under
//! it, and the listener lives exactly as long as the body that owns the list.
//! Ports the `onKeydown` half of `apps/web/src/components/palette/CommandPaletteBody.tsx`;
//! called by `palette::body`, which hands it the rows as the last render left them.
//!
//! The listener is on `window` because the field and the rows are two different
//! elements and only one of them can hold focus. Escape is NOT here: the dialog
//! that hosts this body dismisses itself, and a second Escape handler would
//! close the palette twice for one press.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::store::palette::PaletteItem;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::closure::Closure;

/// Install the listener, returning the guard that removes it.
#[cfg(target_arch = "wasm32")]
pub fn install(
    rows: Rc<RefCell<Vec<PaletteItem>>>,
    select: EventHandler<PaletteItem>,
    mut active_index: Signal<usize>,
) -> ListKeys {
    use wasm_bindgen::JsCast as _;

    let callback =
        Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |event: web_sys::KeyboardEvent| {
            let count = rows.borrow().len();
            let cursor = *active_index.peek();
            match event.key().as_str() {
                "ArrowDown" => {
                    event.prevent_default();
                    active_index.set((cursor + 1).min(count.saturating_sub(1)));
                }
                "ArrowUp" => {
                    event.prevent_default();
                    active_index.set(cursor.saturating_sub(1));
                }
                "Enter" => {
                    event.prevent_default();
                    let selected = rows.borrow().get(cursor).cloned();
                    if let Some(item) = selected {
                        select.call(item);
                    }
                }
                _ => {}
            }
        });
    if let Some(window) = web_sys::window() {
        let _ = window.add_event_listener_with_callback_and_bool(
            "keydown",
            callback.as_ref().unchecked_ref(),
            true,
        );
    }
    ListKeys {
        callback: Some(callback),
    }
}

/// A native build has no document to listen on.
#[cfg(not(target_arch = "wasm32"))]
pub fn install(
    _rows: Rc<RefCell<Vec<PaletteItem>>>,
    _select: EventHandler<PaletteItem>,
    _active_index: Signal<usize>,
) -> ListKeys {
    ListKeys {}
}

/// The live listener, dropped with the body so a closed palette stops answering
/// keys. One shape on both targets so the guard the body holds has the same
/// lifetime everywhere; a native build simply has no callback to release.
pub struct ListKeys {
    #[cfg(target_arch = "wasm32")]
    callback: Option<Closure<dyn FnMut(web_sys::KeyboardEvent)>>,
}

impl std::fmt::Debug for ListKeys {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ListKeys")
    }
}

impl Drop for ListKeys {
    fn drop(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::JsCast as _;

            if let (Some(window), Some(callback)) = (web_sys::window(), self.callback.as_ref()) {
                let _ = window.remove_event_listener_with_callback(
                    "keydown",
                    callback.as_ref().unchecked_ref(),
                );
            }
        }
    }
}
