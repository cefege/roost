//! Aggregates ancestry-verified integration and screen observations into the
//! identified `AgentStatusUpdate` frames sent to the coordinator. Ports v2
//! `apps/worker/src/agents/registry.ts`. One registry owns one worker epoch;
//! each uninterrupted agent-kind/pid incarnation owns one occupant token.
//! Fed by `agents::detector` (screen) and `agents::report_server`
//! (integration); read by `agents::prompt_control`; publishes through the
//! uplink to `runtime::link_loop::agent_status`. The transitions live in
//! `registry_recompute.rs`.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use roost_observability::clock::{EventClock, SystemClock};
use roost_protocol::wire::agent_status::{
    AgentOccupantId, AgentRuntimeState, AgentStatusSource, AgentStatusUpdate, StatusEpoch,
};
use roost_protocol::wire::brand::SessionId;

use super::BuiltinAgentId;
use super::process_scan::AgentProcessIdentity;
use super::registry_recompute::{Recompute, effective_frame};
use crate::agent_occupancy::{
    CandidateLoss, IntegrationCandidate, ProcessCandidate, ScreenCandidate, SessionEntry,
    process_key,
};
use crate::session::ids::{MintError, mint_uuid};

/// v2 `INTEGRATION_LEASE_MS`: an integration that stops reporting for this
/// long is treated as gone.
pub const INTEGRATION_LEASE_MS: i64 = 30_000;

/// v2's lease timer interval (`setInterval(expireLeases, 1_000)`).
pub const LEASE_SWEEP_INTERVAL: Duration = Duration::from_secs(1);

/// v2 `IntegrationStatusReport`: one admitted integration report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationStatusReport {
    pub session_id: SessionId,
    pub agent_id: BuiltinAgentId,
    pub process_id: u32,
    pub state: AgentRuntimeState,
    pub message: Option<String>,
    pub seq: u64,
    pub active: bool,
}

/// v2 `ScreenStatusReport`: one stabilised screen observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenStatusReport {
    pub agent_id: BuiltinAgentId,
    pub process_id: u32,
    pub state: AgentRuntimeState,
    pub visible_blocker: bool,
}

/// Exact status fence plus the process identity that proved it. Process ids
/// never leave the worker; prompt admission compares this proof twice around
/// the shared keeper-input queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatusPrivateProof {
    pub status_epoch: StatusEpoch,
    pub occupant_id: AgentOccupantId,
    pub revision: i64,
    pub state: AgentRuntimeState,
    pub source: AgentStatusSource,
    pub process: AgentProcessIdentity,
}

/// Where the registry's frames go (v2 `publish`). Production: the uplink.
pub trait AgentStatusPublisher: Send + Sync {
    fn publish(&self, status: AgentStatusUpdate);
}

/// v2 `AgentStatusRegistryOptions`, minus the timer flag: the lease sweep is
/// started explicitly by [`AgentStatusRegistry::spawn_lease_sweep`].
pub struct AgentStatusRegistryOptions {
    pub publish: Arc<dyn AgentStatusPublisher>,
    pub clock: Arc<dyn EventClock>,
    pub lease_ms: i64,
}

pub(super) struct RegistryState {
    pub(super) entries: BTreeMap<SessionId, SessionEntry>,
    pub(super) revision: i64,
}

/// v2 `AgentStatusRegistry`.
pub struct AgentStatusRegistry {
    publish: Arc<dyn AgentStatusPublisher>,
    clock: Arc<dyn EventClock>,
    lease_ms: i64,
    status_epoch: StatusEpoch,
    state: Mutex<RegistryState>,
    lease_sweep: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for AgentStatusRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentStatusRegistry")
            .field("status_epoch", &self.status_epoch)
            .field("lease_ms", &self.lease_ms)
            .finish_non_exhaustive()
    }
}

