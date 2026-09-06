// Browser projection tests for durable observed-agent identity admission.
// Frames exercise the same epoch/occupant fences as the coordinator while
// preserving legacy display and detached transition snapshots.

import { beforeEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  AgentOccupantId,
  AgentStatus,
  StatusEpoch,
  asSessionId,
  type AgentStatus as AgentStatusValue,
  type AgentStatusIdentity,
} from "@roost/shared/wire";
import {
  AgentStatusFrameSchema,
  type AgentStatusFrame as SyncAgentStatusFrame,
} from "@roost/shared/proto/sync_pb";
import {
  applyAgentStatusFrame,
  clearAgentStatusForSession,
  markAgentStatusSessionOpen,
  resetAgentStatusProjection,
  subscribeAgentStatus,
} from "../src/store/agent-status.ts";
import { rootStore } from "../src/store/root.ts";

const SESSION_ID = asSessionId("11111111-1111-4111-8111-111111111111");
const EPOCH_A = StatusEpoch.parse("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
const EPOCH_B = StatusEpoch.parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
const EPOCH_C = StatusEpoch.parse("cccccccc-cccc-4ccc-8ccc-cccccccccccc");
const OCCUPANT_A = AgentOccupantId.parse("bbbbbbbb-1111-4111-8111-bbbbbbbbbbbb");
const OCCUPANT_B = AgentOccupantId.parse("aaaaaaaa-2222-4222-8222-aaaaaaaaaaaa");
const OCCUPANT_C = AgentOccupantId.parse("cccccccc-3333-4333-8333-cccccccccccc");
const OCCUPANT_E = AgentOccupantId.parse("eeeeeeee-5555-4555-8555-eeeeeeeeeeee");
const OCCUPANT_D = AgentOccupantId.parse("dddddddd-4444-4444-8444-dddddddddddd");

const IDENTITY_A: AgentStatusIdentity = {
  status_epoch: EPOCH_A,
  occupant_id: OCCUPANT_A,
  source: "integration",
};
const IDENTITY_A_SCREEN: AgentStatusIdentity = { ...IDENTITY_A, source: "screen" };
const IDENTITY_B: AgentStatusIdentity = {
  status_epoch: EPOCH_A,
  occupant_id: OCCUPANT_B,
  source: "integration",
};
const IDENTITY_C: AgentStatusIdentity = {
  status_epoch: EPOCH_B,
  occupant_id: OCCUPANT_C,
  source: "screen",
};
const IDENTITY_D: AgentStatusIdentity = {
  status_epoch: EPOCH_C,
  occupant_id: OCCUPANT_D,
  source: "integration",
};

function status(
  state: AgentStatusValue["state"],
  revision: number,
  identity?: AgentStatusIdentity,
): AgentStatusValue {
  return AgentStatus.parse({
    session_id: SESSION_ID,
    agent_id: "omp",
    state,
    revision,
    completed_revision: 0,
    updated_at: revision,
    active: true,
    ...identity,
  });
}

function frame(
  value: AgentStatusValue | (Omit<AgentStatusValue, "active"> & { active: false }),
  identityOverride: Partial<Pick<SyncAgentStatusFrame, "statusEpoch" | "occupantId" | "source">> = {},
): SyncAgentStatusFrame {
  return create(AgentStatusFrameSchema, {
    sessionId: value.session_id,
    agentId: value.agent_id,
    state: value.state,
    message: value.message,
    revision: BigInt(value.revision),
    completedRevision: BigInt(value.completed_revision),
    updatedAt: value.updated_at,
    active: value.active,
    statusEpoch: value.status_epoch,
    occupantId: value.occupant_id,
    source: value.source,
    ...identityOverride,
  });
}

beforeEach(() => resetAgentStatusProjection());

describe("SPA identified agent status projection", () => {
  test("fences retired occupants and epochs without ordering UUID text", () => {
    expect(applyAgentStatusFrame(frame(status("working", 10, IDENTITY_A)))).toBe(true);
    expect(applyAgentStatusFrame(frame(status("blocked", 11, IDENTITY_A_SCREEN)))).toBe(true);
    expect(rootStore.agent_status[SESSION_ID]?.source).toBe("screen");

    expect(applyAgentStatusFrame(frame(status("working", 1, IDENTITY_B)))).toBe(true);
    expect(rootStore.agent_status[SESSION_ID]?.occupant_id).toBe(OCCUPANT_B);
    expect(applyAgentStatusFrame(frame({
      ...status("blocked", 12, IDENTITY_A),
      active: false,
    }))).toBe(false);
    expect(applyAgentStatusFrame(frame({
      ...status("working", 2, IDENTITY_B),
      active: false,
    }))).toBe(true);
    expect(applyAgentStatusFrame(frame({
      ...status("working", 0, IDENTITY_C),
      active: false,
    }))).toBe(false);

    expect(applyAgentStatusFrame(frame(status("working", 0, IDENTITY_C)))).toBe(true);
    const oldEpochNewOccupant = { ...IDENTITY_B, occupant_id: OCCUPANT_E };
    expect(applyAgentStatusFrame(frame(status("blocked", 99, oldEpochNewOccupant)))).toBe(false);
    expect(rootStore.agent_status[SESSION_ID]?.status_epoch).toBe(EPOCH_B);
  });

  test("only a matching inactive erases current and unknown inactive does not retire", () => {
    expect(applyAgentStatusFrame(frame(status("working", 5, IDENTITY_C)))).toBe(true);
    expect(applyAgentStatusFrame(frame({
      ...status("working", 1, IDENTITY_D),
      active: false,
    }))).toBe(false);
    expect(rootStore.agent_status[SESSION_ID]?.status_epoch).toBe(EPOCH_B);
    expect(applyAgentStatusFrame(frame(status("working", 1, IDENTITY_D)))).toBe(true);
    expect(applyAgentStatusFrame(frame({
      ...status("working", 2, IDENTITY_D),
      active: false,
    }))).toBe(true);
    expect(rootStore.agent_status[SESSION_ID]).toBeUndefined();
    expect(applyAgentStatusFrame(frame(status("blocked", 3, IDENTITY_D)))).toBe(false);
    expect(applyAgentStatusFrame(frame(status("working", 100)))).toBe(false);
  });

  test("fences closed sessions with or without a current status", () => {
    clearAgentStatusForSession(SESSION_ID);
    expect(applyAgentStatusFrame(frame(status("working", 1, IDENTITY_A)))).toBe(false);
    markAgentStatusSessionOpen(SESSION_ID);
    expect(applyAgentStatusFrame(frame(status("working", 1, IDENTITY_A)))).toBe(true);
    clearAgentStatusForSession(SESSION_ID);
    expect(applyAgentStatusFrame(frame(status("working", 1, IDENTITY_B)))).toBe(false);
  });

  test("rejects a partial identity triple at the shared schema boundary", () => {
    const partial = frame(status("working", 1), { statusEpoch: EPOCH_A });
    expect(applyAgentStatusFrame(partial)).toBe(false);
    expect(rootStore.agent_status[SESSION_ID]).toBeUndefined();
  });

  test("publishes previous identified values detached from Solid store mutation", () => {
    let previous: AgentStatusValue | null = null;
    const unsubscribe = subscribeAgentStatus((change) => {
      if (change.previous) previous = change.previous;
    });
    try {
      applyAgentStatusFrame(frame(status("working", 4, IDENTITY_A)));
      applyAgentStatusFrame(frame(status("blocked", 5, IDENTITY_A_SCREEN)));
      expect(previous).toMatchObject({
        state: "working",
        revision: 4,
        status_epoch: EPOCH_A,
        occupant_id: OCCUPANT_A,
        source: "integration",
      });
      expect(rootStore.agent_status[SESSION_ID]).toMatchObject({
        state: "blocked",
        revision: 5,
        source: "screen",
      });
    } finally {
      unsubscribe();
    }
  });
});
