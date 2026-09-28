//! Bounded browser-to-worker attachment-peer signaling. Grant authority stays in
//! `AttachmentGrantOwner`; this owner stores only pending operations and exact
//! worker-generation fences. Typed worker answers never use generic RPC JSON,
//! and credentials, SDP and candidates never enter a coordinator log line.
//! Built once on `AttachmentsRuntime`; called by `rpc_direct`, settled by
//! `worker_link::direct_results`; its table is `peer_table`. Ports
//! `apps/coord/src/attachments/attachment-peer-negotiations.ts`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use connectrpc::{ConnectError, ErrorCode};
use roost_proto::{
    SessionsNegotiateAttachmentPeerRequest, SessionsNegotiateAttachmentPeerResponse,
    WLocalAttachmentPeerAnswer, WLocalAttachmentPeerError,
};
use roost_protocol::terminal_peer::sdp::inspect_terminal_peer_sdp;
use sha2::Digest;
use tokio::time::Instant;

use crate::attachments::grant_state::{
    AttachmentGrantInvalidation, AttachmentGrantPort, is_exact_attachment_worker,
};
use crate::attachments::peer_state::{
    AttachmentPeerCaller, AttachmentPeerSignalConfig, PeerKey, PeerOutcome, PendingAttachmentPeer,
    assert_attachment_peer_request_shape, attachment_peer_error as refusal,
    attachment_peer_worker_failure, require_owned_grant, require_stable_grant, require_worker,
};
use crate::attachments::peer_table::{PeerTable, invalidation_names};
use crate::attachments::transfer_limits::{
    ATTACHMENT_TRANSFER_PEER_ERROR_REASONS, ATTACHMENT_TRANSFER_PEER_NATIVE_ANSWER_DEADLINE_MS,
};
use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::workers::attachment_send::{
    AttachmentPeerOfferSend, send_attachment_peer_cancel, send_attachment_peer_offer,
};

/// Signaling admission and typed answer correlation, one per process.
pub struct AttachmentPeerNegotiations {
    workers: Arc<WorkerRegistry>,
    grants: Arc<dyn AttachmentGrantPort>,
    table: Mutex<PeerTable>,
}

impl std::fmt::Debug for AttachmentPeerNegotiations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let table = self.table();
        formatter
            .debug_struct("AttachmentPeerNegotiations")
            .field("admitting", &table.admitting.len())
            .field("pending", &table.pending.len())
            .finish_non_exhaustive()
    }
}

/// Releases an admission that never became a pending offer (v2's `finally`).
struct AdmissionRelease<'a> {
    owner: &'a AttachmentPeerNegotiations,
    key: Option<PeerKey>,
}

impl Drop for AdmissionRelease<'_> {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.owner.table().admitting.remove(&key);
        }
    }
}

/// A browser that abandons its call cancels its offer on the worker too
/// (v2's abort listener); a settled offer is already gone, so this no-ops.
struct AbandonCancel<'a> {
    owner: &'a AttachmentPeerNegotiations,
    request_id: String,
}

impl Drop for AbandonCancel<'_> {
    fn drop(&mut self) {
        let cancelled = refusal(ErrorCode::Canceled, "attachment peer negotiation cancelled");
        self.owner.cancel_pending(&self.request_id, cancelled, true);
    }
}

impl AttachmentPeerNegotiations {
    /// An owner over the worker registry and the grant authority it trusts,
    /// subscribed to that authority's invalidations.
    #[must_use]
    pub fn new(workers: Arc<WorkerRegistry>, grants: Arc<dyn AttachmentGrantPort>) -> Arc<Self> {
        let owner = Arc::new(Self {
            workers,
            grants: Arc::clone(&grants),
            table: Mutex::default(),
        });
        let listener: Weak<Self> = Arc::downgrade(&owner);
        grants.subscribe_invalidation(Box::new(move |invalidation| {
            if let Some(owner) = listener.upgrade() {
                owner.cancel_invalidated(invalidation);
            }
        }));
        owner
    }

