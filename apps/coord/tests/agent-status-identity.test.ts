// Coordinator status identity tests pin replacement, retirement, close/open,
// and legacy compatibility independently from transport and notification
// delivery. UUIDs remain opaque equality tokens, never sort keys.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  AgentOccupantId,
  AgentStatusUpdate,
  SessionEvent,
  StatusEpoch,
  asChannelId,
  asSessionId,
  asWorkerFp,
  type AgentStatusUpdate as AgentStatusUpdateValue,
} from "@roost/shared/wire";
import {
  getAgentStatusSnapshot,
  handleWorkerAgentStatus,
  startAgentStatusHub,
  stopAgentStatusHub,
} from "../src/agent-status-hub.ts";
import { agentStatusBus, sessionBus } from "../src/buses.ts";
import { cacheSessionWorker, evictSessionWorker } from "../src/byte-hub.ts";

const SESSION_ID = asSessionId("11111111-1111-4111-8111-111111111111");
const WORKER_FP = asWorkerFp("a1".repeat(32));
const EPOCH_A = StatusEpoch.parse("ffffffff-ffff-4fff-8fff-ffffffffffff");
const EPOCH_B = StatusEpoch.parse("00000000-0000-4000-8000-000000000000");
const OCCUPANT_A = AgentOccupantId.parse("ffffffff-aaaa-4aaa-8aaa-ffffffffffff");
const OCCUPANT_B = AgentOccupantId.parse("cccccccc-aaaa-4aaa-8aaa-cccccccccccc");
const OCCUPANT_C = AgentOccupantId.parse("88888888-aaaa-4aaa-8aaa-888888888888");
const OCCUPANT_D = AgentOccupantId.parse("44444444-aaaa-4aaa-8aaa-444444444444");

function identifiedStatus(
  statusEpoch: typeof EPOCH_A,
  occupantId: typeof OCCUPANT_A,
  overrides: Partial<AgentStatusUpdateValue> = {},
): AgentStatusUpdateValue {
  return AgentStatusUpdate.parse({
    session_id: SESSION_ID,
    agent_id: "omp",
    state: "working",
    revision: 1,
    completed_revision: 0,
    updated_at: 1_780_000_000_000,
    active: true,
    status_epoch: statusEpoch,
    occupant_id: occupantId,
    source: "integration",
    ...overrides,
  });
}

function legacyStatus(
  overrides: Partial<AgentStatusUpdateValue> = {},
): AgentStatusUpdateValue {
  return AgentStatusUpdate.parse({
    session_id: SESSION_ID,
    agent_id: "omp",
    state: "working",
    revision: 1,
    completed_revision: 0,
    updated_at: 1_780_000_000_000,
    active: true,
    ...overrides,
  });
}

beforeEach(() => {
  stopAgentStatusHub();
  startAgentStatusHub();
  cacheSessionWorker(SESSION_ID, WORKER_FP, 7);
});

afterEach(() => {
  stopAgentStatusHub();
  evictSessionWorker(SESSION_ID);
});

