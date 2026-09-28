//! The worker's one in-memory registry of browser direct-terminal grants. The
//! coordinator installs only SHA-256 digests; callers read the current public
//! scope and never a digest or a secret. Expiry is active, so a connected
//! carrier cannot outlive its authorization while the coordinator is away.
//! Called by `super::door` (install/revoke from the link), `super::sockets`
//! and the peer owner. Ports `apps/worker/src/local-door/local-terminal-grants.ts`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use roost_proto::DLocalTerminalGrant;
use tokio::runtime::Handle;
use tokio::task::AbortHandle;
use tokio::time::Instant;

use super::grant_scope::{
    GrantChange, GrantCredential, GrantRemovalReason, LocalTerminalGrant, MAX_GRANTS, MAX_TTL_MS,
    PeerGrantAuthorization, capability_matches, is_sha256_hex, sha256_hex, valid_id,
    valid_session_ids,
};

/// A subscriber. Called with no store lock held, possibly from inside another
/// owner's lock (a lazy expiry during a predicate), so it only fences and
/// defers; it never calls back into that owner synchronously.
pub type GrantListener = Arc<dyn Fn(&GrantChange) + Send + Sync>;

struct StoredGrant {
    public: Arc<LocalTerminalGrant>,
    secret_sha256: String,
    /// Which install this is, so a stale expiry timer cannot remove a renewal.
    install: u64,
}

#[derive(Default)]
struct GrantState {
    grants: HashMap<String, StoredGrant>,
    expiry_timers: HashMap<String, AbortHandle>,
    listeners: Vec<(u64, GrantListener)>,
    expired_grant_ids: VecDeque<String>,
    next_id: u64,
    disposed: bool,
}

struct GrantShared {
    /// This worker's process epoch; empty means the store fences no epoch.
    worker_epoch: String,
    runtime: Handle,
    state: Mutex<GrantState>,
}

/// v2 `LocalTerminalGrantStore`. Cheap to clone; clones share one registry.
#[derive(Clone)]
pub struct LocalTerminalGrantStore {
    shared: Arc<GrantShared>,
}

impl std::fmt::Debug for LocalTerminalGrantStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = formatter.debug_struct("LocalTerminalGrantStore");
        debug.field("worker_epoch", &self.shared.worker_epoch);
        if let Ok(state) = self.shared.state.try_lock() {
            debug.field("grants", &state.grants.len());
        }
        debug.finish_non_exhaustive()
    }
}

impl LocalTerminalGrantStore {
    /// A store fenced to `worker_epoch`; expiry timers run on `runtime`.
    pub fn new(worker_epoch: impl Into<String>, runtime: Handle) -> Self {
        let shared = GrantShared { worker_epoch: worker_epoch.into(), runtime, state: Mutex::default() };
        Self { shared: Arc::new(shared) }
    }

    /// Register a subscriber; the id is what `unsubscribe` takes.
    pub fn subscribe(&self, listener: GrantListener) -> u64 {
        let mut state = self.lock();
        state.next_id += 1;
        let id = state.next_id;
        state.listeners.push((id, listener));
        id
    }

    pub fn unsubscribe(&self, subscription: u64) {
        self.lock().listeners.retain(|(id, _)| *id != subscription);
    }

    /// Install or renew one coordinator-authorized scope. The error is the
    /// message the coordinator's acknowledged install is answered with.
    pub fn install(&self, frame: &DLocalTerminalGrant) -> Result<Arc<LocalTerminalGrant>, String> {
        let mut changes = Vec::new();
        let installed = self.install_locked(frame, &mut changes);
        self.notify(&changes);
        let grant = installed?;
        tracing::info!(
            grant_id = %grant.grant_id,
            device_fingerprint = %grant.device_fingerprint,
            tab_id = %grant.tab_id,
            sessions = grant.session_ids.len(),
            ttl_ms = frame.ttl_ms,
            renewed = matches!(changes.last(), Some(GrantChange::Renewed { .. })),
            "local terminal grant installed"
        );
        Ok(grant)
    }

    /// The current public scope. A live predicate calls this at its final
    /// keeper boundary rather than retaining an old allow-list.
    pub fn current(&self, grant_id: &str) -> Option<Arc<LocalTerminalGrant>> {
        let mut changes = Vec::new();
        let current = current_locked(&mut self.lock(), grant_id, &mut changes);
        self.notify(&changes);
        current
    }

