//! Volatile agent-state contracts shared by the worker, the coordinator, and
//! the browser.
//!
//! Identity is an all-or-none fencing triple: `status_epoch`, `occupant_id`,
//! and `source` are present together or all absent. Absence stays legal because
//! a worker deployed before durable observation still reports state, and a
//! status that could only be read one way would strand those rows on a rolling
//! upgrade.
//!
//! `occupant_exited` is the same kind of compatibility field: a worker that
//! never retained exits omits it, and decoding that as "released" would retire
//! live rows across the upgrade.

use serde::{Deserialize, Serialize};

use crate::validate::uuid;
use crate::validate::{all_or_none, integer_in_range, max_utf8_bytes, non_empty};
use crate::wire::brand::SessionId;
use crate::{ProtocolError, ProtocolResult};

pub const AGENT_ID_MAX_LENGTH: usize = 32;
pub const AGENT_STATUS_MESSAGE_MAX_LENGTH: usize = 512;

/// The largest integer a JavaScript peer can have sent without losing
/// precision. A value past it means the producer was not counting in
/// milliseconds, or in anything, and a revision that does not round-trip is
/// worse than a rejected one.
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentId(String);

impl AgentId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for AgentId {
    type Error = ProtocolError;

    fn try_from(value: &str) -> ProtocolResult<Self> {
        non_empty("agent_id", value)?;
        max_utf8_bytes("agent_id", value, AGENT_ID_MAX_LENGTH)?;
        let shaped = value.starts_with(|first: char| first.is_ascii_lowercase())
            && value.chars().all(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
            });
        if shaped {
            Ok(Self(value.to_owned()))
        } else {
            Err(ProtocolError::new(
                "agent_id",
                "must start with a lowercase letter and hold only [a-z0-9-]",
            ))
        }
    }
}

impl TryFrom<String> for AgentId {
    type Error = ProtocolError;

    fn try_from(value: String) -> ProtocolResult<Self> {
        AgentId::try_from(value.as_str())?;
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeState {
    Working,
    Blocked,
    Idle,
}

impl AgentRuntimeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Idle => "idle",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StatusEpoch(String);

impl StatusEpoch {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for StatusEpoch {
    type Error = ProtocolError;

    fn try_from(value: &str) -> ProtocolResult<Self> {
        uuid("status_epoch", value)?;
        Ok(Self(value.to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentOccupantId(String);

impl AgentOccupantId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for AgentOccupantId {
    type Error = ProtocolError;

    fn try_from(value: &str) -> ProtocolResult<Self> {
        uuid("occupant_id", value)?;
        Ok(Self(value.to_owned()))
    }
}

/// Where the status was observed. Only these two: a status asserted by the
/// worker's own integration and a status a human typed on the screen are the
/// only sources a viewer may be told about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatusSource {
    Integration,
    Screen,
}

impl AgentStatusSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Integration => "integration",
            Self::Screen => "screen",
        }
    }
}

/// The fencing triple, present only when all three fields are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatusIdentity {
    pub status_epoch: StatusEpoch,
    pub occupant_id: AgentOccupantId,
    pub source: AgentStatusSource,
}

/// The status payload, without the `active` flag that separates a retained
/// status from the update that deletes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStatusFields {
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub state: AgentRuntimeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub revision: i64,
    pub completed_revision: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_epoch: Option<StatusEpoch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occupant_id: Option<AgentOccupantId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AgentStatusSource>,
    /// The occupant's last process is gone. Such a row is retained only to
    /// carry a completion, so a viewer that has acknowledged that completion
    /// has no agent left to show.
    #[serde(default)]
    pub occupant_exited: bool,
}

/// The status a worker, coordinator, and browser currently retain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStatus {
    #[serde(flatten)]
    pub common: AgentStatusFields,
    /// Always true. A retained status with `active = false` is a deletion
    /// update, which is the other type.
    pub active: bool,
}

/// A volatile update; `active = false` removes the retained status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStatusUpdate {
    #[serde(flatten)]
    pub common: AgentStatusFields,
    pub active: bool,
}

impl AgentStatusFields {
    pub fn check(&self) -> ProtocolResult<()> {
        if let Some(message) = &self.message {
            max_utf8_bytes(
                "agent_status.message",
                message,
                AGENT_STATUS_MESSAGE_MAX_LENGTH,
            )?;
        }
        integer_in_range("agent_status.revision", self.revision, 0, MAX_SAFE_INTEGER)?;
        integer_in_range(
            "agent_status.completed_revision",
            self.completed_revision,
            0,
            MAX_SAFE_INTEGER,
        )?;
        integer_in_range(
            "agent_status.updated_at",
            self.updated_at,
            0,
            MAX_SAFE_INTEGER,
        )?;
        if self.completed_revision > self.revision {
            return Err(ProtocolError::new(
                "agent_status.completed_revision",
                "must not exceed revision",
            ));
        }
        all_or_none(
            "agent_status",
            [
                self.status_epoch.is_some(),
                self.occupant_id.is_some(),
                self.source.is_some(),
            ],
        )
    }
}

impl AgentStatus {
    pub fn parse(value: serde_json::Value) -> ProtocolResult<Self> {
        let status: AgentStatus = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("agent_status", error.to_string()))?;
        if !status.active {
            return Err(ProtocolError::new(
                "agent_status.active",
                "must be true; an inactive status is an AgentStatusUpdate",
            ));
        }
        status.common.check()?;
        Ok(status)
    }
}

impl AgentStatusUpdate {
    pub fn parse(value: serde_json::Value) -> ProtocolResult<Self> {
        let update: AgentStatusUpdate = serde_json::from_value(value)
            .map_err(|error| ProtocolError::new("agent_status_update", error.to_string()))?;
        update.common.check()?;
        Ok(update)
    }
}

/// The fencing triple, or `None` when this status came from a deployment that
/// predates durable observation. Narrowing to it is the only way a caller may
/// treat a status as owned by a known occupant rather than merely asserted.
pub fn agent_status_identity(status: &AgentStatusFields) -> Option<AgentStatusIdentity> {
    Some(AgentStatusIdentity {
        status_epoch: status.status_epoch.clone()?,
        occupant_id: status.occupant_id.clone()?,
        source: status.source?,
    })
}

/// Whether the status carries a complete identity triple.
pub fn is_identified_agent_status(status: &AgentStatusFields) -> bool {
    agent_status_identity(status).is_some()
}

#[cfg(test)]
mod tests;
