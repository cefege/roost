//! The one door every reconcile pass goes through: callers join the pass in
//! flight, a keeper-update preparation blocks new passes and waits out the
//! current one, a keeper death drives a pass, and a degraded keeper is
//! restarted within a grace window and a bounded budget. Ports
//! `apps/worker/src/boot/boot-reconcile.ts` (`setupReconcile`). Built in
//! `runtime::owners`; boot, the pool's death hook, the session layer's degraded
//! hook and `keeper_pool::update_prepare` call it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use futures_util::FutureExt as _;
use futures_util::future::Shared;
use roost_observability::clock::EventClock;

use super::heartbeat::KeeperReconciliation;
use super::reconcile::OpenSession;
use super::session_reconcile::{ReconcileFailure, ReconcileSummary};
use super::stop::{StopReason, StopRequests};
use crate::keeper_pool::{BoundaryRelease, KeeperUpdateBoundary};
use crate::uplink::OwnerFuture;

/// v2 `KEEPER_DEGRADED_REMEDIATION_GRACE_MS`.
pub const KEEPER_DEGRADED_REMEDIATION_GRACE_MS: i64 = 90_000;
/// v2 `KEEPER_RESTART_BUDGET` within `KEEPER_RESTART_BUDGET_WINDOW_MS`.
pub const KEEPER_RESTART_BUDGET: usize = 2;
pub const KEEPER_RESTART_BUDGET_WINDOW_MS: i64 = 5 * 60_000;
/// v2's refusal while a keeper update is prepared.
pub const RECONCILE_BLOCKED_BY_UPDATE: &str = "keeper update preparation blocks reconciliation";

/// One pass's result (v2 `ReconcileAdmissionOutcome`).
pub type ReconcileOutcome = Result<ReconcileSummary, ReconcileFailure>;

/// One reconcile pass; `runtime::session_reconcile::SessionReconciler` in
/// production.
pub trait ReconcilePass: Send + Sync {
    fn run(self: Arc<Self>, reason: &'static str, rows: Option<Vec<OpenSession>>) -> OwnerFuture<ReconcileOutcome>;
}

/// What remediation acts on: whether a keeper update is prepared (v2
/// `sessionMgr.keeperUpdatePrepared`) and the keeper restart (`restartKeeper`).
pub trait KeeperRemediation: Send + Sync {
    fn keeper_update_prepared(&self) -> bool;
    fn restart_keeper(&self) -> bool;
}

/// The gate. Cheap to clone; every clone is the same gate.
#[derive(Clone)]
pub struct ReconcileGate {
    inner: Arc<Gate>,
}

struct Gate {
    pass: Arc<dyn ReconcilePass>,
    remediation: Arc<dyn KeeperRemediation>,
    clock: Arc<dyn EventClock>,
    reconciliation: KeeperReconciliation,
    stop: StopRequests,
    runtime: tokio::runtime::Handle,
    state: Mutex<GateState>,
}

#[derive(Default)]
struct GateState {
    in_flight: Option<(u64, Shared<OwnerFuture<ReconcileOutcome>>)>,
    next_pass: u64,
    keeper_update_blocked: bool,
    pending_degraded_remediation: bool,
    last_reconcile_ms: i64,
    reconcile_admitted: bool,
    keeper_restarts: VecDeque<i64>,
}

impl ReconcileGate {
    pub fn new(
        pass: Arc<dyn ReconcilePass>,
        remediation: Arc<dyn KeeperRemediation>,
        clock: Arc<dyn EventClock>,
        reconciliation: KeeperReconciliation,
        stop: StopRequests,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let inner = Arc::new(Gate { pass, remediation, clock, reconciliation, stop, runtime, state: Mutex::default() });
        Self { inner }
    }

    /// v2 `reconcileOpenSessions`: join the pass in flight or start one.
    /// `rows` is a set the caller already read (boot); `None` reads it.
    pub fn reconcile_open_sessions(&self, reason: &'static str, rows: Option<Vec<OpenSession>>) -> OwnerFuture<ReconcileOutcome> {
        let mut state = self.inner.lock();
        if state.keeper_update_blocked {
            tracing::info!(reason, "worker: reconcile refused while a keeper update is prepared");
            return Box::pin(std::future::ready(Err(ReconcileFailure::recoverable(RECONCILE_BLOCKED_BY_UPDATE))));
        }
        if let Some((_, current)) = &state.in_flight {
            tracing::info!(reason, "worker: reconcile_joined_inflight");
            return Box::pin(current.clone());
        }
        state.next_pass += 1;
        let pass_id = state.next_pass;
        let gate = self.clone();
        let run: OwnerFuture<ReconcileOutcome> = Box::pin(async move { gate.run(reason, rows).await });
        let shared = run.shared();
        state.in_flight = Some((pass_id, shared.clone()));
        drop(state);
        let settle = self.clone();
        let driven = shared.clone();
        self.inner.runtime.spawn(async move {
            let outcome = driven.await;
            settle.settle(pass_id, &outcome);
        });
        Box::pin(shared)
    }

