import { expect, test } from "bun:test";
import type { ServerWebSocket } from "bun";
import { clone, create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { PbCellGridFrameSchema } from "@roost/shared/proto/cell_pb";
import {
  FirehoseFrameSchema,
  SyncDomain,
  TerminalTitleFrameSchema,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import {
  mutateCellSubscription,
  type CellSubscriptionMutation,
} from "../src/connect/cell-subscriptions.ts";
import type { SyncDeadlineClock } from "../src/connect/sync-ws-deadline.ts";
import { frameMeta } from "../src/connect/sync-feed-frames.ts";
import type { SyncWsData } from "../src/connect/sync-ws-handler.ts";
import {
  APPLICATION_MAX_UNACKED_FRAMES,
  makeSyncV1Delivery,
} from "../src/connect/sync-ws-v1-delivery.ts";
import {
  makeSyncV2Scheduler,
  type SyncV2Scheduler,
} from "../src/connect/sync-ws-v2-scheduler.ts";
import {
  createSyncV2SocketState,
  V2_DOMAIN_MAX_QUEUED_FRAMES,
  type SyncV2DomainState,
} from "../src/connect/sync-ws-v2-state.ts";

const SESSION_A = "11111111-1111-4111-8111-111111111111";
const SESSION_B = "22222222-2222-4222-8222-222222222222";
const OTHER_SESSION = "33333333-3333-4333-8333-333333333333";
const TARGET_SESSION = "44444444-4444-4444-8444-444444444444";

class TestDeadlineClock implements SyncDeadlineClock {
  private nextTimer = 1;
  private readonly timers = new Set<number>();

  now(): number {
    return 1_000;
  }

  setTimeout(_callback: () => void, _delayMs: number): Timer {
    const timer = this.nextTimer++;
    this.timers.add(timer);
    return timer as unknown as Timer;
  }

  clearTimeout(timer: Timer): void {
    this.timers.delete(timer as unknown as number);
  }
}

class TestSocket {
  readonly data: SyncWsData;
  readonly sent: Uint8Array[] = [];
  readonly closes: Array<[number | undefined, string | undefined]> = [];

  constructor(viewerKey: string) {
    this.data = {
      kind: "sync",
      caller: {
        fingerprint: "sync-v2-scheduler-test",
        label: "sync-v2-scheduler-test",
        keyGeneration: 0,
      },
      sinceEventId: 0,
      viewerKey,
      remoteAddress: null,
      feed: null,
      keepaliveTimer: null,
      reauthAtMs: null,
      reauthTimer: null,
      pressureTimer: null,
      pressureFrame: null,
      pressureClosing: false,
      flowControl: true,
      v2: createSyncV2SocketState(),
      lastSentDeliverySeq: 0n,
      ackDeliverySeq: 0n,
      unackedEncodedBytes: 0,
      deliveryQueue: [],
      deliveryTimer: null,
      deliveryWaiters: new Set(),
    };
  }

  send(payload: unknown): number {
    if (!(payload instanceof Uint8Array)) {
      throw new TypeError("expected a binary Sync frame");
    }
    this.sent.push(payload.slice());
    return 1;
  }

  getBufferedAmount(): number {
    return 0;
  }

  close(code?: number, reason?: string): void {
    this.closes.push([code, reason]);
  }
}

type Delivery = ReturnType<typeof makeSyncV1Delivery>;
type TerminalState = SyncV2DomainState;

interface SchedulerHarness {
  readonly socket: TestSocket;
  readonly ws: ServerWebSocket<SyncWsData>;
  readonly scheduler: SyncV2Scheduler;
  readonly delivery: Delivery;
  readonly subscriptions: CellSubscriptionMutation[];
  readonly terminal: TerminalState;
  subscribe(sessionId: string, clientSeq?: bigint): void;
  withdraw(sessionId: string, clientSeq?: bigint): void;
  cleanupSubscriptions(): void;
}

function makeHarness(viewerKey: string, terminalReady: boolean): SchedulerHarness {
  const clock = new TestDeadlineClock();
  const socket = new TestSocket(viewerKey);
  const ws = socket as unknown as ServerWebSocket<SyncWsData>;
  const terminal = socket.data.v2!.domains.get(SyncDomain.TERMINAL)!;
  terminal.ready = terminalReady;
  const subscriptions: CellSubscriptionMutation[] = [];

  let delivery!: Delivery;
  const scheduler = makeSyncV2Scheduler({
    deadlineClock: clock,
    backpressureLimitBytes: Number.MAX_SAFE_INTEGER,
    backpressureTimeoutMs: 60_000,
    closeForBackpressure(target, reason, frame) {
      delivery.closeForBackpressure(target, reason, frame);
    },
    closeForDroppedFrame(target, frame, encodedBytes, bufferedBytes) {
      delivery.closeForDroppedFrame(target, frame, encodedBytes, bufferedBytes);
    },
    rearmApplicationDeadline(target) {
      delivery.rearmApplicationDeadline(target);
    },
  });
  delivery = makeSyncV1Delivery({
    deadlineClock: clock,
    backpressureLimitBytes: Number.MAX_SAFE_INTEGER,
    backpressureTimeoutMs: 60_000,
    cleanupSocket() {},
    scheduleV2: scheduler.scheduleV2,
  });

  const mutate = (sessionId: string, subscribed: boolean, clientSeq: bigint): void => {
    const mutation = mutateCellSubscription(viewerKey, sessionId, subscribed, clientSeq);
    if (!mutation) {
      throw new Error(`cell subscription mutation was rejected for ${sessionId}`);
    }
    subscriptions.push(mutation);
  };

  return {
    socket,
    ws,
    scheduler,
    delivery,
    subscriptions,
    terminal,
    subscribe(sessionId, clientSeq = 1n) {
      mutate(sessionId, true, clientSeq);
    },
    withdraw(sessionId, clientSeq = 2n) {
      mutate(sessionId, false, clientSeq);
    },
    cleanupSubscriptions() {
      for (let index = subscriptions.length - 1; index >= 0; index -= 1) {
        subscriptions[index]!.rollback();
      }
    },
  };
}

function makeCell(sessionId: string, seq: number, full: boolean): FirehoseFrame {
  return create(FirehoseFrameSchema, {
    frame: {
      case: "cellGrid",
      value: create(PbCellGridFrameSchema, {
        sessionId,
        cols: 80,
        rows: 24,
        seq: BigInt(seq),
        full,
        gridEpoch: `${sessionId}:grid`,
      }),
    },
  });
}

function enqueueCell(harness: SchedulerHarness, frame: FirehoseFrame): void {
  harness.scheduler.enqueueV2Frame(harness.ws, frame, frameMeta(frame));
}

function estimatedTerminalBytes(frame: FirehoseFrame, generation: bigint): number {
  const owned = clone(FirehoseFrameSchema, frame);
  owned.deliverySeq = 0n;
  owned.domain = SyncDomain.TERMINAL;
  owned.domainGeneration = generation;
  return toBinary(FirehoseFrameSchema, owned).byteLength + 10;
}

interface CellIdentity {
  sessionId: string;
  full: boolean;
  seq: bigint;
}

function cellIdentity(frame: FirehoseFrame): CellIdentity {
  if (frame.frame.case !== "cellGrid") {
    throw new TypeError(`expected cellGrid, got ${frame.frame.case ?? "unset"}`);
  }
  return {
    sessionId: frame.frame.value.sessionId,
    full: frame.frame.value.full,
    seq: frame.frame.value.seq,
  };
}

function decodedFrames(socket: TestSocket): FirehoseFrame[] {
  return socket.sent.map((bytes) => fromBinary(FirehoseFrameSchema, bytes));
}

function fillApplicationAckWindow(harness: SchedulerHarness): void {
  const filler = create(FirehoseFrameSchema, {
    frame: {
      case: "terminalTitle",
      value: create(TerminalTitleFrameSchema, {
        sessionId: "application-window-filler",
        title: "filler",
      }),
    },
  });
  for (let index = 0; index < APPLICATION_MAX_UNACKED_FRAMES; index += 1) {
    expect(harness.delivery.sendGuarded(harness.ws, filler)).toBe(true);
  }
  expect(harness.socket.data.deliveryQueue).toHaveLength(APPLICATION_MAX_UNACKED_FRAMES);
  expect(harness.socket.data.lastSentDeliverySeq).toBe(BigInt(APPLICATION_MAX_UNACKED_FRAMES));
  harness.socket.sent.length = 0;
}

async function flushMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

test("withdrawn terminal backlog is pruned before a replacement full frame reaches the queue limit", async () => {
  const harness = makeHarness("scheduler-test:overflow-prune", true);
  try {
    harness.subscribe(SESSION_A, 1n);
    harness.socket.data.v2!.announcedSessions.add(SESSION_A);
    fillApplicationAckWindow(harness);

    const queuedA: FirehoseFrame[] = [];
    for (let seq = 1; seq <= V2_DOMAIN_MAX_QUEUED_FRAMES; seq += 1) {
      const frame = makeCell(SESSION_A, seq, false);
      queuedA.push(frame);
      enqueueCell(harness, frame);
    }
    const queuedABytes = queuedA.reduce(
      (total, frame) => total + estimatedTerminalBytes(frame, harness.terminal.generation),
      0,
    );
    expect(harness.terminal.queue).toHaveLength(V2_DOMAIN_MAX_QUEUED_FRAMES);
    expect(harness.terminal.queuedBytes).toBe(queuedABytes);
    expect(harness.socket.data.v2!.queuedFrames).toBe(V2_DOMAIN_MAX_QUEUED_FRAMES);
    expect(harness.socket.data.v2!.queuedBytes).toBe(queuedABytes);
    expect(harness.socket.closes).toEqual([]);
    await flushMicrotasks();
    expect(harness.socket.data.v2!.schedulerPending).toBe(false);
    expect(harness.socket.sent).toEqual([]);
    expect(harness.terminal.queue).toHaveLength(V2_DOMAIN_MAX_QUEUED_FRAMES);
    expect(harness.terminal.queuedBytes).toBe(queuedABytes);

    harness.withdraw(SESSION_A, 2n);
    harness.subscribe(SESSION_B, 1n);
    harness.socket.data.v2!.announcedSessions.add(SESSION_B);
    const replacement = makeCell(SESSION_B, 10_000, true);
    const replacementBytes = estimatedTerminalBytes(replacement, harness.terminal.generation);
    enqueueCell(harness, replacement);

    expect(harness.socket.closes).toEqual([]);
    expect(harness.terminal.queue.map((item) => cellIdentity(item.frame))).toEqual([{
      sessionId: SESSION_B,
      full: true,
      seq: 10_000n,
    }]);
    expect(harness.terminal.queue.some((item) => item.meta.sessionId === SESSION_A)).toBe(false);
    expect(harness.terminal.queue[0]!.estimatedBytes).toBe(replacementBytes);
    expect(harness.terminal.seedInsertIndex).toBe(0);
    expect(harness.terminal.queuedBytes).toBe(replacementBytes);
    expect(harness.socket.data.v2!.queuedFrames).toBe(1);
    expect(harness.socket.data.v2!.queuedBytes).toBe(replacementBytes);

    const blockedThrough = harness.socket.data.lastSentDeliverySeq;
    expect(harness.delivery.applyCumulativeAck(harness.ws, blockedThrough)).toBe(true);
    await flushMicrotasks();

    const sentCells = decodedFrames(harness.socket).filter(
      (frame) => frame.frame.case === "cellGrid",
    );
    expect(sentCells.map(cellIdentity)).toEqual([{
      sessionId: SESSION_B,
      full: true,
      seq: 10_000n,
    }]);
    expect(sentCells[0]!.domain).toBe(SyncDomain.TERMINAL);
    expect(sentCells[0]!.domainGeneration).toBe(harness.terminal.generation);
    expect(sentCells[0]!.deliverySeq).toBe(blockedThrough + 1n);
    expect(harness.terminal.queue).toEqual([]);
    expect(harness.terminal.seedInsertIndex).toBe(0);
    expect(harness.terminal.queuedBytes).toBe(0);
    expect(harness.socket.data.v2!.queuedFrames).toBe(0);
    expect(harness.socket.data.v2!.queuedBytes).toBe(0);
    expect(harness.socket.closes).toEqual([]);

    expect(harness.delivery.applyCumulativeAck(harness.ws, sentCells[0]!.deliverySeq)).toBe(true);
    await flushMicrotasks();
  } finally {
    harness.cleanupSubscriptions();
  }
});

test("a newer full frame supersedes only older cells for its own session", async () => {
  // Holding the domain pre-ready lets the test inspect the real scheduler queue
  // after all enqueue microtasks without racing egress.
  const harness = makeHarness("scheduler-test:full-supersession", false);
  try {
    harness.subscribe(OTHER_SESSION);
    harness.subscribe(TARGET_SESSION);

    const other = makeCell(OTHER_SESSION, 1, false);
    const oldFull = makeCell(TARGET_SESSION, 2, true);
    const oldDelta = makeCell(TARGET_SESSION, 3, false);
    const newFull = makeCell(TARGET_SESSION, 4, true);
    const followingDelta = makeCell(TARGET_SESSION, 5, false);
    enqueueCell(harness, other);
    enqueueCell(harness, oldFull);
    enqueueCell(harness, oldDelta);
    enqueueCell(harness, newFull);
    enqueueCell(harness, followingDelta);
    await flushMicrotasks();

    const survivors = [other, newFull, followingDelta];
    const survivorBytes = survivors.map((frame) =>
      estimatedTerminalBytes(frame, harness.terminal.generation)
    );
    expect(harness.terminal.queue.map((item) => cellIdentity(item.frame))).toEqual([
      { sessionId: OTHER_SESSION, full: false, seq: 1n },
      { sessionId: TARGET_SESSION, full: true, seq: 4n },
      { sessionId: TARGET_SESSION, full: false, seq: 5n },
    ]);
    expect(harness.terminal.queue.map((item) => item.estimatedBytes)).toEqual(survivorBytes);
    expect(harness.terminal.seedInsertIndex).toBe(0);
    expect(harness.terminal.queuedBytes).toBe(
      survivorBytes.reduce((total, bytes) => total + bytes, 0),
    );
    expect(harness.socket.data.v2!.queuedFrames).toBe(3);
    expect(harness.socket.data.v2!.queuedBytes).toBe(harness.terminal.queuedBytes);
    expect(harness.socket.data.v2!.schedulerPending).toBe(false);
    expect(harness.socket.sent).toEqual([]);
    expect(harness.socket.closes).toEqual([]);
  } finally {
    harness.cleanupSubscriptions();
  }
});

test("delta-only terminal chains retain FIFO order", async () => {
  const harness = makeHarness("scheduler-test:delta-fifo", false);
  try {
    harness.subscribe(TARGET_SESSION);
    const deltas = [
      makeCell(TARGET_SESSION, 11, false),
      makeCell(TARGET_SESSION, 12, false),
      makeCell(TARGET_SESSION, 13, false),
    ];
    for (const delta of deltas) enqueueCell(harness, delta);
    await flushMicrotasks();

    const expectedBytes = deltas.map((frame) =>
      estimatedTerminalBytes(frame, harness.terminal.generation)
    );
    expect(harness.terminal.queue.map((item) => cellIdentity(item.frame))).toEqual([
      { sessionId: TARGET_SESSION, full: false, seq: 11n },
      { sessionId: TARGET_SESSION, full: false, seq: 12n },
      { sessionId: TARGET_SESSION, full: false, seq: 13n },
    ]);
    expect(harness.terminal.queue.map((item) => item.estimatedBytes)).toEqual(expectedBytes);
    expect(harness.terminal.seedInsertIndex).toBe(0);
    expect(harness.terminal.queuedBytes).toBe(
      expectedBytes.reduce((total, bytes) => total + bytes, 0),
    );
    expect(harness.socket.data.v2!.queuedFrames).toBe(3);
    expect(harness.socket.data.v2!.queuedBytes).toBe(harness.terminal.queuedBytes);
    expect(harness.socket.data.v2!.schedulerPending).toBe(false);
    expect(harness.socket.sent).toEqual([]);
    expect(harness.socket.closes).toEqual([]);
  } finally {
    harness.cleanupSubscriptions();
  }
});
