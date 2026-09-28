//! The one door every reconcile pass goes through: callers join the pass in
//! flight, every pass holds the agent-reference admission gate and waits for
//! the durable session-event replay before it reads the coordinator, a
//! keeper-update preparation blocks new passes and waits out the current one, a
//! keeper death drives a pass, and a degraded keeper is restarted within a
//! grace window and a bounded budget. Ports
//! `apps/worker/src/boot/boot-reconcile.ts` (`setupReconcile`). Built in
//! `runtime::owners`; boot, the pool's death hook, the session layer's degraded
//! hook and `keeper_pool::update_prepare` call it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use futures_util::FutureExt as _;
use futures_util::future::Shared;
use roost_observability::clock::EventClock;

use super::boot::WorkerBoot;
use super::heart_owners::HeartOwners;
use super::heartbeat::KeeperReconciliation;
use super::keeper_prepare::{KeeperPreparer, KeeperProcess, WorkerKeeperRemediation};
use super::reconcile::OpenSessionSource;
use super::reconcile_restore::WorkerConversationRestorer;
use super::session_reconcile::{ReconcileFailure, ReconcileSummary, SessionReconciler};
use super::session_stack::SessionStack;
use super::stop::{StopReason, StopRequests};
use crate::keeper_pool::KeeperPool;
use crate::keeper_pool::{BoundaryRelease, KeeperUpdateBoundary};
use crate::agents::reference_admission::AgentReferenceAdmissionGate;
use crate::session::durable_delivery::DurableDelivery;
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
    fn run(self: Arc<Self>, reason: &'static str) -> OwnerFuture<ReconcileOutcome>;
}

/// What a pass waits for before it reads the coordinator's recovery state
/// (v2 `referenceAdmission` and `beforeRecoveryRead`): no agent reference is
/// admitted while a pass runs, and the rows the coordinator derives recovery
/// metadata from have all reached it.
#[derive(Debug, Clone)]
pub struct PassAdmission {
    pub reference_admission: AgentReferenceAdmissionGate,
    pub durable_replay: Arc<DurableDelivery>,
}

/// What remediation acts on: whether a keeper update is prepared (v2
/// `sessionMgr.keeperUpdatePrepared`) and the keeper restart (`restartKeeper`).
pub trait KeeperRemediation: Send + Sync {
    fn keeper_update_prepared(&self) -> bool;
    fn restart_keeper(&self) -> bool;
}

/// What boot hands the gate beyond the session stack: the keeper admission
/// inputs a later pass re-runs, the coordinator's session source, and the stop
/// a durability loss requests.
pub struct ReconcileInputs {
    pub boot: WorkerBoot,
    pub process: KeeperProcess,
    pub sessions: Arc<dyn OpenSessionSource>,
    pub stop: StopRequests,
    /// The host the restore materialises its resume command for.
    pub platform: roost_host::HostPlatform,
}

impl std::fmt::Debug for ReconcileInputs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ReconcileInputs").field("process", &self.process).finish_non_exhaustive()
    }
}

impl std::fmt::Debug for ReconcileGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.inner.lock();
        formatter
            .debug_struct("ReconcileGate")
            .field("in_flight", &state.in_flight.is_some())
            .field("keeper_update_blocked", &state.keeper_update_blocked)
            .field("reconcile_admitted", &state.reconcile_admitted)
            .finish_non_exhaustive()
    }
}

/// The gate. Cheap to clone; every clone is the same gate.
#[derive(Clone)]
pub struct ReconcileGate {
    inner: Arc<Gate>,
}

struct Gate {
    pass: Arc<dyn ReconcilePass>,
    admission: PassAdmission,
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
        admission: PassAdmission,
        remediation: Arc<dyn KeeperRemediation>,
        clock: Arc<dyn EventClock>,
        reconciliation: KeeperReconciliation,
        stop: StopRequests,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let inner = Arc::new(Gate { pass, admission, remediation, clock, reconciliation, stop, runtime, state: Mutex::default() });
        Self { inner }
    }

    /// The production gate (v2 `setupReconcile`): the pass over `stack`'s
    /// manager and `pool`, the heart's stray sweeper and reconciliation stamp,
    /// the stack's durable replay and the one reference admission gate, and
    /// the pool's death and the manager's degraded hooks pointed here. Called
    /// once by `runtime::owners`, inside the worker's runtime.
    pub fn start(stack: &SessionStack, pool: &Arc<KeeperPool>, heart: &HeartOwners, inputs: ReconcileInputs, reference_admission: AgentReferenceAdmissionGate) -> Self {
        let restorer = WorkerConversationRestorer::new(inputs.boot.agent_conversation_restore, Arc::clone(&stack.manager), inputs.platform);
        let preparer = KeeperPreparer::new(inputs.boot, inputs.process);
        let pass = SessionReconciler::new(
            Arc::clone(&stack.manager),
            Arc::clone(pool),
            inputs.sessions,
            preparer.clone(),
            Arc::clone(&heart.strays),
            Arc::new(restorer),
        );
        let admission = PassAdmission { reference_admission, durable_replay: Arc::clone(&stack.durable_delivery) };
        let gate = Self::new(
            Arc::new(pass),
            admission,
            Arc::new(WorkerKeeperRemediation::new(Arc::clone(&stack.manager), preparer)),
            Arc::clone(&stack.clock) as Arc<dyn EventClock>,
            heart.reconciliation.clone(),
            inputs.stop,
            tokio::runtime::Handle::current(),
        );
        gate.install_hooks(pool, &stack.manager);
        tracing::info!("worker: the reconcile gate owns boot, keeper-death and degraded-keeper passes");
        gate
    }

    /// v2 `reconcileOpenSessions`: join the pass in flight or start one.
    pub fn reconcile_open_sessions(&self, reason: &'static str) -> OwnerFuture<ReconcileOutcome> {
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
        let run: OwnerFuture<ReconcileOutcome> = Box::pin(async move { gate.run(reason).await });
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
        drop(self.reconcile_open_sessions("keeper_death"));
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
            drop(self.reconcile_open_sessions("keeper_degraded"));
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

    /// v2 `runReconcile`: the pass holds the reference admission gate and
    /// reads the coordinator only once the durable replay has drained, so the
    /// recovery metadata it reads reflects every reference this worker wrote.
    async fn run(&self, reason: &'static str) -> ReconcileOutcome {
        self.inner.lock().reconcile_admitted = false;
        self.inner.reconciliation.started();
        tracing::info!(reason, "worker: reconcile started");
        let pass = Arc::clone(&self.inner.pass);
        let replay = Arc::clone(&self.inner.admission.durable_replay);
        let outcome = self
            .inner
            .admission
            .reference_admission
            .run_exclusive(|| async move {
                if let Err(disposed) = replay.wait_for_replay().await {
                    return Err(ReconcileFailure::recoverable(disposed.to_string()));
                }
                tracing::info!(reason, "worker: the durable session-event replay drained; the pass reads the coordinator");
                pass.run(reason).await
            })
            .await;
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
