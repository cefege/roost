//! The agent status hub: one table of what every agent in the fleet is doing,
//! and the one place a report from a worker is allowed to change it.
//!
//! Ported from `apps/coord/src/agents/agent-status-hub.ts`. v2's
//! `handleWorkerAgentStatus` becomes [`AgentStatusHub::accept_worker_status`];
//! the module-global maps become one value on `CoordServices`, because a second
//! instance would be a second answer to "is that agent still running".
//!
//! OWNERSHIP IS NOT THE PAYLOAD'S TO CLAIM: a frame is accepted only when the
//! byte hub's route cache names the sending worker as that session's owner.
//! THE APPLY-IF-NEWER CHECK LIVES IN EXACTLY ONE PLACE: every path that can
//! change a retained row consults [`AgentStatusOrder`] before it mutates, and a
//! refused frame leaves no trace. Workers reconnect, retry and reorder, so a
//! late report is DROPPED.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use roost_observability::LogFields;
use roost_protocol::wire::agent_status::agent_status_identity;
use roost_protocol::wire::{
    AgentOccupantId, AgentRuntimeState, AgentStatus, AgentStatusFields, AgentStatusUpdate, SessionId,
    StatusEpoch, WorkerFp,
};
use serde_json::Value;

use crate::agents::status_order::AgentStatusOrder;
use crate::agents::status_push::{
    AGENT_STATUS_PUSH_DELAY, AgentStatusPushDelivery, AgentStatusPushSchedule, CurrentAgentStatus,
};
use crate::agents::status_wait::{
    AGENT_STATUS_WAIT_MAX_TIMEOUT_MS, AgentStatusWaitError, AgentStatusWaitRegistry,
    AgentStatusWaitRequest, AgentStatusWaitView, AgentStatusWaiter,
};
use crate::coord_core::CoordCore;
use crate::events::bus_domains::Buses;
/// The ceiling a synthesized revision is held to, so it survives a JSON peer.
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// What the hub did with one worker status frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatusAcceptance {
    /// Applied, and published.
    Accepted,
    /// Not a status this coordinator can hold; nothing changed.
    Invalid,
    /// A newer report already won; the frame was dropped.
    Stale,
    /// No session is bound to that id, so nothing may be claimed for it.
    UnknownSession,
    /// The frame came from a worker that does not own that session.
    WrongWorker,
}

/// The retained tables, all of which only the hub mutates.
#[derive(Default)]
struct HubTables {
    /// The live status of every session that has one, in session-id order. A
    /// `BTreeMap` because THE ORDER IS PART OF THE ANSWER: a client that
    /// re-fetches and a client that applies broadcasts must converge, and the
    /// cheapest guarantee is one place the order can come from.
    active: BTreeMap<SessionId, AgentStatus>,
    /// Per-session admission order, kept on past the status it fences.
    order: BTreeMap<SessionId, AgentStatusOrder>,
    /// When each session closed, until no wait could still be pinned to it.
    tombstones: BTreeMap<SessionId, i64>,
}

struct HubState {
    tables: Mutex<HubTables>,
    waits: AgentStatusWaitRegistry,
    push: AgentStatusPushSchedule,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

/// The coordinator's one table of agent status.
#[derive(Clone)]
pub struct AgentStatusHub {
    state: Arc<HubState>,
}

impl std::fmt::Debug for AgentStatusHub {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentStatusHub")
            .field("sessions", &self.snapshot().len())
            .field("waits", &self.wait_count())
            .finish()
    }
}

impl Default for AgentStatusHub {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentStatusHub {
    /// A hub that has never seen an agent.
    #[must_use]
    pub fn new() -> Self {
        Self::with_seams(Arc::new(crate::serve::now_ms), AGENT_STATUS_PUSH_DELAY)
    }

    /// A hub whose clock and push debounce the caller supplies.
    #[must_use]
    pub fn with_seams(
        now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
        push_delay: Duration,
    ) -> Self {
        Self {
            state: Arc::new(HubState {
                tables: Mutex::new(HubTables::default()),
                waits: AgentStatusWaitRegistry::new(),
                push: AgentStatusPushSchedule::new(push_delay),
                now_ms,
            }),
        }
    }

    /// Install the process-wide push delivery, once at boot.
    pub fn install_push_delivery(&self, delivery: Arc<dyn AgentStatusPushDelivery>) {
        self.state.push.install_delivery(delivery);
    }