    /// Admit one offer against the caller's exact grant, send it to the exact
    /// worker generation, and wait for that generation's typed answer.
    pub async fn negotiate(
        &self,
        caller: &AttachmentPeerCaller,
        request: &SessionsNegotiateAttachmentPeerRequest,
        config: &AttachmentPeerSignalConfig,
    ) -> PeerOutcome {
        assert_attachment_peer_request_shape(request)?;
        if caller.tab_id.as_deref() != Some(request.tab_id.as_str()) {
            let message = "attachment peer tab does not match the authenticated document";
            return Err(refusal(ErrorCode::PermissionDenied, message));
        }
        let offer_digest = hex::encode(sha2::Sha256::digest(request.offer_sdp.as_bytes()));
        let key = PeerKey {
            owner_key: caller.owner_key.clone(),
            tab_id: request.tab_id.clone(),
            worker_fp: request.worker_fp.clone(),
        };
        self.table()
            .claim_admission(&key, request, &offer_digest, &caller.device_fingerprint)?;
        let mut admission = AdmissionRelease {
            owner: self,
            key: Some(key.clone()),
        };
        if inspect_terminal_peer_sdp(&request.offer_sdp).is_err() {
            return Err(refusal(
                ErrorCode::InvalidArgument,
                "attachment peer offer is invalid",
            ));
        }
        let grants = self.grants.as_ref();
        let grant = require_owned_grant(grants, &caller.owner_key, request)?;
        let stable = require_stable_grant(grants, &caller.owner_key, request, &grant)?;
        let worker = require_worker(&self.workers, request, &stable, config)?;
        let deadline = Instant::now()
            + Duration::from_millis(ATTACHMENT_TRANSFER_PEER_NATIVE_ANSWER_DEADLINE_MS);
        let (request_id, mut settled) = self.table().reserve(|settle| PendingAttachmentPeer {
            key,
            device_fingerprint: caller.device_fingerprint.clone(),
            grant_id: stable.grant_id.clone(),
            peer_id: request.peer_id.clone(),
            offer_digest,
            connection_generation: worker.connection_generation.clone(),
            worker: Arc::clone(&worker),
            worker_epoch: request.worker_epoch.clone(),
            settle,
        })?;
        admission.key = None;
        let _abandon = AbandonCancel {
            owner: self,
            request_id: request_id.clone(),
        };
        let budget_ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        let offer = AttachmentPeerOfferSend {
            request_id: request_id.clone(),
            grant_id: stable.grant_id.clone(),
            peer_id: request.peer_id.clone(),
            device_fingerprint: caller.device_fingerprint.clone(),
            tab_id: request.tab_id.clone(),
            worker_epoch: request.worker_epoch.clone(),
            offer_sdp: request.offer_sdp.clone(),
            budget_ms: u32::try_from(budget_ms).unwrap_or(u32::MAX),
            stun_urls: config.stun_urls.clone(),
        };
        if budget_ms == 0 {
            let expired = refusal(
                ErrorCode::DeadlineExceeded,
                "attachment peer answer timed out",
            );
            self.cancel_pending(&request_id, expired, true);
        } else if !send_attachment_peer_offer(&self.workers, &worker, offer) {
            let offline = refusal(
                ErrorCode::Unavailable,
                "attachment peer worker is unavailable",
            );
            self.cancel_pending(&request_id, offline, false);
        } else {
            tracing::debug!(worker_fp = %worker.worker_fp, pending = self.table().pending.len(),
                "attachment peer: offer_sent");
        }
        let received = match tokio::time::timeout_at(deadline, &mut settled).await {
            Ok(received) => received,
            Err(_) => {
                let expired = refusal(
                    ErrorCode::DeadlineExceeded,
                    "attachment peer answer timed out",
                );
                self.cancel_pending(&request_id, expired, true);
                settled.await
            }
        };
        let response = received.unwrap_or_else(|_| {
            Err(refusal(
                ErrorCode::Unavailable,
                "attachment peer signaling is unavailable",
            ))
        })?;
        let current = require_stable_grant(grants, &caller.owner_key, request, &stable)?;
        require_worker(&self.workers, request, &current, config)?;
        Ok(response)
    }

