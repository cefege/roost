//! Bounded, event-driven waits over the coordinator's retained agent status.
//!
//! Ported from `apps/coord/src/agents/agent-status-wait.ts`. A waiter reads
//! session facts only through the [`AgentStatusWaitView`] the status hub hands
//! it, so the hub stays the single owner of retained frames and closure facts
//! and a wait can never answer from a second copy of the table.
//!
//! The request and outcome vocabulary lives here; the registry that holds the
//! waiters, and the waiter itself, live in `registry.rs`.
//!
//! A WAIT IS RELEASED BY THE NEXT CHANGE, NEVER BY POLLING, AND A GONE
//! SUBSCRIBER IS RELEASED BY DROPPING IT. Two consequences are load-bearing:
//!
//! * A wait advances only on real progress. A worker republishes an occupant
//!   whenever only its message or its authority source changed -- an expiring
//!   integration lease falling back to screen detection at the same state -- and
//!   a revision bump alone is not progress. A settled (`idle`) occupant
//!   advances on the worker's `completed_revision`, the exact
//!   working|blocked -> idle turn boundary; an active one advances on the
//!   revision at which that state was actually entered.
//! * [`AgentStatusWaiter`] deregisters on `Drop`. A browser that navigates away
//!   mid-wait drops the handler future, and a wait left behind would keep a
//!   session's slot occupied for the full five minutes -- a slow leak under
//!   exactly the normal use of a long-poll.

use std::collections::HashSet;

use roost_protocol::wire::{
    AgentOccupantId, AgentRuntimeState, AgentStatus, SessionId, StatusEpoch,
};

mod registry;

pub use registry::{AgentStatusWaitRegistry, AgentStatusWaiter};

/// The longest wait a client may ask for (`agent-status-wait.ts:81`).
pub const AGENT_STATUS_WAIT_MAX_TIMEOUT_MS: u64 = 300_000;

/// Waiters one session may hold at once (`agent-status-wait.ts:82`).
pub const AGENT_STATUS_WAIT_MAX_PER_SESSION: usize = 32;

/// Waiters the whole coordinator may hold at once (`agent-status-wait.ts:83`).
pub const AGENT_STATUS_WAIT_MAX_GLOBAL: usize = 2_048;

/// A validated wait for one exact occupant to reach one of some states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatusWaitRequest {
    /// The session the occupant runs in.
    pub session_id: SessionId,
    /// The status epoch the client pinned.
    pub status_epoch: StatusEpoch,
    /// The occupant the client pinned.
    pub occupant_id: AgentOccupantId,
    /// The states that satisfy the wait, in the order the client asked.
    pub desired_states: Vec<AgentRuntimeState>,
    /// The revision the client has already seen, when it is following a
    /// previous wait rather than starting fresh.
    pub after_revision: Option<i64>,
    /// How long the client will wait.
    pub timeout_ms: u64,
}

impl AgentStatusWaitRequest {
    /// Validate a wait, or say why it cannot be admitted.
    ///
    /// Every bound is refused rather than clamped, because a client that asked
    /// for a 400-second wait and silently received 300 would be told
    /// "timed out" for a turn that is still running.
    pub fn new(
        session_id: &str,
        status_epoch: &str,
        occupant_id: &str,
        desired_states: &[String],
        after_revision: Option<i64>,
        timeout_ms: u64,
    ) -> Result<Self, AgentStatusWaitError> {
        let session_id =
            SessionId::try_from(session_id).map_err(|_| AgentStatusWaitError::invalid())?;
        let status_epoch =
            StatusEpoch::try_from(status_epoch).map_err(|_| AgentStatusWaitError::invalid())?;
        let occupant_id =
            AgentOccupantId::try_from(occupant_id).map_err(|_| AgentStatusWaitError::invalid())?;
        let mut unique: HashSet<AgentRuntimeState> = HashSet::new();
        for state in desired_states {
            let state = parse_desired_state(state).ok_or_else(AgentStatusWaitError::invalid)?;
            if !unique.insert(state) {
                return Err(AgentStatusWaitError::invalid());
            }
        }
        if unique.is_empty() {
            return Err(AgentStatusWaitError::invalid());
        }
        if !(1..=AGENT_STATUS_WAIT_MAX_TIMEOUT_MS).contains(&timeout_ms) {
            return Err(AgentStatusWaitError::invalid());
        }
        if after_revision.is_some_and(|revision| revision < 0) {
            return Err(AgentStatusWaitError::invalid());
        }
        Ok(Self {
            session_id,
            status_epoch,
            occupant_id,
            desired_states: desired_states
                .iter()
                .filter_map(|state| parse_desired_state(state))
                .collect(),
            after_revision,
            timeout_ms,
        })
    }
}

