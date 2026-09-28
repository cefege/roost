//! The browser's clocks and page-lifecycle signals, turned into core events:
//! the sweep tick, visibility, and the lifecycle wakes.
//!
//! wasm32 only; installed once by `pump::boot`. The listeners live as long as
//! the pump (held in `PumpInner::listeners`). Ported from
//! `apps/web/src/store/sync-redial.ts:99-135` (`installSyncLifecycleWake`) and
//! `apps/web/src/browser/pageVisible.ts` (the visibility read).

use roost_client_core::ClientEvent;
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use web_sys::VisibilityState;

use super::Pump;

/// How often the core's deadlines are evaluated. Every deadline is a pure
/// function of the reading the sweep carries, so this bounds lateness only.
pub(super) const SWEEP_INTERVAL_MS: i32 = 250;

/// Install the sweep interval and the lifecycle listeners.
pub(super) fn install(pump: &Pump) {
    let Some(window) = web_sys::window() else {
        tracing::error!(target: "pump", "no window: timers not installed");
        return;
    };
    let Some(document) = window.document() else {
        tracing::error!(target: "pump", "no document: timers not installed");
        return;
    };

    let sweep = {
        let pump = pump.clone();
        // The core's clock, not `Date.now()`: every deadline the sweep checks
        // was armed on the monotonic timeline, and a wall-clock reading is ~1.8e12
        // ms "later", which fired the stale watchdog on every sweep.
        let clock = crate::platform::BrowserClock::new();
        Closure::<dyn FnMut()>::new(move || {
            let now_ms = roost_client_core::Clock::now_ms(&clock);
            pump.dispatch(ClientEvent::Sweep { now_ms });
        })
    };
    if window
        .set_interval_with_callback_and_timeout_and_arguments_0(
            sweep.as_ref().unchecked_ref(),
            SWEEP_INTERVAL_MS,
        )
        .is_err()
    {
        tracing::error!(target: "pump", "the browser refused the sweep interval");
    }

    let visibility = {
        let pump = pump.clone();
        let document = document.clone();
        Closure::<dyn FnMut()>::new(move || {
            let visible = document.visibility_state() == VisibilityState::Visible;
            pump.dispatch(ClientEvent::PageVisibilityChanged { visible });
        })
    };
    let visible_wake = {
        let pump = pump.clone();
        Closure::<dyn FnMut()>::new(move || {
            pump.dispatch(ClientEvent::SyncWakeRequested {
                allow_hidden: false,
            });
        })
    };
    let online_wake = {
        let pump = pump.clone();
        Closure::<dyn FnMut()>::new(move || {
            pump.dispatch(ClientEvent::SyncWakeRequested { allow_hidden: true });
        })
    };
    let listen = |target: &web_sys::EventTarget, name: &str, callback: &Closure<dyn FnMut()>| {
        if target
            .add_event_listener_with_callback(name, callback.as_ref().unchecked_ref())
            .is_err()
        {
            tracing::warn!(target: "pump", event = name, "listener refused");
        }
    };
    listen(&document, "visibilitychange", &visibility);
    listen(&document, "resume", &visible_wake);
    listen(&window, "pageshow", &visible_wake);
    listen(&window, "focus", &visible_wake);
    listen(&window, "online", &online_wake);

    let visible = document.visibility_state() == VisibilityState::Visible;
    pump.dispatch(ClientEvent::PageVisibilityChanged { visible });
    tracing::info!(target: "pump", visible, "browser timers and lifecycle listeners installed");

    let mut held = pump.inner.listeners.borrow_mut();
    held.push(Box::new(sweep));
    held.push(Box::new(visibility));
    held.push(Box::new(visible_wake));
    held.push(Box::new(online_wake));
}
