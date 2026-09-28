//! The coordinator's short-lived direct attachment grants and their
//! worker-acknowledged installation. A lease binds one immutable upload
//! descriptor to one device, tab, worker generation and epoch; it never shares
//! terminal-grant authority, and a worker only ever receives the secret's digest.
//! Built once on `AttachmentsRuntime`; minted by `rpc_direct`, read by `peer`,
//! retired by the worker delete and key revocation paths; its table is
//! `grant_table`. Ports `apps/coord/src/attachments/attachment-grant-owner.ts`.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use connectrpc::ConnectError;
use sha2::Digest;

use crate::attachments::grant_state::{
    AttachmentGrantInvalidation, AttachmentGrantInvalidationKind as Kind,
    AttachmentGrantInvalidationListener, AttachmentGrantLease, AttachmentGrantPort,
    AttachmentGrantRequest, AttachmentGrantResult, AttachmentGrantRetireReason,
    assert_attachment_grant_request, attachment_grant_unavailable, current_routable_by_name,
    is_exact_attachment_worker,
};
use crate::attachments::grant_table::{GrantTable, LeaseRecord, PendingGrant};
use crate::attachments::transfer_limits::{
    ATTACHMENT_TRANSFER_GRANT_ACK_DEADLINE_MS, ATTACHMENT_TRANSFER_GRANT_TTL_MS,
};
use crate::coord_core::ids::{draw, render_v4};
use crate::coord_core::worker_handle::WorkerRegistry;
use crate::serve::now_ms;
use crate::terminal_screen::pending_rpcs::PendingRpcs;
use crate::workers::attachment_send::{
    LocalAttachmentGrantInstall, send_local_attachment_grant_request,
    send_local_attachment_grant_revoke,
};

/// The one registry of direct attachment credentials.
pub struct AttachmentGrantOwner {
    workers: Arc<WorkerRegistry>,
    pending_rpcs: Arc<PendingRpcs>,
    table: Mutex<GrantTable>,
    listeners: Mutex<Vec<Arc<AttachmentGrantInvalidationListener>>>,
    this: Weak<Self>,
}

impl std::fmt::Debug for AttachmentGrantOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let table = self.table();
        formatter
            .debug_struct("AttachmentGrantOwner")
            .field("leases", &table.leases.len())
            .field("pending", &table.pending.len())
            .finish_non_exhaustive()
    }
}

/// Releases a minting grant's capacity however the mint ends (v2's `finally`).
struct PendingRelease<'a> {
    owner: &'a AttachmentGrantOwner,
    grant_id: String,
}

impl Drop for PendingRelease<'_> {
    fn drop(&mut self) {
        self.owner.table().pending.remove(&self.grant_id);
    }
}

