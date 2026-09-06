// Agent-status wait tests pin event-driven occupant fencing and every terminal
// cleanup path. Capacity cases exercise the real per-session/global registries;
// no polling or transport fixture is involved.

import { afterEach, beforeEach, describe, expect, test, vi } from "bun:test";
import {
  AgentOccupantId,
  AgentStatusUpdate,
  SessionEvent,
  StatusEpoch,
  asSessionId,
  asWorkerFp,
  type AgentStatusUpdate as AgentStatusUpdateValue,
  type SessionId,
} from "@roost/shared/wire";
import {
  AGENT_STATUS_WAIT_MAX_GLOBAL,
  AGENT_STATUS_WAIT_MAX_PER_SESSION,
  AgentStatusWaitError,
  _agentStatusWaiterStats,
  handleWorkerAgentStatus,
  startAgentStatusHub,
  stopAgentStatusHub,
  waitForAgentStatus,
  type AgentStatusWaitRequest,
} from "../src/agent-status-hub.ts";
import { sessionBus } from "../src/buses.ts";
import { cacheSessionWorker, evictSessionWorker } from "../src/byte-hub.ts";

const SESSION_ID = asSessionId("11111111-1111-4111-8111-111111111111");
const WORKER_FP = asWorkerFp("a1".repeat(32));
const STATUS_EPOCH = StatusEpoch.parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
const OCCUPANT_A = AgentOccupantId.parse("11111111-aaaa-4aaa-8aaa-111111111111");
const OCCUPANT_B = AgentOccupantId.parse("22222222-aaaa-4aaa-8aaa-222222222222");
const extraSessionIds: SessionId[] = [];

function status(overrides: Partial<AgentStatusUpdateValue> = {}): AgentStatusUpdateValue {
  return AgentStatusUpdate.parse({
    session_id: SESSION_ID,
    agent_id: "omp",
    state: "working",
    revision: 1,
    completed_revision: 0,
    updated_at: 1_800_000_000_000,
    active: true,
    status_epoch: STATUS_EPOCH,
    occupant_id: OCCUPANT_A,
    source: "integration",
    ...overrides,
  });
}

function retain(value: AgentStatusUpdateValue): void {
  expect(handleWorkerAgentStatus(WORKER_FP, value)).toBe("accepted");
}

function waitRequest(overrides: Partial<AgentStatusWaitRequest> = {}): AgentStatusWaitRequest {
  return {
    sessionId: SESSION_ID,
    statusEpoch: STATUS_EPOCH,
    occupantId: OCCUPANT_A,
    desiredStates: ["idle"],
    timeoutMs: 300_000,
    ...overrides,
  };
}

function pendingWait(overrides: Partial<AgentStatusWaitRequest> = {}) {
  return waitForAgentStatus(
    waitRequest(overrides),
    new AbortController().signal,
  );
}

async function capacityError(
  request: AgentStatusWaitRequest,
): Promise<AgentStatusWaitError> {
  try {
    await waitForAgentStatus(request, new AbortController().signal);
  } catch (error) {
    expect(error).toBeInstanceOf(AgentStatusWaitError);
    return error as AgentStatusWaitError;
  }
  throw new Error("expected agent-status wait capacity rejection");
}

beforeEach(() => {
  stopAgentStatusHub();
  startAgentStatusHub();
  cacheSessionWorker(SESSION_ID, WORKER_FP, 7);
  retain(status());
});

afterEach(() => {
  stopAgentStatusHub();
  evictSessionWorker(SESSION_ID);
  for (const sessionId of extraSessionIds.splice(0)) evictSessionWorker(sessionId);
  vi.useRealTimers();
});

