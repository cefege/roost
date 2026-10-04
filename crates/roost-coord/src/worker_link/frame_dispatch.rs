//! What one worker socket's frames turn into: the three arms of `FrameDispatch`.
//!
//! Ported from `apps/coord/src/workers/worker-frame-dispatch.ts`. `dispatch.rs`
//! owns the contract and `dispatcher_for` owns the construction; this file owns
//! the decisions. Two of them are worth reading twice: the durable arm offers a
//! `client_seq` BEFORE it appends, and the deferred reap of a committed snapshot
//! is sent from here rather than from inside the append, which is what makes the
//! readiness barrier the thing that releases a force-closed PTY's kill.

use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode};
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream, EventAck};
use roost_protocol::wire::{SessionEvent, WorkerFp};

use crate::coord_core::core::CoordCore;
use crate::coord_core::worker_handle::WorkerHandle;
use crate::events::append::{AppendEventResult, AppendOptions, Caller};
use crate::serve::now_ms;
use crate::worker_link::client_seq::ClientSeqCursor;
use crate::worker_link::dispatch::{
    DispatchFuture, DispatchOutcome, FrameClass, FrameDispatch, FrameRefusal, InboundFrame,
    close_for_append_error,
};
use crate::workers::registry::mark_generation_ready;

/// One worker socket's frame dispatcher.
///
/// Holds the process state and THIS socket's handle, and nothing else: the
/// announced-channel barrier and the frame queue belong to the read loop
/// (`worker_link::connection`), which is the only thing that knows when a frame
/// may be released.
pub struct WorkerFrameDispatcher {
    /// The process state, reached through the core the agent-status hub wants.
    pub(super) core: CoordCore,
    /// This socket's generation: the fence, the barrier, and the sender.
    pub(super) handle: Arc<WorkerHandle>,
    /// This worker's `client_seq` position, shared with every socket it has.
    cursor: Arc<ClientSeqCursor>,
}

impl std::fmt::Debug for WorkerFrameDispatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerFrameDispatcher")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl WorkerFrameDispatcher {
    /// A dispatcher over one socket's handle and one worker's sequence cursor.
    #[must_use]
    pub fn new(core: CoordCore, handle: Arc<WorkerHandle>, cursor: Arc<ClientSeqCursor>) -> Self {
        Self {
            core,
            handle,
            cursor,
        }
    }

    /// Whether the read loop handed us the socket this dispatcher was built for.
    ///
    /// The `worker_fp` on the call is the transport's AUTHENTICATED answer and
    /// the handle's is what the hello claimed. They cannot disagree without a
    /// routing bug, and acting on the disagreement is how one worker's frame
    /// reaches another's state.
    pub(super) fn is_this_socket(&self, worker_fp: &str) -> bool {
        self.handle.worker_fp.as_str() == worker_fp
    }

    /// Whether this socket is still the worker's live generation: not revoked,
    /// and not superseded by a newer one (v2 `worker-conn.ts`
    /// `_isCurrentGeneration`).
    ///
    /// Identity, not readiness: a superseded socket is fenced even when it is
    /// perfectly ready, and a frame it appends under the old generation would
    /// be published against a link the worker has already replaced. A revoked
    /// generation is fenced before the registry lets go of it, so a late answer
    /// arriving between the revoke and the removal settles nothing.
    pub(super) fn is_current_generation(&self) -> bool {
        !self.handle.is_revoked()
            && self
                .core
                .services
                .workers
                .current(&self.handle.worker_fp)
                .is_some_and(|current| Arc::ptr_eq(&current, &self.handle))
    }

    /// The v2 `fenced(what)` guard: a fenced frame is dropped with no ACK and
    /// no close, because the worker that sent it will replay it on its new link.
    pub(super) fn fenced(&self, what: &'static str) -> bool {
        if self.is_current_generation() {
            return false;
        }
        tracing::debug!(
            worker_fp = %self.handle.worker_fp,
            what,
            "a superseded generation dropped a frame"
        );
        true
    }

    /// Whether this socket may carry a frame of this event kind yet.
    ///
    /// Before the snapshot barrier only the five kinds that CAN establish or
    /// reconcile a session are admitted; everything else is dropped SILENTLY
    /// with no ACK, so the worker replays it after its snapshot
    /// (`worker-frame-dispatch.ts:107-121`).
    pub(super) fn may_cross_barrier(&self, event: &SessionEvent) -> bool {
        self.handle.is_ready()
            || matches!(
                event,
                SessionEvent::Opened { .. }
                    | SessionEvent::Closed { .. }
                    | SessionEvent::Respawned { .. }
                    | SessionEvent::AgentReference { .. }
                    | SessionEvent::Snapshot { .. }
            )
    }

    /// Tell the worker which exact sequence settled, best effort.
    ///
    /// The handle's own `send`, not `send_frame_through`: v2's `sendBestEffort`
    /// writes to the socket it holds and ignores the answer, and a first
    /// snapshot is ACKed BEFORE its generation is routable, so requiring
    /// routability here would withhold the ACK that releases the replay barrier.
    fn acknowledge(&self, client_seq: u64) {
        if self
            .handle
            .send(CoordWorkerDownstream::EventAck(EventAck { client_seq }))
            == 0
        {
            tracing::debug!(
                worker_fp = %self.handle.worker_fp,
                client_seq,
                "an event acknowledgement did not reach the socket"
            );
        }
    }

    /// Kill the PTYs a committed snapshot force-closed while their worker was
    /// offline.
    ///
    /// v2's `dispatchSnapshotOrphanReaps`, called from the same place: AFTER the
    /// barrier is crossed, never before. The append was told to defer, so the
    /// ids came back to us instead of being killed from inside the transaction;
    /// that is what makes the readiness barrier the release condition rather
    /// than a comment about one. The kill itself travels the process's single
    /// `LiveEffects`, so a worker that is offline records it owed and reads it
    /// on reconnect.
    fn drain_reaps(&self, worker_fp: &WorkerFp, session_ids: &[String]) {
        let effects = self.core.services.event_log.live_effects();
        for session_id in session_ids {
            effects.kill_orphan_pty(worker_fp, session_id);
        }
    }
}

