//! The grant owner's table: live leases with their expiry timers, the grants
//! still minting, and the capacity rules both count against. Held under
//! `AttachmentGrantOwner`'s one lock; nothing here notifies or sends. Ports the
//! lease, pending and per-document/worker/device count maps of
//! `apps/coord/src/attachments/attachment-grant-owner.ts`.

use std::collections::HashMap;

use connectrpc::ConnectError;

use crate::attachments::grant_state::{
    AttachmentGrantLease, AttachmentGrantRequest, attachment_grant_exhausted,
};
use crate::attachments::transfer_limits::{
    ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_BROWSER_DOCUMENT, ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER,
    ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS, ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS_PER_DEVICE,
};

/// Live leases by grant id, and minting grants by grant id.
#[derive(Debug, Default)]
pub(crate) struct GrantTable {
    pub(crate) leases: HashMap<String, LeaseRecord>,
    pub(crate) pending: HashMap<String, PendingGrant>,
}

/// One live lease and the timer that expires it.
#[derive(Debug)]
pub(crate) struct LeaseRecord {
    pub(crate) lease: AttachmentGrantLease,
    pub(crate) expiry: tokio::task::AbortHandle,
}

/// One grant between admission and its lease; `invalidated` is set by a
/// device revocation or worker retirement that raced the mint.
#[derive(Debug)]
pub(crate) struct PendingGrant {
    pub(crate) owner_key: String,
    pub(crate) tab_id: String,
    pub(crate) device_fingerprint: String,
    pub(crate) worker_fp: String,
    pub(crate) invalidated: bool,
}

impl GrantTable {
    /// Refuse a mint that would exceed the document, worker, process or device
    /// bound, in v2's order.
    pub(crate) fn admit_capacity(
        &self,
        request: &AttachmentGrantRequest,
    ) -> Result<(), ConnectError> {
        let document = |owner: &str, tab: &str| owner == request.owner_key && tab == request.tab_id;
        let leases = self.leases.values().map(|record| &record.lease);
        let pending = self.pending.values();
        let document_count = leases
            .clone()
            .filter(|lease| document(&lease.owner_key, &lease.tab_id))
            .count()
            + pending
                .clone()
                .filter(|grant| document(&grant.owner_key, &grant.tab_id))
                .count();
        if document_count >= ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_BROWSER_DOCUMENT {
            return Err(attachment_grant_exhausted(
                "attachment grant document capacity is exhausted",
            ));
        }
        let worker_count = leases
            .filter(|lease| lease.worker_fp == request.worker_fp)
            .count()
            + pending
                .clone()
                .filter(|grant| grant.worker_fp == request.worker_fp)
                .count();
        if worker_count >= ATTACHMENT_TRANSFER_MAX_ACTIVE_PER_WORKER {
            return Err(attachment_grant_exhausted(
                "attachment grant worker capacity is exhausted",
            ));
        }
        if self.pending.len() >= ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS {
            return Err(attachment_grant_exhausted(
                "attachment grant capacity is exhausted",
            ));
        }
        let device = pending.filter(|grant| grant.device_fingerprint == request.device_fingerprint);
        if device.count() >= ATTACHMENT_TRANSFER_MAX_PENDING_GRANTS_PER_DEVICE {
            return Err(attachment_grant_exhausted(
                "attachment grant device capacity is exhausted",
            ));
        }
        Ok(())
    }

    /// Remove every lease `doomed` names, disarming its expiry, and hand the
    /// leases back for the caller to announce outside the lock.
    pub(crate) fn drop_where(
        &mut self,
        doomed: impl Fn(&AttachmentGrantLease) -> bool,
    ) -> Vec<AttachmentGrantLease> {
        let ids: Vec<String> = self
            .leases
            .values()
            .filter(|record| doomed(&record.lease))
            .map(|record| record.lease.grant_id.clone())
            .collect();
        ids.iter()
            .filter_map(|grant_id| self.leases.remove(grant_id))
            .map(|record| {
                record.expiry.abort();
                record.lease
            })
            .collect()
    }
}
