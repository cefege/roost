//! The in-memory authority for exact direct attachment uploads. The
//! coordinator installs only a secret's digest; this store binds a hello's
//! device, tab, worker epoch and immutable upload descriptor independently.
//! Terminal grants never enter it. Ports v2
//! `apps/worker/src/attachments/attachment-grants.ts`. Built by
//! `runtime::owners`; installed and revoked by `attachments::link`, read by
//! hello admission and the attachment peer owner.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use roost_proto::DLocalAttachmentGrant;

use super::grant_checks::{valid_grant_frame, verify_stored};
use super::grant_listeners::GrantListeners;
pub use super::grant_listeners::{GrantListener, GrantSubscription};
use super::{AttachmentClock, system_clock};

const MAX_GRANTS: usize = 256;

/// One installed grant, as every reader sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentGrant {
    pub grant_id: String,
    pub session_id: String,
    pub upload_id: String,
    pub filename: String,
    pub short_path: bool,
    pub total_bytes: u64,
    pub device_fingerprint: String,
    pub tab_id: String,
    pub worker_epoch: String,
    pub expires_at: Instant,
}

/// What a hello presents against a grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentGrantCredential {
    pub grant_id: String,
    pub secret: String,
    pub session_id: String,
    pub upload_id: String,
    pub filename: String,
    pub short_path: bool,
    pub total_bytes: u64,
    pub device_fingerprint: String,
    pub tab_id: String,
    pub worker_epoch: String,
}

/// What a peer negotiation presents before any hello.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerGrantRequest {
    pub grant_id: String,
    pub device_fingerprint: String,
    pub tab_id: String,
    pub worker_epoch: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerGrantAuthorization {
    Authorized,
    GrantUnavailable,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantRemovalReason {
    Expired,
    Revoked,
    Cleared,
    Disposed,
}

/// A change every subscriber hears, after the store has applied it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantChange {
    Installed {
        grant: AttachmentGrant,
        previous: Option<AttachmentGrant>,
    },
    Removed {
        grant: AttachmentGrant,
        reason: GrantRemovalReason,
    },
}

pub(super) struct StoredGrant {
    pub(super) grant: AttachmentGrant,
    pub(super) secret_sha256: String,
}

#[derive(Default)]
struct GrantState {
    grants: HashMap<String, StoredGrant>,
    disposed: bool,
}

/// The process-owned grant registry. Expiry is applied lazily — when an
/// install, lookup, verification or peer authorization next touches a grant —
/// and each lapsed grant is announced once as `Removed { Expired }`.
pub struct AttachmentGrantStore {
    worker_epoch: String,
    clock: AttachmentClock,
    state: Mutex<GrantState>,
    listeners: GrantListeners,
}

impl std::fmt::Debug for AttachmentGrantStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentGrantStore")
            .field("worker_epoch", &self.worker_epoch)
            .field("grants", &lock(&self.state).grants.len())
            .finish_non_exhaustive()
    }
}

impl AttachmentGrantStore {
    pub fn new(worker_epoch: impl Into<String>, clock: AttachmentClock) -> Self {
        Self {
            worker_epoch: worker_epoch.into(),
            clock,
            state: Mutex::new(GrantState::default()),
            listeners: GrantListeners::default(),
        }
    }

    /// The production store, on the process's monotonic clock.
    pub fn system(worker_epoch: impl Into<String>) -> Self {
        Self::new(worker_epoch, system_clock())
    }

    /// Listeners run with no store lock held, so one may call back in.
    pub fn subscribe(&self, listener: GrantListener) -> GrantSubscription {
        self.listeners.subscribe(listener)
    }

    /// v2 `install`: validate, renew or add, and announce. The error is the
    /// message the coordinator's `rpc-error` carries.
    pub fn install(&self, frame: &DLocalAttachmentGrant) -> Result<AttachmentGrant, String> {
        let now = (self.clock)();
        let mut changes = Vec::new();
        let installed = {
            let mut state = lock(&self.state);
            self.install_locked(&mut state, frame, now, &mut changes)
        };
        self.notify(&changes);
        installed
    }

    pub fn current(&self, grant_id: &str) -> Option<AttachmentGrant> {
        let mut changes = Vec::new();
        let current = {
            let mut state = lock(&self.state);
            self.current_locked(&mut state, grant_id, &mut changes)
                .map(|stored| stored.grant.clone())
        };
        self.notify(&changes);
        current
    }

