//! The durable half of the coordinator link: the SQLite outbox the session
//! lifecycle writes, the ONE row in flight, and the exact acknowledgement that
//! retires it. Owned by [`super::LinkLoop`], the only thing that may drain it.
//! Depends on [`crate::event_store::Journal`] and on `link_barrier::Pump`.
//!
//! The two rules this exists to hold are one `LIMIT 1` and one
//! `DELETE ... WHERE client_seq = ?` in [`crate::event_store::Journal`], and
//! neither can be enforced by a caller that is free to batch. So this file is
//! deliberately thin: it holds no queue, it answers which sequence is in flight
//! and which rows a coordinator answer has authorised this link to remove, and
//! everything else is the store's.
//!
//! The outbox allocates the `client_seq` a row is stored under and the barrier
//! is TOLD it (`Pump::enqueue_durable_at`), so the two never allocate one space
//! twice: a restart's unacknowledged rows replay under the sequences they were
//! written with, and the snapshot draws its own from the outbox
//! (`super::durable_sync`).

use std::sync::Arc;

use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use roost_protocol::wire::event::SessionEvent;

use crate::event_store::database::{Journal, JournalError, JournalStats, PendingRow};
use crate::session::durable_delivery::DurableDelivery;

use super::{DurableWrite, LinkLoop};

/// How many acknowledgements may be waiting to be applied.
///
/// A frame handler that records faster than the drain retires would otherwise
/// grow this without bound, and the bound is the whole reason it is a vector
/// rather than a set drained on the spot.
pub const PENDING_ACKS_CAP: usize = 256;

