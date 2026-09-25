//! The private, opaque reference that lets a respawned agent resume its own
//! conversation, and the envelope bound that keeps it out of the event log's
//! way.
//!
//! Every rule deciding whether a reference is resumable lives here. Worker
//! reporters, coordinator persistence, boot recovery, and restore import these
//! bounds rather than restating them and drifting from what was durably stored.
//!
//! A reference is equality and continuation data only. It never belongs in
//! public session state, which is why the `agent_reference` event is an explicit
//! no-op in the fold.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::wire::brand::SessionId;
use crate::{ProtocolError, ProtocolResult};

pub const AGENT_CONVERSATION_SESSION_ID_MAX_UTF8_BYTES: usize = 512;
pub const AGENT_CONVERSATION_SESSION_PATH_MAX_UTF8_BYTES: usize = 4_096;
/// The Unicode control class Cc, spelled as a pattern. A reference carrying any
/// of these cannot survive the shell command line that resumes it.
pub const AGENT_CONVERSATION_CONTROL_CHARACTER_RE: &str = "[\u{0}-\u{1f}\u{7f}-\u{9f}]";
pub const AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES: usize = 8_192;
/// The only agent whose conversation Roost can resume.
pub const AGENT_CONVERSATION_AGENT_ID: &str = "omp";

/// The byte count every bound in this module is defined in. A Rust string's
/// length already is its UTF-8 byte count, so the check is the comparison and
/// never allocates an encoded copy.
fn has_at_most_utf8_bytes(value: &str, max_bytes: usize) -> bool {
    value.len() <= max_bytes
}

fn contains_control_character(value: &str) -> bool {
    value
        .chars()
        .any(|character| matches!(character as u32, 0x00..=0x1f | 0x7f..=0x9f))
}

/// POSIX or Windows absolute: a leading separator, or a drive letter followed by
/// one. Both shapes count, because the reporting agent may run on a host
/// platform other than the one validating or storing the report.
pub fn is_absolute_agent_conversation_session_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    match bytes {
        [b'/' | b'\\', ..] => true,
        [drive, b':', b'/' | b'\\', ..] => drive.is_ascii_alphabetic(),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentConversationReferenceKind {
    Id,
    Path,
}

impl AgentConversationReferenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Path => "path",
        }
    }

    fn max_utf8_bytes(self) -> usize {
        match self {
            Self::Id => AGENT_CONVERSATION_SESSION_ID_MAX_UTF8_BYTES,
            Self::Path => AGENT_CONVERSATION_SESSION_PATH_MAX_UTF8_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConversationReferenceV1 {
    pub schema_version: i64,
    pub agent_id: String,
    pub kind: AgentConversationReferenceKind,
    pub value: String,
}

impl AgentConversationReferenceV1 {
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        let reference: AgentConversationReferenceV1 =
            serde_json::from_value(value).map_err(|error| {
                ProtocolError::new("agent_conversation_reference", error.to_string())
            })?;
        reference.check()?;
        Ok(reference)
    }

    pub fn check(&self) -> ProtocolResult<()> {
        if self.schema_version != 1 {
            return Err(ProtocolError::new(
                "schema_version",
                format!("must be 1, got {}", self.schema_version),
            ));
        }
        if self.agent_id != AGENT_CONVERSATION_AGENT_ID {
            return Err(ProtocolError::new(
                "agent_id",
                format!("must be {AGENT_CONVERSATION_AGENT_ID:?}"),
            ));
        }
        if self.value.is_empty() {
            return Err(ProtocolError::new(
                "value",
                "agent conversation reference must not be empty",
            ));
        }
        // A `&str` is well-formed Unicode by construction, so the only
        // remaining character rule is the control class.
        if contains_control_character(&self.value) {
            return Err(ProtocolError::new(
                "value",
                "agent conversation reference must not contain control characters",
            ));
        }
        if !has_at_most_utf8_bytes(&self.value, self.kind.max_utf8_bytes()) {
            return Err(ProtocolError::new(
                "value",
                format!(
                    "agent conversation {} must not exceed {} UTF-8 bytes",
                    self.kind.as_str(),
                    self.kind.max_utf8_bytes()
                ),
            ));
        }
        if self.kind == AgentConversationReferenceKind::Path
            && !is_absolute_agent_conversation_session_path(&self.value)
        {
            return Err(ProtocolError::new(
                "value",
                "agent conversation session path must be absolute",
            ));
        }
        Ok(())
    }
}

/// Sequence zero is the worker-list sentinel for a session with no reference
/// event; every durable set or clear passed to the fold stays strictly positive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConversationRecoveryMetadata {
    pub session_id: SessionId,
    pub agent_reference: Option<AgentConversationReferenceV1>,
    pub agent_reference_client_seq: i64,
}

