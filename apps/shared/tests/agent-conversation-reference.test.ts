// Covers the bounded opaque reference, sequence-aware recovery fold, and protobuf adapters.
// These tests keep private recovery semantics identical across worker and coordinator.
// Public Session folding and SessionEvent envelope limits are exercised at the same boundary.

import { describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES,
  AGENT_CONVERSATION_REFERENCE_MAX_UTF8_BYTES,
  AgentConversationReferenceV1Schema,
  AgentConversationRecoveryMetadataSchema,
  foldAgentConversationRecoveryMetadata,
} from "../src/agent-conversation-reference.ts";
import {
  agentConversationReferenceFromProto,
  agentConversationReferenceToProto,
  sessionRecoveryMetadataFromProto,
  sessionRecoveryMetadataToProto,
} from "../src/agent-conversation-reference-proto.ts";
import {
  SessionRecoveryMetadataSchema,
} from "../src/gen/roost/v1/coordinator_pb.ts";
import {
  AgentConversationReferenceV1Schema as AgentConversationReferenceV1ProtoSchema,
} from "../src/gen/roost/v1/wire_pb.ts";
import {
  SessionEvent,
  asSessionId,
  foldEvent,
  type Session,
} from "../src/wire/index.ts";

const SESSION_ID = asSessionId("11111111-1111-4111-8111-111111111111");
const OTHER_SESSION_ID = asSessionId("22222222-2222-4222-8222-222222222222");
const REFERENCE = AgentConversationReferenceV1Schema.parse({
  schema_version: 1,
  agent_id: "omp",
  kind: "path",
  value: "/tmp/a path/'$reference.json",
});

describe("AgentConversationReferenceV1", () => {
  test("preserves opaque values and accepts the exact UTF-8 value bound", () => {
    expect(AgentConversationReferenceV1Schema.parse(REFERENCE)).toEqual(REFERENCE);
    const exact = "😀".repeat(
      AGENT_CONVERSATION_REFERENCE_MAX_UTF8_BYTES / 4,
    );
    expect(AgentConversationReferenceV1Schema.parse({
      ...REFERENCE,
      value: exact,
    }).value).toBe(exact);
  });

  test("rejects unsupported agents, empty/NUL values, unknown fields, and overflow", () => {
    for (const candidate of [
      { ...REFERENCE, agent_id: "pi" },
      { ...REFERENCE, value: "" },
      { ...REFERENCE, value: "before\0after" },
      { ...REFERENCE, value: "\ud800" },
      { ...REFERENCE, value: "\udc00" },
      { ...REFERENCE, value: `${"a".repeat(AGENT_CONVERSATION_REFERENCE_MAX_UTF8_BYTES)}b` },
      { ...REFERENCE, extra: true },
    ]) {
      expect(AgentConversationReferenceV1Schema.safeParse(candidate).success).toBe(false);
    }
  });

  test("bounds the exact serialized event without truncating the opaque value", () => {
    const accepted = SessionEvent.parse({
      kind: "agent_reference",
      session_id: SESSION_ID,
      reference: { ...REFERENCE, value: "a".repeat(4_096) },
      ts: 1,
    });
    expect(Buffer.byteLength(JSON.stringify(accepted), "utf8"))
      .toBeLessThanOrEqual(AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES);
    expect(SessionEvent.safeParse({
      kind: "agent_reference",
      session_id: SESSION_ID,
      reference: { ...REFERENCE, value: "\u0001".repeat(4_096) },
      ts: 1,
    }).success).toBe(false);
    expect(SessionEvent.safeParse({
      kind: "agent_reference",
      session_id: SESSION_ID,
      reference: null,
      trace_id: "a".repeat(AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES),
      ts: 1,
    }).success).toBe(false);
  });

  test("is an explicit no-op in the public Session fold", () => {
    const before = new Map<string, Session>();
    const after = foldEvent(before, SessionEvent.parse({
      kind: "agent_reference",
      session_id: SESSION_ID,
      reference: REFERENCE,
      ts: 1,
    }));
    expect(after).toBe(before);
  });
});

describe("agent conversation recovery ordering", () => {
  test("sets, replaces, clears, and ignores lower or duplicate sequences", () => {
    const set = foldAgentConversationRecoveryMetadata(null, {
      session_id: SESSION_ID,
      reference: REFERENCE,
    }, 7);
    const staleClear = foldAgentConversationRecoveryMetadata(set, {
      session_id: SESSION_ID,
      reference: null,
    }, 6);
    const duplicate = foldAgentConversationRecoveryMetadata(set, {
      session_id: SESSION_ID,
      reference: null,
    }, 7);
    expect(staleClear).toBe(set);
    expect(duplicate).toBe(set);

    const replacement = foldAgentConversationRecoveryMetadata(set, {
      session_id: SESSION_ID,
      reference: { ...REFERENCE, kind: "id", value: "session-next" },
    }, 8);
    expect(replacement.agent_reference?.value).toBe("session-next");
    const cleared = foldAgentConversationRecoveryMetadata(replacement, {
      session_id: SESSION_ID,
      reference: null,
    }, 9);
    expect(cleared).toEqual({
      session_id: SESSION_ID,
      agent_reference: null,
      agent_reference_client_seq: 9,
    });
  });

  test("rejects invalid sequence and cross-session folding", () => {
    const set = foldAgentConversationRecoveryMetadata(null, {
      session_id: SESSION_ID,
      reference: REFERENCE,
    }, 1);
    expect(() => foldAgentConversationRecoveryMetadata(set, {
      session_id: OTHER_SESSION_ID,
      reference: null,
    }, 2)).toThrow("session mismatch");
    expect(() => foldAgentConversationRecoveryMetadata(null, {
      session_id: SESSION_ID,
      reference: null,
    }, 0)).toThrow("sequence must be positive");
    expect(AgentConversationRecoveryMetadataSchema.safeParse({
      session_id: SESSION_ID,
      agent_reference: REFERENCE,
      agent_reference_client_seq: 0,
    }).success).toBe(false);
  });
});

describe("agent conversation reference protobuf adapters", () => {
  test("round-trips a reference and set/clear recovery rows", () => {
    expect(agentConversationReferenceFromProto(
      agentConversationReferenceToProto(REFERENCE),
    )).toEqual(REFERENCE);
    for (const metadata of [
      {
        session_id: SESSION_ID,
        agent_reference: REFERENCE,
        agent_reference_client_seq: 41,
      },
      {
        session_id: SESSION_ID,
        agent_reference: null,
        agent_reference_client_seq: 42,
      },
      {
        session_id: SESSION_ID,
        agent_reference: null,
        agent_reference_client_seq: 0,
      },
    ]) {
      const parsed = AgentConversationRecoveryMetadataSchema.parse(metadata);
      expect(sessionRecoveryMetadataFromProto(
        sessionRecoveryMetadataToProto(parsed),
      )).toEqual(parsed);
    }
  });

  test("rejects malformed generated messages at adapter ingress", () => {
    expect(() => agentConversationReferenceFromProto(create(
      AgentConversationReferenceV1ProtoSchema,
      { schemaVersion: 1, agentId: "omp", kind: "id", value: "" },
    ))).toThrow();
    expect(() => sessionRecoveryMetadataFromProto(create(
      SessionRecoveryMetadataSchema,
      {
        sessionId: SESSION_ID,
        agentReference: agentConversationReferenceToProto(REFERENCE),
        agentReferenceClientSeq: 0n,
      },
    ))).toThrow();
  });
});