/// Why a durable event was not written, or not offered to the barrier.
#[derive(Debug, thiserror::Error)]
pub enum OutboxRefusal {
    #[error(transparent)]
    Store(#[from] JournalError),
    #[error("this link has no durable outbox installed")]
    NoOutbox,
    #[error("a durable event did not encode: {reason}")]
    Unencodable { reason: String },
    #[error(transparent)]
    Mirror(#[from] super::DurableRefusal),
    #[error("the barrier refused the outbox's sequence: {reason}")]
    UnusableSequence { reason: String },
}

impl LinkLoop {
    /// Install the outbox this link replays from and the signal its sink
    /// raises on every change.
    ///
    /// Additive to [`LinkLoop::new`] rather than a parameter, because the outbox
    /// is a FILE: opening it is I/O with a real failure mode, and a constructor
    /// that took a path would be a constructor that can fail before the link
    /// exists. Every row already in the file is offered on the next drain, in
    /// sequence order, which is how a restart's unacknowledged rows reach the
    /// coordinator (v2 `coord-link-unacked.ts` `oldestDurable`).
    pub fn attach_durable_outbox(&mut self, outbox: Arc<Journal>, delivery: Arc<DurableDelivery>) {
        self.durable_rows = Some(outbox);
        self.durable_delivery = Some(delivery);
        self.durable_offered_through = 0;
        self.durable_resync = true;
        tracing::info!("the coordinator link replays from a durable outbox");
    }

    /// The outbox this link replays from, if one is installed.
    pub fn durable_outbox(&self) -> Option<&Arc<Journal>> {
        self.durable_rows.as_ref()
    }

    /// What the outbox file holds, for `roost doctor` and for a boot log.
    pub async fn durable_outbox_stats(&self) -> Result<JournalStats, OutboxRefusal> {
        let outbox = self
            .durable_outbox()
            .ok_or(OutboxRefusal::NoOutbox)?
            .clone();
        Ok(outbox.stats().await?)
    }

    /// Write one durable session event and offer its bytes to the barrier.
    ///
    /// The row is written FIRST and the bytes offered second, in that order and
    /// for the reason that decides the slice: a barrier that took an event the
    /// store never kept would replay nothing after a crash, and a store that
    /// kept an event the barrier never offered would leave a row that no
    /// acknowledgement can retire.
    pub async fn publish_durable_event(
        &mut self,
        event: &SessionEvent,
    ) -> Result<PendingRow, OutboxRefusal> {
        let outbox = self
            .durable_outbox()
            .ok_or(OutboxRefusal::NoOutbox)?
            .clone();
        let row = outbox.append(event).await?;
        let frame = CoordWorkerUpstream::Event {
            event: row.event.clone(),
            client_seq: row.client_seq,
            trace_id: None,
        };
        let bytes =
            self.wire
                .encode_upstream(&frame)
                .map_err(|error| OutboxRefusal::Unencodable {
                    reason: error.to_string(),
                })?;
        self.enqueue_durable_at(row.client_seq, bytes)?;
        Ok(row)
    }

    /// Offer a durable event the outbox has already numbered.
    ///
    /// The same three steps [`LinkLoop::enqueue_durable`] takes — the mirror, the
    /// pump, the apply — except the pump is TOLD the sequence instead of
    /// inventing one. That is the whole reason this is a separate path: the
    /// outbox allocates `client_seq` in blocks, and two allocators of one space
    /// drift apart at every block boundary.
    pub fn enqueue_durable_at(
        &mut self,
        client_seq: u64,
        bytes: Vec<u8>,
    ) -> Result<(), OutboxRefusal> {
        if let Some(refusal) = self.mirror_refusal(bytes.len()) {
            return Err(OutboxRefusal::Mirror(refusal));
        }
        self.durable_bytes += bytes.len();
        // Copied once, for the reason `enqueue_durable` gives: the pump keeps
        // its own copy and never hands it back.
        self.durable.push_back(DurableWrite {
            bytes: bytes.clone(),
            seq: Some(client_seq),
        });
        let action = self
            .pump
            .enqueue_durable_at(client_seq, bytes)
            .map_err(|refusal| OutboxRefusal::UnusableSequence {
                reason: refusal.to_string(),
            })?;
        self.durable_offered_through = self.durable_offered_through.max(client_seq);
        crate::runtime::link_drain::apply_to(self, action);
        Ok(())
    }

    /// The oldest row the outbox still holds, which is the only one that may go
    /// next.
    ///
    /// A store question and not a barrier one: the barrier owns whether a durable
    /// write is permitted at all and how fast, and asking it here would make
    /// "what is waiting" and "what may be written now" one question with two
    /// owners.
    pub async fn oldest_durable_row(&self) -> Result<Option<PendingRow>, OutboxRefusal> {
        let Some(outbox) = self.durable_outbox() else {
            return Ok(None);
        };
        Ok(outbox.replay_head().await?)
    }

    /// Record that the coordinator acknowledged `client_seq`.
    ///
    /// Synchronous, and deliberately separate from
    /// [`LinkLoop::apply_durable_acks`]: the downstream frame handler is
    /// synchronous and must stay that way, and a SQLite delete cannot be. The
    /// handler records WHICH sequence was answered and the drain — which already
    /// runs on the tick — removes the row.
    pub(in crate::runtime) fn note_durable_ack(&mut self, client_seq: u64) {
        if self.pending_acks.len() >= PENDING_ACKS_CAP {
            tracing::error!(
                client_seq,
                pending = self.pending_acks.len(),
                cap = PENDING_ACKS_CAP,
                "acknowledgements are accumulating faster than the link retires rows, so the \
                 barrier has moved on and these rows will be replayed instead"
            );
            return;
        }
        self.pending_acks.push(client_seq);
    }

    /// Retire every row a coordinator acknowledgement authorised. Returns how
    /// many left the outbox.
    ///
    /// Called from the drain, which is the one place on this link that is already
    /// async and already holds the tick. A row whose delete FAILS stays, and
    /// staying is the safe direction: it is replayed under the same sequence and
    /// the coordinator deduplicates on that, so the cost is one duplicate and
    /// the alternative is a fact about a session that never reached anybody.
    pub(in crate::runtime) async fn apply_durable_acks(&mut self) -> usize {
        if self.pending_acks.is_empty() {
            return 0;
        }
        let Some(outbox) = self.durable_rows.clone() else {
            // Acknowledged with no store to retire them from. Reported, not
            // logged at trace: the barrier advanced, so this is visible state
            // nobody can account for.
            let stranded = self.pending_acks.len();
            self.pending_acks.clear();
            tracing::error!(
                stranded,
                "the coordinator acknowledged durable events while this link had no outbox, so \
                 those rows are not anywhere this process can retire"
            );
            return 0;
        };
        let acknowledged = std::mem::take(&mut self.pending_acks);
        let mut retired = 0usize;
        for client_seq in acknowledged {
            match outbox.acknowledge(client_seq).await {
                Ok(true) => retired += 1,
                // A reconnect replays, so the coordinator re-acknowledges a row
                // that has already gone. Normal, and it must not be an error: a
                // duplicate acknowledgement is how a reconnect terminates.
                Ok(false) => tracing::debug!(
                    client_seq,
                    "a durable row was acknowledged again after it had already left the outbox"
                ),
                Err(error) => tracing::error!(
                    client_seq,
                    %error,
                    "a durable row could not be retired; it stays and will be replayed"
                ),
            }
        }
        retired
    }
}

#[cfg(test)]
mod tests;
