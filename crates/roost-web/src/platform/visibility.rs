//! Document visibility, and the automation pin that overrides it.
//!
//! Ports `apps/web/src/browser/pageVisible.ts`. The pin exists so a spec can
//! keep a background tab live, or deterministically exercise hidden-tab
//! lifecycle, without asking Chromium to schedule the page a particular way —
//! and the oracle needs both, because several specs assert behaviour that
//! differs between a foregrounded and a backgrounded tab.
//!
//! WHY THE PIN LIVES UNDER `page_visible()` RATHER THAN IN ITS CALLERS. v2
//! states the rule: every consumer reads visibility through this module,
//! because a raw `document.hidden` read bypasses the pin. A component that
//! read the document directly would be correct in a browser and wrong under the
//! oracle, which is the worst kind of divergence — it passes review and fails
//! only in the run that is supposed to be checking it. So the override is
//! applied inside the one function every caller already uses, and there is no
//! second way to ask.

use std::cell::Cell;

thread_local! {
    /// `None` is the real document state; `Some` is the pin.
    static VISIBILITY_OVERRIDE: Cell<Option<bool>> = const { Cell::new(None) };
}

/// The real document state, with no pin applied.
#[cfg(target_arch = "wasm32")]
fn document_visible() -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .is_none_or(|document| !document.hidden())
}

/// No document to read, so a native build is visible. The same answer the
/// terminal's DOM helpers give for every other browser capability they cannot
/// have, and the one that keeps a native test exercising the same components a
/// browser runs rather than a set that exists only natively.
#[cfg(not(target_arch = "wasm32"))]
fn document_visible() -> bool {
    true
}

/// Whether the browser tab is foregrounded, honouring the automation pin.
pub fn page_visible() -> bool {
    match VISIBILITY_OVERRIDE.with(|pin| pin.get()) {
        Some(pinned) => pinned,
        None => document_visible(),
    }
}

/// Force the pin to `visible`, or release every override with `None`.
///
/// `None` returns the document to its real state, which is what "turn the pin
/// off" has to mean: a spec that leaves the pin engaged has silently changed
/// every later assertion about visibility.
///
/// A pin change is a `visibilitychange` edge, and dispatching it is what makes
/// it one. v2 states the rule in the same place (`setVisibilityOverride`), and
/// the reason is that visibility has two kinds of consumer: the ones that poll
/// `page_visible()`, which see the new pin on their next read, and the ones
/// that CACHE it — a pane's `page_visible`, the foreground gate — which learn
/// of an edge only from the event. Without the dispatch a pinned pane keeps
/// believing it is visible, never parks, and never reveals; the pin is set and
/// cleared correctly and the page behaves as if it had never moved.
pub fn pin_visible(pinned: Option<bool>) {
    let changed = VISIBILITY_OVERRIDE.with(|pin| {
        if pin.get() == pinned {
            return false;
        }
        pin.set(pinned);
        true
    });
    if changed {
        dispatch_visibility_change();
    }
}

/// Deliver the edge to the document, the one target a lifecycle listener lives
/// on. No document means no listener, which is the native build's answer rather
/// than an error.
#[cfg(target_arch = "wasm32")]
fn dispatch_visibility_change() {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    if let Ok(event) = web_sys::Event::new("visibilitychange") {
        let _ = document.dispatch_event(&event);
    }
}

/// A native build has no document and therefore no lifecycle listener.
#[cfg(not(target_arch = "wasm32"))]
fn dispatch_visibility_change() {}

/// Whether a pin is currently engaged, for a diagnostic that reports it rather
/// than assuming the real document state.
pub fn pin_engaged() -> bool {
    VISIBILITY_OVERRIDE.with(|pin| pin.get().is_some())
}
