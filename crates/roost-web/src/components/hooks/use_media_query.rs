//! rust-ui's `use_media_query`: a signal tracking whether a CSS media query
//! matches, and the one place the client asks the browser `matchMedia`
//! ([`media_query_match`] answers once, for readers outside a component).
//! Unlike the registry source, the first render already holds the real answer
//! and the `change` listener leaves with its component.

#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

use dioxus::prelude::*;

/// Whether `query` matches now, or `None` where the browser cannot evaluate a
/// media query.
pub fn media_query_match(query: &str) -> Option<bool> {
    #[cfg(target_arch = "wasm32")]
    {
        media_query_list(query).map(|list| list.matches())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = query;
        None
    }
}

/// Whether `query` matches, kept current as the viewport and the OS
/// preferences change.
#[must_use]
pub fn use_media_query(query: &str) -> ReadSignal<bool> {
    // Read synchronously: a first render that assumed `false` would draw the
    // desktop layout on a phone and swap it a frame later.
    let is_match = use_signal(|| media_query_match(query) == Some(true));
    #[cfg(target_arch = "wasm32")]
    {
        let _listener = use_hook(|| Rc::new(MatchListener::attach(query, is_match)));
    }
    // Converted once: every `From<Signal>` allocates a box the scope keeps.
    use_hook(move || ReadSignal::from(is_match))
}

#[cfg(target_arch = "wasm32")]
fn media_query_list(query: &str) -> Option<web_sys::MediaQueryList> {
    web_sys::window()?.match_media(query).ok().flatten()
}

/// One hook instance's `change` listener. It lives in the component's hook
/// list, so unmounting drops it and removes the listener: a callback that
/// outlived its component would write a dropped signal.
#[cfg(target_arch = "wasm32")]
struct MatchListener {
    list: web_sys::MediaQueryList,
    on_change: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::MediaQueryListEvent)>,
}

#[cfg(target_arch = "wasm32")]
impl MatchListener {
    fn attach(query: &str, is_match: Signal<bool>) -> Option<Self> {
        use wasm_bindgen::JsCast as _;

        let list = media_query_list(query)?;
        let on_change =
            wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::MediaQueryListEvent)>::new(
                move |event: web_sys::MediaQueryListEvent| {
                    let mut is_match = is_match;
                    is_match.set(event.matches());
                },
            );
        list.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref())
            .ok()?;
        Some(Self { list, on_change })
    }
}

#[cfg(target_arch = "wasm32")]
impl Drop for MatchListener {
    fn drop(&mut self) {
        use wasm_bindgen::JsCast as _;

        let _ = self
            .list
            .remove_event_listener_with_callback("change", self.on_change.as_ref().unchecked_ref());
    }
}
