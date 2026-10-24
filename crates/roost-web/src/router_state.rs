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
/// The listener is installed once per scope, not once per render: a listener
/// re-registered on every render would fire N times per Back press. Its
/// registration is held by the same scope, so a scope that ends takes the
/// listener with it and the next mount installs one rather than stacking.
pub fn use_path_signal() -> Signal<String> {
    let path = use_hook(|| Signal::new(current_location()));
    // A hook slot is `Clone`-bound, and nothing here may share the registration
    // — the one owner has to be the scope, or the release would follow the last
    // clone instead of the scope's end. The `Rc` is a carrier, not a share.
    #[cfg(target_arch = "wasm32")]
    use_hook(move || std::rc::Rc::new(install_popstate_listener(path)));
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

/// A listener on the document that is taken back off when the value holding it
/// goes.
///
/// The undo is a closure rather than the `Closure` itself so ONE rule covers
/// every platform: install once, keep the value, drop the value. A forgotten
/// closure has no owner, and an owner nobody can drop is not an owner.
pub struct PopstateListener {
    undo: Option<Box<dyn FnOnce()>>,
}

impl std::fmt::Debug for PopstateListener {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PopstateListener")
            .field("registered", &self.undo.is_some())
            .finish()
    }
}

impl PopstateListener {
    /// Take `undo` as the work that removes this listener from `window`.
    pub fn new(undo: impl FnOnce() + 'static) -> Self {
        Self {
            undo: Some(Box::new(undo)),
        }
    }
}

impl Drop for PopstateListener {
    fn drop(&mut self) {
        if let Some(undo) = self.undo.take() {
            undo();
        }
    }
}

/// Listen for the reader's own Back and Forward, and repaint from them.
///
/// The registration is a value the calling scope keeps, not a forgotten
/// closure. A path signal is scope state and scope state dies with the scope,
/// so a listener that outlived its signal was not merely wasted: the next Back
/// press wrote into a signal with no scope left to repaint from. The access
/// gate remounts — pair, unpair, authorized, unauthorized — so "a shell is
/// mounted once for the life of the document" was a convention nothing
/// enforced, and every remount left another one behind.
#[cfg(target_arch = "wasm32")]
fn install_popstate_listener(mut path: Signal<String>) -> Option<PopstateListener> {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;

    let window = web_sys::window()?;
    let listener = Closure::<dyn FnMut(web_sys::PopStateEvent)>::new(move |_event| {
        path.set(current_location());
    });
    if window
        .add_event_listener_with_callback("popstate", listener.as_ref().unchecked_ref())
        .is_err()
    {
        tracing::warn!(target: "router", "the browser refused the popstate listener");
    }
    // The listener is still ALIVE when the undo runs — the closure borrows it
    // and is dropped only after `remove_event_listener_with_callback` has had
    // it — so the removal never hands `window` a freed callback, which is the
    // failure the `forget` above was written to avoid.
    Some(PopstateListener::new(move || {
        let _ = window
            .remove_event_listener_with_callback("popstate", listener.as_ref().unchecked_ref());
    }))
}
