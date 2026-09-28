//! One attachment peer offer from admission to answer: the admission verdict
//! (checked before native allocation and again after every await), the
//! capacity bounds, the native connection and its answer, and the retirement
//! of the active entry it became. Called by `peer_owner`. Ports
//! `admissionFailure`, `offer` and `handleConnectionClosed` of
//! `apps/worker/src/attachments/attachment-peer-owner.ts`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use roost_proto::{DLocalAttachmentPeerOffer, WLocalAttachmentPeerAnswer};
use roost_protocol::attachment_transfer::{
    MAX_ACTIVE_PER_WORKER, PEER_MAX_NEGOTIATIONS_PER_WORKER,
    PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE, PEER_NATIVE_ANSWER_DEADLINE_MS, PeerErrorReason,
};

use super::grants::PeerGrantAuthorization;
use super::peer_connection::{
    AttachmentPeerConnection, AttachmentPeerConnectionConfig, AttachmentPeerConnectionDeps,
    AttachmentPeerConnectionFailure as Failure,
};
use super::peer_owner::{
    ActivePeer, AttachmentPeerBootstrapState, AttachmentPeerOwner, OwnerInner, OwnerState,
};
use super::peer_request_validation::valid_attachment_peer_offer_identity;
use super::transfer_admission::AttachmentPeerExpectedTuple;
use crate::uplink::{LinkFence, RequestBudget};

/// One offer between admission and its answer.
#[derive(Debug)]
pub(super) struct PendingPeer {
    pub request: DLocalAttachmentPeerOffer,
    pub budget: RequestBudget,
    pub fence: LinkFence,
    pub expected_tuple: AttachmentPeerExpectedTuple,
    /// Identity: a finished negotiation touches only the entry it created.
    pub serial: u64,
    pub connection: Option<AttachmentPeerConnection>,
}

/// The first reason this offer may not proceed, in v2's order.
pub(super) fn admission_failure(
    inner: &OwnerInner,
    request: &DLocalAttachmentPeerOffer,
    budget: &RequestBudget,
    fence: &LinkFence,
) -> Option<PeerErrorReason> {
    let deps = &inner.deps;
    if inner.lock().disposed || request.worker_epoch != deps.process_epoch {
        return Some(PeerErrorReason::ConnectionSuperseded);
    }
    if !deps.enabled {
        return Some(PeerErrorReason::Disabled);
    }
    if !valid_attachment_peer_offer_identity(request) {
        return Some(PeerErrorReason::InvalidOffer);
    }
    if !(deps.is_current_coordinator)(&request.connection_generation) || !fence.is_current() {
        return Some(PeerErrorReason::ConnectionSuperseded);
    }
    if budget.expired(Instant::now()) {
        return Some(PeerErrorReason::IceFailed);
    }
    match (deps.authorize_grant)(request) {
        PeerGrantAuthorization::Authorized => None,
        PeerGrantAuthorization::Expired => Some(PeerErrorReason::Expired),
        PeerGrantAuthorization::GrantUnavailable => Some(PeerErrorReason::GrantUnavailable),
    }
}

/// Pending negotiations one browser document holds.
fn pending_for_actor(state: &OwnerState, device_fingerprint: &str, tab_id: &str) -> usize {
    state
        .pending
        .values()
        .filter(|pending| {
            pending.expected_tuple.device_fingerprint == device_fingerprint
                && pending.expected_tuple.tab_id == tab_id
        })
        .count()
}

