import { describe, expect, test } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  AGENT_ID_MAX_LENGTH,
  AGENT_STATUS_MESSAGE_MAX_LENGTH,
  AgentId,
  AgentStatus,
  AgentStatusUpdate,
} from "../src/wire/agent-status.ts";
import {
  CoordWorkerUpSchema,
  WAgentStatusSchema,
} from "../src/gen/roost/v1/worker_transport_pb.ts";
import {
  AgentStatusFrameSchema,
  FirehoseFrameSchema,
} from "../src/gen/roost/v1/sync_pb.ts";

const sessionId = "11111111-1111-4111-8111-111111111111";
const base = {
  session_id: sessionId,
  agent_id: "omp",
  state: "working",
  revision: 4,
  completed_revision: 2,
  updated_at: 1234,
} as const;

describe("agent status wire schema", () => {
  test("accepts active status and inactive deletion updates", () => {
    expect(AgentStatus.parse({ ...base, active: true })).toMatchObject(base);
    expect(AgentStatusUpdate.parse({ ...base, active: false })).toMatchObject({ active: false });
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
});

describe("agent status protobuf contract", () => {
  test("round-trips worker and Sync frames without loss", () => {
    const protoStatus = {
      sessionId,
      agentId: "omp",
      state: "blocked",
      message: "Approval required",
      revision: 7n,
      completedRevision: 3n,
      updatedAt: 5678,
      active: true,
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
      value: { message: "Approval required", revision: 7n, completedRevision: 3n },
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
      value: { agentId: "omp", state: "blocked", updatedAt: 5678, active: true },
    });
  });
});
