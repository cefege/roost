//! The one composition-owned registry of direct-terminal grant leases: exact
//! live-lease lookup for signaling, device revocation, worker retirement,
//! expiry, and the invalidation fan-out peer negotiation subscribes to.
//! Built once on `terminal_direct::TerminalDirectRuntime`; minting a lease is
//! `grant_refresh`. Ports `apps/coord/src/terminal/direct/terminal-grant-owner.ts`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use connectrpc::ConnectError;
use roost_protocol::wire::WorkerFp;

use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::terminal_direct::grant_state::{
    InvalidationListener, LeaseKey, PendingGrantRefresh, TerminalDirectRetireReason,
    TerminalGrantInvalidation, TerminalGrantInvalidationKind, TerminalGrantLeaseSnapshot,
    lease_key, owner_disposed,
};
use crate::terminal_screen::pending_rpcs::PendingRpcs;
use crate::workers::local_terminal_send::{
    send_local_terminal_grant_revoke, send_terminal_direct_retire,
};

/// Every direct-terminal lease this coordinator has committed.
pub struct TerminalGrantOwner {
    pub(super) workers: Arc<WorkerRegistry>,
    pub(super) pending_rpcs: Arc<PendingRpcs>,
    state: Mutex<GrantState>,
    pub(super) this: Weak<Self>,
}

impl std::fmt::Debug for TerminalGrantOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state();
        formatter
            .debug_struct("TerminalGrantOwner")
            .field("leases", &state.leases.len())
            .field("refreshes", &state.refreshes.len())
            .finish_non_exhaustive()
    }
}

/// The owner's mutable half, behind one lock.
#[derive(Default)]
pub(super) struct GrantState {
    pub(super) leases: BTreeMap<LeaseKey, TerminalGrantLease>,
    pub(super) refreshes: HashMap<LeaseKey, PendingGrantRefresh>,
    listeners: Vec<(u64, InvalidationListener)>,
    next_listener_id: u64,
    pub(super) next_refresh_id: u64,
    pub(super) disposed: bool,
}

/// One committed lease and the timer that forgets it at expiry.
pub(super) struct TerminalGrantLease {
    pub(super) snapshot: TerminalGrantLeaseSnapshot,
    expiry: Option<tokio::task::AbortHandle>,
}

impl Drop for TerminalGrantLease {
    /// Replacing or dropping a lease stops its expiry timer with it.
    fn drop(&mut self) {
        if let Some(expiry) = self.expiry.take() {
            expiry.abort();
        }
    }
}