/// The wire spelling of an agent runtime state.
///
/// The three states are closed by `protocol/spec/agent-metadata.md`, and an
/// unknown one is an invalid request rather than a state to wait for
/// indefinitely.
fn parse_desired_state(value: &str) -> Option<AgentRuntimeState> {
    match value {
        "working" => Some(AgentRuntimeState::Working),
        "blocked" => Some(AgentRuntimeState::Blocked),
        "idle" => Some(AgentRuntimeState::Idle),
        _ => None,
    }
}

/// How a wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatusWaitOutcome {
    /// The retained occupant reached a desired state.
    Matched {
        /// The revision of the frame that matched; the floor a follow-on wait
        /// pins, so the next wait cannot match the frame it just saw.
        matched_revision: i64,
    },
    /// The client's own budget elapsed with no change.
    TimedOut,
    /// The pinned occupant was replaced, retired, or its status went away.
    OccupantChanged,
    /// The session closed, which ends the question the wait was asking.
    SessionClosed,
}

impl AgentStatusWaitOutcome {
    /// The word the RPC response carries.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Matched { .. } => "matched",
            Self::TimedOut => "timed_out",
            Self::OccupantChanged => "occupant_changed",
            Self::SessionClosed => "session_closed",
        }
    }
}

/// Which bound a capacity refusal exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatusWaitCapacity {
    /// One session's own waiter slots.
    Session,
    /// The coordinator's waiter slots.
    Global,
}

/// Why a wait could not be admitted, or was ended early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatusWaitErrorKind {
    /// The request named something no wait could be built from.
    Invalid,
    /// No slot was free.
    Capacity,
    /// The caller went away, or the hub stopped.
    Canceled,
}

impl AgentStatusWaitErrorKind {
    /// The word the log line carries.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Capacity => "capacity",
            Self::Canceled => "canceled",
        }
    }
}

/// A wait refusal, carrying the kind the RPC maps to a Connect code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentStatusWaitError {
    kind: AgentStatusWaitErrorKind,
    capacity: Option<AgentStatusWaitCapacity>,
}

impl AgentStatusWaitError {
    /// A request that could never be admitted.
    #[must_use]
    pub fn invalid() -> Self {
        Self {
            kind: AgentStatusWaitErrorKind::Invalid,
            capacity: None,
        }
    }

    /// A wait that was cut short, by cancellation or by the hub stopping.
    #[must_use]
    pub fn canceled() -> Self {
        Self {
            kind: AgentStatusWaitErrorKind::Canceled,
            capacity: None,
        }
    }

    /// A refusal naming which bound ran out.
    #[must_use]
    pub fn capacity_exhausted(capacity: AgentStatusWaitCapacity) -> Self {
        Self {
            kind: AgentStatusWaitErrorKind::Capacity,
            capacity: Some(capacity),
        }
    }

    /// The kind, for the log line and the RPC mapping.
    #[must_use]
    pub fn kind(self) -> AgentStatusWaitErrorKind {
        self.kind
    }

    /// Which bound ran out, when the refusal was a capacity one.
    #[must_use]
    pub fn capacity(self) -> Option<AgentStatusWaitCapacity> {
        self.capacity
    }

    /// The message every refusal of this kind carries.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self.kind {
            AgentStatusWaitErrorKind::Invalid => "invalid agent status wait request",
            AgentStatusWaitErrorKind::Capacity => "agent status wait capacity exhausted",
            AgentStatusWaitErrorKind::Canceled => "agent status wait canceled",
        }
    }
}

/// The retained-session facts a waiter may read, all owned by the status hub.
pub trait AgentStatusWaitView: Send + Sync {
    /// What is retained for `session_id` right now, if anything.
    fn retained(&self, session_id: &SessionId) -> Option<AgentStatus>;
    /// Whether the session has closed and not reopened since.
    fn is_closed(&self, session_id: &SessionId) -> bool;
    /// The revision at which the retained occupant's state last actually
    /// changed, which is the only advance an active-state wait may honour.
    fn state_change_revision(&self, session_id: &SessionId) -> i64;
}