    /// Validate and retain one status frame from an authenticated worker link.
    pub fn accept_worker_status(
        &self,
        core: &CoordCore,
        worker_fp: &WorkerFp,
        input: Value,
    ) -> AgentStatusAcceptance {
        self.state.sweep_tombstones();
        let update = match AgentStatusUpdate::parse(input) {
            Ok(update) => update,
            Err(error) => {
                roost_observability::log::warn(
                    "agents.status",
                    "frame_dropped",
                    LogFields::new()
                        .set("reason", "invalid")
                        .set("worker_fp", worker_fp.as_str())
                        .set("error", error.to_string()),
                );
                return AgentStatusAcceptance::Invalid;
            }
        };
        let session_id = update.common.session_id.clone();
        let Some(route) = core.services.byte_hub.cached_route(&session_id) else {
            roost_observability::log::info(
                "agents.status",
                "frame_dropped",
                LogFields::new()
                    .set("reason", "unknown_session")
                    .set("worker_fp", worker_fp.as_str())
                    .set("session_id", session_id.as_str()),
            );
            return AgentStatusAcceptance::UnknownSession;
        };
        if &route.worker_fp != worker_fp {
            roost_observability::log::warn(
                "agents.status",
                "worker_mismatch",
                LogFields::new()
                    .set("worker_fp", worker_fp.as_str())
                    .set("owner_worker_fp", route.worker_fp.as_str())
                    .set("session_id", session_id.as_str()),
            );
            return AgentStatusAcceptance::WrongWorker;
        }
        self.apply(&core.services.buses, update)
    }

    /// A session closed: fence it, release its waits, and publish the
    /// synthetic deletion that retires the occupant it held.
    ///
    /// Takes the buses rather than the whole core, because the only place a
    /// session closes is the event publisher, which already holds exactly this.
    pub fn note_session_closed(&self, buses: &Buses, session_id: &SessionId) {
        self.close_session(buses, session_id);
    }

    /// A session reopened: drop the close fence, but NOT the retired occupant,
    /// which stays fenced so a reconnect cannot resurrect it.
    pub fn note_session_opened(&self, session_id: &SessionId) {
        self.state
            .lock(&self.state.tables)
            .tombstones
            .remove(session_id);
    }

    /// End every wait and forget every row, for a stopping hub.
    pub fn stop(&self) {
        self.state.waits.cancel_all();
        self.state.push.clear();
        let mut tables = self.state.lock(&self.state.tables);
        tables.active.clear();
        tables.order.clear();
        tables.tombstones.clear();
    }

    /// Every retained status, in session-id order. The list RPC and a new Sync
    /// subscriber's seed both read THIS, which is what makes a re-fetch and a
    /// broadcast agree.
    #[must_use]
    pub fn snapshot(&self) -> Vec<AgentStatus> {
        self.state
            .lock(&self.state.tables)
            .active
            .values()
            .cloned()
            .collect()
    }

    /// What one session's retained status is.
    #[must_use]
    pub fn status_for(&self, session_id: &SessionId) -> Option<AgentStatus> {
        self.state
            .lock(&self.state.tables)
            .active
            .get(session_id)
            .cloned()
    }

    /// The retained state of one exact pinned occupant, for a pre-prompt
    /// activity check.
    #[must_use]
    pub fn retained_occupant_state(
        &self,
        session_id: &SessionId,
        status_epoch: &StatusEpoch,
        occupant_id: &AgentOccupantId,
    ) -> Option<AgentRuntimeState> {
        let status = self.status_for(session_id)?;
        let identity = agent_status_identity(&status.common)?;
        (identity.status_epoch == *status_epoch && identity.occupant_id == *occupant_id)
            .then_some(status.common.state)
    }

    /// Register a bounded wait. It is registered BEFORE it is evaluated, so a
    /// change landing between the client's last read and this call is not
    /// missed.
    pub fn wait_for_agent_status(
        &self,
        request: AgentStatusWaitRequest,
    ) -> Result<AgentStatusWaiter, AgentStatusWaitError> {
        let waiter = self.state.waits.register(request)?;
        self.state
            .waits
            .evaluate(&self.state, &waiter.request().session_id);
        Ok(waiter)
    }

    /// How many waits are registered.
    #[must_use]
    pub fn wait_count(&self) -> usize {
        self.state.waits.waiter_count()
    }

