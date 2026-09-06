// Focused proof for the volatile agent-status transport lane. It verifies full
// identity encoding, replacement repair under native backpressure, and replay
// of ambiguous successful retirements across the snapshot/reconnect barrier.
import { expect, test, vi } from "bun:test";
import { fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerUpSchema,
  type WAgentStatus,
} from "@roost/shared/proto/worker_transport_pb";
import {
  AgentStatusUpdate,
  type AgentStatusSource,
  type AgentStatusUpdate as AgentStatusUpdateType,
  type WorkerFp,
} from "@roost/shared/wire";
import {
  WS_BUFFERED_HIGH_WATER_BYTES,
} from "../src/transport/coord-link-constants.ts";
import { createCoordLinkOutbox } from "../src/transport/coord-link-outbox.ts";
import type {
  CoordLinkDeps,
  CoordLinkOutbox,
} from "../src/transport/coord-link-types.ts";
import type { SessionEventStore } from "../src/transport/session-event-store.ts";
import { createCoordLinkAgentStatusOutbox } from "../src/transport/coord-link-agent-status.ts";

const SESSION_ID = "11111111-1111-4111-8111-111111111111";
const STATUS_EPOCH = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OCCUPANT_A = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1";
const OCCUPANT_B = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbb2";
const OCCUPANT_C = "cccccccc-cccc-4ccc-8ccc-ccccccccccc3";

function status(
  occupantId: string,
  revision: number,
  active: boolean,
  patch: {
    state?: "working" | "blocked" | "idle";
    source?: AgentStatusSource;
    message?: string;
  } = {},
): AgentStatusUpdateType {
  return AgentStatusUpdate.parse({
    session_id: SESSION_ID,
    agent_id: "omp",
    state: patch.state ?? "working",
    message: patch.message,
    revision,
    completed_revision: 0,
    updated_at: 1_000 + revision,
    active,
    status_epoch: STATUS_EPOCH,
    occupant_id: occupantId,
    source: patch.source ?? "integration",
  });
}

function decodedStatuses(writes: readonly Uint8Array[]): WAgentStatus[] {
  const statuses: WAgentStatus[] = [];
  for (const bytes of writes) {
    const frame = fromBinary(CoordWorkerUpSchema, bytes);
    if (frame.frame.case !== "agentStatus") {
      throw new Error(`unexpected frame ${frame.frame.case}`);
    }
    statuses.push(frame.frame.value);
  }
  return statuses;
}

test("backpressure preserves sent retirement and elides unseen replacement occupants", () => {
  const writes: Uint8Array[] = [];
  let writable = true;
  let drainRequests = 0;
  const outbox = createCoordLinkAgentStatusOutbox({
    encodeUpstream: (frame) => toBinary(CoordWorkerUpSchema, frame),
    tryWriteEncoded: (bytes) => {
      if (!writable) return false;
      writes.push(Uint8Array.from(bytes));
      return true;
    },
    scheduleDrain: () => { drainRequests += 1; },
  });

  const activeA = status(OCCUPANT_A, 1, true, { message: "first" });
  expect(outbox.send(activeA, true)).toBe(true);
  writable = false;
  expect(outbox.send(status(OCCUPANT_A, 2, true, { state: "blocked" }), true)).toBe(true);
  expect(outbox.send(status(OCCUPANT_A, 3, false, { state: "blocked" }), true)).toBe(true);
  expect(outbox.send(status(OCCUPANT_B, 4, true), true)).toBe(true);
  expect(outbox.send(status(OCCUPANT_B, 5, false), true)).toBe(true);
  expect(outbox.send(status(OCCUPANT_C, 6, true, {
    state: "idle",
    source: "screen",
  }), true)).toBe(true);
  expect(outbox.hasPending()).toBe(true);
  expect(drainRequests).toBeGreaterThan(0);

  writable = true;
  outbox.drain();
  expect(outbox.hasPending()).toBe(false);
  const sent = decodedStatuses(writes);
  expect(sent.map((item) => [item.occupantId, item.active])).toEqual([
    [OCCUPANT_A, true],
    [OCCUPANT_A, false],
    [OCCUPANT_C, true],
  ]);
  expect(sent[1]).toMatchObject({
    statusEpoch: STATUS_EPOCH,
    occupantId: OCCUPANT_A,
    source: "integration",
    revision: 3n,
  });
  expect(sent[2]).toMatchObject({
    statusEpoch: STATUS_EPOCH,
    occupantId: OCCUPANT_C,
    source: "screen",
    revision: 6n,
  });
  expect(Object.keys(sent[2]!)).not.toContain("pid");
  expect(Object.keys(sent[2]!)).not.toContain("processId");
});

