//! The terminal view hub: which worker owns a session's views, and the
//! membership and geometry its owner-mode relay publishes.
//!
//! One field on `CoordServices`, reached as `core.services.views`, and handed
//! to the workers domain as the `TerminalViewLifecycle` seam. A respawn reads
//! the geometry the effective viewer set produced from here, so a second hub
//! would be a second geometry for one session.
//!
//! `new()` takes nothing and must keep taking nothing: the terminal memory
//! ceiling the hub sizes itself against is read at call time from
//! `core.services.boot`.

/// The view state one coordinator process holds.
#[derive(Debug, Default)]
pub struct TerminalViewHub;

impl TerminalViewHub {
    /// A hub that has seen no view membership.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
