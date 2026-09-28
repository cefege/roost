//! How a terminal-peer offer ends: the exact current worker's typed answer or
//! refusal, the browser's abort, the answer deadline, a grant invalidation, a
//! worker generation ending, or the owner shutting down. Each path removes the
//! offer once and withdraws it from the worker when the worker still has it.
//! Called by `worker_link::direct_results`, the worker lifecycle and `peer_negotiations`.
//! Ports the settle and cancel half of `apps/coord/src/terminal/direct/terminal-peer-negotiations.ts`.

use std::sync::Arc;

use connectrpc::ConnectError;
use roost_proto::{
    SessionsNegotiateLocalTerminalPeerResponse as PeerResponse, WLocalTerminalPeerAnswer,
    WLocalTerminalPeerError,
};
use roost_protocol::terminal_peer::sdp::inspect_terminal_peer_sdp;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::coord_core::worker_handle::WorkerHandle;
use crate::terminal_direct::grant_state::{
    TerminalGrantInvalidation, TerminalGrantInvalidationKind,
};
use crate::terminal_direct::peer_negotiations::{TerminalPeerNegotiations, signaling_unavailable};
use crate::terminal_direct::peer_state::{
    terminal_peer_cancelled, terminal_peer_deadline_exceeded, terminal_peer_denied,
    terminal_peer_unavailable, terminal_peer_worker_failure,
};
use crate::terminal_direct::peer_table::PeerOutcome;
use crate::workers::terminal_peer_send::{
    TerminalPeerCancelSend, is_terminal_peer_worker_error_reason, send_terminal_peer_cancel,
};

impl TerminalPeerNegotiations {
    /// Settle a pending offer with the exact current worker's typed answer.
    pub fn accept_answer(
        &self,
        source: &Arc<WorkerHandle>,
        answer: &WLocalTerminalPeerAnswer,
    ) -> bool {
        let mut table = self.table();
        let Some(pending) = table.get(&answer.request_id) else {
            return false;
        };
        let (generation, epoch) = (&answer.connection_generation, &answer.worker_epoch);
        if !self.matches_pending(source, pending, generation, epoch, &answer.peer_id) {
            return false;
        }
        if inspect_terminal_peer_sdp(&answer.answer_sdp).is_err() {
            drop(table);
            let error =
                terminal_peer_unavailable("terminal peer worker returned an invalid answer");
            self.cancel_pending(&answer.request_id, error, "invalid_answer", true);
            return true;
        }
        let Some(pending) = table.remove(&answer.request_id) else {
            return false;
        };
        let remaining = table.pending_count();
        drop(table);
        let _ = pending.settle.send(Ok(PeerResponse {
            peer_id: pending.peer_id.clone(),
            answer_sdp: answer.answer_sdp.clone(),
            worker_epoch: pending.worker_epoch.clone(),
            ..Default::default()
        }));
        tracing::debug!(worker_fp = %pending.worker_fp, pending = remaining,
            "terminal peer negotiations: answer_accepted");
        true
    }

    /// Settle a pending offer with the exact current worker's typed refusal.
    pub fn accept_error(
        &self,
        source: &Arc<WorkerHandle>,
        error: &WLocalTerminalPeerError,
    ) -> bool {
        {
            let table = self.table();
            let Some(pending) = table.get(&error.request_id) else {
                return false;
            };
            let (generation, epoch) = (&error.connection_generation, &error.worker_epoch);
            if !is_terminal_peer_worker_error_reason(&error.reason)
                || !self.matches_pending(source, pending, generation, epoch, &error.peer_id)
            {
                return false;
            }
        }
        let failure = terminal_peer_worker_failure(&error.reason);
        self.cancel_pending(&error.request_id, failure, &error.reason, false);
        true
    }

    /// Fail every offer the given socket generation was carrying.
    pub fn cancel_for_worker_handle(&self, worker: &Arc<WorkerHandle>, reason: &str) {
        let doomed = self
            .table()
            .request_ids_where(|pending| Arc::ptr_eq(&pending.worker, worker));
        for request_id in doomed {
            let error = terminal_peer_unavailable("terminal peer worker connection changed");
            self.cancel_pending(&request_id, error, reason, true);
        }
    }

    /// Stop signaling: release admissions and fail every pending offer.
    pub fn dispose(&self) {
        let doomed = {
            let mut table = self.table();
            if table.disposed {
                return;
            }
            table.disposed = true;
            table.release_all_admissions();
            table.request_ids_where(|_| true)
        };
        if let Some(subscription) = self.grant_subscription {
            self.grants.unsubscribe_invalidation(subscription);
        }
        for request_id in doomed {
            self.cancel_pending(&request_id, signaling_unavailable(), "disposed", true);
        }
    }

    /// Wait for the typed answer, the browser's abort, or the answer deadline;
    /// the last two withdraw the offer from the worker.
    pub(super) async fn await_answer(
        &self,
        request_id: &str,
        deadline: Instant,
        abort: &CancellationToken,
        mut settled: oneshot::Receiver<PeerOutcome>,
    ) -> PeerOutcome {
        tokio::select! {
            biased;
            outcome = &mut settled => return outcome.unwrap_or_else(|_| Err(signaling_unavailable())),
            () = abort.cancelled() => {
                self.cancel_pending(request_id, terminal_peer_cancelled(), "aborted", true);
            }
            () = tokio::time::sleep_until(deadline) => {
                self.cancel_pending(request_id, terminal_peer_deadline_exceeded(), "timeout", true);
            }
        }
        settled
            .await
            .unwrap_or_else(|_| Err(signaling_unavailable()))
    }

    pub(super) fn cancel_invalidated(&self, invalidation: &TerminalGrantInvalidation) {
        let doomed = self.table().request_ids_where(|pending| {
            let kind = invalidation.kind;
            kind == TerminalGrantInvalidationKind::Disposed
                || (kind == TerminalGrantInvalidationKind::WorkerRetired
                    && invalidation.worker_fp == pending.worker_fp)
                || (kind == TerminalGrantInvalidationKind::DeviceRevoked
                    && invalidation.device_fingerprint.as_deref()
                        == Some(pending.device_fingerprint.as_str()))
                || invalidation.lease.as_ref().is_some_and(|lease| {
                    lease.grant_id == pending.grant_id
                        && lease.owner_key == pending.owner_key
                        && lease.tab_id == pending.tab_id
                        && lease.worker_fp == pending.worker_fp
                })
        });
        for request_id in doomed {
            let error = terminal_peer_denied("terminal peer grant is unavailable");
            self.cancel_pending(&request_id, error, invalidation.kind.as_str(), true);
        }
    }

    /// Fail one pending offer, withdrawing it from the worker when asked.
    pub(super) fn cancel_pending(
        &self,
        request_id: &str,
        error: ConnectError,
        reason: &str,
        send_cancel: bool,
    ) {
        let (pending, remaining) = {
            let mut table = self.table();
            let Some(pending) = table.remove(request_id) else {
                return;
            };
            (pending, table.pending_count())
        };
        if send_cancel {
            let cancel = TerminalPeerCancelSend {
                request_id: pending.request_id.clone(),
                peer_id: pending.peer_id.clone(),
                worker_epoch: pending.worker_epoch.clone(),
            };
            send_terminal_peer_cancel(&self.workers, &pending.worker, cancel);
        }
        let _ = pending.settle.send(Err(error));
        tracing::debug!(worker_fp = %pending.worker_fp, reason, pending = remaining,
            "terminal peer negotiations: pending_cancelled");
    }
}
