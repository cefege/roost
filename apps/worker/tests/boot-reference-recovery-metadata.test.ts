// Pins the worker-only SessionsList recovery join before keeper reconciliation.
// Missing, duplicate, foreign, or malformed rows fail closed before any session
// mutation can consume an incomplete private reference view.
import { expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
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
  expect(() => _assertExactRecoveryMetadata(
    [first],
    [referenceWithoutSequence],
  )).toThrow("invalid");
});
