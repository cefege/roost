// Pins the worker-only SessionsList recovery join before keeper reconciliation.
// A missing, duplicate, or foreign row fails closed before any session mutation
// can consume an incomplete private reference view; an individual reference the
// bounded contract no longer accepts degrades to none instead, because failing
// boot admission crash-loops the worker for every session on the machine.
import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  AgentConversationReferenceV1Schema as AgentConversationReferenceProtoSchema,
} from "@roost/shared/proto/wire_pb";
import {
  SessionRecoveryMetadataSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  agentConversationReferenceToProto,
} from "@roost/shared/agent-conversation-reference-proto";
import { _assertExactRecoveryMetadata } from "../src/boot-session-reconcile.ts";

const first = "00000000-0000-4000-8000-000000000001";
const second = "00000000-0000-4000-8000-000000000002";

function neverSet(sessionId: string) {
  return create(SessionRecoveryMetadataSchema, {
    sessionId,
    agentReferenceClientSeq: 0n,
  });
}

function withReference(sessionId: string) {
  return create(SessionRecoveryMetadataSchema, {
    sessionId,
    agentReference: agentConversationReferenceToProto({
      schema_version: 1,
      agent_id: "omp",
      kind: "id",
      value: "opaque",
    }),
    agentReferenceClientSeq: 7n,
  });
}

test("recovery metadata is exact, private, and sequence-valid", () => {
  const references = _assertExactRecoveryMetadata(
    [first, second],
    [withReference(first), neverSet(second)],
  );
  expect(references.get(first)).toEqual({
    schema_version: 1,
    agent_id: "omp",
    kind: "id",
    value: "opaque",
  });
  expect(references.get(second)).toBeNull();
  expect(() => _assertExactRecoveryMetadata(
    [first, second],
    [neverSet(first)],
  )).toThrow("incomplete");
  expect(() => _assertExactRecoveryMetadata(
    [first, second],
    [neverSet(first), neverSet(first)],
  )).toThrow("does not match");
  expect(() => _assertExactRecoveryMetadata(
    [first],
    [neverSet(second)],
  )).toThrow("does not match");

  const referenceWithoutSequence = withReference(first);
  referenceWithoutSequence.agentReferenceClientSeq = 0n;
  expect(_assertExactRecoveryMetadata([first], [referenceWithoutSequence]).get(first))
    .toBeNull();
});

test("a stored reference the bounded contract rejects degrades to none", () => {
  const unusable = (value: string, kind: "id" | "path") => {
    const row = create(SessionRecoveryMetadataSchema, {
      sessionId: first,
      agentReference: create(AgentConversationReferenceProtoSchema, {
        schemaVersion: 1,
        agentId: "omp",
        kind,
        value,
      }),
      agentReferenceClientSeq: 7n,
    });
    return _assertExactRecoveryMetadata([first], [row]);
  };
  // Legal under the old single 4096-byte cap, illegal under the kind-aware one.
  expect(unusable("x".repeat(600), "id").get(first)).toBeNull();
  expect(unusable("relative/session.jsonl", "path").get(first)).toBeNull();
  // A row for a session the response never listed is still fatal.
  expect(() => _assertExactRecoveryMetadata([first], [neverSet(second)]))
    .toThrow("does not match");
});
