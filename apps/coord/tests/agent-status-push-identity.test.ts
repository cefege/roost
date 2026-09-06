// Push-debounce tests pin status transition classification, carry-forward, and
// timer settlement to one exact epoch/occupant. Source may change within that
// occupant, while replacements must never inherit or trigger its notification.

import { afterEach, beforeEach, expect, test, vi } from "bun:test";
import {
  AgentOccupantId,
  AgentStatusUpdate,
  StatusEpoch,
  asSessionId,
  asWorkerFp,
  type AgentStatusUpdate as AgentStatusUpdateValue,
} from "@roost/shared/wire";
import {
  handleWorkerAgentStatus,
  startAgentStatusHub,
  stopAgentStatusHub,
} from "../src/agent-status-hub.ts";
import { cacheSessionWorker, evictSessionWorker } from "../src/byte-hub.ts";
import type { KyselyDB } from "../src/db/connection.ts";
import type { AgentPushTransition } from "../src/push-dispatch.ts";

const SESSION_ID = asSessionId("11111111-1111-4111-8111-111111111111");
const WORKER_FP = asWorkerFp("a1".repeat(32));
const EPOCH_A = StatusEpoch.parse("ffffffff-ffff-4fff-8fff-ffffffffffff");
const EPOCH_B = StatusEpoch.parse("00000000-0000-4000-8000-000000000000");
const OCCUPANT_A = AgentOccupantId.parse("ffffffff-aaaa-4aaa-8aaa-ffffffffffff");
const OCCUPANT_B = AgentOccupantId.parse("cccccccc-aaaa-4aaa-8aaa-cccccccccccc");
const OCCUPANT_C = AgentOccupantId.parse("88888888-aaaa-4aaa-8aaa-888888888888");
const OCCUPANT_D = AgentOccupantId.parse("44444444-aaaa-4aaa-8aaa-444444444444");

function status(
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

beforeEach(() => {
  stopAgentStatusHub();
  cacheSessionWorker(SESSION_ID, WORKER_FP, 7);
});

afterEach(() => {
  stopAgentStatusHub();
  evictSessionWorker(SESSION_ID);
  vi.useRealTimers();
});

test("pushes classify, carry, and settle only within one exact occupant", async () => {
  vi.useFakeTimers();
  const deliveries: AgentPushTransition[] = [];
  startAgentStatusHub({
    db: {} as KyselyDB,
    pushAllowedOrigins: ["https://push.example"],
    dispatchPush: async (_db, transition) => { deliveries.push(transition); },
  });

  expect(handleWorkerAgentStatus(WORKER_FP, status(
    EPOCH_A,
    OCCUPANT_A,
    { revision: 1 },
  ))).toBe("accepted");
  expect(handleWorkerAgentStatus(WORKER_FP, status(
    EPOCH_A,
    OCCUPANT_A,
    { revision: 2, state: "blocked" },
  ))).toBe("accepted");
  expect(handleWorkerAgentStatus(WORKER_FP, status(
    EPOCH_A,
    OCCUPANT_A,
    { revision: 3, state: "blocked", source: "screen" },
  ))).toBe("accepted");
  vi.advanceTimersByTime(1_000);
  await Promise.resolve();
  expect(deliveries).toEqual([{
    sessionId: SESSION_ID,
    kind: "blocked",
    statusEpoch: EPOCH_A,
    occupantId: OCCUPANT_A,
    revision: 2,
  }]);

  expect(handleWorkerAgentStatus(WORKER_FP, status(
    EPOCH_A,
    OCCUPANT_A,
    { revision: 4, state: "working", source: "screen" },
  ))).toBe("accepted");
  expect(handleWorkerAgentStatus(WORKER_FP, status(
    EPOCH_A,
    OCCUPANT_B,
    { revision: 1, state: "blocked" },
  ))).toBe("accepted");
  vi.advanceTimersByTime(1_000);
  await Promise.resolve();
  expect(deliveries.map((delivery) => delivery.kind)).toEqual(["blocked"]);

  expect(handleWorkerAgentStatus(WORKER_FP, status(
    EPOCH_A,
    OCCUPANT_B,
    { revision: 2, state: "working" },
  ))).toBe("accepted");
  expect(handleWorkerAgentStatus(WORKER_FP, status(
    EPOCH_A,
    OCCUPANT_B,
    { revision: 3, state: "blocked" },
  ))).toBe("accepted");
  expect(handleWorkerAgentStatus(WORKER_FP, status(
    EPOCH_A,
    OCCUPANT_C,
    { revision: 1, state: "blocked" },
  ))).toBe("accepted");
  vi.advanceTimersByTime(1_000);
  await Promise.resolve();
  expect(deliveries.map((delivery) => delivery.kind)).toEqual(["blocked"]);

  expect(handleWorkerAgentStatus(WORKER_FP, status(
    EPOCH_A,
    OCCUPANT_C,
    { revision: 2, state: "working" },
  ))).toBe("accepted");
  expect(handleWorkerAgentStatus(WORKER_FP, status(
    EPOCH_A,
    OCCUPANT_C,
    { revision: 3, state: "idle", completed_revision: 1 },
  ))).toBe("accepted");
  expect(handleWorkerAgentStatus(WORKER_FP, status(
    EPOCH_B,
    OCCUPANT_D,
    { revision: 3, state: "idle", completed_revision: 1 },
  ))).toBe("accepted");
  vi.advanceTimersByTime(1_000);
  await Promise.resolve();
  expect(deliveries.map((delivery) => delivery.kind)).toEqual(["blocked"]);
});

test("replacement during asynchronous dispatch preparation suppresses the old occupant", async () => {
  vi.useFakeTimers();
  let markPreparationStarted!: () => void;
  const preparationStarted = new Promise<void>((resolve) => {
    markPreparationStarted = resolve;
  });
  let releasePreparation!: () => void;
  const preparationReleased = new Promise<void>((resolve) => {
    releasePreparation = resolve;
  });
  const sent: AgentPushTransition[] = [];
  let prepared: AgentPushTransition | undefined;
  startAgentStatusHub({
    db: {} as KyselyDB,
    pushAllowedOrigins: ["https://push.example"],
    dispatchPush: async (_db, transition, _origins, isCurrent) => {
      prepared = transition;
      markPreparationStarted();
      await preparationReleased;
      if (isCurrent()) sent.push(transition);
    },
  });

  expect(handleWorkerAgentStatus(WORKER_FP, status(
    EPOCH_A,
    OCCUPANT_A,
  ))).toBe("accepted");
  expect(handleWorkerAgentStatus(WORKER_FP, status(
    EPOCH_A,
    OCCUPANT_A,
    { revision: 2, state: "blocked" },
  ))).toBe("accepted");
  vi.advanceTimersByTime(1_000);
  await preparationStarted;
  expect(prepared).toEqual({
    sessionId: SESSION_ID,
    kind: "blocked",
    statusEpoch: EPOCH_A,
    occupantId: OCCUPANT_A,
    revision: 2,
  });

  expect(handleWorkerAgentStatus(WORKER_FP, status(
    EPOCH_A,
    OCCUPANT_B,
    { revision: 1 },
  ))).toBe("accepted");
  releasePreparation();
  await Promise.resolve();
  await Promise.resolve();
  expect(sent).toEqual([]);
});
