//! Global session search across every worker, and the cancellation that
//! retires one.
//!
//! One field on `CoordServices`, reached as `core.services.search`. The ledger
//! it reads is `terminal_screen::search_ledger` — this runtime owns the
//! fan-out and the correlation, never a second copy of the retained text.
//!
//! `new()` takes nothing and must keep taking nothing: anything the domain
//! needs from configuration is read at call time from `core.services.boot`.

/// The global-search state one coordinator process holds.
#[derive(Debug, Default)]
pub struct GlobalSearchRuntime;

impl GlobalSearchRuntime {
    /// A coordinator with no search in flight.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
