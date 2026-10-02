//! The two agent surfaces that outlive a card: the document title's attention
//! count, and the acknowledgement a visible, focused view of a session earns.
//! Renders nothing; it exists for its one effect, mounted ONCE beside
//! `AgentNotifications` in `NotificationDock`. Ports the acknowledgement and
//! title effects of `apps/web/src/components/notifications/AgentNotificationBridge.tsx`.
//!
//! Acknowledgement goes out as `ClientEvent::AgentStatusSeen` and nowhere else.
//! The ledger and the sweep that persists it are the only writers, so a host
//! that marked a row seen on its own would raise an acknowledgement this profile
//! cannot read back after a reload.

use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::Store;
use roost_client_core::store::WorkerPaths;
use roost_client_core::store::prefs::notify::NotifyPref;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast as _;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::closure::Closure;
#[cfg(target_arch = "wasm32")]
use web_sys::{Event, EventTarget};

use super::attention_count::{attention_count, badge_title, base_title};
use crate::platform::BrowserWorkerPaths;
use crate::platform::visibility::page_visible;
use crate::pump::use_store;
use crate::route_session::active_session_for_path;
use crate::router_state::use_location;

/// Renders nothing itself. It exists for its effect, which is the one place a
/// profile's attention is counted and spent.
#[component]
pub fn AgentAttention() -> Element {
    let pump = use_store();
    let path = use_location();
    let attended = use_page_attention();
    let base = use_hook(|| base_title(&current_title()));

    use_effect(move || {
        // Four edges bring this here: the store moved (a status revision, or an
        // acknowledgement), the viewed session changed, the tab came back to the
        // foreground, or the window took focus back.
        let revision = pump.revision();
        let _ = revision.read();
        // `Signal` call syntax, so each of these three SUBSCRIBES this effect:
        // a bare `peek` would leave a status revision or a focus return with
        // nothing to repaint behind it.
        let route = path();
        let is_attended = attended();

        let (owed, count, badge_enabled) = {
            let core = pump.core();
            let core = core.borrow();
            let store = core.store();
            (
                unacknowledged_view(is_attended, store, &BrowserWorkerPaths, &route),
                attention_count(store.agent_status.statuses().values(), &store.agent_seen),
                store.prefs.notify.get(NotifyPref::TitleBadge),
            )
        };
        // The borrow is released before the dispatch: an effect that completes
        // synchronously would otherwise re-enter a held `RefCell`.
        if let Some(session_id) = owed {
            pump.dispatch(ClientEvent::AgentStatusSeen { session_id });
        }
        publish_title(&badge_title(&base, count, badge_enabled));
    });

    rsx! {}
}

/// The session whose newest revision this profile still owes an acknowledgement
/// for, and only while this tab is actually looking at it.
///
/// The gate is visible AND focused rather than visible alone: a Roost window
/// parked on a second monitor is being read, not looked at, and acknowledging
/// there would clear a blocked agent nobody has seen.
#[must_use]
pub fn unacknowledged_view(
    attended: bool,
    store: &Store,
    paths: &dyn WorkerPaths,
    path: &str,
) -> Option<String> {
    if !attended {
        return None;
    }
    let session = active_session_for_path(store, paths, path)?;
    let status = store.agent_status.status(&session.id)?;
    let acknowledged = store.agent_seen.acknowledged_revision(status);
    (status.common.revision > acknowledged).then(|| session.id.to_string())
}

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
pub(super) fn attended_now() -> bool {
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

/// The attention signal plus the listeners that move it.
///
/// Visibility and focus are EDGES, not values: what is owed is owed on the way
/// BACK, and nothing re-renders unless a listener writes the new answer. Each
/// listener is held here and taken back off when the scope that installed it
/// ends, so a remount cannot stack a second set on the same document.
struct PageAttention {
    read: Signal<bool>,
    #[cfg(target_arch = "wasm32")]
    listeners: Vec<(EventTarget, &'static str, Closure<dyn FnMut(Event)>)>,
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

/// The title the document carries now.
#[cfg(target_arch = "wasm32")]
fn current_title() -> String {
    web_sys::window()
        .and_then(|window| window.document())
        .map_or_else(|| base_title(""), |document| document.title())
}

/// No document to read, so the bare title is the base.
#[cfg(not(target_arch = "wasm32"))]
fn current_title() -> String {
    base_title("")
}

/// Put `title` on the document. A host with no document has nothing to badge,
/// and that is a capability this build does not have rather than an error.
#[cfg(target_arch = "wasm32")]
fn publish_title(title: &str) {
    if let Some(document) = web_sys::window().and_then(|window| window.document()) {
        document.set_title(title);
    }
}

/// No document to write to.
#[cfg(not(target_arch = "wasm32"))]
fn publish_title(_title: &str) {}
