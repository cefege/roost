//! How rows the session layer writes to the durable outbox reach the link, and
//! how the snapshot is numbered beside them: every row not yet offered goes to
//! the pump oldest first under the `client_seq` it is stored under (a restart's
//! unacknowledged rows included), the snapshot draws its sequence from the same
//! outbox, and the durable-replay barrier is decided here. Called by
//! `runtime::link_drain::drain`. Ports `pump`/`oldestDurable`/
//! `hasBlockingSessionEventReservation`/`startSnapshotBarrier` and the replay
//! barrier edges of v2 `apps/worker/src/transport/coord-link-unacked.ts`.

use std::sync::Arc;

use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use crate::event_store::database::{Journal, PendingRow};
use crate::link_barrier::Barrier;
use crate::runtime::snapshot_source::SnapshotActivation;

use super::durable::OutboxRefusal;
use super::{Authorised, LinkLoop};

impl LinkLoop {
    /// Hold the snapshot until the returned activation is released: v2 boot
    /// dials and replays first and activates the provider only once its first
    /// reconcile pass settled (`main.ts:296-303`). Called once, before
    /// [`LinkLoop::run`], by `runtime::boot_sequence`.
    pub fn hold_snapshot_until_activated(&mut self) -> SnapshotActivation {
        let hold = SnapshotActivation::held(std::sync::Arc::clone(&self.wake));
        self.snapshot_hold = Some(hold.clone());
        tracing::info!("the link's snapshot is held until the boot reconcile activates it");
        hold
    }

    /// Whether boot still holds the snapshot back.
    pub(in crate::runtime) fn snapshot_held(&self) -> bool {
        self.snapshot_hold
            .as_ref()
            .is_some_and(|hold| !hold.is_active())
    }

    /// v2 `send`/`snapshotStateChanged` with the snapshot not yet in flight: a
    /// durable row or blocking claim sends the barrier back to replay, so the
    /// row goes out now rather than behind a snapshot taken after it.
    fn reopen_replay_before_unsent_snapshot(&mut self) {
        if self.snapshot_wanted && self.pump.barrier() == Barrier::Snapshot {
            self.snapshot_wanted = false;
            self.pump.abandon_snapshot();
            tracing::info!(
                "a durable change arrived before the snapshot was written; replay resumes"
            );
        }
    }

    /// Offer every row the outbox holds beyond what the pump already has.
    ///
    /// Runs when the sink signalled a change, and on every pass while a
    /// previous one stopped short. A row the mirror refuses is NOT skipped: the
    /// pass stops there and the next resumes at the same row, because a durable
    /// row offered out of order is acknowledged under the wrong sequence.
    pub(in crate::runtime) async fn sync_durable_rows(&mut self) {
        let Some(delivery) = self.durable_delivery.clone() else {
            return;
        };
        let changed = delivery.take_store_changed();
        let Some(outbox) = self.durable_rows.clone() else {
            return;
        };
        if changed || self.durable_resync {
            self.durable_resync = false;
            match outbox.pending().await {
                Ok(rows) => self.offer_new_rows(rows),
                Err(error) => {
                    self.durable_resync = true;
                    tracing::error!(%error, "the durable outbox could not be read; its rows wait for the next pass");
                }
            }
            self.refresh_snapshot_blocking(&outbox).await;
        }
        self.decide_replay_barrier();
    }

    fn offer_new_rows(&mut self, rows: Vec<PendingRow>) {
        for row in rows {
            if row.client_seq <= self.durable_offered_through {
                continue;
            }
            self.reopen_replay_before_unsent_snapshot();
            // `enqueue_durable_at` moves `durable_offered_through` on.
            let client_seq = row.client_seq;
            if let Err(refusal) = self.offer_row(row) {
                self.durable_resync = true;
                tracing::warn!(client_seq, %refusal, "a durable row waits: the link cannot take it yet");
                return;
            }
        }
    }