impl AgentStatusRegistry {
    /// A registry with a fresh status epoch. The revision starts at the wall
    /// clock in microseconds, as v2's does, so a restarted worker's revisions
    /// never run behind the ones it published before.
    pub fn new(options: AgentStatusRegistryOptions) -> Result<Arc<Self>, MintError> {
        let status_epoch = StatusEpoch::try_from(mint_uuid()?.as_str())
            .map_err(|error| MintError::Entropy(error.to_string()))?;
        tracing::info!(status_epoch = %status_epoch.as_str(), "the agent-status registry opened an epoch");
        Ok(Arc::new(Self {
            publish: options.publish,
            clock: options.clock,
            lease_ms: options.lease_ms,
            status_epoch,
            state: Mutex::new(RegistryState {
                entries: BTreeMap::new(),
                revision: SystemClock.now_epoch_ms().saturating_mul(1_000),
            }),
            lease_sweep: Mutex::new(None),
        }))
    }

    pub fn status_epoch(&self) -> &StatusEpoch {
        &self.status_epoch
    }

    /// v2's lease timer: expire integrations every second until disposed.
    pub fn spawn_lease_sweep(self: &Arc<Self>, runtime: &tokio::runtime::Handle) {
        let registry = Arc::downgrade(self);
        let task = runtime.spawn(async move {
            let mut ticker = tokio::time::interval(LEASE_SWEEP_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(registry) = registry.upgrade() else {
                    return;
                };
                registry.expire_leases();
            }
        });
        if let Some(previous) = lock(&self.lease_sweep).replace(task) {
            previous.abort();
        }
    }

