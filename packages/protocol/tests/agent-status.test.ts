// Pins legacy and identified agent-status validation across Zod and protobuf.
// The tests cover worker/Sync transport plus coordinator read and wait DTOs
// without inventing process provenance for identityless deployment frames.

import { describe, expect, test } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  AGENT_ID_MAX_LENGTH,
  AGENT_STATUS_MESSAGE_MAX_LENGTH,
  AgentId,
  AgentOccupantId,
  AgentStatus,
  AgentStatusSource,
  AgentStatusUpdate,
  StatusEpoch,
  isIdentifiedAgentStatus,
  type AgentStatusIdentity,
} from "../src/wire/agent-status.ts";
import {
  AgentStatusGetRequestSchema,
  AgentStatusGetResponseSchema,
  AgentStatusListRequestSchema,
  AgentStatusListResponseSchema,
  AgentStatusViewSchema,
  AgentStatusWaitRequestSchema,
  AgentStatusWaitResponseSchema,
  CoordinatorService,
} from "../src/gen/roost/v1/coordinator_pb.ts";
import {
  CoordWorkerUpSchema,
  WAgentStatusSchema,
} from "../src/gen/roost/v1/worker_transport_pb.ts";
import {
  AgentStatusFrameSchema,
  FirehoseFrameSchema,
} from "../src/gen/roost/v1/sync_pb.ts";

const sessionId = "11111111-1111-4111-8111-111111111111";
const statusEpoch = "22222222-2222-4222-8222-222222222222";
const occupantId = "33333333-3333-4333-8333-333333333333";
const identity = {
  status_epoch: statusEpoch,
  occupant_id: occupantId,
  source: "integration",
} as const;
const base = {
  session_id: sessionId,
  agent_id: "omp",
  state: "working",
  revision: 4,
  completed_revision: 2,
  updated_at: 1234,
} as const;

describe("agent status wire schema", () => {
  test("accepts active status and inactive deletion updates without legacy provenance", () => {
    const legacyStatus = AgentStatus.parse({ ...base, active: true });
    const legacyDeletion = AgentStatusUpdate.parse({ ...base, active: false });
    expect(legacyStatus).toMatchObject(base);
    expect(legacyDeletion).toMatchObject({ active: false });
    expect(legacyStatus.status_epoch).toBeUndefined();
    expect(legacyStatus.occupant_id).toBeUndefined();
    expect(legacyStatus.source).toBeUndefined();
    expect(isIdentifiedAgentStatus(legacyStatus)).toBe(false);
  });

  test("bounds agent ids and messages", () => {
    expect(AgentId.safeParse("codex").success).toBe(true);
    for (const invalid of ["", "Codex", "agent_name", "a".repeat(AGENT_ID_MAX_LENGTH + 1)]) {
      expect(AgentId.safeParse(invalid).success).toBe(false);
    }
    expect(AgentStatusUpdate.safeParse({
      ...base,
      message: "x".repeat(AGENT_STATUS_MESSAGE_MAX_LENGTH + 1),
      active: true,
    }).success).toBe(false);
  });

  test("validates identity UUIDs and observed source", () => {
    for (const schema of [StatusEpoch, AgentOccupantId]) {
      expect(schema.safeParse(statusEpoch).success).toBe(true);
      expect(schema.safeParse("not-a-uuid").success).toBe(false);
    }
    expect(AgentStatusSource.safeParse("integration").success).toBe(true);
    expect(AgentStatusSource.safeParse("screen").success).toBe(true);
    expect(AgentStatusSource.safeParse("worker").success).toBe(false);

    expect(AgentStatus.safeParse({
      ...base,
      ...identity,
      status_epoch: "not-a-uuid",
      active: true,
    }).success).toBe(false);
  });

  test("requires safe ordered revisions and timestamps", () => {
    for (const patch of [
      { revision: Number.MAX_SAFE_INTEGER + 1 },
      { revision: Number.NaN },
      { revision: -1 },
      { completed_revision: 5 },
      { updated_at: Number.POSITIVE_INFINITY },
    ]) {
      expect(AgentStatusUpdate.safeParse({ ...base, ...patch, active: true }).success).toBe(false);
    }
  });

  test("rejects every partial identity triple", () => {
    for (const partialIdentity of [
      { status_epoch: statusEpoch },
      { occupant_id: occupantId },
      { source: "integration" },
      { status_epoch: statusEpoch, occupant_id: occupantId },
      { status_epoch: statusEpoch, source: "integration" },
      { occupant_id: occupantId, source: "integration" },
    ]) {
      expect(AgentStatus.safeParse({
        ...base,
        ...partialIdentity,
        active: true,
      }).success).toBe(false);
      expect(AgentStatusUpdate.safeParse({
        ...base,
        ...partialIdentity,
        active: false,
      }).success).toBe(false);
    }
  });

  test("narrows only complete identified statuses", () => {
    const identifiedStatus = AgentStatus.parse({
      ...base,
      ...identity,
      active: true,
    });
    expect(isIdentifiedAgentStatus(identifiedStatus)).toBe(true);
    if (!isIdentifiedAgentStatus(identifiedStatus)) {
      throw new Error("identified status did not narrow");
    }
    const narrowedIdentity: AgentStatusIdentity = identifiedStatus;
    expect(narrowedIdentity).toMatchObject(identity);
    const identifiedDeletion = AgentStatusUpdate.parse({
      ...base,
      ...identity,
      active: false,
    });
    expect(isIdentifiedAgentStatus(identifiedDeletion)).toBe(true);
  });
});

