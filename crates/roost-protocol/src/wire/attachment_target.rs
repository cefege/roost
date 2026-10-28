//! The validated IDs an attachment can target: a terminal session or agent conversation.
//! This is narrower than arbitrary strings and deliberately does not broaden `SessionId`.
//! It is shared by relay frames, direct grants, and worker-side attachment storage.

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::fmt;

use super::SessionId;
use crate::{ProtocolError, ProtocolResult, validate::uuid};

/// A validated terminal session or agent conversation identifier used only by attachment flows.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttachmentTargetId(String);

impl AttachmentTargetId {
    /// The validated identifier as carried by attachment requests.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this target uses the agent conversation ID namespace.
    #[must_use]
    pub fn is_conversation(&self) -> bool {
        self.0.starts_with("c-")
    }

    /// Validate a UUID terminal session ID or `c-` plus 24 lowercase hex digits.
    pub fn check(&self) -> ProtocolResult<()> {
        if uuid("attachment_target_id", &self.0).is_ok() || is_conversation_id(&self.0) {
            Ok(())
        } else {
            Err(ProtocolError::new(
                "attachment_target_id",
                "must be a terminal UUID or a conversation ID",
            ))
        }
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

fn is_conversation_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 26
        && bytes.starts_with(b"c-")
        && bytes[2..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

#[cfg(test)]
mod tests {
    use super::AttachmentTargetId;

    #[test]
    fn accepts_terminal_and_agent_conversation_ids_but_rejects_other_strings() {
        assert!(AttachmentTargetId::try_from("00000000-0000-4000-8000-000000000001").is_ok());
        assert!(AttachmentTargetId::try_from("c-0123456789abcdef01234567").is_ok());
        assert!(AttachmentTargetId::try_from("c-0123456789abcdef0123456g").is_err());
        assert!(AttachmentTargetId::try_from("arbitrary-session-id").is_err());
    }
}
