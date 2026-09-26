//! Everything the append path does **around** the transaction rather than inside
//! it: validate, normalize, reserve, and release.
//!
//! Ported from the pre-transaction and post-throw halves of
//! `apps/coord/src/events/event-transaction.ts:76-113` and `:283-323`. These are
//! the steps whose position relative to the commit is the contract, so they are
//! not buried inside the transaction body that sits between them.
//!
//! **Before:** re-validate an `agent_reference` against the event schema, refuse a
//! snapshot over 1,024 sessions, normalize the six worker-controlled strings, and
//! reserve the publication slot.
//!
//! **After a throw:** release the reservation, so a failed append leaves no slot
//! behind for the worker's retry to trip over.

use std::sync::PoisonError;

use roost_protocol::wire::SessionEvent;

use crate::events::append::{AppendError, AppendOptions, Caller, Reservation};
use crate::events::persistence_input::{
    MAX_WORKER_SNAPSHOT_SESSIONS, normalize_persisted_worker_event,
};

/// Validate and normalize an event before anything durable happens.
pub(crate) fn prepare_event(event: SessionEvent) -> Result<SessionEvent, AppendError> {
    let event = revalidate_agent_reference(event)?;
    refuse_oversized_snapshot(&event)?;
    // One normalized value feeds the durable JSON, the projection fold, the
    // channel-index publication and the live Sync publication. Normalizing a row
    // later would make replay disagree with the sessions projection.
    Ok(normalize_persisted_worker_event(event))
}

fn refuse_oversized_snapshot(event: &SessionEvent) -> Result<(), AppendError> {
    let SessionEvent::Snapshot { sessions, .. } = event else {
        return Ok(());
    };
    if sessions.len() > MAX_WORKER_SNAPSHOT_SESSIONS {
        return Err(AppendError::SnapshotTooLarge {
            sessions: sessions.len(),
            limit: MAX_WORKER_SNAPSHOT_SESSIONS,
        });
    }
    Ok(())
}

/// A worker sequence the `events.client_seq` INTEGER column can hold.
///
/// The wire field is a `uint64` and the column is an `INTEGER`, so a sequence past
/// `i64::MAX` is representable on the wire and not in the log. It is refused at
/// the boundary rather than wrapped into a negative sequence, which would collide
/// with a real one.
pub(crate) fn as_client_seq(value: u64) -> Result<i64, AppendError> {
    i64::try_from(value).map_err(|_| AppendError::ClientSeqOutOfRange { client_seq: value })
}

/// A durable id the `events.id` INTEGER column can hold.
pub(crate) fn as_event_id(value: u64) -> Result<i64, AppendError> {
    i64::try_from(value).map_err(|_| AppendError::EventIdOutOfRange { id: i64::MAX })
}

/// Re-validate an `agent_reference` against the event schema, at the boundary.
///
/// The decoded value is already typed, so the checks that matter are the ones a
/// decode does not perform: the serialized envelope's byte bound, and the
/// reference's own rules (`roost_protocol::wire::SessionEvent::parse`).
fn revalidate_agent_reference(event: SessionEvent) -> Result<SessionEvent, AppendError> {
    if !matches!(event, SessionEvent::AgentReference { .. }) {
        return Ok(event);
    }
    let value = serde_json::to_value(&event)?;
    SessionEvent::parse(value).map_err(|_| AppendError::InvalidAgentReference)
}

/// Claim the publication slot for this caller's sequence, before the commit.
///
/// Reserving first is what makes two concurrent deliveries of one `client_seq`
/// serialize instead of double-publishing, and exceeding the bound is refused
/// here, before the transaction, so a full store costs the caller nothing else.
pub(crate) fn reserve_publication_slot(
    options: &AppendOptions<'_>,
    caller: &Caller,
) -> Result<(), AppendError> {
    let Some(reservation) = caller.reservation() else {
        return Ok(());
    };
    let Some(store) = options.pending_publications.as_ref() else {
        return Ok(());
    };
    store
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .reserve(reservation.worker_fp.as_str(), reservation.client_seq)
        .map_err(|_| AppendError::PublicationCapacity)
}

/// Release a reservation whose append never reached the commit.
pub(crate) fn release_reservation(options: &AppendOptions<'_>, caller: &Caller) {
    let (Some(Reservation { worker_fp, client_seq }), Some(store)) = (
        caller.reservation(),
        options.pending_publications.as_ref(),
    ) else {
        return;
    };
    store
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .release(worker_fp.as_str(), client_seq);
}
