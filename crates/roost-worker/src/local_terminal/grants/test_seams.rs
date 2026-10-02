//! The smoke harness's seams on the grant store: advancing its clock (with the
//! sweep that fences whatever lapsed) and taking one session out of every live
//! grant. Called by `crate::smoke_faults`; compiled only with the `smoke`
//! feature. Ports `_sweepExpiredForTest` / `_shrinkSessionForTest` of
//! `apps/worker/src/local-door/local-terminal-grants.ts`.

use roost_proto::DLocalTerminalGrant;

use super::LocalTerminalGrantStore;
use crate::local_terminal::grant_scope::GrantRemovalReason;
use crate::local_terminal::grant_state::{remove_locked, sweep_locked};

impl LocalTerminalGrantStore {
    /// v2 `advanceGrantClock`: move the grant clock forward, then sweep at
    /// once so every port whose grant lapsed is fenced now, not at its timer.
    pub fn advance_clock(&self, milliseconds: u64) -> Result<(), String> {
        self.shared.clock.advance(milliseconds)?;
        tracing::info!(milliseconds, "the smoke harness advanced the grant clock");
        let mut changes = Vec::new();
        sweep_locked(&mut self.lock(), self.shared.clock.now(), &mut changes);
        self.notify(&changes);
        Ok(())
    }

    /// v2 `_shrinkSessionForTest`: every live grant naming `session_id` is
    /// renewed without it under its own id, secret and remaining TTL — or
    /// removed when nothing would remain or it already lapsed. Answers how
    /// many grants changed.
    pub fn shrink_session(&self, session_id: &str) -> usize {
        let mut changes = Vec::new();
        let mut changed = 0;
        let mut renewals = Vec::new();
        {
            let mut state = self.lock();
            let now = self.shared.clock.now();
            let affected: Vec<String> = state
                .grants
                .values()
                .filter(|stored| stored.public.session_ids.iter().any(|id| id == session_id))
                .map(|stored| stored.public.grant_id.clone())
                .collect();
            for grant_id in affected {
                let Some(stored) = state.grants.get(&grant_id) else {
                    continue;
                };
                let grant = &stored.public;
                let remaining: Vec<String> = grant
                    .session_ids
                    .iter()
                    .filter(|id| *id != session_id)
                    .cloned()
                    .collect();
                let expired = grant.expires_at <= now;
                changed += 1;
                if remaining.is_empty() || expired {
                    let reason = if expired {
                        GrantRemovalReason::Expired
                    } else {
                        GrantRemovalReason::Cleared
                    };
                    remove_locked(&mut state, &grant_id, reason, &mut changes);
                    continue;
                }
                let remaining_ms = grant
                    .expires_at
                    .duration_since(now)
                    .as_micros()
                    .div_ceil(1000);
                renewals.push(DLocalTerminalGrant {
                    request_id: grant_id.clone(),
                    grant_id,
                    secret_sha256: stored.secret_sha256.clone(),
                    session_ids: remaining,
                    device_fingerprint: grant.device_fingerprint.clone(),
                    tab_id: grant.tab_id.clone(),
                    ttl_ms: u32::try_from(remaining_ms).unwrap_or(u32::MAX).max(1),
                    worker_epoch: grant.worker_epoch.clone(),
                    ..Default::default()
                });
            }
        }
        for renewal in &renewals {
            if let Err(error) = self.install_locked(renewal, &mut changes) {
                tracing::warn!(%error, grant_id = %renewal.grant_id, "a shrunk grant could not be renewed");
            }
        }
        tracing::info!(session_id, changed, "the smoke harness shrank grant scopes");
        self.notify(&changes);
        changed
    }
}
