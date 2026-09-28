//! The finite authority one admitted direct port runs on. Grant expiry gates a
//! new hello only; an authenticated port is bounded instead by a hard lifetime
//! and an idle deadline that valid activity refreshes but can never push past
//! the hard one. Ports v2
//! `apps/worker/src/attachments/attachment-transfer-lease.ts` as a pure
//! deadline; the direct sockets arm the timer and fail the port at expiry.

use std::time::{Duration, Instant};

use roost_protocol::attachment_transfer::{ACTIVE_MAX_MS, IDLE_MS};

const ACTIVE_MAX: Duration = Duration::from_millis(ACTIVE_MAX_MS);
const IDLE: Duration = Duration::from_millis(IDLE_MS);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentTransferLease {
    hard_deadline: Instant,
    idle_deadline: Instant,
    expired: bool,
}

impl AttachmentTransferLease {
    /// v2 `start`: both clocks run from admission.
    pub fn start(now: Instant) -> Self {
        Self {
            hard_deadline: now + ACTIVE_MAX,
            idle_deadline: now + IDLE,
            expired: false,
        }
    }

    /// Whether the port may still act. Expiry latches: once false, always false.
    pub fn allows_activity(&mut self, now: Instant) -> bool {
        if !self.expired && now < self.deadline() {
            return true;
        }
        if !self.expired {
            self.expired = true;
            tracing::info!("an attachment transfer lease expired");
        }
        false
    }

    /// A valid chunk or acknowledgement: refresh the idle deadline, never
    /// beyond the hard one.
    pub fn note_valid_activity(&mut self, now: Instant) -> bool {
        if !self.allows_activity(now) {
            return false;
        }
        self.idle_deadline = now + IDLE;
        true
    }

    /// When the carrier's timer must next ask [`Self::allows_activity`].
    pub fn deadline(&self) -> Instant {
        self.hard_deadline.min(self.idle_deadline)
    }
}