describe("coordinator agent occupant ordering", () => {
  test("accepts lower revisions and lexically lower new identities while fencing retirees", () => {
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_A,
      { revision: 90 },
    ))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_A,
      { revision: 89 },
    ))).toBe("stale");

    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_B,
      { revision: 999, active: false },
    ))).toBe("stale");
    expect(getAgentStatusSnapshot()).toMatchObject([{
      occupant_id: OCCUPANT_A,
      revision: 90,
    }]);

    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_B,
      { revision: 1, state: "blocked" },
    ))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_A,
      { revision: 91, state: "idle", completed_revision: 1 },
    ))).toBe("stale");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_A,
      { revision: 92, active: false },
    ))).toBe("stale");

    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_B,
      OCCUPANT_C,
      { revision: 0, state: "idle" },
    ))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_D,
      { revision: 999 },
    ))).toBe("stale");
    expect(getAgentStatusSnapshot()).toMatchObject([{
      status_epoch: EPOCH_B,
      occupant_id: OCCUPANT_C,
      revision: 0,
      state: "idle",
    }]);

    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_B,
      OCCUPANT_C,
      { revision: 1, state: "idle", active: false },
    ))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_B,
      OCCUPANT_C,
      { revision: 2 },
    ))).toBe("stale");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_B,
      OCCUPANT_D,
      { revision: 0 },
    ))).toBe("accepted");
  });

  test("a reconnect resend cannot rewind its exact occupant revision", () => {
    const connected = identifiedStatus(EPOCH_A, OCCUPANT_A, {
      revision: 7,
      state: "blocked",
    });
    expect(handleWorkerAgentStatus(WORKER_FP, connected)).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, connected)).toBe("stale");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_A,
      { revision: 6, state: "working" },
    ))).toBe("stale");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_A,
      { revision: 8, state: "idle", completed_revision: 1 },
    ))).toBe("accepted");
    expect(getAgentStatusSnapshot()).toMatchObject([{
      status_epoch: EPOCH_A,
      occupant_id: OCCUPANT_A,
      revision: 8,
      state: "idle",
    }]);
  });

  test("retires the prior epoch even when its display was already inactive", () => {
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_A,
      { revision: 5 },
    ))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_A,
      { revision: 6, active: false },
    ))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_B,
      OCCUPANT_B,
      { revision: 0 },
    ))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_C,
      { revision: 100 },
    ))).toBe("stale");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_C,
      { revision: 101, active: false },
    ))).toBe("stale");
    expect(getAgentStatusSnapshot()).toMatchObject([{
      status_epoch: EPOCH_B,
      occupant_id: OCCUPANT_B,
      revision: 0,
    }]);
  });

  test("legacy frames yield permanently after an identified occupant is accepted", () => {
    expect(handleWorkerAgentStatus(WORKER_FP, legacyStatus({ revision: 40 }))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_A,
      { revision: 1 },
    ))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, legacyStatus({ revision: 100 }))).toBe("stale");
    expect(handleWorkerAgentStatus(WORKER_FP, legacyStatus({
      revision: 101,
      active: false,
    }))).toBe("stale");
    expect(getAgentStatusSnapshot()).toMatchObject([{
      status_epoch: EPOCH_A,
      occupant_id: OCCUPANT_A,
      revision: 1,
    }]);

    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_A,
      { revision: 2, active: false },
    ))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, legacyStatus({ revision: 102 }))).toBe("stale");
    expect(getAgentStatusSnapshot()).toEqual([]);
  });

  test("close publishes the exact identity and keeps it retired across a fresh open", () => {
    expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
      EPOCH_A,
      OCCUPANT_A,
      { revision: 8, source: "screen" },
    ))).toBe("accepted");
    const published: AgentStatusUpdateValue[] = [];
    const unsubscribe = agentStatusBus.subscribe((update) => published.push(update));
    try {
      sessionBus.publish(SessionEvent.parse({
        kind: "closed",
        session_id: SESSION_ID,
        exit_code: 0,
        ts: 1_780_000_000_001,
      }));
      expect(published).toHaveLength(1);
      expect(published[0]).toMatchObject({
        active: false,
        revision: 9,
        status_epoch: EPOCH_A,
        occupant_id: OCCUPANT_A,
        source: "screen",
      });
      expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
        EPOCH_A,
        OCCUPANT_B,
        { revision: 0 },
      ))).toBe("stale");

      sessionBus.publish(SessionEvent.parse({
        kind: "opened",
        session_id: SESSION_ID,
        worker_fp: WORKER_FP,
        channel: asChannelId(7),
        session_kind: "shell",
        cwd: "/tmp",
        ts: 1_780_000_000_002,
      }));
      expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
        EPOCH_A,
        OCCUPANT_B,
        { revision: 0 },
      ))).toBe("accepted");
      expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
        EPOCH_A,
        OCCUPANT_A,
        { revision: 100 },
      ))).toBe("stale");
      expect(handleWorkerAgentStatus(WORKER_FP, identifiedStatus(
        EPOCH_A,
        OCCUPANT_A,
        { revision: 101, active: false },
      ))).toBe("stale");
      expect(getAgentStatusSnapshot()).toMatchObject([{
        status_epoch: EPOCH_A,
        occupant_id: OCCUPANT_B,
        revision: 0,
      }]);
    } finally {
      unsubscribe();
    }
  });
});

