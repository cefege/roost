//! A durable event claim a reconcile pass holds on a session's behalf, given
//! back when the outcome it was taken for does not happen. v2 tracks the same
//! thing as `resumeCloseOwned` / `respawnEventOwned` / `futureCloseOwned`
//! (`apps/worker/src/boot/boot-session-reconcile.ts`); `runtime::session_reconcile`
//! is the only caller.
//!
//! A GUARD, because `Reservation` is `Copy` with no `Drop`: a claim that goes
//! out of scope is not given back, it keeps its row and its reserved bytes
//! against the store's caps, and enough of them refuse every later spawn.

use std::sync::Arc;

use crate::event_store::{DurableEventKind, Reservation};
use crate::session::lifecycle::SessionManager;
use crate::session::sinks::SessionEventError;

pub(super) struct DurableClaim {
    manager: Arc<SessionManager>,
    reservation: Reservation,
    /// Cleared when the claim is handed over or given back; a still-armed
    /// claim is released by `Drop`.
    armed: bool,
}

impl DurableClaim {
    /// Reserve durable capacity for one event of `kind`.
    pub(super) async fn take(
        manager: &Arc<SessionManager>,
        kind: DurableEventKind,
    ) -> Result<Self, SessionEventError> {
        let reservation = manager.reserve_event(kind).await?;
        Ok(Self {
            manager: Arc::clone(manager),
            reservation,
            armed: true,
        })
    }

    /// The claim, lent to a call that may consume it; the guard stays armed
    /// until the caller knows whether it did.
    pub(super) fn reservation(&self) -> Reservation {
        self.reservation
    }

    /// Hand the claim over for good: its new owner spends or releases it.
    pub(super) fn disarm(mut self) -> Reservation {
        self.armed = false;
        self.reservation
    }

    /// Give the claim back now.
    pub(super) async fn release(mut self) {
        self.armed = false;
        self.manager.release_reservation(self.reservation).await;
    }
}

impl Drop for DurableClaim {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let manager = Arc::clone(&self.manager);
        let reservation = self.reservation;
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    manager.release_reservation(reservation).await;
                });
            }
            Err(_) => tracing::error!(
                reservation = reservation.id(),
                "a durable claim was dropped with no runtime to give it back on; it is \
                 reclaimed when its lease expires"
            ),
        }
    }
}
