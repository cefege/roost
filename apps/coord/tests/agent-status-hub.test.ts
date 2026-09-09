// Coordinator agent-status hub tests cover legacy revision compatibility,
// worker/session ownership validation, close cleanup, and baseline push
// debounce behavior. Identity ordering and transport projection stay focused.

import { afterEach, beforeEach, describe, expect, test, vi } from "bun:test";
import {
  AgentOccupantId,
  AgentStatus,
  AgentStatusUpdate,
  SessionEvent,
  StatusEpoch,
  asSessionId,
  asWorkerFp,
  type AgentStatusUpdate as AgentStatusUpdateValue,
} from "@roost/shared/wire";
import type { KyselyDB } from "../src/db/connection.ts";
import {
  getAgentStatusSnapshot,
  handleWorkerAgentStatus,
  startAgentStatusHub,
  stopAgentStatusHub,
} from "../src/agent-status-hub.ts";
import { agentStatusBus, sessionBus } from "../src/buses.ts";
import { cacheSessionWorker, evictSessionWorker } from "../src/byte-hub.ts";

const SID = asSessionId("11111111-1111-4111-8111-111111111111");
const WORKER = asWorkerFp("a1".repeat(32));
const OTHER_WORKER = asWorkerFp("b2".repeat(32));
const STATUS_EPOCH = StatusEpoch.parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
const OCCUPANT_ID = AgentOccupantId.parse("11111111-aaaa-4aaa-8aaa-111111111111");

function status(overrides: Partial<AgentStatusUpdateValue> = {}): AgentStatusUpdateValue {
  return AgentStatusUpdate.parse({
    session_id: SID,
    agent_id: "omp",
    state: "working",
    revision: 1,
    completed_revision: 0,
    updated_at: 1_780_000_000_000,
    active: true,
    ...overrides,
  });
}

function identifiedStatus(
  overrides: Partial<AgentStatusUpdateValue> = {},
): AgentStatusUpdateValue {
  return status({
    status_epoch: STATUS_EPOCH,
    occupant_id: OCCUPANT_ID,
    source: "integration",
    ...overrides,
  });
}

beforeEach(() => {
  stopAgentStatusHub();
  startAgentStatusHub();
  cacheSessionWorker(SID, WORKER, 7);
});

afterEach(() => {
  stopAgentStatusHub();
  evictSessionWorker(SID);
  vi.useRealTimers();
});

describe("coordinator agent status hub", () => {
  test("retains active state and rejects stale or equal revisions", () => {
    const published: AgentStatusUpdateValue[] = [];
    const unsubscribe = agentStatusBus.subscribe((update) => published.push(update));
    try {
      expect(handleWorkerAgentStatus(WORKER, status())).toBe("accepted");
      expect(handleWorkerAgentStatus(WORKER, status({ state: "blocked" }))).toBe("stale");
      expect(handleWorkerAgentStatus(WORKER, status({ revision: 0 }))).toBe("stale");
      expect(getAgentStatusSnapshot()).toEqual([AgentStatus.parse(status())]);
      expect(published).toHaveLength(1);
    } finally {
      unsubscribe();
    }
  });

  test("publishes inactive deletion and keeps its revision floor", () => {
    expect(handleWorkerAgentStatus(WORKER, status())).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER, status({ revision: 2, active: false }))).toBe("accepted");
    expect(getAgentStatusSnapshot()).toHaveLength(0);
    expect(handleWorkerAgentStatus(WORKER, status({ revision: 1 }))).toBe("stale");
  });

  test("rejects invalid, unknown-session, and cross-worker claims", () => {
    expect(handleWorkerAgentStatus(WORKER, { ...status(), state: "finished" })).toBe("invalid");
    evictSessionWorker(SID);
    expect(handleWorkerAgentStatus(WORKER, status())).toBe("unknown-session");
    cacheSessionWorker(SID, OTHER_WORKER, 7);
    expect(handleWorkerAgentStatus(WORKER, status())).toBe("wrong-worker");
  });

  test("clears retained state when the terminal closes", () => {
    expect(handleWorkerAgentStatus(WORKER, status({ revision: 8 }))).toBe("accepted");
    const published: AgentStatusUpdateValue[] = [];
    const unsubscribe = agentStatusBus.subscribe((update) => published.push(update));
    try {
      sessionBus.publish(SessionEvent.parse({
        kind: "closed",
        session_id: SID,
        exit_code: 0,
        ts: 1_780_000_000_001,
      }));
      expect(getAgentStatusSnapshot()).toHaveLength(0);
      expect(published).toHaveLength(1);
      expect(published[0]).toMatchObject({ session_id: SID, revision: 9, active: false });
    } finally {
      unsubscribe();
    }
  });

  test("delays, cancels, and replaces push transitions", async () => {
    vi.useFakeTimers();
    stopAgentStatusHub();
    const deliveries: Array<{ sessionId: string; kind: "blocked" | "done" }> = [];
    // The injected dispatcher never reads db; only its identity is required by
    // the production dependency contract in this timer-focused test.
    const db = {} as KyselyDB;
    startAgentStatusHub({
      db,
      pushAllowedOrigins: ["https://push.example"],
      dispatchPush: async (_db, transition) => {
        deliveries.push({ sessionId: transition.sessionId, kind: transition.kind });
      },
    });

    expect(handleWorkerAgentStatus(WORKER, identifiedStatus())).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER, identifiedStatus({
      revision: 2,
      state: "blocked",
      message: "Approval needed",
    }))).toBe("accepted");
    vi.advanceTimersByTime(999);
    expect(deliveries).toHaveLength(0);
    vi.advanceTimersByTime(1);
    await Promise.resolve();
    expect(deliveries).toEqual([{ sessionId: SID, kind: "blocked" }]);

    expect(handleWorkerAgentStatus(WORKER, identifiedStatus({
      revision: 3,
      state: "working",
    }))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER, identifiedStatus({
      revision: 4,
      state: "blocked",
    }))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER, identifiedStatus({
      revision: 5,
      state: "working",
    }))).toBe("accepted");
    vi.advanceTimersByTime(1_000);
    await Promise.resolve();
    expect(deliveries).toHaveLength(1);

    expect(handleWorkerAgentStatus(WORKER, identifiedStatus({
      revision: 6,
      state: "blocked",
    }))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER, identifiedStatus({
      revision: 7,
      state: "idle",
      completed_revision: 1,
    }))).toBe("accepted");
    vi.advanceTimersByTime(1_000);
    await Promise.resolve();
    expect(deliveries.at(-1)).toEqual({ sessionId: SID, kind: "done" });
  });

});
