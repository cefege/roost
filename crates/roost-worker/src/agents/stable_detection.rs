//! Stabilizes screen-derived state for one observed agent process. Ports v2
//! `apps/worker/src/agents/stable-detection.ts`. The process identity is part
//! of the key so pid replacement cannot inherit pending idle confirmation or
//! disappear behind an unchanged agent kind and state. Owned by
//! `agents::detector`, which feeds it manifest detections and forwards what it
//! reports to `agents::registry`.

use std::collections::{HashMap, HashSet};

use roost_protocol::wire::agent_status::AgentRuntimeState;
use roost_protocol::wire::brand::SessionId;

use super::BuiltinAgentId;
use super::manifest_engine::ManifestDetection;
use super::process_scan::AgentProcessIdentity;
use super::registry::ScreenStatusReport;

/// v2 `PENDING_IDLE_CONFIRMATIONS`.
const PENDING_IDLE_CONFIRMATIONS: u32 = 3;
/// v2 `PENDING_IDLE_CAP_MS`.
const PENDING_IDLE_CAP_MS: i64 = 700;
/// v2 `AGENT_ACQUISITION_GRACE_MS`: a freshly acquired agent is judged by
/// whatever is on the grid at the moment the process probe recognized it — a
/// half-painted TUI, or the previous program's leftover screen. The first
/// evaluation is withheld until a later observation agrees or this expires.
const AGENT_ACQUISITION_GRACE_MS: i64 = 3_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct StableEntry {
    agent_id: BuiltinAgentId,
    process_id: u32,
    state: AgentRuntimeState,
    visible_blocker: bool,
    visible_idle: bool,
    visible_working: bool,
    pending_idle_started_at: Option<i64>,
    pending_idle_confirmations: u32,
    acquired_at: i64,
    acquisition_grace_open: bool,
}

impl StableEntry {
    fn report(&self) -> ScreenStatusReport {
        ScreenStatusReport {
            agent_id: self.agent_id,
            process_id: self.process_id,
            state: self.state,
            visible_blocker: self.visible_blocker,
        }
    }
}

/// v2 `StableScreenDetector`.
#[derive(Debug, Default)]
pub struct StableScreenDetector {
    entries: HashMap<SessionId, StableEntry>,
}

impl StableScreenDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// v2 `observe`: a report when the stabilised screen state changed.
    pub fn observe(
        &mut self,
        session_id: &SessionId,
        identity: &AgentProcessIdentity,
        detection: &ManifestDetection,
        now: i64,
    ) -> Option<ScreenStatusReport> {
        let previous = self.entries.get(session_id);
        let identity_changed = previous.is_some_and(|previous| {
            previous.agent_id != identity.agent_id || previous.process_id != identity.pid
        });
        let state = match detection.state {
            Some(state) if !detection.skip_state_update => state,
            _ => {
                if identity_changed {
                    self.entries.remove(session_id);
                }
                return None;
            }
        };

        let mut next = StableEntry {
            agent_id: identity.agent_id,
            process_id: identity.pid,
            state,
            visible_idle: detection.visible_idle,
            visible_blocker: detection.visible_blocker,
            visible_working: detection.visible_working,
            pending_idle_started_at: None,
            pending_idle_confirmations: 0,
            acquired_at: match previous {
                Some(previous) if !identity_changed => previous.acquired_at,
                _ => now,
            },
            acquisition_grace_open: false,
        };
        let Some(previous) = previous.filter(|_| !identity_changed) else {
            next.acquisition_grace_open = true;
            self.entries.insert(session_id.clone(), next);
            return None;
        };
        if previous.acquisition_grace_open {
            let repeated = previous.state == next.state;
            let expired = now - previous.acquired_at >= AGENT_ACQUISITION_GRACE_MS;
            if !repeated && !expired {
                next.acquisition_grace_open = true;
                self.entries.insert(session_id.clone(), next);
                return None;
            }
            let report = next.report();
            self.entries.insert(session_id.clone(), next);
            tracing::debug!(session = %session_id, state = state.as_str(), "an acquired agent's screen state settled");
            return Some(report);
        }

        let plain_working_to_idle = previous.state == AgentRuntimeState::Working
            && next.state == AgentRuntimeState::Idle
            && !next.visible_idle
            && !next.visible_blocker;
        let changed = previous.state != next.state
            || previous.visible_idle != next.visible_idle
            || previous.visible_blocker != next.visible_blocker
            || previous.visible_working != next.visible_working;
        if plain_working_to_idle {
            let entry = self.entries.get_mut(session_id)?;
            let Some(started_at) = entry.pending_idle_started_at else {
                entry.pending_idle_started_at = Some(now);
                entry.pending_idle_confirmations = 0;
                return None;
            };
            if now - started_at < PENDING_IDLE_CAP_MS {
                entry.pending_idle_confirmations += 1;
                if entry.pending_idle_confirmations < PENDING_IDLE_CONFIRMATIONS {
                    return None;
                }
            }
        }
        let report = next.report();
        self.entries.insert(session_id.clone(), next);
        changed.then_some(report)
    }

    /// v2 `current`: the stabilised state held for a session, if any.
    pub fn current(&self, session_id: &SessionId) -> Option<ScreenStatusReport> {
        self.entries.get(session_id).map(StableEntry::report)
    }

    pub fn release(&mut self, session_id: &SessionId) {
        self.entries.remove(session_id);
    }

    pub fn retain(&mut self, live: &HashSet<SessionId>) {
        self.entries
            .retain(|session_id, _| live.contains(session_id));
    }
}
