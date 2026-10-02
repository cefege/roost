//! The arrival bell: how the client's reader thread tells the worker that a
//! frame landed on the event stream, so the worker delivers it at once rather
//! than at its next idle poll. Rung by [`crate::client_io::read_frames`];
//! waited on by the worker's keeper dispatch loop through
//! [`crate::client::KeeperClient::arrival_bell`]. Depends on `std` only.

use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

/// A latched "something arrived" flag a waiter can block on.
///
/// LATCHED, so a frame that lands between the worker's drain and its wait is
/// not lost: the ring stays set until the next [`ArrivalBell::wait`] consumes
/// it, and that wait returns at once. A ring the waiter did not need costs one
/// empty drain, never a missed frame.
#[derive(Debug, Default)]
pub struct ArrivalBell {
    rung: Mutex<bool>,
    ringing: Condvar,
}

impl ArrivalBell {
    /// A frame reached the event stream, or the stream closed.
    pub(crate) fn ring(&self) {
        *self.rung.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.ringing.notify_all();
    }

    /// Wait until the bell rings or `timeout` passes, consuming the ring.
    /// Returns whether it rang.
    pub fn wait(&self, timeout: Duration) -> bool {
        let rung = self.rung.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut rung, _) = self
            .ringing
            .wait_timeout_while(rung, timeout, |rung| !*rung)
            .unwrap_or_else(PoisonError::into_inner);
        std::mem::replace(&mut *rung, false)
    }
}