    fn lock(&self) -> MutexGuard<'_, RegistryState> {
        lock(&self.state)
    }

    fn recompute(
        &self,
        state: &mut RegistryState,
        session_id: &SessionId,
        now: i64,
        loss: CandidateLoss,
    ) {
        Recompute {
            publish: &*self.publish,
            status_epoch: &self.status_epoch,
        }
        .run(state, session_id, now, loss);
    }

    /// v2 `reportIntegration`: `false` for a retired reporter or a stale seq.
    pub fn report_integration(&self, report: IntegrationStatusReport) -> bool {
        let mut state = self.lock();
        let entry = state.entries.entry(report.session_id.clone()).or_default();
        let reporter_key = process_key(report.agent_id, report.process_id);
        if entry.retired_process_keys.contains(&reporter_key) {
            return false;
        }
        let previous_seq = entry.integration_seq_by_process.get(&reporter_key);
        if previous_seq.is_some_and(|previous| report.seq <= *previous) {
            return false;
        }
        entry
            .integration_seq_by_process
            .insert(reporter_key, report.seq);
        let now = self.clock.now_epoch_ms();
        let mut loss = CandidateLoss::Exit;
        if report.active {
            entry.integration = Some(IntegrationCandidate {
                process: ProcessCandidate {
                    agent_id: report.agent_id,
                    process_id: report.process_id,
                    process_key: reporter_key,
                    state: report.state,
                },
                message: report.message,
                seq: report.seq,
                lease_until: now.saturating_add(self.lease_ms),
            });
        } else if entry
            .integration
            .as_ref()
            .is_some_and(|integration| integration.process.process_key == reporter_key)
        {
            entry.integration = None;
            loss = CandidateLoss::Withdrawn;
        }
        self.recompute(&mut state, &report.session_id, now, loss);
        true
    }

    /// v2 `reportScreen`: `false` while a retired process is still on screen.
    pub fn report_screen(&self, session_id: &SessionId, report: ScreenStatusReport) -> bool {
        let mut state = self.lock();
        let entry = state.entries.entry(session_id.clone()).or_default();
        let observed_key = process_key(report.agent_id, report.process_id);
        if entry.retired_process_keys.contains(&observed_key) {
            if !entry.screen_absence_observed {
                return false;
            }
            entry.retired_process_keys.remove(&observed_key);
            entry.integration_seq_by_process.remove(&observed_key);
        }
        entry.screen_absence_observed = false;
        entry.screen = Some(ScreenCandidate {
            process: ProcessCandidate {
                agent_id: report.agent_id,
                process_id: report.process_id,
                process_key: observed_key,
                state: report.state,
            },
            visible_blocker: report.visible_blocker,
        });
        let now = self.clock.now_epoch_ms();
        self.recompute(&mut state, session_id, now, CandidateLoss::Exit);
        true
    }

    /// v2 `clearScreen`: the screen shows no agent any more.
    pub fn clear_screen(&self, session_id: &SessionId) {
        let mut state = self.lock();
        let Some(entry) = state.entries.get_mut(session_id) else {
            return;
        };
        if entry.screen.take().is_some() {
            let now = self.clock.now_epoch_ms();
            self.recompute(&mut state, session_id, now, CandidateLoss::Exit);
        }
        if let Some(entry) = state.entries.get_mut(session_id) {
            entry.screen_absence_observed = true;
        }
    }

    /// v2 `expireLeases` at the registry clock's now.
    pub fn expire_leases(&self) {
        self.expire_leases_at(self.clock.now_epoch_ms());
    }

    pub fn expire_leases_at(&self, now: i64) {
        let mut state = self.lock();
        let expired: Vec<SessionId> = state
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry
                    .integration
                    .as_ref()
                    .is_some_and(|integration| integration.lease_until <= now)
            })
            .map(|(session_id, _)| session_id.clone())
            .collect();
        for session_id in expired {
            self.recompute(&mut state, &session_id, now, CandidateLoss::Exit);
        }
    }

    /// v2 `closeSession`: withdraw everything and forget the session.
    pub fn close_session(&self, session_id: &SessionId) {
        let mut state = self.lock();
        let Some(entry) = state.entries.get_mut(session_id) else {
            return;
        };
        entry.integration = None;
        entry.screen = None;
        let now = self.clock.now_epoch_ms();
        self.recompute(&mut state, session_id, now, CandidateLoss::Withdrawn);
        state.entries.remove(session_id);
    }

    /// v2 `retainSessions`: close every session not in `live`.
    pub fn retain_sessions(&self, live: &HashSet<SessionId>) {
        let gone: Vec<SessionId> = self
            .lock()
            .entries
            .keys()
            .filter(|session_id| !live.contains(*session_id))
            .cloned()
            .collect();
        for session_id in gone {
            self.close_session(&session_id);
        }
    }

    /// v2 `currentPrivateProof`. A prompt cannot use an integration row during
    /// the lease timer's one-second sweep gap, nor the row an exited agent
    /// left behind to carry its completion.
    pub fn current_private_proof(&self, session_id: &SessionId) -> Option<AgentStatusPrivateProof> {
        let mut state = self.lock();
        let now = self.clock.now_epoch_ms();
        self.recompute(&mut state, session_id, now, CandidateLoss::Exit);
        let effective = state.entries.get(session_id)?.effective.as_ref()?;
        if !effective.occupant_live {
            return None;
        }
        Some(AgentStatusPrivateProof {
            status_epoch: self.status_epoch.clone(),
            occupant_id: effective.occupant_id.clone(),
            revision: effective.revision,
            state: effective.process.state,
            source: effective.source,
            process: AgentProcessIdentity {
                agent_id: effective.process.agent_id,
                pid: effective.process.process_id,
                foreground: None,
            },
        })
    }

    /// v2 `resend`: republish every retained occupant, unchanged. Called when
    /// the coordinator link's snapshot barrier clears.
    pub fn resend(&self) {
        let state = self.lock();
        let frames = snapshot_of(&state, &self.status_epoch);
        tracing::info!(
            statuses = frames.len(),
            "the agent-status registry resent its occupants"
        );
        for frame in frames {
            self.publish.publish(frame);
        }
    }

    /// v2 `snapshot`: every retained occupant as an active frame.
    pub fn snapshot(&self) -> Vec<AgentStatusUpdate> {
        snapshot_of(&self.lock(), &self.status_epoch)
    }

    /// v2 `dispose`: stop the lease sweep and forget every session.
    pub fn dispose(&self) {
        if let Some(task) = lock(&self.lease_sweep).take() {
            task.abort();
        }
        self.lock().entries.clear();
        tracing::info!("the agent-status registry was disposed");
    }
}

fn snapshot_of(state: &RegistryState, status_epoch: &StatusEpoch) -> Vec<AgentStatusUpdate> {
    state
        .entries
        .iter()
        .filter_map(|(session_id, entry)| {
            let effective = entry.effective.as_ref()?;
            effective_frame(
                session_id,
                status_epoch,
                effective,
                true,
                effective.revision,
                effective.updated_at,
            )
        })
        .collect()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