    /// Check a Hello's credential; the error is the close reason.
    pub fn verify(&self, credential: GrantCredential<'_>) -> Result<Arc<LocalTerminalGrant>, &'static str> {
        let mut changes = Vec::new();
        let verdict = {
            let mut state = self.lock();
            match current_locked(&mut state, credential.grant_id, &mut changes) {
                None if state.expired_grant_ids.iter().any(|id| id == credential.grant_id) => {
                    Err("local terminal grant expired")
                }
                None => Err("unknown local terminal grant"),
                Some(grant) if grant.device_fingerprint != credential.device_fingerprint => {
                    Err("grant is bound to another device")
                }
                Some(grant) if grant.tab_id != credential.tab_id => Err("grant is bound to another tab"),
                Some(grant) => {
                    let installed = state.grants.get(credential.grant_id).map_or("", |stored| stored.secret_sha256.as_str());
                    if capability_matches(installed, &sha256_hex(credential.secret)) {
                        Ok(grant)
                    } else {
                        Err("local terminal grant secret mismatch")
                    }
                }
            }
        };
        self.notify(&changes);
        verdict
    }

    /// A peer offer carries no secret: its tuple is checked against the live
    /// scope and an exact worker epoch.
    pub fn authorize_peer(&self, grant_id: &str, device_fingerprint: &str, tab_id: &str, worker_epoch: &str) -> PeerGrantAuthorization {
        let mut changes = Vec::new();
        let verdict = {
            let mut state = self.lock();
            let epoch = self.shared.worker_epoch.as_str();
            match state.grants.get(grant_id) {
                None if state.expired_grant_ids.iter().any(|id| id == grant_id) => PeerGrantAuthorization::Expired,
                None => PeerGrantAuthorization::GrantUnavailable,
                Some(stored) if stored.public.expires_at <= Instant::now() => {
                    remove_locked(&mut state, grant_id, GrantRemovalReason::Expired, &mut changes);
                    PeerGrantAuthorization::Expired
                }
                Some(_) if epoch.is_empty() || worker_epoch != epoch => PeerGrantAuthorization::GrantUnavailable,
                Some(stored) if stored.public.worker_epoch != epoch => PeerGrantAuthorization::GrantUnavailable,
                Some(stored) if stored.public.device_fingerprint == device_fingerprint && stored.public.tab_id == tab_id => {
                    PeerGrantAuthorization::Authorized
                }
                Some(_) => PeerGrantAuthorization::GrantUnavailable,
            }
        };
        self.notify(&changes);
        verdict
    }

    /// Remove every grant bound to a device.
    pub fn revoke_device(&self, device_fingerprint: &str) {
        let mut changes = Vec::new();
        {
            let mut state = self.lock();
            let revoked: Vec<String> = state
                .grants
                .values()
                .filter(|stored| stored.public.device_fingerprint == device_fingerprint)
                .map(|stored| stored.public.grant_id.clone())
                .collect();
            for grant_id in &revoked {
                remove_locked(&mut state, grant_id, GrantRemovalReason::Revoked, &mut changes);
            }
        }
        if !changes.is_empty() {
            tracing::info!(device_fingerprint, grants = changes.len(), "local terminal grants revoked");
        }
        self.notify(&changes);
    }

    /// Remove one grant, telling subscribers why.
    pub fn remove(&self, grant_id: &str, reason: GrantRemovalReason) {
        let mut changes = Vec::new();
        remove_locked(&mut self.lock(), grant_id, reason, &mut changes);
        self.notify(&changes);
    }

    /// Refuse every later install, remove every grant, stop every timer and
    /// forget every subscriber.
    pub fn dispose(&self) {
        let mut changes = Vec::new();
        let listeners = {
            let mut state = self.lock();
            if state.disposed {
                return;
            }
            state.disposed = true;
            let grant_ids: Vec<String> = state.grants.keys().cloned().collect();
            for grant_id in &grant_ids {
                remove_locked(&mut state, grant_id, GrantRemovalReason::Disposed, &mut changes);
            }
            for (_, timer) in state.expiry_timers.drain() {
                timer.abort();
            }
            std::mem::take(&mut state.listeners)
        };
        for change in &changes {
            for (_, listener) in &listeners {
                listener(change);
            }
        }
        tracing::info!("the local terminal grant store was disposed");
    }

    fn install_locked(&self, frame: &DLocalTerminalGrant, changes: &mut Vec<GrantChange>) -> Result<Arc<LocalTerminalGrant>, String> {
        let mut state = self.lock();
        if state.disposed {
            return Err("local terminal grant store is disposed".to_owned());
        }
        let session_ids = valid_session_ids(&frame.session_ids);
        if !valid_id(&frame.grant_id) {
            return Err("grant_id is invalid".to_owned());
        }
        if !is_sha256_hex(&frame.secret_sha256) {
            return Err("secret_sha256 must be a lowercase hex SHA-256 digest".to_owned());
        }
        let Some(session_ids) = session_ids else {
            return Err("session_ids is invalid".to_owned());
        };
        if !valid_id(&frame.device_fingerprint) {
            return Err("device_fingerprint is invalid".to_owned());
        }
        if !valid_id(&frame.tab_id) {
            return Err("tab_id is invalid".to_owned());
        }
        if !frame.worker_epoch.is_empty() && !valid_id(&frame.worker_epoch) {
            return Err("worker_epoch is invalid".to_owned());
        }
        let epoch = self.shared.worker_epoch.as_str();
        if !epoch.is_empty() && !frame.worker_epoch.is_empty() && frame.worker_epoch != epoch {
            return Err("worker_epoch does not match this worker".to_owned());
        }
        if frame.ttl_ms == 0 || frame.ttl_ms > MAX_TTL_MS {
            return Err(format!("ttl_ms must be within 1..{MAX_TTL_MS}"));
        }
        sweep_locked(&mut state, changes);
        let prior = state.grants.get(&frame.grant_id).map(|stored| Arc::clone(&stored.public));
        if prior.is_none() && state.grants.len() >= MAX_GRANTS {
            return Err("local terminal grant capacity is full".to_owned());
        }
        let public = Arc::new(LocalTerminalGrant {
            grant_id: frame.grant_id.clone(),
            session_ids,
            device_fingerprint: frame.device_fingerprint.clone(),
            tab_id: frame.tab_id.clone(),
            worker_epoch: frame.worker_epoch.clone(),
            expires_at: Instant::now() + Duration::from_millis(u64::from(frame.ttl_ms)),
        });
        if let Some(timer) = state.expiry_timers.remove(&frame.grant_id) {
            timer.abort();
        }
        state.next_id += 1;
        let install = state.next_id;
        let stored = StoredGrant { public: Arc::clone(&public), secret_sha256: frame.secret_sha256.clone(), install };
        state.grants.insert(frame.grant_id.clone(), stored);
        state.expired_grant_ids.retain(|id| *id != frame.grant_id);
        let timer = self.arm_expiry(frame.grant_id.clone(), install, public.expires_at);
        state.expiry_timers.insert(frame.grant_id.clone(), timer);
        changes.push(match prior {
            Some(previous) => GrantChange::Renewed {
                grant: Arc::clone(&public),
                removed_session_ids: previous.session_ids.iter().filter(|id| !public.session_ids.contains(id)).cloned().collect(),
            },
            None => GrantChange::Installed { grant: Arc::clone(&public) },
        });
        Ok(public)
    }

    /// Active expiry: a task that removes this install when it lapses, and
    /// re-arms when the grant was renewed further out under the same install.
    fn arm_expiry(&self, grant_id: String, install: u64, expires_at: Instant) -> AbortHandle {
        let weak: Weak<GrantShared> = Arc::downgrade(&self.shared);
        let task = self.shared.runtime.spawn(async move {
            tokio::time::sleep_until(expires_at).await;
            let Some(shared) = weak.upgrade() else {
                return;
            };
            LocalTerminalGrantStore { shared }.expire_install(&grant_id, install);
        });
        task.abort_handle()
    }

    fn expire_install(&self, grant_id: &str, install: u64) {
        let mut changes = Vec::new();
        {
            let mut state = self.lock();
            let current = state.grants.get(grant_id).is_some_and(|stored| stored.install == install);
            if !current {
                return;
            }
            state.expiry_timers.remove(grant_id);
            remove_locked(&mut state, grant_id, GrantRemovalReason::Expired, &mut changes);
        }
        self.notify(&changes);
    }

    fn notify(&self, changes: &[GrantChange]) {
        if changes.is_empty() {
            return;
        }
        let listeners: Vec<GrantListener> = self.lock().listeners.iter().map(|(_, listener)| Arc::clone(listener)).collect();
        for change in changes {
            for listener in &listeners {
                listener(change);
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, GrantState> {
        self.shared.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The live scope, removed first when it has expired.
fn current_locked(state: &mut GrantState, grant_id: &str, changes: &mut Vec<GrantChange>) -> Option<Arc<LocalTerminalGrant>> {
    let stored = state.grants.get(grant_id)?;
    if stored.public.expires_at > Instant::now() {
        return Some(Arc::clone(&stored.public));
    }
    remove_locked(state, grant_id, GrantRemovalReason::Expired, changes);
    None
}

fn remove_locked(state: &mut GrantState, grant_id: &str, reason: GrantRemovalReason, changes: &mut Vec<GrantChange>) {
    let Some(stored) = state.grants.remove(grant_id) else {
        return;
    };
    if let Some(timer) = state.expiry_timers.remove(grant_id) {
        timer.abort();
    }
    if reason == GrantRemovalReason::Expired && !state.expired_grant_ids.iter().any(|id| id == grant_id) {
        if state.expired_grant_ids.len() >= MAX_GRANTS {
            state.expired_grant_ids.pop_front();
        }
        state.expired_grant_ids.push_back(grant_id.to_owned());
    }
    tracing::info!(grant_id, reason = reason.as_str(), "local terminal grant removed");
    changes.push(GrantChange::Removed { grant: stored.public, reason });
}

fn sweep_locked(state: &mut GrantState, changes: &mut Vec<GrantChange>) {
    let now = Instant::now();
    let expired: Vec<String> = state
        .grants
        .values()
        .filter(|stored| stored.public.expires_at <= now)
        .map(|stored| stored.public.grant_id.clone())
        .collect();
    for grant_id in &expired {
        remove_locked(state, grant_id, GrantRemovalReason::Expired, changes);
    }
}
