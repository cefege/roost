//! The attachment-peer owner's bounded table: which browser documents are
//! being admitted against which workers, which offers wait for an answer, and
//! the capacity rules both share. Held under `AttachmentPeerNegotiations`' one
//! lock, so a slot moves from admitted to pending without ever being counted
//! twice. Ports the admission and correlation maps of
//! `apps/coord/src/attachments/attachment-peer-negotiations.ts`.

use std::collections::HashMap;

use connectrpc::{ConnectError, ErrorCode};
use roost_proto::SessionsNegotiateAttachmentPeerRequest;
use tokio::sync::oneshot;

use crate::attachments::grant_state::{
    AttachmentGrantInvalidation, AttachmentGrantInvalidationKind,
};
use crate::attachments::peer_state::{
    PeerAdmission, PeerKey, PeerOutcome, PendingAttachmentPeer, attachment_peer_error as refusal,
};
use crate::attachments::transfer_limits::{
    ATTACHMENT_TRANSFER_PEER_MAX_NEGOTIATIONS_PER_WORKER,
    ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS,
    ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE,
};
use crate::coord_core::ids::{draw, render_v4};

/// Admissions by document and worker; pending offers by correlation id.
#[derive(Debug, Default)]
pub(crate) struct PeerTable {
    pub(crate) admitting: HashMap<PeerKey, PeerAdmission>,
    pub(crate) pending: HashMap<String, PendingAttachmentPeer>,
}

impl PeerTable {
    /// Claim the document's one negotiation slot against a worker, inside
    /// every process, device and worker bound.
    pub(crate) fn claim_admission(
        &mut self,
        key: &PeerKey,
        request: &SessionsNegotiateAttachmentPeerRequest,
        offer_digest: &str,
        device_fingerprint: &str,
    ) -> Result<(), ConnectError> {
        let existing = self
            .admitting
            .get(key)
            .map(|admission| (&admission.peer_id, &admission.offer_digest))
            .or_else(|| {
                self.pending
                    .values()
                    .find(|pending| pending.key == *key)
                    .map(|pending| (&pending.peer_id, &pending.offer_digest))
            });
        if let Some((peer_id, digest)) = existing {
            if *peer_id == request.peer_id && digest != offer_digest {
                return Err(refusal(
                    ErrorCode::InvalidArgument,
                    "attachment peer peer_id conflicts with an in-flight offer",
                ));
            }
            return Err(refusal(
                ErrorCode::AlreadyExists,
                "attachment peer negotiation is already pending for this document and worker",
            ));
        }
        if self.pending.len() + self.admitting.len()
            >= ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS
        {
            return Err(exhausted(
                "attachment peer negotiation capacity is exhausted",
            ));
        }
        if self.count(|device, _| device == device_fingerprint)
            >= ATTACHMENT_TRANSFER_PEER_MAX_PENDING_NEGOTIATIONS_PER_DEVICE
        {
            return Err(exhausted(
                "attachment peer device negotiation capacity is exhausted",
            ));
        }
        if self.count(|_, worker| worker == request.worker_fp)
            >= ATTACHMENT_TRANSFER_PEER_MAX_NEGOTIATIONS_PER_WORKER
        {
            return Err(exhausted(
                "attachment peer worker negotiation capacity is exhausted",
            ));
        }
        self.admitting.insert(
            key.clone(),
            PeerAdmission {
                peer_id: request.peer_id.clone(),
                offer_digest: offer_digest.to_owned(),
                device_fingerprint: device_fingerprint.to_owned(),
                worker_fp: request.worker_fp.clone(),
            },
        );
        Ok(())
    }

    /// Move the admission into a pending offer under a fresh correlation id.
    pub(crate) fn reserve(
        &mut self,
        pending_for: impl FnOnce(oneshot::Sender<PeerOutcome>) -> PendingAttachmentPeer,
    ) -> Result<(String, oneshot::Receiver<PeerOutcome>), ConnectError> {
        let request_id = (0..8)
            .filter_map(|_| draw::<16>().ok().map(render_v4))
            .find(|candidate| !self.pending.contains_key(candidate))
            .ok_or_else(|| exhausted("attachment peer request capacity is exhausted"))?;
        let (settle, settled) = oneshot::channel();
        let pending = pending_for(settle);
        self.admitting.remove(&pending.key);
        self.pending.insert(request_id.clone(), pending);
        Ok((request_id, settled))
    }

    /// The correlation ids of every pending offer `doomed` names.
    pub(crate) fn pending_where(
        &self,
        doomed: impl Fn(&PendingAttachmentPeer) -> bool,
    ) -> Vec<String> {
        self.pending
            .iter()
            .filter(|(_, pending)| doomed(pending))
            .map(|(request_id, _)| request_id.clone())
            .collect()
    }

    /// Admissions and pending offers whose (device, worker) `matches`.
    fn count(&self, matches: impl Fn(&str, &str) -> bool) -> usize {
        let admitted = self
            .admitting
            .values()
            .filter(|admission| matches(&admission.device_fingerprint, &admission.worker_fp));
        let pending = self
            .pending
            .values()
            .filter(|pending| matches(&pending.device_fingerprint, &pending.key.worker_fp));
        admitted.count() + pending.count()
    }
}

/// Whether a grant invalidation takes this pending offer's authority away:
/// its exact lease, its whole worker, or its revoked device.
pub(crate) fn invalidation_names(
    invalidation: &AttachmentGrantInvalidation,
    pending: &PendingAttachmentPeer,
) -> bool {
    let key = &pending.key;
    let lease_matches = invalidation.lease.as_ref().is_some_and(|lease| {
        lease.grant_id == pending.grant_id
            && lease.owner_key == key.owner_key
            && lease.tab_id == key.tab_id
            && lease.worker_fp == key.worker_fp
    });
    lease_matches
        || match invalidation.kind {
            AttachmentGrantInvalidationKind::WorkerRetired => {
                invalidation.worker_fp == key.worker_fp
            }
            AttachmentGrantInvalidationKind::DeviceRevoked => {
                invalidation.device_fingerprint.as_deref()
                    == Some(pending.device_fingerprint.as_str())
            }
            AttachmentGrantInvalidationKind::GrantExpired => false,
        }
}

fn exhausted(message: &str) -> ConnectError {
    refusal(ErrorCode::ResourceExhausted, message)
}
