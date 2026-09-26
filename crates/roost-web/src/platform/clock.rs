//! `Clock` over the browser's monotonic performance timeline.
//!
//! Owned by `platform`, called by `ClientCore::handle` through the `Clock`
//! trait, and the only place in the app that reads a clock: the client core
//! takes one instant per `handle` and hands that number to every deadline, so a
//! second reader is a second answer to "what time is it".
//!
//! `performance.now()`, not `Date.now()`. Four deadlines depend on this reading,
//! and a wall clock that steps backwards — an NTP correction, a laptop resuming
//! — turns a satisfied deadline into an unsatisfied one for the length of the
//! step.
//!
//! The epoch is the navigation, not the Unix epoch, because every deadline in
//! the client core is a DELTA from the moment it was armed and nothing in the
//! browser protocol is a wall-clock time.

use roost_client_core::Clock;
use web_sys::Performance;

/// The browser's monotonic clock, in milliseconds since navigation start.
#[derive(Debug, Clone, Default)]
pub struct BrowserClock {
    /// `None` when the window has no `performance`, which a worker and a native
    /// test binary do not. Every deadline then reads zero and fires on its first
    /// sweep, which is a coherent client rather than a panic in an event handler.
    performance: Option<Performance>,
}

impl BrowserClock {
    /// A clock over this window's `performance` timeline.
    pub fn new() -> Self {
        Self {
            performance: web_sys::window().and_then(|window| window.performance()),
        }
    }
}

impl Clock for BrowserClock {
    fn now_ms(&self) -> u64 {
        // `performance.now()` is fractional and non-negative. A NaN or negative
        // reading is clamped rather than cast, because a NaN silently becomes
        // zero under a cast — and a deadline that silently reads zero fires at
        // once, which looks like a storm rather than a missing clock.
        let Some(reading) = self.performance.as_ref().map(Performance::now) else {
            return 0;
        };
        if !reading.is_finite() || reading <= 0.0 {
            return 0;
        }
        reading as u64
    }
}