impl FrameDispatch for WorkerFrameDispatcher {
    fn handle_now(&self, worker_fp: &str, frame: InboundFrame) -> DispatchOutcome {
        if !self.is_this_socket(worker_fp) {
            return DispatchOutcome::Refused;
        }
        match frame.class {
            FrameClass::Live => self.handle_live(worker_fp, frame),
            FrameClass::Rpc => self.handle_rpc(worker_fp, frame),
            // A durable frame on the synchronous arm is a read loop that
            // classified it wrong. REFUSED, NOT CLOSED, and the reason is v2's:
            // the event was neither appended nor acknowledged, so the worker
            // still holds it in its outbox and replays it. Closing would only
            // tell it to do the same thing more expensively.
            FrameClass::Durable => self.refuse(frame.channel, "durable_frame_on_the_sync_arm"),
        }
    }

    fn handle_durable<'a>(
        &'a mut self,
        worker_fp: &'a str,
        frame: InboundFrame,
    ) -> DispatchFuture<'a> {
        Box::pin(async move { self.handle_one_durable(worker_fp, frame).await })
    }
}

impl WorkerFrameDispatcher {
    /// One durable `SessionEvent`: the gates, the sequence, the append, and the
    /// decisions the commit makes possible.
    async fn handle_one_durable(&self, worker_fp: &str, frame: InboundFrame) -> DispatchOutcome {
        if !self.is_this_socket(worker_fp) {
            return DispatchOutcome::Refused;
        }
        let InboundFrame {
            channel,
            frame: upstream,
            ..
        } = frame;
        let CoordWorkerUpstream::Event {
            event, client_seq, ..
        } = upstream
        else {
            return self.refuse(channel, "durable_arm_mismatch");
        };
        // A zero sequence is a frame from a producer that never allocated one.
        // It is dropped rather than admitted as sequence 0, because a cursor
        // that had seen 0 would then expect 0 again forever.
        if client_seq == 0 {
            return self.refuse(channel, "invalid_event_client_seq");
        }
        if self.fenced("event") {
            return DispatchOutcome::Refused;
        }
        if !self.may_cross_barrier(&event) {
            // Folder metadata is replaceable, and the worker replays its journal
            // one unacknowledged row at a time before the snapshot: withholding
            // this ACK stalled that replay forever. The next change re-sends it.
            if crate::worker_link::dispatch::is_folder_metadata(&event) {
                tracing::info!(
                    worker_fp,
                    client_seq,
                    "pre-snapshot folder metadata dropped"
                );
                self.acknowledge(client_seq);
                return DispatchOutcome::Handled;
            }
            tracing::debug!(
                worker_fp,
                client_seq,
                "a durable frame arrived before the snapshot barrier"
            );
            return DispatchOutcome::Refused;
        }
        // A held keeper-update fence withholds the ACK so the worker replays this
        // entry after the update: acking here loses the durable record of which
        // PTYs are live (`worker-frame-dispatch.ts:122-127`).
        let Ok(lease) = self.core.services.write_gate.acquire_shared() else {
            return DispatchOutcome::Refused;
        };

        // THE SLOT IS RECORDED BEFORE THE APPEND RUNS, under the write lease, so
        // the cursor and the durable row agree on the order sequences arrived.
        // Nothing here refuses: v2 appends every positive sequence and lets the
        // unique index dedupe (`worker-frame-dispatch.ts:62-71`), and a worker
        // resuming its outbox after a coordinator restart starts above 1.
        if let crate::worker_link::client_seq::SeqVerdict::Resumed { expected, offered } =
            self.cursor.offer(client_seq).await
        {
            tracing::info!(
                worker_fp,
                expected,
                offered,
                "a worker resumed its durable sequence past what this cursor saw"
            );
        }

        let appended = self.append(event, client_seq).await;
        drop(lease);
        match appended {
            Err(error) => {
                tracing::error!(worker_fp, client_seq, %error, "a durable append failed");
                DispatchOutcome::Close(close_for_append_error(&error))
            }
            Ok(result) => self.after_append(worker_fp, client_seq, result),
        }
    }