describe("agent status protobuf contract", () => {
  test("round-trips identified worker and Sync frames without loss", () => {
    const protoStatus = {
      sessionId,
      agentId: "omp",
      state: "blocked",
      message: "Approval required",
      revision: 7n,
      completedRevision: 3n,
      updatedAt: 5678,
      active: true,
      statusEpoch,
      occupantId,
      source: "integration",
      occupantExited: true,
    };
    const workerStatus = create(WAgentStatusSchema, protoStatus);
    const workerFrame = create(CoordWorkerUpSchema, {
      frame: { case: "agentStatus", value: workerStatus },
    });
    const workerRoundTrip = fromBinary(
      CoordWorkerUpSchema,
      toBinary(CoordWorkerUpSchema, workerFrame),
    );
    expect(workerRoundTrip.frame).toMatchObject({
      case: "agentStatus",
      value: {
        revision: 7n,
        statusEpoch,
        occupantId,
        source: "integration",
        occupantExited: true,
      },
    });

    const syncStatus = create(AgentStatusFrameSchema, protoStatus);
    const syncFrame = create(FirehoseFrameSchema, {
      frame: { case: "agentStatus", value: syncStatus },
    });
    const syncRoundTrip = fromBinary(
      FirehoseFrameSchema,
      toBinary(FirehoseFrameSchema, syncFrame),
    );
    expect(syncRoundTrip.frame).toMatchObject({
      case: "agentStatus",
      value: {
        agentId: "omp",
        statusEpoch,
        occupantId,
        source: "integration",
        occupantExited: true,
      },
    });
  });

  test("round-trips fully legacy identityless transports without fabrication", () => {
    const legacyStatus = {
      sessionId,
      agentId: "omp",
      state: "idle",
      revision: 1n,
      completedRevision: 0n,
      updatedAt: 100,
      active: true,
    };
    const workerRoundTrip = fromBinary(
      WAgentStatusSchema,
      toBinary(WAgentStatusSchema, create(WAgentStatusSchema, legacyStatus)),
    );
    const syncRoundTrip = fromBinary(
      AgentStatusFrameSchema,
      toBinary(AgentStatusFrameSchema, create(AgentStatusFrameSchema, legacyStatus)),
    );
    for (const roundTripStatus of [workerRoundTrip, syncRoundTrip]) {
      expect(roundTripStatus.statusEpoch).toBeUndefined();
      expect(roundTripStatus.occupantId).toBeUndefined();
      expect(roundTripStatus.source).toBeUndefined();
      // A worker deployed before exit retention omits the field; decoding it as
      // "released" would retire live rows across a rolling upgrade.
      expect(roundTripStatus.occupantExited).toBe(false);
    }
  });

  test("exposes explicit coordinator status DTOs and service methods", () => {
    expect(CoordinatorService.methods.map((method) => method.localName)).toEqual(
      expect.arrayContaining(["agentStatusGet", "agentStatusList", "agentStatusWait"]),
    );
    const view = create(AgentStatusViewSchema, {
      sessionId,
      agentId: "omp",
      state: "working",
      revision: 9n,
      completedRevision: 4n,
      updatedAt: 9000,
      active: true,
      statusEpoch,
      occupantId,
      source: "integration",
      promptable: true,
    });
    const legacyView = create(AgentStatusViewSchema, {
      sessionId,
      agentId: "omp",
      state: "idle",
      revision: 2n,
      completedRevision: 1n,
      updatedAt: 8000,
      active: true,
      promptable: false,
    });
    const getRequest = create(AgentStatusGetRequestSchema, { sessionId });
    const getResponse = create(AgentStatusGetResponseSchema, { status: view });
    const listRequest = create(AgentStatusListRequestSchema);
    const listResponse = create(AgentStatusListResponseSchema, {
      statuses: [view, legacyView],
    });
    const waitRequest = create(AgentStatusWaitRequestSchema, {
      sessionId,
      statusEpoch,
      occupantId,
      desiredStates: ["blocked", "idle"],
      afterRevision: 9n,
      timeoutMs: 30_000,
    });
    const waitResponse = create(AgentStatusWaitResponseSchema, {
      outcome: "matched",
    });

    expect(fromBinary(
      AgentStatusGetRequestSchema,
      toBinary(AgentStatusGetRequestSchema, getRequest),
    )).toMatchObject({ sessionId });
    expect(fromBinary(
      AgentStatusGetResponseSchema,
      toBinary(AgentStatusGetResponseSchema, getResponse),
    ).status).toMatchObject({ statusEpoch, occupantId, promptable: true });
    expect(fromBinary(
      AgentStatusListRequestSchema,
      toBinary(AgentStatusListRequestSchema, listRequest),
    )).toBeDefined();
    const roundTripStatuses = fromBinary(
      AgentStatusListResponseSchema,
      toBinary(AgentStatusListResponseSchema, listResponse),
    ).statuses;
    expect(roundTripStatuses[0]).toMatchObject({ sessionId, source: "integration" });
    expect(roundTripStatuses[1]).toMatchObject({ promptable: false });
    expect(roundTripStatuses[1]?.statusEpoch).toBeUndefined();
    expect(roundTripStatuses[1]?.occupantId).toBeUndefined();
    expect(roundTripStatuses[1]?.source).toBeUndefined();
    expect(fromBinary(
      AgentStatusWaitRequestSchema,
      toBinary(AgentStatusWaitRequestSchema, waitRequest),
    )).toMatchObject({
      sessionId,
      statusEpoch,
      occupantId,
      desiredStates: ["blocked", "idle"],
      afterRevision: 9n,
      timeoutMs: 30_000,
    });
    expect(fromBinary(
      AgentStatusWaitResponseSchema,
      toBinary(AgentStatusWaitResponseSchema, waitResponse),
    ).outcome).toBe("matched");
  });
});
