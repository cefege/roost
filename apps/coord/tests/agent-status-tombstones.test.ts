// Pins the agent-status hub's close bookkeeping as bounded rather than
// monotonic: a tombstone older than the longest admissible wait releases both
// the stale fence and that session's admission order, while a younger one keeps
// fencing. The bounded wait is the hub's only tombstone reader that does not
// itself trigger the sweep, so it is the observer these cases use.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  AgentOccupantId,
  AgentStatusUpdate,
  SessionEvent,
  StatusEpoch,
  asSessionId,
  asWorkerFp,
  type AgentStatusUpdate as AgentStatusUpdateValue,
} from "@roost/protocol/wire";
import {
  _backdateAgentStatusTombstone,
  handleWorkerAgentStatus,
  startAgentStatusHub,
  stopAgentStatusHub,
  waitForAgentStatus,
} from "../src/agent-status-hub.ts";
import {
  AGENT_STATUS_WAIT_MAX_TIMEOUT_MS,
  type AgentStatusWaitResult,
} from "../src/agent-status-wait.ts";
import { sessionBus } from "../src/buses.ts";
import { cacheSessionWorker, evictSessionWorker } from "../src/byte-hub.ts";

const AGED_SESSION_ID = asSessionId("11111111-1111-4111-8111-111111111111");
const YOUNG_SESSION_ID = asSessionId("22222222-2222-4222-8222-222222222222");
const DRIVER_SESSION_ID = asSessionId("33333333-3333-4333-8333-333333333333");
const SESSION_IDS = [AGED_SESSION_ID, YOUNG_SESSION_ID, DRIVER_SESSION_ID];
const WORKER_FP = asWorkerFp("a1".repeat(32));
const STATUS_EPOCH = StatusEpoch.parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
const OCCUPANT_ID = AgentOccupantId.parse("11111111-aaaa-4aaa-8aaa-111111111111");

function status(
  sessionId: string,
  overrides: Partial<AgentStatusUpdateValue> = {},
): AgentStatusUpdateValue {
  return AgentStatusUpdate.parse({
    session_id: sessionId,
    agent_id: "omp",
    state: "working",
    revision: 1,
    completed_revision: 0,
    updated_at: 1_800_000_000_000,
    active: true,
    status_epoch: STATUS_EPOCH,
    occupant_id: OCCUPANT_ID,
    source: "integration",
    ...overrides,
  });
}

function closeSession(sessionId: string): void {
  sessionBus.publish(SessionEvent.parse({
    kind: "closed",
    session_id: sessionId,
    exit_code: 0,
    ts: 1_800_000_000_001,
  }));
}

/** `session_closed` while the tombstone is retained, `occupant_changed` once it
 *  is gone; registration resolves against the retained view without sweeping. */
function observeClosure(sessionId: string): Promise<AgentStatusWaitResult> {
  return waitForAgentStatus({
    sessionId,
    statusEpoch: STATUS_EPOCH,
    occupantId: OCCUPANT_ID,
    desiredStates: ["idle"],
    timeoutMs: AGENT_STATUS_WAIT_MAX_TIMEOUT_MS,
  }, new AbortController().signal);
}

function expireTombstone(sessionId: string): void {
  expect(_backdateAgentStatusTombstone(
    sessionId,
    Date.now() - AGENT_STATUS_WAIT_MAX_TIMEOUT_MS - 1,
  )).toBe(true);
}

beforeEach(() => {
  stopAgentStatusHub();
  startAgentStatusHub();
  for (const sessionId of SESSION_IDS) cacheSessionWorker(sessionId, WORKER_FP, 7);
});

afterEach(() => {
  stopAgentStatusHub();
  for (const sessionId of SESSION_IDS) evictSessionWorker(sessionId);
});

describe("agent status tombstone expiry", () => {
  test("a worker frame drops expired tombstones and keeps younger ones", async () => {
    expect(handleWorkerAgentStatus(WORKER_FP, status(AGED_SESSION_ID))).toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, status(YOUNG_SESSION_ID))).toBe("accepted");
    closeSession(AGED_SESSION_ID);
    closeSession(YOUNG_SESSION_ID);
    await expect(observeClosure(AGED_SESSION_ID))
      .resolves.toEqual({ outcome: "session_closed" });

    expireTombstone(AGED_SESSION_ID);
    expect(handleWorkerAgentStatus(WORKER_FP, status(DRIVER_SESSION_ID))).toBe("accepted");

    await expect(observeClosure(AGED_SESSION_ID))
      .resolves.toEqual({ outcome: "occupant_changed" });
    await expect(observeClosure(YOUNG_SESSION_ID))
      .resolves.toEqual({ outcome: "session_closed" });
    // Re-admitting the exact epoch/occupant the close retired only succeeds
    // once the session's admission order went with its tombstone.
    expect(handleWorkerAgentStatus(WORKER_FP, status(AGED_SESSION_ID, { revision: 2 })))
      .toBe("accepted");
    expect(handleWorkerAgentStatus(WORKER_FP, status(YOUNG_SESSION_ID, { revision: 2 })))
      .toBe("stale");
  });

  test("a session close drops expired tombstones with no status traffic", async () => {
    expect(handleWorkerAgentStatus(WORKER_FP, status(AGED_SESSION_ID))).toBe("accepted");
    closeSession(AGED_SESSION_ID);
    expireTombstone(AGED_SESSION_ID);

    closeSession(DRIVER_SESSION_ID);

    await expect(observeClosure(AGED_SESSION_ID))
      .resolves.toEqual({ outcome: "occupant_changed" });
    await expect(observeClosure(DRIVER_SESSION_ID))
      .resolves.toEqual({ outcome: "session_closed" });
  });
});
