//! The durable close claim a survivor's adoption holds, and the guard that
//! gives it back. `runtime::adoption::adopt_survivors` is the only caller: it
//! takes one per survivor and disarms it when an adoption takes ownership.
//! Depends on `session::lifecycle::SessionManager` for the reservation itself.

use std::sync::Arc;

use crate::browser_commands::Refusal;
use crate::event_store::{DurableEventKind, Reservation};
use crate::session::lifecycle::SessionManager;

/// A durable claim for one survivor's eventual close, given back if this scope
/// ends without spending it.
///
/// WHY A GUARD AND NOT A CALL ON EACH ARM. `Reservation` is `#[derive(Clone,
/// Copy)]` with no `Drop`, so the type that exists to own a claim says in its
/// signature that it is copyable and therefore owned by nobody. Seven arms of
/// `adopt_survivors` took a claim and left by `continue` without giving it
/// back, and a seventh site — the `resolve_shell_spec` refusal — never reached
/// any release path at all. Every one of those arms reads as correct code.
///
/// A per-arm release would have fixed those seven and been worth nothing the
/// next time somebody adds an eighth `continue`, which is the only failure
/// this whole exercise exists to prevent. The value here is FORWARD: the
/// release is in `Drop`, so an arm that does not exist yet, and a panic on an
/// arm that does, both give the claim back without anyone having to remember.
///
/// `Drop` CANNOT AWAIT, so the release is spawned. The claim's lease is
/// reclaimed at the next open if the spawn does not run — that is the
/// documented repair in `session/journal_sink.rs`, and it is why this is
/// acceptable and not merely convenient.
pub(super) struct CloseClaim {
    manager: Arc<SessionManager>,
    reservation: Reservation,
    /// Set when the claim has been handed to an adoption, which owns it from
    /// there and gives it back itself.
    disarmed: bool,
}

impl CloseClaim {
    /// Reserve durable capacity for the close, or report why there is none.
    pub(super) async fn take(manager: &Arc<SessionManager>) -> Result<Self, Refusal> {
        let reservation = manager.reserve(DurableEventKind::Closed).await?;
        Ok(Self {
            manager: Arc::clone(manager),
            reservation,
            disarmed: false,
        })
    }

    /// Hand the claim over, and stop guarding it.
    ///
    /// THE ONLY EXIT FROM GUARDING, and it is deliberately not `into_inner`:
    /// the name says what happened to the guard, not just that a value came
    /// out, and a reader at the call site is deciding who owns a durable
    /// resource from now on.
    pub(super) fn disarm(mut self) -> Reservation {
        self.disarmed = true;
        self.reservation
    }
}

impl Drop for CloseClaim {
    fn drop(&mut self) {
        if self.disarmed {
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