describe("agent status occupant waits", () => {
  test("matches the retained exact occupant synchronously after registration", async () => {
    retain(status({ revision: 2, state: "blocked" }));
    await expect(pendingWait({ desiredStates: ["blocked"] }))
      .resolves.toEqual({ outcome: "matched" });
    expect(_agentStatusWaiterStats()).toEqual({ total: 0, sessions: 0 });
  });

  test("fences afterRevision and then matches a future accepted update", async () => {
    const afterRevision = pendingWait({
      desiredStates: ["working"],
      afterRevision: 1,
    });
    expect(_agentStatusWaiterStats().total).toBe(1);
    retain(status({ revision: 2, state: "working" }));
    await expect(afterRevision).resolves.toEqual({ outcome: "matched" });

    const futureState = pendingWait({ desiredStates: ["idle"] });
    retain(status({ revision: 3, state: "blocked" }));
    expect(_agentStatusWaiterStats().total).toBe(1);
    retain(status({ revision: 4, state: "idle", completed_revision: 1 }));
    await expect(futureState).resolves.toEqual({ outcome: "matched" });
    expect(_agentStatusWaiterStats().total).toBe(0);
  });

  test("resolves replacement and inactive updates as occupant_changed", async () => {
    const replacement = pendingWait();
    retain(status({
      revision: 1,
      occupant_id: OCCUPANT_B,
    }));
    await expect(replacement).resolves.toEqual({ outcome: "occupant_changed" });

    const inactive = pendingWait({ occupantId: OCCUPANT_B });
    retain(status({
      revision: 2,
      occupant_id: OCCUPANT_B,
      active: false,
    }));
    await expect(inactive).resolves.toEqual({ outcome: "occupant_changed" });
    expect(_agentStatusWaiterStats().total).toBe(0);
  });

  test("session close wins over the synthetic inactive publication", async () => {
    const waiting = pendingWait();
    sessionBus.publish({
      ...SessionEvent.parse({
        kind: "closed",
        session_id: SESSION_ID,
        exit_code: 0,
        ts: 1_800_000_000_001,
      }),
      _dashboard_id: "agent-status-wait-dashboard",
    });
    await expect(waiting).resolves.toEqual({ outcome: "session_closed" });
    expect(_agentStatusWaiterStats()).toEqual({ total: 0, sessions: 0 });
  });

  test("times out and removes timer, listener, and registry entries", async () => {
    vi.useFakeTimers();
    const waiting = pendingWait({ timeoutMs: 50 });
    vi.advanceTimersByTime(50);
    await expect(waiting).resolves.toEqual({ outcome: "timed_out" });
    expect(_agentStatusWaiterStats()).toEqual({ total: 0, sessions: 0 });
  });

  test("abort rejects as cancellation and leaves no terminal outcome", async () => {
    const controller = new AbortController();
    const waiting = waitForAgentStatus(waitRequest(), controller.signal);
    controller.abort();
    await expect(waiting).rejects.toMatchObject({
      name: "AgentStatusWaitError",
      kind: "canceled",
    });
    expect(_agentStatusWaiterStats()).toEqual({ total: 0, sessions: 0 });
  });

  test("enforces 32 waiters per session and releases them on stop", async () => {
    const waits = Array.from(
      { length: AGENT_STATUS_WAIT_MAX_PER_SESSION },
      () => pendingWait(),
    );
    expect((await capacityError(waitRequest())).capacity).toBe("session");
    expect(_agentStatusWaiterStats()).toEqual({
      total: AGENT_STATUS_WAIT_MAX_PER_SESSION,
      sessions: 1,
    });
    const settled = Promise.allSettled(waits);
    stopAgentStatusHub();
    expect((await settled).every((result) => result.status === "rejected")).toBe(true);
    expect(_agentStatusWaiterStats()).toEqual({ total: 0, sessions: 0 });
  });

  test("enforces 2048 global waiters across independently bounded sessions", async () => {
    const waits: Array<Promise<unknown>> = [];
    const sessionCount = AGENT_STATUS_WAIT_MAX_GLOBAL / AGENT_STATUS_WAIT_MAX_PER_SESSION;
    for (let index = 0; index < sessionCount + 1; index += 1) {
      const suffix = index.toString(16).padStart(12, "0");
      const sessionId = asSessionId(`30000000-0000-4000-8000-${suffix}`);
      const occupantId = AgentOccupantId.parse(`40000000-0000-4000-8000-${suffix}`);
      extraSessionIds.push(sessionId);
      cacheSessionWorker(sessionId, WORKER_FP, 100 + index);
      retain(status({ session_id: sessionId, occupant_id: occupantId }));
      if (index < sessionCount) {
        for (let waiterIndex = 0; waiterIndex < AGENT_STATUS_WAIT_MAX_PER_SESSION; waiterIndex += 1) {
          waits.push(pendingWait({ sessionId, occupantId }));
        }
      } else {
        expect((await capacityError(waitRequest({ sessionId, occupantId }))).capacity).toBe("global");
      }
    }
    expect(_agentStatusWaiterStats().total).toBe(AGENT_STATUS_WAIT_MAX_GLOBAL);
    const settled = Promise.allSettled(waits);
    stopAgentStatusHub();
    expect((await settled).every((result) => result.status === "rejected")).toBe(true);
    expect(_agentStatusWaiterStats()).toEqual({ total: 0, sessions: 0 });
  });

  test("rejects malformed identities, duplicate states, revisions, and timeout bounds", async () => {
    for (const request of [
      waitRequest({ statusEpoch: "invalid" }),
      waitRequest({ desiredStates: [] }),
      waitRequest({ desiredStates: ["idle", "idle"] }),
      waitRequest({ desiredStates: ["done"] }),
      waitRequest({ afterRevision: -1 }),
      waitRequest({ timeoutMs: 0 }),
      waitRequest({ timeoutMs: 300_001 }),
    ]) {
      await expect(waitForAgentStatus(request, new AbortController().signal))
        .rejects.toBeInstanceOf(AgentStatusWaitError);
    }
    expect(_agentStatusWaiterStats()).toEqual({ total: 0, sessions: 0 });
  });
});