    fn offer_row(&mut self, row: PendingRow) -> Result<(), OutboxRefusal> {
        let client_seq = row.client_seq;
        let frame = CoordWorkerUpstream::Event {
            event: row.event,
            client_seq,
            trace_id: None,
        };
        let bytes =
            self.wire
                .encode_upstream(&frame)
                .map_err(|error| OutboxRefusal::Unencodable {
                    reason: error.to_string(),
                })?;
        self.enqueue_durable_at(client_seq, bytes)?;
        tracing::debug!(
            client_seq,
            "a durable row was offered to the coordinator link"
        );
        Ok(())
    }

    /// v2 `hasBlockingSessionEventReservation`: a claim taken for an event that
    /// does not exist yet holds the snapshot back.
    async fn refresh_snapshot_blocking(&mut self, outbox: &Arc<Journal>) {
        match outbox.claims().await {
            Ok((_, blocking)) => {
                if blocking > 0 {
                    self.reopen_replay_before_unsent_snapshot();
                }
                let action = self.pump.set_snapshot_blocked(blocking > 0);
                crate::runtime::link_drain::apply_to(self, action);
            }
            Err(error) => {
                self.durable_resync = true;
                tracing::error!(%error, "the outbox's claims could not be read; the snapshot waits");
            }
        }
    }

    /// v2 `markDrained`/`markPending`: drained once the barrier has left replay
    /// with no durable event left and no claim holding the snapshot back.
    pub(in crate::runtime) fn decide_replay_barrier(&self) {
        let Some(delivery) = &self.durable_delivery else {
            return;
        };
        let past_replay = matches!(self.pump.barrier(), Barrier::Snapshot | Barrier::Live);
        if past_replay
            && self.durable.is_empty()
            && !self.pump.snapshot_blocked()
            && !self.durable_resync
        {
            delivery.mark_drained();
        } else {
            delivery.mark_pending();
        }
    }

    /// The snapshot the barrier asked for, framed as v2's `Event{snapshot,
    /// client_seq}` under a sequence the outbox draws only while no committed
    /// row is left unoffered. A row that beat the draw sends the barrier back
    /// to replay: the snapshot would describe a state that already moved on.
    pub(in crate::runtime) async fn authorise_snapshot(&mut self) {
        if !std::mem::take(&mut self.snapshot_wanted) || self.pump.barrier() != Barrier::Snapshot {
            return;
        }
        // v2 `startSnapshotBarrier` with no provider yet: the stage waits,
        // drained, until the activation wakes the link.
        if self.snapshot_held() {
            self.snapshot_wanted = true;
            return;
        }
        let event = match self.snapshot.snapshot() {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(%error, "the barrier cannot leave the snapshot stage yet");
                return;
            }
        };
        if let Some(outbox) = self.durable_rows.clone() {
            match outbox.snapshot_sequence(self.durable_offered_through).await {
                Ok(Some(seq)) => self.pump.reassign_snapshot_sequence(seq),
                Ok(None) => return self.resume_replay_before_snapshot().await,
                Err(error) => {
                    self.snapshot_wanted = true;
                    tracing::error!(%error, "the snapshot sequence could not be drawn; the snapshot waits");
                    return;
                }
            }
        }
        let Some(client_seq) = self.pump.snapshot_sequence() else {
            tracing::error!("the barrier asked for a snapshot it holds no sequence for");
            return;
        };
        let frame = CoordWorkerUpstream::Event {
            event,
            client_seq,
            trace_id: None,
        };
        match self.wire.encode_upstream(&frame) {
            Ok(bytes) => {
                self.snapshot_since = None;
                tracing::info!(
                    client_seq,
                    bytes = bytes.len(),
                    "publishing the worker snapshot"
                );
                self.authorised = Some(Authorised::Snapshot(bytes));
            }
            Err(error) => tracing::warn!(%error, "the worker snapshot did not encode"),
        }
    }

    async fn resume_replay_before_snapshot(&mut self) {
        tracing::info!(
            "a durable row was committed before the snapshot drew its sequence; replay resumes"
        );
        self.pump.abandon_snapshot();
        self.durable_resync = true;
        self.sync_durable_rows().await;
        let action = self.pump.note_durable_appeared();
        crate::runtime::link_drain::apply_to(self, action);
    }
}