    /// v2 `verify`: the grant a hello's credential proves, or why not.
    pub fn verify(
        &self,
        credential: &AttachmentGrantCredential,
    ) -> Result<AttachmentGrant, &'static str> {
        let mut changes = Vec::new();
        let verdict = {
            let mut state = lock(&self.state);
            match self.current_locked(&mut state, &credential.grant_id, &mut changes) {
                None => Err("attachment grant is unavailable"),
                Some(stored) => verify_stored(stored, credential),
            }
        };
        self.notify(&changes);
        verdict
    }

    /// v2 `authorizePeer`: whether a peer negotiation may proceed under a grant.
    pub fn authorize_peer(&self, request: &PeerGrantRequest) -> PeerGrantAuthorization {
        let now = (self.clock)();
        let mut changes = Vec::new();
        let authorization = {
            let mut state = lock(&self.state);
            match state.grants.get(&request.grant_id) {
                None => PeerGrantAuthorization::GrantUnavailable,
                Some(stored) if stored.grant.expires_at <= now => {
                    remove(
                        &mut state,
                        &request.grant_id,
                        GrantRemovalReason::Expired,
                        &mut changes,
                    );
                    PeerGrantAuthorization::Expired
                }
                Some(stored) => {
                    let grant = &stored.grant;
                    let same_epoch = request.worker_epoch == self.worker_epoch
                        && grant.worker_epoch == self.worker_epoch;
                    let same_document = grant.device_fingerprint == request.device_fingerprint
                        && grant.tab_id == request.tab_id;
                    if same_epoch && same_document {
                        PeerGrantAuthorization::Authorized
                    } else {
                        PeerGrantAuthorization::GrantUnavailable
                    }
                }
            }
        };
        self.notify(&changes);
        authorization
    }

    /// Every grant this device holds, withdrawn at once.
    pub fn revoke_device(&self, device_fingerprint: &str) -> usize {
        let mut changes = Vec::new();
        {
            let mut state = lock(&self.state);
            let revoked: Vec<String> = state
                .grants
                .values()
                .filter(|stored| stored.grant.device_fingerprint == device_fingerprint)
                .map(|stored| stored.grant.grant_id.clone())
                .collect();
            for grant_id in revoked {
                remove(
                    &mut state,
                    &grant_id,
                    GrantRemovalReason::Revoked,
                    &mut changes,
                );
            }
        }
        if !changes.is_empty() {
            tracing::info!(grants = changes.len(), "attachment grants revoked");
        }
        self.notify(&changes);
        changes.len()
    }

    /// Withdraw every grant, announce it, then forget every listener.
    pub fn dispose(&self) {
        let mut changes = Vec::new();
        {
            let mut state = lock(&self.state);
            if state.disposed {
                return;
            }
            state.disposed = true;
            let grant_ids: Vec<String> = state.grants.keys().cloned().collect();
            for grant_id in grant_ids {
                remove(
                    &mut state,
                    &grant_id,
                    GrantRemovalReason::Disposed,
                    &mut changes,
                );
            }
        }
        self.notify(&changes);
        self.listeners.clear();
    }

    fn install_locked(
        &self,
        state: &mut GrantState,
        frame: &DLocalAttachmentGrant,
        now: Instant,
        changes: &mut Vec<GrantChange>,
    ) -> Result<AttachmentGrant, String> {
        if state.disposed {
            return Err("attachment grant store is disposed".to_owned());
        }
        if !valid_grant_frame(frame, &self.worker_epoch) {
            return Err("attachment grant is invalid".to_owned());
        }
        sweep_expired(state, now, changes);
        let previous = state
            .grants
            .remove(&frame.grant_id)
            .map(|stored| stored.grant);
        if previous.is_none() && state.grants.len() >= MAX_GRANTS {
            return Err("attachment grant capacity is full".to_owned());
        }
        let grant = AttachmentGrant {
            grant_id: frame.grant_id.clone(),
            session_id: frame.session_id.clone(),
            upload_id: frame.upload_id.clone(),
            filename: frame.filename.clone(),
            short_path: frame.short_path,
            total_bytes: frame.total_bytes,
            device_fingerprint: frame.device_fingerprint.clone(),
            tab_id: frame.tab_id.clone(),
            worker_epoch: frame.worker_epoch.clone(),
            expires_at: now + Duration::from_millis(u64::from(frame.ttl_ms)),
        };
        let stored = StoredGrant {
            grant: grant.clone(),
            secret_sha256: frame.secret_sha256.clone(),
        };
        state.grants.insert(frame.grant_id.clone(), stored);
        tracing::info!(
            ttl_ms = frame.ttl_ms,
            renewed = previous.is_some(),
            "attachment grant installed"
        );
        changes.push(GrantChange::Installed {
            grant: grant.clone(),
            previous,
        });
        Ok(grant)
    }

    fn current_locked<'state>(
        &self,
        state: &'state mut GrantState,
        grant_id: &str,
        changes: &mut Vec<GrantChange>,
    ) -> Option<&'state StoredGrant> {
        let now = (self.clock)();
        let expired = state.grants.get(grant_id)?.grant.expires_at <= now;
        if expired {
            remove(state, grant_id, GrantRemovalReason::Expired, changes);
            return None;
        }
        state.grants.get(grant_id)
    }

    fn notify(&self, changes: &[GrantChange]) {
        self.listeners.notify(changes);
    }
}

fn remove(
    state: &mut GrantState,
    grant_id: &str,
    reason: GrantRemovalReason,
    changes: &mut Vec<GrantChange>,
) {
    if let Some(stored) = state.grants.remove(grant_id) {
        tracing::debug!(?reason, "an attachment grant was removed");
        changes.push(GrantChange::Removed {
            grant: stored.grant,
            reason,
        });
    }
}

fn sweep_expired(state: &mut GrantState, now: Instant, changes: &mut Vec<GrantChange>) {
    let expired: Vec<String> = state
        .grants
        .values()
        .filter(|stored| stored.grant.expires_at <= now)
        .map(|stored| stored.grant.grant_id.clone())
        .collect();
    for grant_id in expired {
        remove(state, &grant_id, GrantRemovalReason::Expired, changes);
    }
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
