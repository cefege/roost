//! The worker's one in-memory registry of browser direct-terminal grants. The
//! coordinator installs only SHA-256 digests; callers read the current public
//! scope and never a digest or a secret. Expiry is active, so a connected
//! carrier cannot outlive its authorization while the coordinator is away.
//! Called by `super::door` (install/revoke from the link), `super::sockets`
//! and the peer owner. Ports `apps/worker/src/local-door/local-terminal-grants.ts`.

use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use roost_proto::DLocalTerminalGrant;
use tokio::runtime::Handle;
use tokio::task::AbortHandle;
use tokio::time::Instant;

use super::grant_scope::{
    GrantChange, GrantCredential, GrantRemovalReason, LocalTerminalGrant, MAX_GRANTS,
    PeerGrantAuthorization, capability_matches, sha256_hex,
};
use super::grant_state::{
    GrantClock, GrantState, StoredGrant, current_locked, remove_locked, sweep_locked,
    validated_session_ids,
};

#[cfg(feature = "smoke")]
mod test_seams;

/// A subscriber. Called with no store lock held, possibly from inside another
/// owner's lock (a lazy expiry during a predicate), so it only fences and
/// defers; it never calls back into that owner synchronously.
pub type GrantListener = Arc<dyn Fn(&GrantChange) + Send + Sync>;

struct GrantShared {
    /// This worker's process epoch; empty means the store fences no epoch.
    worker_epoch: String,
    runtime: Handle,
    state: Mutex<GrantState>,
    /// The clock every expiry decision reads.
    clock: GrantClock,
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
        let shared = GrantShared {
            worker_epoch: worker_epoch.into(),
            runtime,
            state: Mutex::default(),
            clock: GrantClock::default(),
        };
        Self {
            shared: Arc::new(shared),
        }
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
        let now = self.shared.clock.now();
        let current = current_locked(&mut self.lock(), grant_id, now, &mut changes);
        self.notify(&changes);
        current
    }

    /// Check a Hello's credential; the error is the close reason.
    pub fn verify(
        &self,
        credential: GrantCredential<'_>,
    ) -> Result<Arc<LocalTerminalGrant>, &'static str> {
        let mut changes = Vec::new();
        let verdict = {
            let mut state = self.lock();
            match current_locked(
                &mut state,
                credential.grant_id,
                self.shared.clock.now(),
                &mut changes,
            ) {
                None if state
                    .expired_grant_ids
                    .iter()
                    .any(|id| id == credential.grant_id) =>
                {
                    Err("local terminal grant expired")
                }
                None => Err("unknown local terminal grant"),
                Some(grant) if grant.device_fingerprint != credential.device_fingerprint => {
                    Err("grant is bound to another device")
                }
                Some(grant) if grant.tab_id != credential.tab_id => {
                    Err("grant is bound to another tab")
                }
                Some(grant) => {
                    let installed = state
                        .grants
                        .get(credential.grant_id)
                        .map_or("", |stored| stored.secret_sha256.as_str());
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
    pub fn authorize_peer(
        &self,
        grant_id: &str,
        device_fingerprint: &str,
        tab_id: &str,
        worker_epoch: &str,
    ) -> PeerGrantAuthorization {
        let mut changes = Vec::new();
        let verdict = {
            let mut state = self.lock();
            let epoch = self.shared.worker_epoch.as_str();
            match state.grants.get(grant_id) {
                None if state.expired_grant_ids.iter().any(|id| id == grant_id) => {
                    PeerGrantAuthorization::Expired
                }
                None => PeerGrantAuthorization::GrantUnavailable,
                Some(stored) if stored.public.expires_at <= self.shared.clock.now() => {
                    remove_locked(
                        &mut state,
                        grant_id,
                        GrantRemovalReason::Expired,
                        &mut changes,
                    );
                    PeerGrantAuthorization::Expired
                }
                Some(_) if epoch.is_empty() || worker_epoch != epoch => {
                    PeerGrantAuthorization::GrantUnavailable
                }
                Some(stored) if stored.public.worker_epoch != epoch => {
                    PeerGrantAuthorization::GrantUnavailable
                }
                Some(stored)
                    if stored.public.device_fingerprint == device_fingerprint
                        && stored.public.tab_id == tab_id =>
                {
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
                remove_locked(
                    &mut state,
                    grant_id,
                    GrantRemovalReason::Revoked,
                    &mut changes,
                );
            }
        }
        if !changes.is_empty() {
            tracing::info!(
                device_fingerprint,
                grants = changes.len(),
                "local terminal grants revoked"
            );
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
                remove_locked(
                    &mut state,
                    grant_id,
                    GrantRemovalReason::Disposed,
                    &mut changes,
                );
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

    fn install_locked(
        &self,
        frame: &DLocalTerminalGrant,
        changes: &mut Vec<GrantChange>,
    ) -> Result<Arc<LocalTerminalGrant>, String> {
        let mut state = self.lock();
        if state.disposed {
            return Err("local terminal grant store is disposed".to_owned());
        }
        let session_ids = validated_session_ids(frame, &self.shared.worker_epoch)?;
        sweep_locked(&mut state, self.shared.clock.now(), changes);
        let prior = state
            .grants
            .get(&frame.grant_id)
            .map(|stored| Arc::clone(&stored.public));
        if prior.is_none() && state.grants.len() >= MAX_GRANTS {
            return Err("local terminal grant capacity is full".to_owned());
        }
        let ttl = Duration::from_millis(u64::from(frame.ttl_ms));
        let public = Arc::new(LocalTerminalGrant {
            grant_id: frame.grant_id.clone(),
            session_ids,
            device_fingerprint: frame.device_fingerprint.clone(),
            tab_id: frame.tab_id.clone(),
            worker_epoch: frame.worker_epoch.clone(),
            expires_at: self.shared.clock.now() + ttl,
        });
        if let Some(timer) = state.expiry_timers.remove(&frame.grant_id) {
            timer.abort();
        }
        state.next_id += 1;
        let install = state.next_id;
        let stored = StoredGrant {
            public: Arc::clone(&public),
            secret_sha256: frame.secret_sha256.clone(),
            install,
        };
        state.grants.insert(frame.grant_id.clone(), stored);
        state.expired_grant_ids.retain(|id| *id != frame.grant_id);
        // The timer sleeps on real time; a harness-advanced clock is swept
        // by the advance itself.
        let timer = self.arm_expiry(frame.grant_id.clone(), install, Instant::now() + ttl);
        state.expiry_timers.insert(frame.grant_id.clone(), timer);
        changes.push(match prior {
            Some(previous) => GrantChange::Renewed {
                grant: Arc::clone(&public),
                removed_session_ids: previous
                    .session_ids
                    .iter()
                    .filter(|id| !public.session_ids.contains(id))
                    .cloned()
                    .collect(),
            },
            None => GrantChange::Installed {
                grant: Arc::clone(&public),
            },
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
            let current = state
                .grants
                .get(grant_id)
                .is_some_and(|stored| stored.install == install);
            if !current {
                return;
            }
            state.expiry_timers.remove(grant_id);
            remove_locked(
                &mut state,
                grant_id,
                GrantRemovalReason::Expired,
                &mut changes,
            );
        }
        self.notify(&changes);
    }

    fn notify(&self, changes: &[GrantChange]) {
        if changes.is_empty() {
            return;
        }
        let listeners: Vec<GrantListener> = self
            .lock()
            .listeners
            .iter()
            .map(|(_, listener)| Arc::clone(listener))
            .collect();
        for change in changes {
            for listener in &listeners {
                listener(change);
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, GrantState> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
