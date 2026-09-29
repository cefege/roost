//! The keeper-lifecycle half of the reconcile gate: a keeper death drives a
//! pass at once, and a degraded keeper is restarted within a grace window and a
//! bounded budget once a pass has landed. Owned by `runtime::reconcile_gate`,
//! which serializes the passes; v2's `apps/worker/src/boot/boot-reconcile.ts`
//! `setOnKeeperDeath` / `setOnKeeperDegraded` / `remediateDegradedKeeper` are
//! the authority.

use std::sync::{Arc, Weak};

use super::reconcile_gate::{
    Gate, KEEPER_DEGRADED_REMEDIATION_GRACE_MS, KEEPER_RESTART_BUDGET,
    KEEPER_RESTART_BUDGET_WINDOW_MS, ReconcileGate,
};

impl ReconcileGate {
    /// v2 `setOnKeeperDeath` body.
    pub fn on_keeper_death(&self) {
        if self.inner.remediation.keeper_update_prepared() {
            tracing::info!("worker: keeper_death_reconcile_suppressed");
            return;
        }
        tracing::warn!("worker: keeper_death_reconcile");
        drop(self.reconcile_open_sessions("keeper_death"));
    }

    /// v2 `setOnKeeperDegraded` body. A degradation that lands mid-pass waits
    /// for it: the restart is the pass's own conclusion, not a parallel one.
    pub fn on_keeper_degraded(&self) {
        let mut state = self.inner.lock();
        if state.in_flight.is_some() {
            state.pending_degraded_remediation = true;
            tracing::info!("worker: keeper_degraded_reconcile_inflight");
            return;
        }
        if !state.reconcile_admitted {
            state.pending_degraded_remediation = true;
            drop(state);
            tracing::info!("worker: keeper_degraded_reconcile_retry");
            drop(self.reconcile_open_sessions("keeper_degraded"));
            return;
        }
        drop(state);
        self.remediate_degraded_keeper();
    }

    /// The pass that just landed, and what it means for a waiting degradation.
    pub(super) fn settle(&self, pass_id: u64, outcome_ok: bool) {
        let remediate = {
            let mut state = self.inner.lock();
            if state
                .in_flight
                .as_ref()
                .is_some_and(|(current, _)| *current == pass_id)
            {
                state.in_flight = None;
            }
            let remediate = state.pending_degraded_remediation && outcome_ok;
            if remediate {
                state.pending_degraded_remediation = false;
            }
            remediate
        };
        if remediate {
            self.remediate_degraded_keeper();
        }
    }

    /// v2 `remediateDegradedKeeper`: outside the grace window and within the
    /// budget, restart the keeper.
    fn remediate_degraded_keeper(&self) {
        if self.inner.remediation.keeper_update_prepared() {
            tracing::info!("worker: keeper_degraded_restart_suppressed");
            return;
        }
        let now_ms = self.inner.clock.now_epoch_ms();
        let mut state = self.inner.lock();
        let since_reconcile_ms = now_ms - state.last_reconcile_ms;
        if since_reconcile_ms < KEEPER_DEGRADED_REMEDIATION_GRACE_MS {
            tracing::info!(since_reconcile_ms, "worker: keeper_degraded_skip_transient");
            return;
        }
        let window_start = now_ms - KEEPER_RESTART_BUDGET_WINDOW_MS;
        while state
            .keeper_restarts
            .front()
            .is_some_and(|at| *at < window_start)
        {
            state.keeper_restarts.pop_front();
        }
        if state.keeper_restarts.len() >= KEEPER_RESTART_BUDGET {
            tracing::error!(
                restarts = state.keeper_restarts.len(),
                window_ms = KEEPER_RESTART_BUDGET_WINDOW_MS,
                "worker: keeper_degraded_unrecoverable"
            );
            return;
        }
        state.keeper_restarts.push_back(now_ms);
        let restart_n = state.keeper_restarts.len();
        drop(state);
        tracing::warn!(
            since_reconcile_ms,
            restart_n,
            "worker: keeper_degraded_restart"
        );
        self.inner.remediation.restart_keeper();
    }

    /// Point the pool's death hook and the session layer's degraded hook here.
    /// Weak, so the hooks do not keep the gate (and everything it holds) alive.
    pub fn install_hooks(
        &self,
        pool: &crate::keeper_pool::KeeperPool,
        manager: &crate::session::lifecycle::SessionManager,
    ) {
        let death: Weak<Gate> = Arc::downgrade(&self.inner);
        pool.set_on_keeper_death(Arc::new(move || {
            if let Some(inner) = death.upgrade() {
                ReconcileGate { inner }.on_keeper_death();
            }
        }));
        let degraded: Weak<Gate> = Arc::downgrade(&self.inner);
        manager.keeper_health().set_hook(Arc::new(move || {
            if let Some(inner) = degraded.upgrade() {
                ReconcileGate { inner }.on_keeper_degraded();
            }
        }));
    }
}
