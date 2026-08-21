import { expect } from "bun:test";
import type { ServerWebSocket } from "bun";
import { clone, create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { PbCellGridFrameSchema } from "@roost/shared/proto/cell_pb";
import {
  FirehoseFrameSchema,
  SyncDomain,
  TerminalTitleFrameSchema,
  TerminalViewStateFrameSchema,
  TerminalViewStatus,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import type { SyncDeadlineClock } from "../src/connect/sync-ws-deadline.ts";
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
  type SyncV2DomainState,
} from "../src/connect/sync-ws-v2-state.ts";

export const SESSION_A = "11111111-1111-4111-8111-111111111111";
export const SESSION_B = "22222222-2222-4222-8222-222222222222";
export const OTHER_SESSION = "33333333-3333-4333-8333-333333333333";
export const TARGET_SESSION = "44444444-4444-4444-8444-444444444444";

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

export class TestSocket {
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

export interface SchedulerHarness {
  readonly socket: TestSocket;
  readonly ws: ServerWebSocket<SyncWsData>;
  readonly scheduler: SyncV2Scheduler;
  readonly delivery: Delivery;
  readonly terminal: TerminalState;
}

export function makeHarness(viewerKey: string, terminalReady: boolean): SchedulerHarness {
  const clock = new TestDeadlineClock();
  const socket = new TestSocket(viewerKey);
  const ws = socket as unknown as ServerWebSocket<SyncWsData>;
  const terminal = socket.data.v2!.domains.get(SyncDomain.TERMINAL)!;
  terminal.ready = terminalReady;

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

  return { socket, ws, scheduler, delivery, terminal };
}

export function makeCell(sessionId: string, seq: number, full: boolean): FirehoseFrame {
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

export function makeState(sessionId: string, streamId: string): FirehoseFrame {
  return create(FirehoseFrameSchema, {
    frame: {
      case: "terminalViewState",
      value: create(TerminalViewStateFrameSchema, {
        viewId: "55555555-5555-4555-8555-555555555555",
        sessionId,
        revision: 1n,
        active: true,
        streamId,
        status: TerminalViewStatus.ACCEPTED,
        effectiveCols: 80,
        effectiveRows: 24,
      }),
    },
  });
}

export function estimatedTerminalBytes(frame: FirehoseFrame, generation: bigint): number {
  const owned = clone(FirehoseFrameSchema, frame);
  owned.deliverySeq = 0n;
  owned.domain = SyncDomain.TERMINAL;
  owned.domainGeneration = generation;
  return toBinary(FirehoseFrameSchema, owned).byteLength + 10;
}

export interface CellIdentity {
  sessionId: string;
  full: boolean;
  seq: bigint;
}

export function cellIdentity(frame: FirehoseFrame): CellIdentity {
  if (frame.frame.case !== "cellGrid") {
    throw new TypeError(`expected cellGrid, got ${frame.frame.case ?? "unset"}`);
  }
  return {
    sessionId: frame.frame.value.sessionId,
    full: frame.frame.value.full,
    seq: frame.frame.value.seq,
  };
}

export function decodedFrames(socket: TestSocket): FirehoseFrame[] {
  return socket.sent.map((bytes) => fromBinary(FirehoseFrameSchema, bytes));
}

export function fillApplicationAckWindow(harness: SchedulerHarness): void {
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

export async function flushMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}
