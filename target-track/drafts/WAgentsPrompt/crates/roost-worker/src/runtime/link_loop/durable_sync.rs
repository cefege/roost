//! How rows the session layer writes to the durable outbox reach the link: the
//! outbox is the source of truth, and every row it holds that the pump has not
//! been offered is offered, oldest first, under the exact `client_seq` it is
//! stored under — at attach (a restart's unacknowledged rows) and after every
//! change the durable sink signals. Also decides the durable-replay barrier.
//! Called by `super::super::link_drain::drain` and the boot sequence. Ports
//! `pump`/`oldestDurable`/`hasBlockingSessionEventReservation` and the
//! `replayBarrier` edges of v2 `apps/worker/src/transport/coord-link-unacked.ts`.

use std::sync::Arc;

use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use crate::event_store::database::{Journal, PendingRow};
use crate::link_barrier::Barrier;
use crate::session::durable_delivery::DurableDelivery;

use super::LinkLoop;
use super::durable::OutboxRefusal;

impl LinkLoop {
    /// Offer every row the outbox holds beyond what the pump already has.
    ///
    /// Runs when the sink signalled a change, and on every pass while a
    /// previous one stopped short (the writer's mirror was full). A row the
    /// mirror refuses is NOT skipped: the pass stops there and the next one
    /// resumes at the same row, because a durable row offered out of order is a
    /// row the coordinator would acknowledge under the wrong sequence.
    pub(in crate::runtime) async fn sync_durable_rows(&mut self) {
        let Some(delivery) = self.durable_delivery.clone() else {
            return;
        };
        let changed = delivery.take_store_changed();
        if !changed && !self.durable_resync {
            self.decide_replay_barrier(&delivery);
            return;
        }
        let Some(outbox) = self.durable_rows.clone() else {
            return;
        };
        self.durable_resync = false;
        match outbox.pending().await {
            Ok(rows) => self.offer_new_rows(rows),
            Err(error) => {
                // Retried on the next pass: the rows are still in the file.
                self.durable_resync = true;
                tracing::error!(%error, "the durable outbox could not be read; its rows wait for the next pass");
            }
        }
        self.refresh_snapshot_blocking(&outbox).await;
        self.decide_replay_barrier(&delivery);
    }

    fn offer_new_rows(&mut self, rows: Vec<PendingRow>) {
        for row in rows.into_iter().filter(|row| row.client_seq > self.durable_offered_through) {
            let client_seq = row.client_seq;
            match self.offer_row(row) {
                Ok(()) => self.durable_offered_through = client_seq,
                Err(refusal) => {
                    self.durable_resync = true;
                    tracing::warn!(client_seq, %refusal, "a durable row waits: the link cannot take it yet");
                    return;
                }
            }
        }
    }

    fn offer_row(&mut self, row: PendingRow) -> Result<(), OutboxRefusal> {
        let frame = CoordWorkerUpstream::Event {
            event: row.event,
            client_seq: row.client_seq,
            trace_id: None,
        };
        let bytes = self
            .wire
            .encode_upstream(&frame)
            .map_err(|error| OutboxRefusal::Unencodable { reason: error.to_string() })?;
        self.enqueue_durable_at(row.client_seq, bytes)?;
        tracing::debug!(client_seq = row.client_seq, "a durable row was offered to the coordinator link");
        Ok(())
    }

    /// v2 `hasBlockingSessionEventReservation`: a claim taken for an event
    /// that does not exist yet holds the snapshot back, because the snapshot
    /// would describe a session set that is about to change.
    async fn refresh_snapshot_blocking(&mut self, outbox: &Arc<Journal>) {
        match outbox.claims().await {
            Ok((_, blocking)) => {
                let action = self.pump.set_snapshot_blocked(blocking > 0);
                crate::runtime::link_drain::apply_to(self, action);
            }
            Err(error) => {
                self.durable_resync = true;
                tracing::error!(%error, "the outbox's claims could not be read; the snapshot waits");
            }
        }
    }

    /// v2 `markDrained`/`markPending`: drained once the barrier is past replay
    /// with no durable event left and no claim holding the snapshot back.
    pub(in crate::runtime) fn decide_replay_barrier(&self, delivery: &DurableDelivery) {
        let past_replay = matches!(self.pump.barrier(), Barrier::Snapshot | Barrier::Live);
        if past_replay && self.durable.is_empty() && !self.pump.snapshot_blocked() && !self.durable_resync {
            delivery.mark_drained();
        } else {
            delivery.mark_pending();
        }
    }
}
