//! One reconcile pass over the coordinator's complete open-session set: every
//! durable outcome is reserved before the keeper or the session table is
//! touched, then each session is adopted from its surviving keeper channel or
//! respawned onto a fresh one, and finally strays are reaped. Ports
//! `apps/worker/src/boot/boot-session-reconcile.ts` (`reconcileCoordinatorSessions`).
//! `runtime::reconcile_gate` serializes the passes; boot, keeper death and
//! keeper degradation all run through it.

use super::keeper_prepare::KeeperPreparer;
use super::reconcile::{OpenSessionSet, OpenSessionSource};
use super::reconcile_gate::{ReconcileOutcome, ReconcilePass};
use super::session_reconcile_admission::Admission;
use crate::keeper_pool::KeeperPool;
use crate::session::lifecycle::SessionManager;
use crate::session::respawn_replace::{RespawnError, RespawnRequest};
use crate::session::resume::{AdoptFailure, AdoptRefusal, AdoptionRequest};
use crate::session::spawn::ClaimsOnFailure;
use crate::session::stray_reap::StraySweeper;
use crate::uplink::OwnerFuture;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// v2 `BOOT_SESSION_ADMISSION_TIMEOUT_MS`.
pub const BOOT_SESSION_ADMISSION_TIMEOUT: Duration = Duration::from_secs(10);
/// v2 retries a transient respawn failure this many times in all.
pub const RESPAWN_ATTEMPTS: u32 = 3;

/// What an admitted pass did (v2 `ReconcileAdmissionSuccess`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileSummary {
    pub candidates: usize,
    pub resumed: usize,
    pub respawned: usize,
    pub strays_reaped: usize,
}

/// Why a pass was not admitted. `fatal` is v2's durability error, which the
/// worker must not survive (v2 rethrows it to the uncaught handler).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct ReconcileFailure {
    pub reason: String,
    pub fatal: bool,
}

impl ReconcileFailure {
    pub fn recoverable(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            fatal: false,
        }
    }
}

/// The production pass.
pub struct SessionReconciler {
    pub(crate) manager: Arc<SessionManager>,
    pub(crate) pool: Arc<KeeperPool>,
    pub(crate) sessions: Arc<dyn OpenSessionSource>,
    pub(crate) keeper: KeeperPreparer,
    pub(crate) sweeper: Arc<StraySweeper>,
    /// The keeper boot admitted before this reconciler existed: its survivors
    /// are capacity-checked by the first pass that reaches the check.
    pub(crate) boot_survivors_unchecked: AtomicBool,
}

impl std::fmt::Debug for SessionReconciler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionReconciler")
            .field("keeper", &self.keeper)
            .finish_non_exhaustive()
    }
}

impl SessionReconciler {
    pub fn new(
        manager: Arc<SessionManager>,
        pool: Arc<KeeperPool>,
        sessions: Arc<dyn OpenSessionSource>,
        keeper: KeeperPreparer,
        sweeper: Arc<StraySweeper>,
    ) -> Self {
        Self {
            manager,
            pool,
            sessions,
            keeper,
            sweeper,
            boot_survivors_unchecked: AtomicBool::new(true),
        }
    }

    /// One pass. The rows are read here, after the gate saw the durable replay
    /// drain (v2 reads inside every pass).
    pub async fn pass(&self, reason: &'static str) -> Result<ReconcileSummary, ReconcileFailure> {
        let OpenSessionSet { rows } = self.read_rows().await?;
        let admissions = self.admit_all(&rows).await?;
        // Survivor retirement, keeper creation and the survivor-set capacity
        // check all come after the complete reservation batch (v2 `:172-176`).
        let prepared = match self.keeper.prepare(&self.pool, rows.len()).await {
            Ok(admitted_now) => self.admit_survivor_set(
                self.boot_survivors_unchecked.swap(false, Ordering::AcqRel) || admitted_now,
            ),
            Err(failure) => Err(failure),
        };
        if let Err(failure) = prepared {
            release_admissions(admissions).await;
            return Err(failure);
        }
        self.sweeper.start_post_admission_maintenance();
        if let Err(fault) = self.manager.advance_past_keeper() {
            tracing::warn!(%fault, "reconcile: the keeper's channel list could not be read to advance channel ids");
        }
        let mut summary = ReconcileSummary {
            candidates: admissions.len(),
            ..ReconcileSummary::default()
        };
        let mut respawn_failed = 0usize;
        let open_sessions = rows.len();
        let mut pending = admissions.into_iter();
        while let Some(admission) = pending.next() {
            match self.resume_or_respawn(admission, open_sessions).await {
                Ok(Outcome::Resumed) => summary.resumed += 1,
                Ok(Outcome::Respawned) => summary.respawned += 1,
                Ok(Outcome::Unresolved) => respawn_failed += 1,
                Err(failure) => {
                    release_admissions(pending.collect()).await;
                    return Err(failure);
                }
            }
        }
        if respawn_failed > 0 {
            return Err(ReconcileFailure::recoverable(format!(
                "reconcile left {respawn_failed} coordinator session(s) unresolved"
            )));
        }
        summary.strays_reaped = self.sweeper.reap_stray_keeper_channels().await;
        tracing::info!(
            reason,
            candidates = summary.candidates,
            resumed = summary.resumed,
            respawned = summary.respawned,
            respawn_failed = 0,
            strays_reaped = summary.strays_reaped,
            "worker: resume_attempted"
        );
        Ok(summary)
    }