impl AttachmentPeerOwner {
    pub(super) fn admit_pending(
        &self,
        request: &DLocalAttachmentPeerOffer,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> Result<u64, PeerErrorReason> {
        let mut state = self.inner.lock();
        let over_capacity = state.pending.len() >= PEER_MAX_NEGOTIATIONS_PER_WORKER
            || state.active.len() + state.pending.len() >= MAX_ACTIVE_PER_WORKER
            || state.pending.contains_key(&request.request_id)
            || state.active.contains_key(&request.peer_id)
            || state
                .pending
                .values()
                .any(|pending| pending.request.peer_id == request.peer_id)
            || pending_for_actor(&state, &request.device_fingerprint, &request.tab_id)
                >= PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE;
        if over_capacity {
            return Err(PeerErrorReason::Capacity);
        }
        state.next_serial += 1;
        let serial = state.next_serial;
        let expected_tuple = AttachmentPeerExpectedTuple {
            peer_id: request.peer_id.clone(),
            grant_id: request.grant_id.clone(),
            device_fingerprint: request.device_fingerprint.clone(),
            tab_id: request.tab_id.clone(),
            worker_epoch: request.worker_epoch.clone(),
        };
        let pending = PendingPeer {
            request: request.clone(),
            budget,
            fence,
            expected_tuple,
            serial,
            connection: None,
        };
        state.pending.insert(request.request_id.clone(), pending);
        tracing::info!(pending = state.pending.len(), "attachment peer negotiating");
        Ok(serial)
    }

    pub(super) async fn negotiate(
        &self,
        request: &DLocalAttachmentPeerOffer,
        serial: u64,
        config: AttachmentPeerConnectionConfig,
        expected_remote_fingerprint: String,
        budget: RequestBudget,
    ) -> Result<WLocalAttachmentPeerAnswer, PeerErrorReason> {
        let bootstrap_state = self.bootstrap().await;
        let expected_tuple = self.assert_current(request, serial)?;
        let native = self.inner.lock().native.clone();
        let (AttachmentPeerBootstrapState::Ready, Some(factory)) = (bootstrap_state, native) else {
            return Err(match bootstrap_state {
                AttachmentPeerBootstrapState::Disabled => PeerErrorReason::Disabled,
                _ => PeerErrorReason::NativeUnavailable,
            });
        };
        let packet_budget = self.inner.deps.packet_budget.create_peer_budget();
        let (weak, peer_id) = (Arc::downgrade(&self.inner), request.peer_id.clone());
        let connection = AttachmentPeerConnection::new(AttachmentPeerConnectionDeps {
            factory,
            peer_id: request.peer_id.clone(),
            expected_tuple,
            expected_remote_fingerprint,
            config,
            packet_budget: packet_budget.clone(),
            open_peer_port: Arc::clone(&self.inner.deps.open_peer_port),
            on_closed: Box::new(move |reason: Failure| {
                if let Some(inner) = weak.upgrade() {
                    connection_closed(&inner, &peer_id, serial, reason);
                }
            }),
        })
        .map_err(|failure| {
            packet_budget.dispose();
            failure.reason()
        })?;
        let stored = match self.inner.lock().pending.get_mut(&request.request_id) {
            Some(pending) if pending.serial == serial => {
                pending.connection = Some(connection.clone());
                true
            }
            _ => false,
        };
        if !stored {
            connection.close(Failure::ConnectionSuperseded);
            return Err(PeerErrorReason::ConnectionSuperseded);
        }
        let deadline = budget
            .remaining(Instant::now())
            .min(Duration::from_millis(PEER_NATIVE_ANSWER_DEADLINE_MS));
        let answer_sdp = connection
            .answer(request.offer_sdp.clone(), deadline)
            .await
            .map_err(Failure::reason)?;
        if connection.is_closed() {
            return Err(PeerErrorReason::IceFailed);
        }
        self.assert_current(request, serial)?;
        let pending = self
            .inner
            .take_pending(&request.request_id, serial)
            .ok_or(PeerErrorReason::ConnectionSuperseded)?;
        let mut state = self.inner.lock();
        let active = ActivePeer {
            expected_tuple: pending.expected_tuple,
            connection,
            serial,
        };
        state.active.insert(request.peer_id.clone(), active);
        tracing::info!(peers = state.active.len(), "attachment peer established");
        Ok(WLocalAttachmentPeerAnswer {
            request_id: request.request_id.clone(),
            connection_generation: request.connection_generation.clone(),
            worker_epoch: self.inner.deps.process_epoch.clone(),
            peer_id: request.peer_id.clone(),
            answer_sdp,
            ..Default::default()
        })
    }

    /// The pending entry is still this offer's and its admission still holds.
    pub(super) fn assert_current(
        &self,
        request: &DLocalAttachmentPeerOffer,
        serial: u64,
    ) -> Result<AttachmentPeerExpectedTuple, PeerErrorReason> {
        let (budget, fence, expected_tuple) = {
            let state = self.inner.lock();
            let pending = state
                .pending
                .get(&request.request_id)
                .filter(|pending| pending.serial == serial);
            let pending = pending.ok_or(PeerErrorReason::ConnectionSuperseded)?;
            (
                pending.budget,
                pending.fence.clone(),
                pending.expected_tuple.clone(),
            )
        };
        match admission_failure(&self.inner, request, &budget, &fence) {
            Some(failure) => Err(failure),
            None => Ok(expected_tuple),
        }
    }
}

/// Only the active entry this connection became is retired by its close.
fn connection_closed(inner: &OwnerInner, peer_id: &str, serial: u64, reason: Failure) {
    let mut state = inner.lock();
    if state
        .active
        .get(peer_id)
        .is_none_or(|active| active.serial != serial)
    {
        return;
    }
    state.active.remove(peer_id);
    tracing::info!(
        peers = state.active.len(),
        reason = reason.as_str(),
        "attachment peer closed"
    );
}