test("an occupant that never reached the socket needs no retirement frame", () => {
  const writes: Uint8Array[] = [];
  let writable = false;
  const outbox = createCoordLinkAgentStatusOutbox({
    encodeUpstream: (frame) => toBinary(CoordWorkerUpSchema, frame),
    tryWriteEncoded: (bytes) => {
      if (!writable) return false;
      writes.push(Uint8Array.from(bytes));
      return true;
    },
    scheduleDrain: () => undefined,
  });

  outbox.send(status(OCCUPANT_A, 1, true), false);
  outbox.send(status(OCCUPANT_A, 2, false), false);
  outbox.send(status(OCCUPANT_B, 3, true, { state: "blocked" }), false);
  writable = true;
  outbox.drain();

  expect(decodedStatuses(writes).map((item) => [item.occupantId, item.active]))
    .toEqual([[OCCUPANT_B, true]]);
});

test("new worker transport rejects identityless status instead of emitting legacy frames", () => {
  const writes: Uint8Array[] = [];
  const outbox = createCoordLinkAgentStatusOutbox({
    encodeUpstream: (frame) => toBinary(CoordWorkerUpSchema, frame),
    tryWriteEncoded: (bytes) => {
      writes.push(Uint8Array.from(bytes));
      return true;
    },
    scheduleDrain: () => undefined,
  });
  const legacy = AgentStatusUpdate.parse({
    session_id: SESSION_ID,
    agent_id: "omp",
    state: "working",
    revision: 1,
    completed_revision: 0,
    updated_at: 1_001,
    active: true,
  });

  expect(outbox.send(legacy, true)).toBe(false);
  expect(writes).toEqual([]);
  expect(outbox.hasPending()).toBe(false);
});

test("successful retirements replay in order without an active snapshot", () => {
  vi.useFakeTimers();
  const firstWrites: Uint8Array[] = [];
  const secondWrites: Uint8Array[] = [];
  const thirdWrites: Uint8Array[] = [];
  let nextClientSeq = 0;
  const eventStore = {
    pendingEvents: () => [],
    stats: () => ({ blockingReservedRows: 0 }),
    nextClientSeq: () => ++nextClientSeq,
    acknowledge: () => true,
  } as unknown as SessionEventStore;
  let currentStatus: AgentStatusUpdateType | undefined;
  let outbox!: CoordLinkOutbox;
  const deps = {
    sessionEventStore: eventStore,
    onSnapshotReady: () => {
      if (currentStatus) outbox.sendAgentStatus(currentStatus);
    },
  } as unknown as CoordLinkDeps;
  outbox = createCoordLinkOutbox(deps, () => false);
  outbox.activateSnapshotProvider(() => ({
    kind: "snapshot",
    worker_fp: "a".repeat(64) as WorkerFp,
    sessions: [],
    ts: 1_000,
  }));
  const attachAndFinishSnapshot = (
    socket: WebSocket,
    writes: Uint8Array[],
    reconnected: boolean,
  ) => {
    outbox.attachSocket(socket, (bytes) => { writes.push(Uint8Array.from(bytes)); });
    outbox.acceptHelloAck(reconnected);
    const snapshot = fromBinary(CoordWorkerUpSchema, writes.at(-1)!);
    if (snapshot.frame.case !== "event") throw new Error("snapshot was not sent");
    outbox.ackEvent(Number(snapshot.frame.value.clientSeq));
  };
  const socket = () => ({
    readyState: WebSocket.OPEN,
    bufferedAmount: 0,
  } as WebSocket);

  try {
    attachAndFinishSnapshot(socket(), firstWrites, false);
    expect(outbox.sendAgentStatus(status(OCCUPANT_A, 1, true))).toBe(true);
    expect(outbox.sendAgentStatus(status(OCCUPANT_A, 2, false))).toBe(true);
    expect(decodedStatuses(firstWrites.slice(1)).map((item) => item.active))
      .toEqual([true, false]);
    expect(outbox.sendAgentStatus(status(OCCUPANT_B, 3, true))).toBe(true);
    expect(outbox.sendAgentStatus(status(OCCUPANT_B, 4, false))).toBe(true);

    outbox.detachSocket();
    attachAndFinishSnapshot(socket(), secondWrites, true);
    expect(decodedStatuses(secondWrites.slice(1)).map((item) => [
      item.occupantId,
      item.active,
    ])).toEqual([
      [OCCUPANT_A, false],
      [OCCUPANT_B, false],
    ]);

    currentStatus = status(OCCUPANT_C, 5, true);
    expect(outbox.sendAgentStatus(currentStatus)).toBe(true);
    outbox.detachSocket();
    attachAndFinishSnapshot(socket(), thirdWrites, true);
    expect(decodedStatuses(thirdWrites.slice(1)).map((item) => [
      item.occupantId,
      item.active,
    ])).toEqual([
      [OCCUPANT_A, false],
      [OCCUPANT_B, false],
      [OCCUPANT_C, true],
    ]);
  } finally {
    outbox.clearDrainTimer();
    outbox.reset();
    outbox.detachSocket();
    vi.useRealTimers();
  }
});

