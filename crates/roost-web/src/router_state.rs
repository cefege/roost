//! In-app navigation: the path the shell renders, kept in step with the address
//! bar in both directions, and the v2 `useNavigate`/`useLocation` pair every
//! component below the gate reads from context. Owned by `GatedApp`; every rail,
//! title, sidebar row, deck tab and error link navigates through it. Ports the
//! router half of `apps/web/src/App.tsx`.
//!
//! WHY THIS EXISTS. `pushState` moves the address bar without reloading, and
//! nothing re-renders on its own, so a link that only changed the URL left the
//! shell painting the path it had before. The old answer was to let the anchor
//! do a full document load — which tears down `ClientCore`, every replica and
//! every socket, and sends the access gate back to `Checking` for the length of a
//! network round trip. So the two halves have to be one operation: this module
//! writes the address bar AND the signal, and a `popstate` listener does the
//! same for Back and Forward.
//!
//! The signal is the render source of truth, not `location`, because only the
//! signal re-renders. `popstate` is what keeps the two from drifting when the
//! reader uses the browser's own Back button rather than a rail.

use dioxus::prelude::*;

use crate::platform::location::{current_location, navigate};

/// The path the shell renders, seeded from the address bar and kept in step
/// with it.
///
/// The listener is installed once per shell, not once per render: a listener
/// re-registered on every render would fire N times per Back press.
pub fn use_path_signal() -> Signal<String> {
    let path = use_hook(|| Signal::new(current_location()));
    #[cfg(target_arch = "wasm32")]
    use_hook(move || install_popstate_listener(path));
    path
}

/// The handler every in-app link hands its clicks to.
///
/// It closes over the signal so that one call moves BOTH the address bar and
/// the rendered path, which is the whole point: a link that moved only one of
/// them either repaints nothing or reloads the document. A link already showing
/// is not re-navigated, because `pushState` would add a history entry the reader
/// has to press Back through twice to leave a page they never left.
pub fn navigation_handler(path: Signal<String>) -> EventHandler<String> {
    EventHandler::new(move |next: String| navigate_path(path, next))
}

/// Move the address bar and the rendered path together, from anywhere — a
/// timer or a listener outside a component's handler included, which is why the
/// safety net and the UI bridge call this rather than an `EventHandler`.
pub fn navigate_path(path: Signal<String>, next: String) {
    let mut path = path;
    if *path.peek() == next {
        return;
    }
    tracing::info!(target: "router", to = %next, "navigate");
    navigate(&next);
    path.set(next);
}

/// The router in context: the rendered path and the handler that moves it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouterContext {
    /// The path the shell renders.
    pub path: Signal<String>,
    /// Moves the address bar and `path` together.
    pub navigate: EventHandler<String>,
}

/// Provide the router to every component below the calling one.
pub fn provide_router(path: Signal<String>, navigate: EventHandler<String>) {
    use_context_provider(|| RouterContext { path, navigate });
}

/// v2 `useNavigate`: the handler an in-app link or action calls with an href.
pub fn use_navigate() -> EventHandler<String> {
    use_context::<RouterContext>().navigate
}

/// v2 `useLocation().pathname`: the rendered path. Reading it subscribes.
pub fn use_location() -> Signal<String> {
    use_context::<RouterContext>().path
}

/// Listen for the reader's own Back and Forward, and repaint from them.
///
/// `Closure::forget` is deliberate: the listener must outlive the component
/// that registered it, and a shell is mounted once for the life of the
/// document. Dropping the handle would leave `popstate` firing into freed
/// memory the first time the shell unmounted.
#[cfg(target_arch = "wasm32")]
fn install_popstate_listener(mut path: Signal<String>) {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        return;
    };
    let listener = Closure::<dyn FnMut(web_sys::PopStateEvent)>::new(move |_event| {
        path.set(current_location());
    });
    if window
        .add_event_listener_with_callback("popstate", listener.as_ref().unchecked_ref())
        .is_err()
    {
        tracing::warn!(target: "router", "the browser refused the popstate listener");
    }
    listener.forget();
}