impl AttachmentGrantOwner {
    /// An owner over the process's worker registry and the pending-request
    /// table a grant acknowledgement settles.
    #[must_use]
    pub fn new(workers: Arc<WorkerRegistry>, pending_rpcs: Arc<PendingRpcs>) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            workers,
            pending_rpcs,
            table: Mutex::new(GrantTable::default()),
            listeners: Mutex::new(Vec::new()),
            this: Weak::clone(this),
        })
    }

    /// Install one immutable credential; the plaintext secret is returned only
    /// after the exact worker generation acknowledged its digest, and only if
    /// `authorize` still passes afterwards.
    pub async fn grant<Authorize, Authorized>(
        &self,
        request: AttachmentGrantRequest,
        authorize: Authorize,
    ) -> Result<AttachmentGrantResult, ConnectError>
    where
        Authorize: Fn() -> Authorized,
        Authorized: Future<Output = Result<(), ConnectError>>,
    {
        assert_attachment_grant_request(&request)?;
        self.sweep(now_ms());
        self.table().admit_capacity(&request)?;
        let unavailable = || attachment_grant_unavailable("attachment grant worker is unavailable");
        let worker =
            current_routable_by_name(&self.workers, &request.worker_fp).ok_or_else(unavailable)?;
        let worker_epoch = worker.process_epoch.clone().ok_or_else(unavailable)?;
        let exact = || {
            is_exact_attachment_worker(&self.workers, &worker, &worker_epoch)
                .then_some(())
                .ok_or_else(unavailable)
        };
        exact()?;
        let grant_id = mint_grant_id()?;
        self.table().pending.insert(
            grant_id.clone(),
            PendingGrant {
                owner_key: request.owner_key.clone(),
                tab_id: request.tab_id.clone(),
                device_fingerprint: request.device_fingerprint.clone(),
                worker_fp: request.worker_fp.clone(),
                invalidated: false,
            },
        );
        let _release = PendingRelease {
            owner: self,
            grant_id: grant_id.clone(),
        };
        authorize().await?;
        self.require_pending_live(&grant_id)?;
        exact()?;
        let secret = hex::encode(draw::<32>().map_err(|error| entropy_failure(&error))?);
        let mut install = send_local_attachment_grant_request(
            &self.workers,
            &self.pending_rpcs,
            &worker,
            &worker_epoch,
            LocalAttachmentGrantInstall {
                grant_id: grant_id.clone(),
                secret_sha256: hex::encode(sha2::Sha256::digest(secret.as_bytes())),
                descriptor: request.descriptor.clone(),
                device_fingerprint: request.device_fingerprint.clone(),
                tab_id: request.tab_id.clone(),
                ttl_ms: ATTACHMENT_TRANSFER_GRANT_TTL_MS,
            },
            now_ms(),
        )?;
        crate::attachments::relay::settle_within(
            &mut install,
            ATTACHMENT_TRANSFER_GRANT_ACK_DEADLINE_MS,
        )
        .await?;
        self.require_pending_live(&grant_id)?;
        exact()?;
        authorize().await?;
        self.require_pending_live(&grant_id)?;
        exact()?;
        let lease = AttachmentGrantLease {
            grant_id,
            owner_key: request.owner_key,
            device_fingerprint: request.device_fingerprint,
            tab_id: request.tab_id,
            worker_fp: request.worker_fp,
            worker_epoch,
            descriptor: request.descriptor,
            expires_at_ms: now_ms() + i64::from(ATTACHMENT_TRANSFER_GRANT_TTL_MS),
            worker_handle: Arc::clone(&worker),
        };
        self.install_lease(lease.clone());
        tracing::info!(worker_fp = %lease.worker_fp, "attachment grant: grant_installed");
        Ok(AttachmentGrantResult { lease, secret })
    }

    /// Drop a device's leases and minting grants, and tell every current
    /// worker to forget the digests it holds for that device.
    pub fn revoke_device(&self, device_fingerprint: &str) {
        let dropped = {
            let mut table = self.table();
            for pending in table.pending.values_mut() {
                pending.invalidated |= pending.device_fingerprint == device_fingerprint;
            }
            table.drop_where(|lease| lease.device_fingerprint == device_fingerprint)
        };
        let count = dropped.len();
        self.notify_dropped(dropped, Kind::DeviceRevoked);
        let notified = self
            .workers
            .routable_fps()
            .iter()
            .filter_map(|worker_fp| self.workers.current_routable(worker_fp))
            .filter(|worker| {
                send_local_attachment_grant_revoke(&self.workers, worker, device_fingerprint)
            })
            .count();
        tracing::info!(
            leases_dropped = count,
            workers_notified = notified,
            "attachment grant: device_revoked"
        );
    }

    /// Drop every known grant of a deleted or revoked worker, including the
    /// ones still minting. Subscribers hear the retirement even when no lease
    /// existed, because a negotiation may be admitted against the worker.
    pub fn retire_worker(&self, worker_fp: &str, reason: AttachmentGrantRetireReason) {
        let dropped = {
            let mut table = self.table();
            for pending in table.pending.values_mut() {
                pending.invalidated |= pending.worker_fp == worker_fp;
            }
            table.drop_where(|lease| lease.worker_fp == worker_fp)
        };
        let count = dropped.len();
        if dropped.is_empty() {
            self.notify(&AttachmentGrantInvalidation {
                kind: Kind::WorkerRetired,
                lease: None,
                worker_fp: worker_fp.to_owned(),
                device_fingerprint: None,
            });
        }
        self.notify_dropped(dropped, Kind::WorkerRetired);
        tracing::info!(
            worker_fp,
            leases_dropped = count,
            reason = reason.as_str(),
            "attachment grant: worker_retired"
        );
    }

    /// Expire the coordinator's bookkeeping; workers enforce the TTL themselves.
    pub fn sweep(&self, now_ms: i64) {
        let expired = self
            .table()
            .drop_where(|lease| lease.expires_at_ms <= now_ms);
        for lease in &expired {
            tracing::info!(worker_fp = %lease.worker_fp, "attachment grant: grant_expired");
        }
        self.notify_dropped(expired, Kind::GrantExpired);
    }

    fn require_pending_live(&self, grant_id: &str) -> Result<(), ConnectError> {
        match self.table().pending.get(grant_id) {
            Some(pending) if !pending.invalidated => Ok(()),
            _ => Err(attachment_grant_unavailable(
                "attachment grant is unavailable",
            )),
        }
    }

    /// Record the lease and arm its expiry: a negotiation admitted against it
    /// hears the expiry when it happens, not when it next asks.
    fn install_lease(&self, lease: AttachmentGrantLease) {
        let owner = Weak::clone(&self.this);
        let delay_ms = (lease.expires_at_ms - now_ms() + 1).max(1);
        let expiry = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms.unsigned_abs())).await;
            if let Some(owner) = owner.upgrade() {
                owner.sweep(now_ms());
            }
        });
        let record = LeaseRecord {
            lease,
            expiry: expiry.abort_handle(),
        };
        self.table()
            .leases
            .insert(record.lease.grant_id.clone(), record);
    }

    fn notify_dropped(&self, dropped: Vec<AttachmentGrantLease>, kind: Kind) {
        for lease in dropped {
            self.notify(&AttachmentGrantInvalidation {
                kind,
                worker_fp: lease.worker_fp.clone(),
                device_fingerprint: Some(lease.device_fingerprint.clone()),
                lease: Some(lease),
            });
        }
    }

    /// Announce outside the table lock, so a subscriber may read the owner back.
    fn notify(&self, invalidation: &AttachmentGrantInvalidation) {
        let listeners = self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        for listener in listeners {
            listener(invalidation);
        }
    }

    fn table(&self) -> MutexGuard<'_, GrantTable> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl AttachmentGrantPort for AttachmentGrantOwner {
    fn owned_grant(
        &self,
        owner_key: &str,
        tab_id: &str,
        worker_fp: &str,
        grant_id: &str,
    ) -> Option<AttachmentGrantLease> {
        self.sweep(now_ms());
        let lease = self.table().leases.get(grant_id)?.lease.clone();
        let owned =
            lease.owner_key == owner_key && lease.tab_id == tab_id && lease.worker_fp == worker_fp;
        (owned
            && is_exact_attachment_worker(&self.workers, &lease.worker_handle, &lease.worker_epoch))
        .then_some(lease)
    }

    fn subscribe_invalidation(&self, listener: AttachmentGrantInvalidationListener) {
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::new(listener));
    }
}

fn mint_grant_id() -> Result<String, ConnectError> {
    draw::<16>()
        .map(render_v4)
        .map_err(|error| entropy_failure(&error))
}

fn entropy_failure(error: &std::io::Error) -> ConnectError {
    tracing::error!(%error, "attachment grant: no entropy for a grant identity or secret");
    ConnectError::new(
        connectrpc::ErrorCode::Internal,
        "attachment grant could not be minted",
    )
}
