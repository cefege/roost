//! The pairing ceremony's per-process state: in-flight pair requests and the
//! retention that expires them.
//!
//! One field on `CoordServices`, reached as `core.services.pairing`. The seven
//! `Pair*` methods and the scheduled retention sweep both act on this one
//! instance, so a second one would be a second answer to "which requests are
//! still redeemable".
//!
//! `new()` takes nothing and must keep taking nothing: anything the ceremony
//! needs from configuration is read at call time from `core.services.boot`.

/// The pairing state one coordinator process holds.
#[derive(Debug, Default)]
pub struct PairingRuntime;

impl PairingRuntime {
    /// An empty ceremony: no request has been created yet.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}