    /// The append itself, over the process's ONE event log.
    ///
    /// `Caller::worker` is given the HANDLE's fingerprint, never the one the
    /// event claimed: a claim is checked for agreement and is not an identity,
    /// and admission binds durable rows to what the transport authenticated.
    async fn append(
        &self,
        event: SessionEvent,
        client_seq: u64,
    ) -> Result<AppendEventResult, ConnectError> {
        let services = &self.core.services;
        let tenant = services.boot.require_tenant().inspect_err(|_| {
            tracing::error!(
                worker_fp = %self.handle.worker_fp,
                "a worker link cannot append before the tenancy scope is established"
            );
        })?;
        let caller = Caller::worker(
            self.handle.worker_fp.clone(),
            client_seq,
            &tenant.dashboard_id,
        );
        let fence = || self.is_current_generation();
        let mut options = AppendOptions {
            now_ms: now_ms(),
            buses: &services.buses,
            live_effects: services.event_log.live_effects().as_ref(),
            pending_publications: Some(Arc::clone(services.event_log.pending_publications())),
            can_publish: Some(&fence),
            extra_work: None,
            // SET FOR EVERY WORKER CONNECTION, NOT ONLY FOR SNAPSHOTS. The append
            // consults the flag only inside a `Snapshot` arm, so the narrowed
            // form v2 passes reaches the same decision; and the field's own
            // contract is about the CALLER ("worker connections defer, direct
            // coordinator callers do not"). That is what makes the ids come back
            // to `drain_reaps` here instead of being killed from inside the
            // commit, before this socket crossed its barrier.
            defer_snapshot_reap: true,
        };
        services
            .event_log
            .append_event(event, &caller, &mut options)
            .await
            .map_err(|error| ConnectError::new(ErrorCode::Internal, error.to_string()))
    }

    /// What a committed append makes the socket do next.
    fn after_append(
        &self,
        worker_fp: &str,
        client_seq: u64,
        result: AppendEventResult,
    ) -> DispatchOutcome {
        // A refusal is a DATA outcome: no ACK and no close, so a prober cannot
        // tell "never existed" from "not yours" (`event-admission.ts`).
        if !result.admitted {
            tracing::debug!(
                worker_fp,
                client_seq,
                "a durable append was refused as data"
            );
            return DispatchOutcome::Refused;
        }
        if self.fenced("event_post_commit") {
            return DispatchOutcome::Refused;
        }
        if result.replay_rejected {
            tracing::warn!(
                worker_fp,
                client_seq,
                "a dedupe replay carried a different payload for one sequence"
            );
            return DispatchOutcome::Close(FrameRefusal::DedupeMismatch.close());
        }
        if matches!(result.event, SessionEvent::Snapshot { .. }) {
            return self.after_snapshot(worker_fp, client_seq, result);
        }
        self.core
            .services
            .sessions
            .resolve_spawn_on_opened(&self.handle.worker_fp, &result.event);
        self.acknowledge(client_seq);
        DispatchOutcome::Handled
    }

    /// A committed snapshot crosses the barrier, is ACKed, and only then are the
    /// force-closed PTYs it gave back killed.
    ///
    /// The unpublished case returns before ALL THREE. A dedupe or a stale
    /// generation did not install this connection's exact live set, so it must
    /// neither become ready, nor release a kill, nor be ACKed.
    fn after_snapshot(
        &self,
        worker_fp: &str,
        client_seq: u64,
        result: AppendEventResult,
    ) -> DispatchOutcome {
        if !result.published {
            tracing::debug!(
                worker_fp,
                client_seq,
                "a snapshot committed without publishing"
            );
            return DispatchOutcome::Refused;
        }
        if mark_generation_ready(
            &self.core.services.buses,
            &self.core.services.workers,
            &self.handle,
        ) {
            tracing::info!(
                worker_fp = %self.handle.worker_fp,
                "a worker snapshot crossed its readiness barrier"
            );
        }
        self.acknowledge(client_seq);
        self.drain_reaps(&self.handle.worker_fp, &result.snapshot_reap_ids);
        DispatchOutcome::Handled
    }

    /// A frame this dispatcher will not process, named for the log line.
    pub(crate) fn refuse(&self, channel: u32, reason: &'static str) -> DispatchOutcome {
        tracing::warn!(
            worker_fp = %self.handle.worker_fp,
            reason,
            channel,
            "a frame was refused before it was processed"
        );
        DispatchOutcome::Refused
    }
}