    /// The one mutation point: validate against the retained order, then
    /// publish, wake the waits, and arm the push debounce.
    fn apply(&self, buses: &Buses, update: AgentStatusUpdate) -> AgentStatusAcceptance {
        let session_id = update.common.session_id.clone();
        let previous = {
            let mut tables = self.state.lock(&self.state.tables);
            if tables.tombstones.contains_key(&session_id) {
                return AgentStatusAcceptance::Stale;
            }
            let previous = tables.active.get(&session_id).cloned();
            let order = tables.order.entry(session_id.clone()).or_default();
            if !order.accepts(previous.as_ref(), &update) {
                return AgentStatusAcceptance::Stale;
            }
            order.record(&update);
            if update.active {
                tables.active.insert(
                    session_id.clone(),
                    AgentStatus {
                        common: update.common.clone(),
                        active: true,
                    },
                );
            } else {
                tables.active.remove(&session_id);
            }
            previous
        };
        self.state.waits.evaluate(&self.state, &session_id);
        buses.agent_status_bus.publish(update.clone());
        roost_observability::log::debug(
            "agents.status",
            "status_accepted",
            LogFields::new()
                .set("session_id", session_id.as_str())
                .set("active", update.active)
                .set("revision", update.common.revision),
        );
        let table: Arc<dyn CurrentAgentStatus> = Arc::clone(&self.state);
        self.state.push.arm(table, previous.as_ref(), &update);
        AgentStatusAcceptance::Accepted
    }

    /// A session closed. Waiters are released FIRST, so a client learns its
    /// session is gone rather than that its occupant was replaced.
    fn close_session(&self, buses: &Buses, session_id: &SessionId) {
        self.state.sweep_tombstones();
        self.state
            .lock(&self.state.tables)
            .tombstones
            .insert(session_id.clone(), (self.state.now_ms)());
        self.state.waits.close_session(session_id);
        self.state.push.cancel(session_id);
        let current = self
            .state
            .lock(&self.state.tables)
            .active
            .remove(session_id);
        let Some(current) = current else {
            return;
        };
        let inactive = AgentStatusUpdate {
            common: AgentStatusFields {
                revision: current
                    .common
                    .revision
                    .saturating_add(1)
                    .min(MAX_SAFE_INTEGER),
                updated_at: (self.state.now_ms)().max(current.common.updated_at),
                ..current.common.clone()
            },
            active: false,
        };
        self.state
            .lock(&self.state.tables)
            .order
            .entry(session_id.clone())
            .or_default()
            .record_close(&current, inactive.common.revision);
        buses.agent_status_bus.publish(inactive);
    }
}

impl CurrentAgentStatus for HubState {
    fn current(&self, session_id: &SessionId) -> Option<AgentStatus> {
        self.lock(&self.tables).active.get(session_id).cloned()
    }
}

impl AgentStatusWaitView for HubState {
    fn retained(&self, session_id: &SessionId) -> Option<AgentStatus> {
        self.current(session_id)
    }

    fn is_closed(&self, session_id: &SessionId) -> bool {
        self.lock(&self.tables).tombstones.contains_key(session_id)
    }

    fn state_change_revision(&self, session_id: &SessionId) -> i64 {
        self.lock(&self.tables)
            .order
            .get(session_id)
            .map_or(0, AgentStatusOrder::state_change_revision)
    }
}

impl HubState {
    /// Drop close fences nothing may consult any more. A closed session's fence
    /// and its admission order expire together; kept forever instead, both maps
    /// would gain one entry per closed session for the life of the process.
    fn sweep_tombstones(&self) {
        let expired_at_or_before = (self.now_ms)() - AGENT_STATUS_WAIT_MAX_TIMEOUT_MS as i64;
        let mut swept = 0_u64;
        {
            let mut tables = self.lock(&self.tables);
            let stale: Vec<SessionId> = tables
                .tombstones
                .iter()
                .filter(|(_, closed_at_ms)| **closed_at_ms <= expired_at_or_before)
                .map(|(session_id, _)| session_id.clone())
                .collect();
            for session_id in stale {
                tables.tombstones.remove(&session_id);
                tables.order.remove(&session_id);
                swept += 1;
            }
        }
        if swept > 0 {
            roost_observability::log::info(
                "agents.status",
                "tombstones_swept",
                LogFields::new().set("count", swept),
            );
        }
    }

    fn lock<T>(&self, mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
