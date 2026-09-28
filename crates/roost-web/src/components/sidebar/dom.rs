//! The sidebar's browser adapters: the document-level Cmd-F listener, focusing
//! the filter input on the next frame, the debounce timer, and the
//! `terminalOwnsKeyboard` DOM read v2's `SidebarRoot.tsx` made. Called only by
//! `sidebar_root`; the chord decision is `platform::browser_platform`'s.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

use super::sidebar_new_terminal::{MachineMenuAnchor, machine_menu_anchor};
use super::sidebar_search::SIDEBAR_SEARCH_INPUT_ID;
use crate::platform::browser_platform::{
    PlatformShortcut, browser_platform, matches_platform_shortcut, shortcut_key,
};

/// Removes the keydown listener when dropped.
pub struct SearchShortcutListener {
    callback: Closure<dyn FnMut(web_sys::KeyboardEvent)>,
}

impl std::fmt::Debug for SearchShortcutListener {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SearchShortcutListener")
            .finish_non_exhaustive()
    }
}

impl Drop for SearchShortcutListener {
    fn drop(&mut self) {
        if let Some(document) = web_sys::window().and_then(|window| window.document()) {
            let _ = document.remove_event_listener_with_callback(
                "keydown",
                self.callback.as_ref().unchecked_ref(),
            );
        }
    }
}

/// Listen for the sidebar-search shortcut on the document. `on_match` runs
/// after the event is claimed, then the filter input is focused next frame.
pub fn listen_for_search_shortcut(on_match: impl Fn() + 'static) -> Option<SearchShortcutListener> {
    let document = web_sys::window()?.document()?;
    let platform = browser_platform();
    let callback =
        Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |event: web_sys::KeyboardEvent| {
            if event.default_prevented() || terminal_owns_keyboard() {
                return;
            }
            if !matches_platform_shortcut(
                &shortcut_key(&event),
                PlatformShortcut::SidebarSearch,
                platform,
            ) {
                return;
            }
            event.prevent_default();
            on_match();
            focus_search_input_next_frame();
        });
    document
        .add_event_listener_with_callback("keydown", callback.as_ref().unchecked_ref())
        .ok()?;
    Some(SearchShortcutListener { callback })
}

/// A mounted terminal deck or pane owns the keyboard.
fn terminal_owns_keyboard() -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .is_some_and(|document| {
            ["[data-testid=\"terminal-deck\"]", "[data-pane]"]
                .iter()
                .any(|selector| document.query_selector(selector).ok().flatten().is_some())
        })
}

fn focus_search_input_next_frame() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let focus = Closure::once_into_js(move || {
        let input = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id(SIDEBAR_SEARCH_INPUT_ID))
            .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
        if let Some(input) = input {
            let _ = input.focus();
        }
    });
    let _ = window.request_animation_frame(focus.unchecked_ref());
}

/// Whether the document is in the foreground; the age ticker pauses while not.
pub fn page_visible() -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .is_none_or(|document| !document.hidden())
}

/// Resolve after `delay_ms`, for the filter debounce and the age ticker.
pub async fn sleep_ms(delay_ms: u32) {
    let resolved: Rc<RefCell<Option<js_sys::Function>>> = Rc::new(RefCell::new(None));
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        *resolved.borrow_mut() = Some(resolve);
    });
    let resolve = resolved.borrow_mut().take();
    if let (Some(window), Some(resolve)) = (web_sys::window(), resolve) {
        let delay = i32::try_from(delay_ms).unwrap_or(i32::MAX);
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, delay);
    }
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// The upward anchor for the menu under the trigger `trigger_id`.
pub fn machine_trigger_anchor(trigger_id: &str) -> Option<MachineMenuAnchor> {
    let window = web_sys::window()?;
    let rect = window
        .document()?
        .get_element_by_id(trigger_id)?
        .get_bounding_client_rect();
    let dimension = |read: Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>| {
        read.ok().and_then(|value| value.as_f64())
    };
    let width = dimension(window.inner_width()).unwrap_or(rect.right());
    let height = dimension(window.inner_height()).unwrap_or(rect.bottom());
    Some(machine_menu_anchor(
        rect.right(),
        rect.top(),
        rect.bottom(),
        width,
        height,
    ))
}

/// Roving focus, activation and Escape inside the menu `menu_id`.
pub fn run_menu_key_by_id(
    event: &dioxus::prelude::KeyboardEvent,
    menu_id: &str,
    on_escape: impl FnOnce() + 'static,
) {
    let Some(native) = event.data().downcast::<web_sys::KeyboardEvent>().cloned() else {
        return;
    };
    let Some(menu) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(menu_id))
    else {
        return;
    };
    crate::components::context_menu::run_menu_key(&native, &menu, on_escape, || {});
}

/// Focus the element `id`, when it is focusable.
pub fn focus_by_id(id: &str) {
    let element = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(id))
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
    if let Some(element) = element {
        let _ = element.focus();
    }
}
