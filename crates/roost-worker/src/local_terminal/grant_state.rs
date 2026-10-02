//! The grant store's locked state and the pure steps over it: install-frame
//! validation, lazy expiry, removal with its expired-id memory, the sweep,
//! and the clock they read. Called only by
//! `super::grants::LocalTerminalGrantStore` with its lock held. Ports the
//! private helpers of `apps/worker/src/local-door/local-terminal-grants.ts`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use roost_proto::DLocalTerminalGrant;
use tokio::task::AbortHandle;
use tokio::time::Instant;

use super::grant_scope::{
    GrantChange, GrantRemovalReason, LocalTerminalGrant, MAX_GRANTS, MAX_TTL_MS, is_sha256_hex,
    valid_id, valid_session_ids,
};
use super::grants::GrantListener;

pub(super) struct StoredGrant {
    pub(super) public: Arc<LocalTerminalGrant>,
    pub(super) secret_sha256: String,
    /// Which install this is, so a stale expiry timer cannot remove a renewal.
    pub(super) install: u64,
}

/// The clock every expiry decision reads: monotonic time, plus how far a smoke
/// harness has advanced it (v2 `TerminalPeerTestFaultState.now()`).
#[derive(Debug, Default)]
pub(super) struct GrantClock {
    #[cfg(feature = "smoke")]
    offset_ms: std::sync::atomic::AtomicU64,
}

impl GrantClock {
    pub(super) fn now(&self) -> Instant {
        let now = Instant::now();
        #[cfg(feature = "smoke")]
        let now = now
            .checked_add(std::time::Duration::from_millis(
                self.offset_ms.load(std::sync::atomic::Ordering::SeqCst),
            ))
            .unwrap_or(now);
        now
    }

    /// v2 `advanceGrantClock`'s bookkeeping; the error is the harness's reply.
    /// The offset stays a JavaScript safe integer, and far enough below the
    /// platform's `Instant` limit that the longest TTL still fits on top.
    #[cfg(feature = "smoke")]
    pub(super) fn advance(&self, milliseconds: u64) -> Result<(), &'static str> {
        const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
        if milliseconds == 0 || milliseconds > MAX_SAFE_INTEGER {
            return Err("grant clock advance must be a positive safe integer");
        }
        let fits = |total: u64| {
            total <= MAX_SAFE_INTEGER
                && Instant::now()
                    .checked_add(std::time::Duration::from_millis(
                        total + u64::from(MAX_TTL_MS),
                    ))
                    .is_some()
        };
        let ordering = std::sync::atomic::Ordering::SeqCst;
        self.offset_ms
            .fetch_update(ordering, ordering, |offset| {
                offset
                    .checked_add(milliseconds)
                    .filter(|total| fits(*total))
            })
            .map(|_| ())
            .map_err(|_| "grant clock advance exceeds the supported range")
    }
}

#[derive(Default)]
pub(super) struct GrantState {
    pub(super) grants: HashMap<String, StoredGrant>,
    pub(super) expiry_timers: HashMap<String, AbortHandle>,
    pub(super) listeners: Vec<(u64, GrantListener)>,
    pub(super) expired_grant_ids: VecDeque<String>,
    pub(super) next_id: u64,
    pub(super) disposed: bool,
}

/// v2's install checks, in v2's order; the error is the refusal message. The
/// session set comes back validated.
pub(super) fn validated_session_ids(
    frame: &DLocalTerminalGrant,
    worker_epoch: &str,
) -> Result<Vec<String>, String> {
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
    if !worker_epoch.is_empty()
        && !frame.worker_epoch.is_empty()
        && frame.worker_epoch != worker_epoch
    {
        return Err("worker_epoch does not match this worker".to_owned());
    }
    if frame.ttl_ms == 0 || frame.ttl_ms > MAX_TTL_MS {
        return Err(format!("ttl_ms must be within 1..{MAX_TTL_MS}"));
    }
    Ok(session_ids)
}

/// The live scope, removed first when it has expired.
pub(super) fn current_locked(
    state: &mut GrantState,
    grant_id: &str,
    now: Instant,
    changes: &mut Vec<GrantChange>,
) -> Option<Arc<LocalTerminalGrant>> {
    let stored = state.grants.get(grant_id)?;
    if stored.public.expires_at > now {
        return Some(Arc::clone(&stored.public));
    }
    remove_locked(state, grant_id, GrantRemovalReason::Expired, changes);
    None
}

pub(super) fn remove_locked(
    state: &mut GrantState,
    grant_id: &str,
    reason: GrantRemovalReason,
    changes: &mut Vec<GrantChange>,
) {
    let Some(stored) = state.grants.remove(grant_id) else {
        return;
    };
    if let Some(timer) = state.expiry_timers.remove(grant_id) {
        timer.abort();
    }
    if reason == GrantRemovalReason::Expired
        && !state.expired_grant_ids.iter().any(|id| id == grant_id)
    {
        if state.expired_grant_ids.len() >= MAX_GRANTS {
            state.expired_grant_ids.pop_front();
        }
        state.expired_grant_ids.push_back(grant_id.to_owned());
    }
    tracing::info!(
        grant_id,
        reason = reason.as_str(),
        "local terminal grant removed"
    );
    changes.push(GrantChange::Removed {
        grant: stored.public,
        reason,
    });
}

pub(super) fn sweep_locked(state: &mut GrantState, now: Instant, changes: &mut Vec<GrantChange>) {
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