impl TerminalGrantOwner {
    /// An owner over the process's worker registry and pending-request table.
    #[must_use]
    pub fn new(workers: Arc<WorkerRegistry>, pending_rpcs: Arc<PendingRpcs>) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            workers,
            pending_rpcs,
            state: Mutex::new(GrantState::default()),
            this: this.clone(),
        })
    }

    /// A still-owned lease on the exact worker generation that installed it,
    /// rebound to a same-epoch reconnect.
    #[must_use]
    pub fn owned_grant(
        &self,
        owner_key: &str,
        tab_id: &str,
        worker_fp: &str,
        grant_id: &str,
    ) -> Option<TerminalGrantLeaseSnapshot> {
        self.sweep(crate::rpc::service::now_ms());
        let worker = current_routable(&self.workers, worker_fp);
        let mut state = self.state();
        let lease = state
            .leases
            .get_mut(&lease_key(owner_key, tab_id, worker_fp))?;
        let worker = worker?;
        if lease.snapshot.grant_id != grant_id
            || worker.process_epoch != lease.snapshot.worker_epoch
        {
            return None;
        }
        if !Arc::ptr_eq(&lease.snapshot.worker_handle, &worker) {
            lease.snapshot.worker_handle = worker;
            tracing::info!(worker_fp, "terminal grant owner: grant_rebound");
        }
        Some(lease.snapshot.clone())
    }

    /// Drop every lease a device holds and tell every routable worker.
    pub fn revoke_device(&self, device_fingerprint: &str) -> Result<usize, ConnectError> {
        let invalidations = {
            let mut state = self.open_state()?;
            for refresh in state.refreshes.values_mut() {
                if refresh.device_fingerprint == device_fingerprint {
                    refresh.invalidated = true;
                }
            }
            drop_leases(
                &mut state,
                TerminalGrantInvalidationKind::DeviceRevoked,
                None,
                |lease| lease.device_fingerprint == device_fingerprint,
            )
        };
        let dropped = invalidations.len();
        self.notify(&invalidations);
        let notified = self
            .workers
            .routable_fps()
            .iter()
            .filter_map(|worker_fp| self.workers.current_routable(worker_fp))
            .filter(|worker| {
                send_local_terminal_grant_revoke(&self.workers, worker, device_fingerprint)
            })
            .count();
        tracing::info!(
            leases_dropped = dropped,
            workers_notified = notified,
            "terminal grant owner: device_revoked"
        );
        Ok(dropped)
    }

    /// Send retirement while the worker's current handle still admits it, then
    /// invalidate every lease on that worker.
    pub fn retire_worker(
        &self,
        worker_fp: &str,
        reason: TerminalDirectRetireReason,
    ) -> Result<(), ConnectError> {
        drop(self.open_state()?);
        let worker = current_routable(&self.workers, worker_fp);
        let worker_epoch = worker
            .as_ref()
            .and_then(|worker| worker.process_epoch.clone());
        if let (Some(worker), Some(epoch)) = (&worker, &worker_epoch) {
            send_terminal_direct_retire(&self.workers, worker, epoch, reason);
        }
        let mut invalidations = {
            let mut state = self.open_state()?;
            for refresh in state.refreshes.values_mut() {
                if refresh.worker_fp == worker_fp {
                    refresh.invalidated = true;
                }
            }
            let kind = TerminalGrantInvalidationKind::WorkerRetired;
            drop_leases(&mut state, kind, Some(reason), |lease| {
                lease.worker_fp == worker_fp
            })
        };
        let dropped = invalidations.len();
        if dropped == 0 {
            invalidations.push(TerminalGrantInvalidation {
                kind: TerminalGrantInvalidationKind::WorkerRetired,
                lease: None,
                worker_fp: worker_fp.to_owned(),
                worker_epoch,
                device_fingerprint: None,
                removed_session_ids: Vec::new(),
                reason: Some(reason),
            });
        }
        self.notify(&invalidations);
        tracing::info!(
            worker_fp,
            leases_dropped = dropped,
            reason = reason.as_str(),
            "terminal grant owner: worker_retired"
        );
        Ok(())
    }

    /// Hear every later invalidation; the id unsubscribes.
    pub fn subscribe_invalidation(
        &self,
        listener: InvalidationListener,
    ) -> Result<u64, ConnectError> {
        let mut state = self.open_state()?;
        state.next_listener_id += 1;
        let id = state.next_listener_id;
        state.listeners.push((id, listener));
        Ok(id)
    }

    /// Stop one subscriber hearing invalidations.
    pub fn unsubscribe_invalidation(&self, id: u64) {
        self.state()
            .listeners
            .retain(|(listener_id, _)| *listener_id != id);
    }

    /// Every live lease, for diagnostics and tests; never a secret or digest.
    #[must_use]
    pub fn list(&self) -> Vec<TerminalGrantLeaseSnapshot> {
        self.sweep(crate::rpc::service::now_ms());
        self.state()
            .leases
            .values()
            .map(|lease| lease.snapshot.clone())
            .collect()
    }

    /// Forget every lease whose coordinator lifetime ended by `now_ms`; workers
    /// enforce their own copy of the TTL independently.
    pub fn sweep(&self, now_ms: i64) {
        let expired = {
            let mut state = self.state();
            let kind = TerminalGrantInvalidationKind::GrantExpired;
            drop_leases(&mut state, kind, None, |lease| {
                lease.expires_at_ms <= now_ms
            })
        };
        for invalidation in &expired {
            tracing::info!(worker_fp = %invalidation.worker_fp, "terminal grant owner: grant_expired");
        }
        self.notify(&expired);
    }

    /// Stop every timer and make each in-flight refresh fail its next check.
    pub fn dispose(&self) {
        let invalidations = {
            let mut state = self.state();
            if state.disposed {
                return;
            }
            state.disposed = true;
            for refresh in state.refreshes.values_mut() {
                refresh.invalidated = true;
            }
            drop_leases(
                &mut state,
                TerminalGrantInvalidationKind::Disposed,
                None,
                |_| true,
            )
        };
        self.notify(&invalidations);
        self.state().listeners.clear();
    }

    /// The lock, recovered from poisoning: every critical section is a map edit.
    pub(super) fn state(&self) -> MutexGuard<'_, GrantState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The lock, refused once the owner is disposed.
    pub(super) fn open_state(&self) -> Result<MutexGuard<'_, GrantState>, ConnectError> {
        let state = self.state();
        if state.disposed {
            return Err(owner_disposed());
        }
        Ok(state)
    }

    /// Commit a lease under its tuple, arming the expiry sweep.
    pub(super) fn install_lease(
        &self,
        state: &mut GrantState,
        snapshot: TerminalGrantLeaseSnapshot,
        now_ms: i64,
    ) {
        let delay_ms = (snapshot.expires_at_ms - now_ms + 1).max(1);
        let owner = self.this.clone();
        let expiry = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms.unsigned_abs())).await;
            if let Some(owner) = owner.upgrade() {
                owner.sweep(crate::rpc::service::now_ms());
            }
        });
        let key = lease_key(&snapshot.owner_key, &snapshot.tab_id, &snapshot.worker_fp);
        let lease = TerminalGrantLease {
            snapshot,
            expiry: Some(expiry.abort_handle()),
        };
        state.leases.insert(key, lease);
    }

    /// Tell every subscriber, outside the lock, one invalidation at a time.
    pub(super) fn notify(&self, invalidations: &[TerminalGrantInvalidation]) {
        if invalidations.is_empty() {
            return;
        }
        let listeners: Vec<InvalidationListener> = self
            .state()
            .listeners
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for invalidation in invalidations {
            for listener in &listeners {
                let delivered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    listener(invalidation);
                }));
                if delivered.is_err() {
                    tracing::warn!(
                        kind = invalidation.kind.as_str(),
                        "terminal grant owner: invalidation_listener_failed"
                    );
                }
            }
        }
    }
}

/// The fingerprint's routable generation, for a browser-supplied fingerprint
/// that may not even be well-formed (which no generation can answer to).
pub(super) fn current_routable(
    workers: &WorkerRegistry,
    worker_fp: &str,
) -> Option<Arc<WorkerHandle>> {
    let worker_fp = WorkerFp::try_from(worker_fp).ok()?;
    workers.current_routable(&worker_fp)
}

/// Remove every lease `doomed` selects and describe each removal.
fn drop_leases(
    state: &mut GrantState,
    kind: TerminalGrantInvalidationKind,
    reason: Option<TerminalDirectRetireReason>,
    doomed: impl Fn(&TerminalGrantLeaseSnapshot) -> bool,
) -> Vec<TerminalGrantInvalidation> {
    let keys: Vec<LeaseKey> = state
        .leases
        .iter()
        .filter(|(_, lease)| doomed(&lease.snapshot))
        .map(|(key, _)| key.clone())
        .collect();
    keys.iter()
        .filter_map(|key| state.leases.remove(key))
        .map(|lease| {
            let removed = lease.snapshot.session_ids.clone();
            TerminalGrantInvalidation::of_lease(kind, &lease.snapshot, removed, reason)
        })
        .collect()
}
