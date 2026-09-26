//! Branded identity types: an id that passed its shape check is a different
//! type from a string, so a session id can never be passed where a worker
//! fingerprint belongs.
//!
//! TypeScript made these nominal with Zod's `.brand()`. Rust makes them nominal
//! with a private field and a shape-checking `TryFrom`, so the two types do not
//! convert into each other at all. Each one also deserializes through its own
//! check, because a brand a decoder can mint from an arbitrary string is not a
//! brand. Depends on `validate` for the UUID shape and on `fingerprint`, which
//! owns the one definition of what a rendered fingerprint looks like.

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::fingerprint::is_fingerprint_hex;
use crate::validate::uuid;
use crate::{ProtocolError, ProtocolResult};

/// SHA-256 hex of a worker's ed25519 pubkey, lowercase and 64 characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct WorkerFp(String);

impl WorkerFp {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn check(&self) -> ProtocolResult<()> {
        if is_fingerprint_hex(&self.0) {
            Ok(())
        } else {
            Err(ProtocolError::new(
                "worker_fp",
                "must be 64 lowercase hex characters",
            ))
        }
    }
}

impl fmt::Display for WorkerFp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for WorkerFp {
    type Error = ProtocolError;

    fn try_from(value: String) -> ProtocolResult<Self> {
        let candidate = Self(value);
        candidate.check()?;
        Ok(candidate)
    }
}

impl TryFrom<&str> for WorkerFp {
    type Error = ProtocolError;

    fn try_from(value: &str) -> ProtocolResult<Self> {
        Self::try_from(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for WorkerFp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        WorkerFp::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// A uuid the worker mints when it opens a session.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn check(&self) -> ProtocolResult<()> {
        uuid("session_id", &self.0)
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for SessionId {
    type Error = ProtocolError;

    fn try_from(value: String) -> ProtocolResult<Self> {
        let candidate = Self(value);
        candidate.check()?;
        Ok(candidate)
    }
}

impl TryFrom<&str> for SessionId {
    type Error = ProtocolError;

    fn try_from(value: &str) -> ProtocolResult<Self> {
        Self::try_from(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        SessionId::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Worker-local PTY id. Not stable across worker restarts, which is why a
/// respawn rebinds it rather than treating it as part of the session identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChannelId(u32);

impl ChannelId {
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ChannelId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl TryFrom<i64> for ChannelId {
    type Error = ProtocolError;

    fn try_from(value: i64) -> ProtocolResult<Self> {
        match u32::try_from(value) {
            Ok(channel) => Ok(Self(channel)),
            Err(_) => Err(ProtocolError::new(
                "channel",
                format!("must be an integer in 0..={}, got {value}", u32::MAX),
            )),
        }
    }
}

/// The first-class grouping bucket sessions hang off. Absent on pre-migration
/// rows, where a client falls back to a single implicit workspace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn check(&self) -> ProtocolResult<()> {
        uuid("workspace_id", &self.0)
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for WorkspaceId {
    type Error = ProtocolError;

    fn try_from(value: String) -> ProtocolResult<Self> {
        let candidate = Self(value);
        candidate.check()?;
        Ok(candidate)
    }
}

impl TryFrom<&str> for WorkspaceId {
    type Error = ProtocolError;

    fn try_from(value: &str) -> ProtocolResult<Self> {
        Self::try_from(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for WorkspaceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        WorkspaceId::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// A queued task a worker pulls and claims.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TaskId(String);

impl TaskId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn check(&self) -> ProtocolResult<()> {
        uuid("task_id", &self.0)
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for TaskId {
    type Error = ProtocolError;

    fn try_from(value: String) -> ProtocolResult<Self> {
        let candidate = Self(value);
        candidate.check()?;
        Ok(candidate)
    }
}

impl TryFrom<&str> for TaskId {
    type Error = ProtocolError;

    fn try_from(value: &str) -> ProtocolResult<Self> {
        Self::try_from(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for TaskId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        TaskId::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// One row of the MCP relay registry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct McpRelayId(String);

impl McpRelayId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn check(&self) -> ProtocolResult<()> {
        uuid("mcp_relay_id", &self.0)
    }
}

impl fmt::Display for McpRelayId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for McpRelayId {
    type Error = ProtocolError;

    fn try_from(value: String) -> ProtocolResult<Self> {
        let candidate = Self(value);
        candidate.check()?;
        Ok(candidate)
    }
}

impl TryFrom<&str> for McpRelayId {
    type Error = ProtocolError;

    fn try_from(value: &str) -> ProtocolResult<Self> {
        Self::try_from(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for McpRelayId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        McpRelayId::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Log-correlation id: hex, at least eight characters. Defined here because it
/// is a branded identity like every other id on the wire, and every frame that
/// carries a `trace_id` must spell the check once.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct TraceId(String);

impl TraceId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn check(&self) -> ProtocolResult<()> {
        let shaped = self.0.len() >= 8 && self.0.bytes().all(|byte| byte.is_ascii_hexdigit());
        if shaped {
            Ok(())
        } else {
            Err(ProtocolError::new(
                "trace_id",
                "must be at least 8 hex characters",
            ))
        }
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for TraceId {
    type Error = ProtocolError;

    fn try_from(value: String) -> ProtocolResult<Self> {
        let candidate = Self(value);
        candidate.check()?;
        Ok(candidate)
    }
}

impl TryFrom<&str> for TraceId {
    type Error = ProtocolError;

    fn try_from(value: &str) -> ProtocolResult<Self> {
        Self::try_from(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for TraceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        TraceId::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FINGERPRINT: &str = "abababababababababababababababababababababababababababababababab";
    const SESSION: &str = "00000000-0000-4000-8000-000000000001";

    #[test]
    fn a_fingerprint_is_lowercase_hex_of_exactly_sixty_four() {
        assert!(WorkerFp::try_from(FINGERPRINT).is_ok());
        assert!(WorkerFp::try_from(FINGERPRINT.to_uppercase()).is_err());
        assert!(WorkerFp::try_from(&FINGERPRINT[..63]).is_err());
        assert!(WorkerFp::try_from(format!("{FINGERPRINT}0")).is_err());
    }

    #[test]
    fn a_uuid_id_accepts_either_case_and_rejects_a_fingerprint() {
        assert!(SessionId::try_from(SESSION).is_ok());
        assert!(SessionId::try_from(SESSION.to_uppercase()).is_ok());
        assert!(SessionId::try_from("not-a-uuid").is_err());
        // The brands are only distinct if a worker fingerprint is not a session
        // id, even though both passed *a* shape check.
        assert!(SessionId::try_from(FINGERPRINT).is_err());
        assert!(WorkspaceId::try_from(FINGERPRINT).is_err());
        assert!(TaskId::try_from(FINGERPRINT).is_err());
        assert!(McpRelayId::try_from(FINGERPRINT).is_err());
    }

    #[test]
    fn a_channel_is_a_nonnegative_u32_because_the_proto_carries_one() {
        assert_eq!(ChannelId::try_from(0).unwrap().as_u32(), 0);
        assert_eq!(
            ChannelId::try_from(u32::MAX as i64).unwrap().as_u32(),
            u32::MAX
        );
        assert!(ChannelId::try_from(-1).is_err());
        assert!(ChannelId::try_from(u32::MAX as i64 + 1).is_err());
    }

    #[test]
    fn a_trace_id_needs_at_least_eight_hex_characters() {
        assert!(TraceId::try_from("deadbeef").is_ok());
        assert!(TraceId::try_from("deadbee").is_err());
        assert!(TraceId::try_from("deadbeeg").is_err());
    }
}