test("pending replacement survives socket detach and reconnect resend", () => {
  vi.useFakeTimers();
  const firstWrites: Uint8Array[] = [];
  const secondWrites: Uint8Array[] = [];
  let nextClientSeq = 0;
  const eventStore = {
    pendingEvents: () => [],
    stats: () => ({ blockingReservedRows: 0 }),
    nextClientSeq: () => ++nextClientSeq,
    acknowledge: () => true,
  } as unknown as SessionEventStore;
  let outbox!: CoordLinkOutbox;
  const deps = {
    sessionEventStore: eventStore,
    onSnapshotReady: ({ reconnected }: { reconnected: boolean }) => {
      if (reconnected) outbox.sendAgentStatus(status(OCCUPANT_B, 3, true));
    },
  } as unknown as CoordLinkDeps;
  outbox = createCoordLinkOutbox(deps, () => false);
  const firstSocket = {
    readyState: WebSocket.OPEN,
    bufferedAmount: 0,
  } as WebSocket;
  const secondSocket = {
    readyState: WebSocket.OPEN,
    bufferedAmount: 0,
  } as WebSocket;
  outbox.activateSnapshotProvider(() => ({
    kind: "snapshot",
    worker_fp: "a".repeat(64) as WorkerFp,
    sessions: [],
    ts: 1_000,
  }));

  try {
    outbox.attachSocket(firstSocket, (bytes) => { firstWrites.push(Uint8Array.from(bytes)); });
    outbox.acceptHelloAck(false);
    const firstSnapshot = fromBinary(CoordWorkerUpSchema, firstWrites.at(-1)!);
    if (firstSnapshot.frame.case !== "event") throw new Error("first snapshot was not sent");
    outbox.ackEvent(Number(firstSnapshot.frame.value.clientSeq));
    expect(outbox.sendAgentStatus(status(OCCUPANT_A, 1, true))).toBe(true);

    Object.assign(firstSocket, { bufferedAmount: WS_BUFFERED_HIGH_WATER_BYTES });
    expect(outbox.sendAgentStatus(status(OCCUPANT_A, 2, false))).toBe(true);
    expect(outbox.sendAgentStatus(status(OCCUPANT_B, 3, true))).toBe(true);
    outbox.clearDrainTimer();
    outbox.detachSocket();

    outbox.attachSocket(secondSocket, (bytes) => { secondWrites.push(Uint8Array.from(bytes)); });
    outbox.acceptHelloAck(true);
    const secondSnapshot = fromBinary(CoordWorkerUpSchema, secondWrites.at(-1)!);
    if (secondSnapshot.frame.case !== "event") throw new Error("second snapshot was not sent");
    outbox.ackEvent(Number(secondSnapshot.frame.value.clientSeq));

    const reconnectedStatuses = decodedStatuses(secondWrites.slice(1));
    expect(reconnectedStatuses.map((item) => [item.occupantId, item.active])).toEqual([
      [OCCUPANT_A, false],
      [OCCUPANT_B, true],
    ]);
    expect(reconnectedStatuses[1]).toMatchObject({
      statusEpoch: STATUS_EPOCH,
      occupantId: OCCUPANT_B,
      revision: 3n,
    });
  } finally {
    outbox.clearDrainTimer();
    outbox.reset();
    outbox.detachSocket();
    vi.useRealTimers();
  }
});
