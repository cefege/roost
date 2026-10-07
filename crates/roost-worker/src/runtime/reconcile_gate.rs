//! The one door every reconcile pass goes through: callers join the pass in
//! flight, each pass waits for durable session-event replay before reading the
//! coordinator, keeper-update preparation waits out the current pass, keeper
//! death drives a pass, and a degraded keeper is restarted within a grace
//! window and bounded budget. Built in `runtime::owners`; boot, the pool's
//! death hook, the session layer's degraded hook and
//! `keeper_pool::update_prepare` call it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures_util::FutureExt as _;
use futures_util::future::Shared;
use roost_observability::clock::EventClock;

use super::boot::WorkerBoot;
use super::heart_owners::HeartOwners;
use super::heartbeat::KeeperReconciliation;
use super::keeper_prepare::{KeeperPreparer, KeeperProcess, WorkerKeeperRemediation};
use super::reconcile::OpenSessionSource;
use super::session_reconcile::{ReconcileFailure, ReconcileSummary, SessionReconciler};
use super::session_stack::SessionStack;
use super::stop::{StopReason, StopRequests};
use crate::keeper_pool::KeeperPool;
use crate::keeper_pool::{BoundaryRelease, KeeperUpdateBoundary};
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

/// What a pass waits for before reading the coordinator's open-session set.
#[derive(Debug, Clone)]
pub struct PassAdmission {
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
}

impl std::fmt::Debug for ReconcileInputs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReconcileInputs")
            .field("process", &self.process)
            .finish_non_exhaustive()
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
    pub(crate) inner: Arc<Gate>,
}

pub(crate) struct Gate {
    pub(crate) pass: Arc<dyn ReconcilePass>,
    pub(crate) admission: PassAdmission,
    pub(crate) remediation: Arc<dyn KeeperRemediation>,
    pub(crate) clock: Arc<dyn EventClock>,
    pub(crate) reconciliation: KeeperReconciliation,
    pub(crate) stop: StopRequests,
    pub(crate) runtime: tokio::runtime::Handle,
    pub(crate) state: Mutex<GateState>,
}

#[derive(Default)]
pub(crate) struct GateState {
    pub(crate) in_flight: Option<(u64, Shared<OwnerFuture<ReconcileOutcome>>)>,
    pub(crate) next_pass: u64,
    pub(crate) keeper_update_blocked: bool,
    pub(crate) pending_degraded_remediation: bool,
    pub(crate) last_reconcile_ms: i64,
    pub(crate) reconcile_admitted: bool,
    pub(crate) keeper_restarts: VecDeque<i64>,
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
        let inner = Arc::new(Gate {
            pass,
            admission,
            remediation,
            clock,
            reconciliation,
            stop,
            runtime,
            state: Mutex::default(),
        });
        Self { inner }
    }

    /// The production gate (v2 `setupReconcile`): the pass over `stack`'s
    /// manager and `pool`, the heart's stray sweeper and reconciliation stamp,
    /// the stack's durable replay, and the pool's death and the manager's
    /// degraded hooks pointed here. Called once by `runtime::owners`, inside
    /// the worker's runtime.
    pub fn start(
        stack: &SessionStack,
        pool: &Arc<KeeperPool>,
        heart: &HeartOwners,
        inputs: ReconcileInputs,
    ) -> Self {
        let preparer = KeeperPreparer::new(inputs.boot, inputs.process);
        let pass = SessionReconciler::new(
            Arc::clone(&stack.manager),
            Arc::clone(pool),
            inputs.sessions,
            preparer.clone(),
            Arc::clone(&heart.strays),
        );
        let admission = PassAdmission {
            durable_replay: Arc::clone(&stack.durable_delivery),
        };
        let gate = Self::new(
            Arc::new(pass),
            admission,
            Arc::new(WorkerKeeperRemediation::new(
                Arc::clone(&stack.manager),
                preparer,
            )),
            Arc::clone(&stack.clock) as Arc<dyn EventClock>,
            heart.reconciliation.clone(),
            inputs.stop,
            tokio::runtime::Handle::current(),
        );
        gate.install_hooks(pool, &stack.manager);
        tracing::info!(
            "worker: the reconcile gate owns boot, keeper-death and degraded-keeper passes"
        );
        gate
    }

    /// v2 `reconcileOpenSessions`: join the pass in flight or start one.
    pub fn reconcile_open_sessions(&self, reason: &'static str) -> OwnerFuture<ReconcileOutcome> {
        let mut state = self.inner.lock();
        if state.keeper_update_blocked {
            tracing::info!(
                reason,
                "worker: reconcile refused while a keeper update is prepared"
            );
            return Box::pin(std::future::ready(Err(ReconcileFailure::recoverable(
                RECONCILE_BLOCKED_BY_UPDATE,
            ))));
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
            settle.settle(pass_id, outcome.is_ok());
        });
        Box::pin(shared)
    }

    /// v2 `runReconcile`: the pass reads the coordinator only once the durable
    /// replay has drained, so the open-session set it reads reflects every
    /// session event this worker wrote.
    async fn run(&self, reason: &'static str) -> ReconcileOutcome {
        self.inner.lock().reconcile_admitted = false;
        self.inner.reconciliation.started();
        tracing::info!(reason, "worker: reconcile started");
        let pass = Arc::clone(&self.inner.pass);
        let replay = Arc::clone(&self.inner.admission.durable_replay);
        let outcome = async move {
            if let Err(disposed) = replay.wait_for_replay().await {
                return Err(ReconcileFailure::recoverable(disposed.to_string()));
            }
            tracing::info!(
                reason,
                "worker: the durable session-event replay drained; the pass reads the coordinator"
            );
            pass.run(reason).await
        }
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
                (
                    changed,
                    state.in_flight.as_ref().map(|(_, current)| current.clone()),
                )
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
    pub(crate) fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
