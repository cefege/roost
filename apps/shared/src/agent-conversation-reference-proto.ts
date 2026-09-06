// Maps private agent-conversation references and recovery rows to protobuf.
// Coordinator handlers and worker boot recovery share this strict boundary.
// Every decoded value is reparsed through the canonical bounded Zod contract.

import { create } from "@bufbuild/protobuf";
import {
  AgentConversationReferenceV1Schema as AgentConversationReferenceV1ProtoSchema,
  type AgentConversationReferenceV1 as AgentConversationReferenceV1Proto,
} from "./gen/roost/v1/wire_pb.ts";
import {
  SessionRecoveryMetadataSchema as SessionRecoveryMetadataProtoSchema,
  type SessionRecoveryMetadata as SessionRecoveryMetadataProto,
} from "./gen/roost/v1/coordinator_pb.ts";
import {
  AgentConversationReferenceV1Schema,
  AgentConversationRecoveryMetadataSchema,
  type AgentConversationReferenceV1,
  type AgentConversationRecoveryMetadata,
} from "./agent-conversation-reference.ts";

export function agentConversationReferenceToProto(
  reference: AgentConversationReferenceV1,
): AgentConversationReferenceV1Proto {
  const checked = AgentConversationReferenceV1Schema.parse(reference);
  return create(AgentConversationReferenceV1ProtoSchema, {
    schemaVersion: checked.schema_version,
    agentId: checked.agent_id,
    kind: checked.kind,
    value: checked.value,
  });
}

export function agentConversationReferenceFromProto(
  reference: AgentConversationReferenceV1Proto,
): AgentConversationReferenceV1 {
  return AgentConversationReferenceV1Schema.parse({
    schema_version: reference.schemaVersion,
    agent_id: reference.agentId,
    kind: reference.kind,
    value: reference.value,
  });
}

export function sessionRecoveryMetadataToProto(
  metadata: AgentConversationRecoveryMetadata,
): SessionRecoveryMetadataProto {
  const checked = AgentConversationRecoveryMetadataSchema.parse(metadata);
  return create(SessionRecoveryMetadataProtoSchema, {
    sessionId: checked.session_id,
    agentReference: checked.agent_reference
      ? agentConversationReferenceToProto(checked.agent_reference)
      : undefined,
    agentReferenceClientSeq: BigInt(checked.agent_reference_client_seq),
  });
}

export function sessionRecoveryMetadataFromProto(
  metadata: SessionRecoveryMetadataProto,
): AgentConversationRecoveryMetadata {
  return AgentConversationRecoveryMetadataSchema.parse({
    session_id: metadata.sessionId,
    agent_reference: metadata.agentReference
      ? agentConversationReferenceFromProto(metadata.agentReference)
      : null,
    agent_reference_client_seq: Number(metadata.agentReferenceClientSeq),
  });
}