    /// v2 `handleKeeperSurvivor`'s capacity gate: every channel of a keeper
    /// this pass just admitted must fit before any is attached, so capacity
    /// never admits a partial set. A keeper this worker already drives holds
    /// channels whose cores are already counted, so it is not re-admitted.
    fn admit_survivor_set(&self, admitted: bool) -> Result<(), ReconcileFailure> {
        if !admitted {
            return Ok(());
        }
        let survivors = self
            .pool
            .keeper_channels()
            .map_err(|error| ReconcileFailure::recoverable(error.to_string()))?
            .iter()
            .map(|held| held.channel_id)
            .collect::<std::collections::HashSet<_>>()
            .len();
        let capacity = self.manager.terminal_core_capacity();
        capacity
            .assert_can_adopt_survivors(survivors)
            .map_err(|refusal| {
                let snapshot = capacity.snapshot();
                tracing::error!(
                    survivor_channels = survivors,
                    capacity = snapshot.capacity,
                    used = snapshot.used,
                    pending = snapshot.pending,
                    refusal_count = snapshot.refusal_count,
                    %refusal,
                    "worker: keeper_survivor_capacity_refused"
                );
                ReconcileFailure::recoverable(refusal.to_string())
            })
    }

    /// v2 `:178-308`: adopt, or respawn with bounded transient retries.
    async fn resume_or_respawn(
        &self,
        admission: Admission,
        open_sessions: usize,
    ) -> Result<Outcome, ReconcileFailure> {
        let Admission {
            session_id,
            channel_id,
            cwd,
            shell_spec,
            resume_close,
            respawn_event,
            future_close,
        } = admission;
        let request = AdoptionRequest {
            session_id: session_id.clone(),
            channel_id,
            folder: shell_spec.cwd.clone(),
            shell_spec: shell_spec.clone(),
            close_reservation: resume_close.disarm(),
        };
        match self.manager.adopt_survivor(&request).await {
            Ok(_) => {
                respawn_event.release().await;
                future_close.release().await;
                return Ok(Outcome::Resumed);
            }
            Err(AdoptFailure {
                refusal: AdoptRefusal::TerminalCoreCapacity { refusal, .. },
                ..
            }) => {
                respawn_event.release().await;
                future_close.release().await;
                return Err(ReconcileFailure::recoverable(refusal.to_string()));
            }
            Err(failure) => {
                tracing::info!(session_id = %session_id, refusal = %failure.refusal, killed = failure.abandoned, "reconcile: not adopted; respawning")
            }
        }
        for attempt in 1..=RESPAWN_ATTEMPTS {
            let respawn = RespawnRequest {
                session_id: session_id.clone(),
                cwd: cwd.clone(),
                shell_spec: Some(shell_spec.clone()),
                cols: None,
                rows: None,
            };
            match self
                .manager
                .respawn_session(
                    respawn,
                    respawn_event.reservation(),
                    future_close.reservation(),
                    ClaimsOnFailure::Keep,
                )
                .await
            {
                Ok(_) => {
                    respawn_event.disarm();
                    future_close.disarm();
                    return Ok(Outcome::Respawned);
                }
                Err(RespawnError::Durability(reason)) => {
                    respawn_event.release().await;
                    future_close.release().await;
                    return Err(ReconcileFailure {
                        reason,
                        fatal: true,
                    });
                }
                Err(RespawnError::Capacity(refusal)) => {
                    respawn_event.release().await;
                    future_close.release().await;
                    return Err(ReconcileFailure::recoverable(refusal.to_string()));
                }
                Err(RespawnError::Failed(reason)) => {
                    let transient = is_transient(&reason);
                    if attempt < RESPAWN_ATTEMPTS && transient {
                        tracing::info!(session_id = %session_id, attempt, error = %reason, "worker: respawn_retry_transient");
                        if let Err(failure) = self.keeper.prepare(&self.pool, open_sessions).await {
                            tracing::debug!(%failure, "the bounded retry reports the final failure");
                        }
                        tokio::time::sleep(Duration::from_millis(u64::from(attempt) * 400)).await;
                        continue;
                    }
                    tracing::warn!(session_id = %session_id, %cwd, error = %reason, transient, after_retry = attempt > 1, "worker: respawn_failed");
                    respawn_event.release().await;
                    if transient {
                        future_close.release().await;
                    } else {
                        self.manager
                            .emit_closed_tombstone(&session_id, Some(future_close.disarm()))
                            .await;
                    }
                    return Ok(Outcome::Unresolved);
                }
            }
        }
        Ok(Outcome::Unresolved)
    }
}

impl ReconcilePass for SessionReconciler {
    fn run(self: Arc<Self>, reason: &'static str) -> OwnerFuture<ReconcileOutcome> {
        Box::pin(async move { self.pass(reason).await })
    }
}

/// Give back every claim of admissions the pass will not reach (v2 `finally`).
pub(crate) async fn release_admissions(admissions: Vec<Admission>) {
    for admission in admissions {
        admission.future_close.release().await;
        admission.respawn_event.release().await;
        admission.resume_close.release().await;
    }
}

enum Outcome {
    Resumed,
    Respawned,
    Unresolved,
}

/// v2 `/socket closed|not connected|ENOTCONN|not ready|timeout|SpawnErr|keeper/i`.
pub fn is_transient(error: &str) -> bool {
    let lowered = error.to_ascii_lowercase();
    [
        "socket closed",
        "not connected",
        "enotconn",
        "not ready",
        "timeout",
        "spawnerr",
        "keeper",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}
