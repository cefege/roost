//! The validated terminal session IDs an attachment can target.
//! This is narrower than arbitrary strings and deliberately does not broaden `SessionId`.
//! It is shared by relay frames, direct grants, and worker-side attachment storage.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::fmt;

use super::SessionId;
use crate::{ProtocolError, ProtocolResult, validate::uuid};

/// A validated terminal session identifier used only by attachment flows.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttachmentTargetId(String);

impl AttachmentTargetId {
    /// The validated identifier as carried by attachment requests.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Validate a UUID terminal session ID.
    pub fn check(&self) -> ProtocolResult<()> {
        uuid("attachment_target_id", &self.0)
    }
}

impl fmt::Display for AttachmentTargetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for AttachmentTargetId {
    type Error = ProtocolError;

    fn try_from(value: String) -> ProtocolResult<Self> {
        let target = Self(value);
        target.check()?;
        Ok(target)
    }
}

impl TryFrom<&str> for AttachmentTargetId {
    type Error = ProtocolError;

    fn try_from(value: &str) -> ProtocolResult<Self> {
        Self::try_from(value.to_owned())
    }
}

impl From<SessionId> for AttachmentTargetId {
    fn from(value: SessionId) -> Self {
        Self(value.as_str().to_owned())
    }
}

impl Serialize for AttachmentTargetId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AttachmentTargetId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::try_from(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::AttachmentTargetId;

    #[test]
    fn accepts_terminal_ids_but_rejects_other_strings() {
        assert!(AttachmentTargetId::try_from("00000000-0000-4000-8000-000000000001").is_ok());
        assert!(AttachmentTargetId::try_from("c-0123456789abcdef01234567").is_err());
        assert!(AttachmentTargetId::try_from("c-0123456789abcdef0123456g").is_err());
        assert!(AttachmentTargetId::try_from("arbitrary-session-id").is_err());
    }
}
