//! The durable `SessionEventSink`: a claim taken against the SQLite outbox, a
//! write that consumes it, and a refusal that fails closed. `session::sinks`
//! declares the seam; `session::spawn` and `session::lifecycle` call it; this is
//! the one implementation that is not a test fake. Depends on
//! `crate::event_store` for the journal and on nothing that depends on it back.
//!
//! IT DELEGATES. Every method here is a call to a `Journal` operation and a
//! mapping of its error, with no buffering, no queueing and no "assume it
//! worked" path. That is the whole contract, and it is the reason this file is
//! nearly empty: a sink that queued a write and reported success would satisfy
//! the trait and lose a `closed` event, and a `closed` event that never reaches
//! the coordinator is a session the coordinator still lists as open after this
//! worker has gone. The failure would be invisible in the direction that matters.
//!
//! THE HANDLE IS AN `Arc`, NOT A BORROW, and the futures capture the CLONE. Each
//! method's future is built by cloning the `Arc` and moving it in, so a future
//! never borrows the sink and stays `Send` while it is in flight — a borrow
//! would make every caller that holds a sink across an await non-`Send` and take
//! `SessionManager` with it.

use std::sync::Arc;

use roost_protocol::wire::event::SessionEvent;

use crate::event_store::DurableEventKind;
use crate::event_store::Journal;
use crate::event_store::Reservation;
use crate::event_store::database::claims::ClaimRefusal;

use super::sinks::{EventFuture, SessionEventError, SessionEventSink};

/// The payload budget one event of a kind is admitted against.
///
/// It is the SAME number for every kind, and that is deliberate: the outbox's
/// cap is a single byte budget, so a kind that reserved more than a terminal
/// line would be admitted by its own arithmetic and refused by the file. A
/// caller that needs a different figure for its kind gets a different store.
const DEFAULT_PAYLOAD_BYTES: usize = 64 * 1024;

/// The durable sink, over one opened outbox.
#[derive(Debug, Clone)]
pub struct JournalSink {
    journal: Arc<Journal>,
}

impl JournalSink {
    /// The sink over an already-opened journal.
    ///
    /// Takes the `Arc` rather than opening anything, because opening is a boot
    /// decision with a refusal attached — a store that cannot be opened is a
    /// worker that must not start — and a constructor that quietly opened one
    /// would make that decision twice.
    pub fn new(journal: Arc<Journal>) -> Self {
        Self { journal }
    }
}

impl SessionEventSink for JournalSink {
    fn reserve(
        &self,
        kind: DurableEventKind,
    ) -> EventFuture<'_, Result<Reservation, SessionEventError>> {
        let journal = Arc::clone(&self.journal);
        Box::pin(async move {
            journal
                .reserve(kind, DEFAULT_PAYLOAD_BYTES)
                .await
                .map_err(claim_refusal)
        })
    }

    fn hold(&self, reservation: Reservation) -> EventFuture<'_, ()> {
        let journal = Arc::clone(&self.journal);
        Box::pin(async move {
            if let Err(error) = journal.hold(reservation).await {
                // Logged and NOT propagated, because `hold` returns nothing: it
                // marks a claim committed, and a store that could not record
                // that leaves the claim blocking a snapshot it should not. The
                // write still happened, so the caller's event is not at risk —
                // but the operator is told, because the next boot inherits it.
                tracing::error!(
                    reservation = reservation.id(),
                    error = %error,
                    "a committed claim could not be marked committed; it will still block \
                     this worker's snapshot until its lease expires"
                );
            }
        })
    }

    fn release(&self, reservation: Reservation) -> EventFuture<'_, ()> {
        let journal = Arc::clone(&self.journal);
        Box::pin(async move {
            if let Err(error) = journal.release(reservation).await {
                // The same shape as `hold` and for the same reason: the caller
                // gets no channel to be told through, and a leaked claim is
                // recoverable by the lease reclaim at the next open. Silence
                // would not be — a store that quietly stops giving capacity back
                // is how a worker eventually refuses every write.
                tracing::error!(
                    reservation = reservation.id(),
                    error = %error,
                    "a claim was not given back; it is reclaimed when its lease expires"
                );
            }
        })
    }

    fn emit<'a>(
        &'a self,
        event: &'a SessionEvent,
        reservation: Option<Reservation>,
    ) -> EventFuture<'a, Result<(), SessionEventError>> {
        let journal = Arc::clone(&self.journal);
        Box::pin(async move {
            match reservation {
                // The claim is CONSUMED by the write, inside the insert's own
                // transaction, so a crash cannot leave the row written and the
                // claim held — which would be a second copy of one event.
                Some(reservation) => journal
                    .emit(reservation, event)
                    .await
                    .map(|_| ())
                    .map_err(|error| SessionEventError::Store(error.to_string())),
                None => journal
                    .append(event)
                    .await
                    .map(|_| ())
                    .map_err(|error| SessionEventError::Store(error.to_string())),
            }
        })
    }
}

/// A claim refusal, named for what the caller can do about it.
///
/// The split is the whole point of `ClaimRefusal`: its `Refused` arm is the
/// in-memory vocabulary (`ReserveError`, which is `Copy` so every `match` on it
/// is allocation-free), and its `Store` arm is the file saying no. Only the
/// first is a CAPACITY answer, and only the first becomes
/// `SessionEventError::Reserve` — a store that did not answer must not tell a
/// caller to free capacity, because there is nothing to free.
fn claim_refusal(refusal: ClaimRefusal) -> SessionEventError {
    match refusal {
        ClaimRefusal::Refused(error) => SessionEventError::Reserve(error),
        ClaimRefusal::Store { reason } => SessionEventError::Store(reason),
    }
}