impl AgentConversationRecoveryMetadata {
    pub fn parse(value: Value) -> ProtocolResult<Self> {
        let metadata: AgentConversationRecoveryMetadata =
            serde_json::from_value(value).map_err(|error| {
                ProtocolError::new("agent_conversation_recovery_metadata", error.to_string())
            })?;
        metadata.check()?;
        Ok(metadata)
    }

    pub fn check(&self) -> ProtocolResult<()> {
        crate::validate::nonnegative(
            "agent_conversation_recovery_metadata.agent_reference_client_seq",
            self.agent_reference_client_seq,
        )?;
        if let Some(reference) = &self.agent_reference {
            reference
                .check()
                .map_err(|error| error.within("agent_conversation_recovery_metadata"))?;
        }
        if self.agent_reference.is_some() && self.agent_reference_client_seq == 0 {
            return Err(ProtocolError::new(
                "agent_conversation_recovery_metadata.agent_reference_client_seq",
                "stored agent conversation reference requires a positive sequence",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentConversationReferenceUpdate {
    pub session_id: SessionId,
    pub reference: Option<AgentConversationReferenceV1>,
}

/// Apply one worker-ordered update. A stale set or a stale clear cannot replace
/// newer state, which is why the sequence comparison is `<=` and not `<`.
pub fn fold_agent_conversation_recovery_metadata(
    previous: Option<&AgentConversationRecoveryMetadata>,
    event: &AgentConversationReferenceUpdate,
    client_seq: i64,
) -> ProtocolResult<AgentConversationRecoveryMetadata> {
    if client_seq <= 0 {
        return Err(ProtocolError::new(
            "agent_conversation_recovery_metadata.agent_reference_client_seq",
            "agent conversation reference client sequence must be positive",
        ));
    }
    if let Some(previous) = previous {
        if previous.session_id != event.session_id {
            return Err(ProtocolError::new(
                "agent_conversation_recovery_metadata.session_id",
                "agent conversation recovery session mismatch",
            ));
        }
        if client_seq <= previous.agent_reference_client_seq {
            return Ok(previous.clone());
        }
    }
    let metadata = AgentConversationRecoveryMetadata {
        session_id: event.session_id.clone(),
        agent_reference: event.reference.clone(),
        agent_reference_client_seq: client_seq,
    };
    metadata.check()?;
    Ok(metadata)
}

/// Bound the exact durable JSON envelope after its individual fields parsed.
/// The envelope is what the log stores, so a value inside its own bound can
/// still push the stored record past the limit: JSON escaping doubles a
/// backslash, and a `trace_id` is measured here rather than field by field.
pub fn is_agent_conversation_reference_event_envelope_bounded(event: &Value) -> bool {
    if let Some(Value::String(trace_id)) = event.get("trace_id")
        && !has_at_most_utf8_bytes(trace_id, AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES)
    {
        return false;
    }
    match serde_json::to_string(event) {
        Ok(serialized) => has_at_most_utf8_bytes(
            &serialized,
            AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES,
        ),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "11111111-1111-4111-8111-111111111111";
    const OTHER_SESSION: &str = "22222222-2222-4222-8222-222222222222";

    fn reference_value() -> Value {
        serde_json::json!({
            "schema_version": 1,
            "agent_id": "omp",
            "kind": "path",
            "value": "/tmp/a path/'$reference.json",
        })
    }

    fn session_id() -> SessionId {
        SessionId::try_from(SESSION).expect("a uuid")
    }

    #[test]
    fn both_absolute_shapes_and_each_kind_at_its_exact_bound_are_accepted() {
        assert!(AgentConversationReferenceV1::parse(reference_value()).is_ok());
        let mut exact = reference_value();
        exact["value"] = serde_json::json!(format!(
            "/{}abc",
            "\u{1f600}".repeat(AGENT_CONVERSATION_SESSION_PATH_MAX_UTF8_BYTES / 4 - 1)
        ));
        let parsed = AgentConversationReferenceV1::parse(exact.clone()).expect("exact bound");
        assert_eq!(parsed.value, exact["value"].as_str().unwrap());

        for absolute in [
            "C:\\Users\\a\\.omp\\sessions\\s.jsonl",
            "C:/Users/a/.omp/sessions/s.jsonl",
            "\\\\server\\share\\s.jsonl",
        ] {
            let mut value = reference_value();
            value["value"] = serde_json::json!(absolute);
            assert!(
                AgentConversationReferenceV1::parse(value).is_ok(),
                "{absolute}"
            );
        }

        let mut id = reference_value();
        id["kind"] = serde_json::json!("id");
        id["value"] =
            serde_json::json!("\u{1f600}".repeat(AGENT_CONVERSATION_SESSION_ID_MAX_UTF8_BYTES / 4));
        assert!(AgentConversationReferenceV1::parse(id).is_ok());
    }

    #[test]
    fn a_relative_path_a_control_character_and_a_known_agent_are_all_refused() {
        for relative in [
            "relative/session.jsonl",
            "./session.jsonl",
            "~/x.jsonl",
            "C:",
            "C:relative.jsonl",
        ] {
            let mut value = reference_value();
            value["value"] = serde_json::json!(relative);
            assert!(
                AgentConversationReferenceV1::parse(value).is_err(),
                "{relative}"
            );
        }
        for control in ["\u{0}", "\n", "\u{1b}", "\u{7f}", "\u{9f}"] {
            let mut value = reference_value();
            value["value"] = serde_json::json!(format!("/tmp/before{control}after"));
            assert!(
                AgentConversationReferenceV1::parse(value).is_err(),
                "{control:?}"
            );
        }
        let mut wrong_agent = reference_value();
        wrong_agent["agent_id"] = serde_json::json!("pi");
        assert!(AgentConversationReferenceV1::parse(wrong_agent).is_err());
        let mut unknown_field = reference_value();
        unknown_field["extra"] = serde_json::json!(true);
        assert!(AgentConversationReferenceV1::parse(unknown_field).is_err());
    }

    #[test]
    fn a_stale_or_duplicate_update_cannot_replace_newer_state() {
        let reference = AgentConversationReferenceV1::parse(reference_value()).unwrap();
        let set = fold_agent_conversation_recovery_metadata(
            None,
            &AgentConversationReferenceUpdate {
                session_id: session_id(),
                reference: Some(reference.clone()),
            },
            7,
        )
        .unwrap();

        let clear = AgentConversationReferenceUpdate {
            session_id: session_id(),
            reference: None,
        };
        assert_eq!(
            fold_agent_conversation_recovery_metadata(Some(&set), &clear, 6).unwrap(),
            set
        );
        assert_eq!(
            fold_agent_conversation_recovery_metadata(Some(&set), &clear, 7).unwrap(),
            set
        );
        let cleared = fold_agent_conversation_recovery_metadata(Some(&set), &clear, 9).unwrap();
        assert_eq!(cleared.agent_reference, None);
        assert_eq!(cleared.agent_reference_client_seq, 9);
    }

    #[test]
    fn a_non_positive_sequence_and_a_foreign_session_are_refused() {
        let set = fold_agent_conversation_recovery_metadata(
            None,
            &AgentConversationReferenceUpdate {
                session_id: session_id(),
                reference: None,
            },
            1,
        )
        .unwrap();
        let foreign = fold_agent_conversation_recovery_metadata(
            Some(&set),
            &AgentConversationReferenceUpdate {
                session_id: SessionId::try_from(OTHER_SESSION).unwrap(),
                reference: None,
            },
            2,
        );
        assert!(foreign.unwrap_err().reason.contains("session mismatch"));
        let update = AgentConversationReferenceUpdate {
            session_id: session_id(),
            reference: None,
        };
        assert!(fold_agent_conversation_recovery_metadata(None, &update, 0).is_err());
    }

    #[test]
    fn a_stored_reference_without_a_sequence_is_refused() {
        let reference = AgentConversationReferenceV1::parse(reference_value()).unwrap();
        let metadata = AgentConversationRecoveryMetadata {
            session_id: session_id(),
            agent_reference: Some(reference),
            agent_reference_client_seq: 0,
        };
        assert!(metadata.check().is_err());
    }
}
