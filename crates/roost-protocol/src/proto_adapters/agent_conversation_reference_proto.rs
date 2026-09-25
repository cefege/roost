//! The agent conversation reference's protobuf form, including the one place
//! its `trace_id` travels on the wire. Coordinator handlers and worker boot
//! recovery share this boundary, so both directions re-check the value rather
//! than trusting whichever side assembled it.
//!
//! The reference is a message field wherever it appears, so its presence is
//! message presence: an absent reference clears it, and a null one is not a
//! state this contract has.

use roost_proto::AgentConversationReferenceV1 as PbAgentConversationReferenceV1;
use roost_proto::SessionRecoveryMetadata as PbSessionRecoveryMetadata;

use crate::agent_conversation_reference::{
    AgentConversationRecoveryMetadata, AgentConversationReferenceKind, AgentConversationReferenceV1,
};
use crate::wire::brand::SessionId;
use crate::{ProtocolError, ProtocolResult};

const METADATA: &str = "agent_conversation_recovery_metadata";

/// The `kind` the domain closes and the wire spells as a plain string. A newer
/// peer may name a kind this build has never seen, so the mismatch is an error
/// naming the field and never a default.
fn reference_kind_from_str(
    field: &str,
    value: &str,
) -> ProtocolResult<AgentConversationReferenceKind> {
    [
        AgentConversationReferenceKind::Id,
        AgentConversationReferenceKind::Path,
    ]
    .into_iter()
    .find(|kind| kind.as_str() == value)
    .ok_or_else(|| ProtocolError::new(field, format!("unknown agent conversation kind {value:?}")))
}

fn sequence_to_wire(field: &str, value: i64) -> ProtocolResult<u64> {
    u64::try_from(value)
        .map_err(|_| ProtocolError::new(field, format!("must not be negative, got {value}")))
}

fn sequence_from_wire(field: &str, value: u64) -> ProtocolResult<i64> {
    i64::try_from(value).map_err(|_| ProtocolError::new(field, format!("must not exceed {value}")))
}

pub fn agent_conversation_reference_to_proto(
    reference: &AgentConversationReferenceV1,
) -> ProtocolResult<PbAgentConversationReferenceV1> {
    reference.check()?;
    let schema_version = u32::try_from(reference.schema_version).map_err(|_| {
        ProtocolError::new(
            "schema_version",
            format!("must not be negative, got {}", reference.schema_version),
        )
    })?;
    Ok(PbAgentConversationReferenceV1 {
        schema_version,
        agent_id: reference.agent_id.clone(),
        kind: reference.kind.as_str().to_owned(),
        value: reference.value.clone(),
        ..Default::default()
    })
}

/// `None` in, `None` out: the reference is a message field, so an absent one
/// clears the value rather than describing a reference with nothing in it.
pub fn agent_conversation_reference_from_proto(
    reference: Option<&PbAgentConversationReferenceV1>,
) -> ProtocolResult<Option<AgentConversationReferenceV1>> {
    let Some(reference) = reference else {
        return Ok(None);
    };
    let decoded = AgentConversationReferenceV1 {
        schema_version: sequence_from_wire("schema_version", u64::from(reference.schema_version))?,
        agent_id: reference.agent_id.clone(),
        kind: reference_kind_from_str("kind", &reference.kind)?,
        value: reference.value.clone(),
    };
    decoded.check()?;
    Ok(Some(decoded))
}

pub fn session_recovery_metadata_to_proto(
    metadata: &AgentConversationRecoveryMetadata,
) -> ProtocolResult<PbSessionRecoveryMetadata> {
    metadata.check()?;
    Ok(PbSessionRecoveryMetadata {
        session_id: metadata.session_id.as_str().to_owned(),
        agent_reference: metadata
            .agent_reference
            .as_ref()
            .map(agent_conversation_reference_to_proto)
            .transpose()?
            .into(),
        agent_reference_client_seq: sequence_to_wire(
            format!("{METADATA}.agent_reference_client_seq").as_str(),
            metadata.agent_reference_client_seq,
        )?,
        ..Default::default()
    })
}

pub fn session_recovery_metadata_from_proto(
    metadata: &PbSessionRecoveryMetadata,
) -> ProtocolResult<AgentConversationRecoveryMetadata> {
    let decoded = AgentConversationRecoveryMetadata {
        session_id: SessionId::try_from(metadata.session_id.as_str())
            .map_err(|error| error.within(METADATA))?,
        agent_reference: agent_conversation_reference_from_proto(
            metadata.agent_reference.as_option(),
        )
        .map_err(|error| error.within(METADATA))?,
        agent_reference_client_seq: sequence_from_wire(
            format!("{METADATA}.agent_reference_client_seq").as_str(),
            metadata.agent_reference_client_seq,
        )?,
    };
    decoded.check()?;
    Ok(decoded)
}