    /// v2 `setOnKeeperDeath` body.
    pub fn on_keeper_death(&self) {
        if self.inner.remediation.keeper_update_prepared() {
            tracing::info!("worker: keeper_death_reconcile_suppressed");
            return;
        }
        tracing::warn!("worker: keeper_death_reconcile");
        drop(self.reconcile_open_sessions("keeper_death", None));
    }

    /// v2 `setOnKeeperDegraded` body.
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
            drop(self.reconcile_open_sessions("keeper_degraded", None));
            return;
        }
        drop(state);
        self.remediate_degraded_keeper();
    }

    /// Point the pool's death hook and the session layer's degraded hook here.
    /// Weak, so the hooks do not keep the gate (and everything it holds) alive.
    pub fn install_hooks(&self, pool: &crate::keeper_pool::KeeperPool, manager: &crate::session::lifecycle::SessionManager) {
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

    /// v2 `runReconcile`.
    async fn run(&self, reason: &'static str, rows: Option<Vec<OpenSession>>) -> ReconcileOutcome {
        self.inner.lock().reconcile_admitted = false;
        self.inner.reconciliation.started();
        tracing::info!(reason, "worker: reconcile started");
        let outcome = Arc::clone(&self.inner.pass).run(reason, rows).await;
        match &outcome {
            Ok(_) => {
                let now_ms = self.inner.clock.now_epoch_ms();
                {
                    let mut state = self.inner.lock();
                    state.reconcile_admitted = true;
                    state.last_reconcile_ms = now_ms;
                }
                self.inner.reconciliation.reconciled(now_ms);
            }
            Err(failure) if failure.fatal => {
                tracing::error!(reason, error = %failure, "worker: a reconcile lost session-event durability; the worker stops");
                self.inner.stop.request(StopReason::DurabilityLost);
            }
            Err(failure) => tracing::warn!(reason, error = %failure, "worker: resume_failed"),
        }
        outcome
    }

    fn settle(&self, pass_id: u64, outcome: &ReconcileOutcome) {
        let remediate = {
            let mut state = self.inner.lock();
            if state.in_flight.as_ref().is_some_and(|(current, _)| *current == pass_id) {
                state.in_flight = None;
            }
            let remediate = state.pending_degraded_remediation && outcome.is_ok();
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
        while state.keeper_restarts.front().is_some_and(|at| *at < window_start) {
            state.keeper_restarts.pop_front();
        }
        if state.keeper_restarts.len() >= KEEPER_RESTART_BUDGET {
            tracing::error!(restarts = state.keeper_restarts.len(), window_ms = KEEPER_RESTART_BUDGET_WINDOW_MS, "worker: keeper_degraded_unrecoverable");
            return;
        }
        state.keeper_restarts.push_back(now_ms);
        let restart_n = state.keeper_restarts.len();
        drop(state);
        tracing::warn!(since_reconcile_ms, restart_n, "worker: keeper_degraded_restart");
        self.inner.remediation.restart_keeper();
    }
}

impl KeeperUpdateBoundary for ReconcileGate {
    /// v2 `acquireKeeperUpdateBoundary`: block new passes, wait out the one in
    /// flight, and hand back a release that reopens only what this call closed.
    fn acquire(&self) -> OwnerFuture<Result<BoundaryRelease, String>> {
        let gate = self.clone();
        Box::pin(async move {
            let (changed, active) = {
                let mut state = gate.inner.lock();
                let changed = !state.keeper_update_blocked;
                state.keeper_update_blocked = true;
                (changed, state.in_flight.as_ref().map(|(_, current)| current.clone()))
            };
            if let Some(active) = active
                && let Err(failure) = active.await
                && failure.fatal
            {
                if changed {
                    gate.inner.lock().keeper_update_blocked = false;
                }
                return Err(failure.reason);
            }
            tracing::info!(changed, "worker: the keeper update boundary is held");
            let release: BoundaryRelease = Box::new(move || {
                if changed {
                    gate.inner.lock().keeper_update_blocked = false;
                    tracing::info!("worker: the keeper update boundary was released");
                }
            });
            Ok(release)
        })
    }
}

impl Gate {
    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
