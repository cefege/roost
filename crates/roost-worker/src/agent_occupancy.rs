//! The private occupancy model behind the agent-status registry: the
//! per-process candidates a session can offer, the effective occupant
//! published from them, and why a candidate went away. Ports v2
//! `apps/worker/src/agents/occupancy.ts`. `agents::registry` owns every
//! transition over these shapes; nothing here reaches the wire, so process ids
//! stay worker-private.

use std::collections::{HashMap, HashSet};

use roost_protocol::wire::agent_status::{AgentOccupantId, AgentRuntimeState, AgentStatusSource};

use crate::agents::BuiltinAgentId;

/// v2 `processKey`: the (agent kind, pid) pair one incarnation is known by.
/// The kind is part of it because a restart can hand the same numeric pid to a
/// different agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessKey {
    pub agent_id: BuiltinAgentId,
    pub process_id: u32,
}

pub fn process_key(agent_id: BuiltinAgentId, process_id: u32) -> ProcessKey {
    ProcessKey {
        agent_id,
        process_id,
    }
}

/// v2 `ProcessCandidate`: one process a session believes is an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCandidate {
    pub agent_id: BuiltinAgentId,
    pub process_id: u32,
    pub process_key: ProcessKey,
    pub state: AgentRuntimeState,
}

/// v2 `IntegrationCandidate`: an integration's report, alive until its lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationCandidate {
    pub process: ProcessCandidate,
    pub message: Option<String>,
    pub seq: u64,
    pub lease_until: i64,
}

/// v2 `ScreenCandidate`. It carries whether the manifest matched a rule marked
/// `visible_blocker`: an on-screen prompt is direct evidence a human is being
/// waited on, and it may correct an integration that is not a full-lifecycle
/// state authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenCandidate {
    pub process: ProcessCandidate,
    pub visible_blocker: bool,
}

/// v2 `EffectiveEntry`: the occupant a session publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveEntry {
    pub process: ProcessCandidate,
    pub message: Option<String>,
    pub source: AgentStatusSource,
    pub occupant_id: AgentOccupantId,
    pub revision: i64,
    pub completed_revision: i64,
    pub updated_at: i64,
    /// False once the occupant's last candidate disappeared and this row only
    /// survives to carry a completion no viewer has acknowledged. A dead
    /// occupant can neither back a prompt proof nor be reclaimed by a later
    /// observation of the same numeric pid.
    pub occupant_live: bool,
}

/// v2 `SessionEntry`: everything one session has been told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    pub integration: Option<IntegrationCandidate>,
    pub integration_seq_by_process: HashMap<ProcessKey, u64>,
    pub screen: Option<ScreenCandidate>,
    pub screen_absence_observed: bool,
    pub effective: Option<EffectiveEntry>,
    pub retired_process_keys: HashSet<ProcessKey>,
}

impl Default for SessionEntry {
    /// A session nothing has been observed in: the screen is known absent, so
    /// the first sighting of any process is a new incarnation.
    fn default() -> Self {
        Self {
            integration: None,
            integration_seq_by_process: HashMap::new(),
            screen: None,
            screen_absence_observed: true,
            effective: None,
            retired_process_keys: HashSet::new(),
        }
    }
}

/// Why an occupant's last candidate went away. An exit keeps an
/// unacknowledged completion alive as a forced idle transition — an agent that
/// finishes and then leaves is still done, and only a viewer clears that. An
/// integration's explicit `active: false` is instead the withdrawal verb: it
/// retires the row, as does closing the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateLoss {
    Exit,
    Withdrawn,
}