    /// Settle an offer with the exact generation's answer. `false` when nothing
    /// waits under that identity from that source.
    pub fn accept_answer(
        &self,
        source: &Arc<WorkerHandle>,
        answer: &WLocalAttachmentPeerAnswer,
    ) -> bool {
        if !self.matches_pending(
            source,
            &answer.request_id,
            &answer.connection_generation,
            &answer.worker_epoch,
            &answer.peer_id,
        ) {
            return false;
        }
        if inspect_terminal_peer_sdp(&answer.answer_sdp).is_err() {
            let invalid = refusal(
                ErrorCode::Unavailable,
                "attachment peer worker returned an invalid answer",
            );
            self.cancel_pending(&answer.request_id, invalid, true);
            return true;
        }
        let Some(pending) = self.table().pending.remove(&answer.request_id) else {
            return false;
        };
        let _ = pending
            .settle
            .send(Ok(SessionsNegotiateAttachmentPeerResponse {
                peer_id: pending.peer_id.clone(),
                answer_sdp: answer.answer_sdp.clone(),
                worker_epoch: pending.worker_epoch.clone(),
                ..Default::default()
            }));
        tracing::debug!(worker_fp = %source.worker_fp, pending = self.table().pending.len(),
            "attachment peer: answer_accepted");
        true
    }

    /// Fail an offer with the worker's fixed refusal reason.
    pub fn accept_error(
        &self,
        source: &Arc<WorkerHandle>,
        error: &WLocalAttachmentPeerError,
    ) -> bool {
        if !ATTACHMENT_TRANSFER_PEER_ERROR_REASONS.contains(&error.reason.as_str())
            || !self.matches_pending(
                source,
                &error.request_id,
                &error.connection_generation,
                &error.worker_epoch,
                &error.peer_id,
            )
        {
            return false;
        }
        self.cancel_pending(
            &error.request_id,
            attachment_peer_worker_failure(&error.reason),
            false,
        );
        true
    }

    /// Fail every offer the ended generation would have answered.
    pub fn cancel_for_worker_handle(&self, worker: &Arc<WorkerHandle>, reason: &str) {
        let doomed = self
            .table()
            .pending_where(|pending| Arc::ptr_eq(&pending.worker, worker));
        for request_id in doomed {
            let changed = refusal(
                ErrorCode::Unavailable,
                "attachment peer worker connection changed",
            );
            self.cancel_pending(&request_id, changed, true);
        }
        tracing::debug!(worker_fp = %worker.worker_fp, reason, "attachment peer: worker_handle_cancelled");
    }

    fn matches_pending(
        &self,
        source: &Arc<WorkerHandle>,
        request_id: &str,
        connection_generation: &str,
        worker_epoch: &str,
        peer_id: &str,
    ) -> bool {
        let table = self.table();
        table.pending.get(request_id).is_some_and(|pending| {
            Arc::ptr_eq(source, &pending.worker)
                && source.connection_generation == pending.connection_generation
                && is_exact_attachment_worker(&self.workers, source, &pending.worker_epoch)
                && connection_generation == pending.connection_generation
                && worker_epoch == pending.worker_epoch
                && peer_id == pending.peer_id
        })
    }

    fn cancel_invalidated(&self, invalidation: &AttachmentGrantInvalidation) {
        let doomed = self
            .table()
            .pending_where(|pending| invalidation_names(invalidation, pending));
        for request_id in doomed {
            let revoked = refusal(
                ErrorCode::PermissionDenied,
                "attachment peer grant is unavailable",
            );
            self.cancel_pending(&request_id, revoked, true);
        }
    }

    fn cancel_pending(&self, request_id: &str, error: ConnectError, send_cancel: bool) {
        let Some(pending) = self.table().pending.remove(request_id) else {
            return;
        };
        if send_cancel {
            send_attachment_peer_cancel(
                &self.workers,
                &pending.worker,
                request_id,
                &pending.peer_id,
                &pending.worker_epoch,
            );
        }
        let _ = pending.settle.send(Err(error));
        tracing::debug!(worker_fp = %pending.worker.worker_fp, pending = self.table().pending.len(),
            "attachment peer: pending_cancelled");
    }

    fn table(&self) -> MutexGuard<'_, PeerTable> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
