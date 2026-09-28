//! Getting the pool a keeper again between reconcile passes, and restarting a
//! degraded one. A connected pool needs nothing; after a keeper death the same
//! admission boot runs (adopt a survivor, retire an empty incompatible one, or
//! start a fresh keeper) and the pool reconnects to it. Ports v2's
//! `prepareKeeper` (`main.ts` → `handleKeeperSurvivor`), `pool.ensure()` and
//! `restartKeeper()` (`keeper/multiplexed-client.ts`). Built at boot; used by
//! `runtime::session_reconcile` and `runtime::reconcile_gate`.

use std::sync::{Arc, Mutex, PoisonError};

use super::boot::WorkerBoot;
use super::reconcile_gate::KeeperRemediation;
use super::keeper_boot::{KeeperBootOutcome, ensure_keeper};
use super::session_reconcile::ReconcileFailure;
use crate::keeper_pool::KeeperPool;
use crate::session::lifecycle::SessionManager;

/// The keeper process this worker spawned, if any: v2 `_keeperProc`. An
/// adopted keeper has none, so a restart of it is a no-op, exactly as in v2.
#[derive(Debug, Clone, Default)]
pub struct KeeperProcess(Arc<Mutex<Option<u32>>>);

impl KeeperProcess {
    /// A keeper this worker started is running as `pid`.
    pub(super) fn started(&self, pid: u32) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(pid);
    }

    /// The keeper this worker started ended; a later restart must not signal a
    /// recycled pid.
    pub(super) fn ended(&self, pid: u32) {
        let mut held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if *held == Some(pid) {
            *held = None;
        }
    }

    /// SIGTERM the keeper this worker started: its socket closes, the pool
    /// reports the death, and the death reconcile starts a clean keeper.
    pub fn terminate(&self) -> bool {
        let Some(pid) = *self.0.lock().unwrap_or_else(PoisonError::into_inner) else {
            tracing::info!("keeper.restart_degraded: this worker did not start the running keeper, so there is no process to restart");
            return false;
        };
        let delivered = roost_keeper::process_reap::terminate_process(pid);
        tracing::warn!(pid, delivered, "keeper.restart_degraded: the keeper process was asked to stop");
        delivered
    }
}

/// What a reconcile pass calls before it adopts or respawns.
#[derive(Debug, Clone)]
pub struct KeeperPreparer {
    boot: WorkerBoot,
    process: KeeperProcess,
}

impl KeeperPreparer {
    pub fn new(boot: WorkerBoot, process: KeeperProcess) -> Self {
        Self { boot, process }
    }

    /// Nothing while the pool still has its keeper; otherwise admit one and
    /// reconnect. `open_sessions` is the coordinator's open-session count, the
    /// other half of the proof a replacement needs. `true` when a keeper was
    /// admitted by this call.
    pub async fn prepare(&self, pool: &KeeperPool, open_sessions: usize) -> Result<bool, ReconcileFailure> {
        if pool.is_connected() {
            return Ok(false);
        }
        let outcome = ensure_keeper(&self.boot, Some(open_sessions), &self.boot.log_dir, &self.process)
            .await
            .map_err(|error| ReconcileFailure::recoverable(format!("{error:#}")))?;
        let keeper = match outcome {
            KeeperBootOutcome::Adopted { keeper, .. } | KeeperBootOutcome::StartedFresh { keeper } => keeper,
            KeeperBootOutcome::Held { decision } => {
                return Err(ReconcileFailure::recoverable(format!("the keeper was not admitted: {decision:?}")));
            }
        };
        let client = keeper
            .into_client()
            .ok_or_else(|| ReconcileFailure::recoverable("the admitted keeper connection is shared and cannot be driven"))?;
        pool.reconnect(client);
        tracing::info!(open_sessions, "reconcile: the pool is driving a newly admitted keeper");
        Ok(true)
    }

    /// v2 `restartKeeper`.
    pub fn restart(&self) -> bool {
        self.process.terminate()
    }
}

/// The production remediation: the session layer's keeper-update flag (v2
/// `sessionMgr.keeperUpdatePrepared`) and this worker's keeper restart.
pub struct WorkerKeeperRemediation {
    manager: Arc<SessionManager>,
    preparer: KeeperPreparer,
}

impl WorkerKeeperRemediation {
    pub fn new(manager: Arc<SessionManager>, preparer: KeeperPreparer) -> Self {
        Self { manager, preparer }
    }
}

impl std::fmt::Debug for WorkerKeeperRemediation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("WorkerKeeperRemediation").field("preparer", &self.preparer).finish_non_exhaustive()
    }
}

impl KeeperRemediation for WorkerKeeperRemediation {
    fn keeper_update_prepared(&self) -> bool {
        self.manager.keeper_update_prepared()
    }

    fn restart_keeper(&self) -> bool {
        self.preparer.restart()
    }
}
