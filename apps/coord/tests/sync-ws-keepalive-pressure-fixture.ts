/**
 * Owns deterministic clock and socket fixtures shared by keepalive and flow-control suites.
 * Discovered tests pair these fakes with a per-file coordinator fixture to exercise real handler wiring.
 * It depends on Sync protobuf encoding, the title bus, and the handler's deadline-clock seam.
 */
import type { ServerWebSocket } from "bun";
import { expect } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncClientFrameSchema,
  TerminalTitleFrameSchema,
} from "@roost/shared/proto/sync_pb";
import { titleBus } from "../src/buses.ts";
import {
  makeSyncWsHandler,
  type SyncWsData,
} from "../src/connect/sync-ws-handler.ts";
import type { WsDeadlineClock } from "../src/connect/ws-auth-deadline.ts";
import {
  SYNC_WS_KEEPALIVE_DASHBOARD_ID,
  type SyncWsKeepaliveCoordFixture,
} from "./sync-ws-keepalive-coord-fixture.ts";

interface PressureTimer {
  at: number;
  callback: () => void;
}

export interface PressureHandler {
  open(ws: ServerWebSocket<SyncWsData>): void;
  message(ws: ServerWebSocket<SyncWsData>, message: string | Buffer): void;
  drain(ws: ServerWebSocket<SyncWsData>): void;
  close(ws: ServerWebSocket<SyncWsData>): void;
  publishRelocation(handoffId: string, sourceUrl: string, targetUrl: string): void;
}

export interface PressureHarness {
  clock: PressureClock;
  handler: PressureHandler;
  socket: PressureSocket;
  serverSocket: ServerWebSocket<SyncWsData>;
}

export class PressureClock implements WsDeadlineClock {
  nowMs = 0;
  nextId = 1;
  timers = new Map<number, PressureTimer>();

  now(): number {
    return this.nowMs;
  }

  setTimeout(callback: () => void, delayMs: number): Timer {
    const id = this.nextId++;
    this.timers.set(id, { at: this.nowMs + delayMs, callback });
    return id as unknown as Timer;
  }

  clearTimeout(timer: Timer): void {
    this.timers.delete(timer as unknown as number);
  }

  advance(ms: number): void {
    const target = this.nowMs + ms;
    while (true) {
      const due = [...this.timers.entries()]
        .filter(([, timer]) => timer.at <= target)
        .sort((leftEntry, rightEntry) => leftEntry[1].at - rightEntry[1].at)[0];
      if (!due) break;
      this.timers.delete(due[0]);
      this.nowMs = due[1].at;
      due[1].callback();
    }
    this.nowMs = target;
  }
}

export class PressureSocket {
  readonly data: SyncWsData;
  readonly keepaliveSendResult: number;
  readonly keepaliveBufferedBytes: number;
  sendCount = 0;
  lastFrameKind = "";
  frameKinds: string[] = [];
  closes: Array<[number | undefined, string | undefined]> = [];
  deliverySeqs: bigint[] = [];
  dataSendResult = 1;
  dataBufferedBytes = 0;
  sendError: Error | null = null;
  bufferedAmountError: Error | null = null;
  onSend: ((frameKind: string) => void) | null = null;

  constructor(
    fixture: SyncWsKeepaliveCoordFixture,
    keepaliveSendResult: number,
    keepaliveBufferedBytes: number,
    flowControl = false,
  ) {
    this.keepaliveSendResult = keepaliveSendResult;
    this.keepaliveBufferedBytes = keepaliveBufferedBytes;
    this.data = {
      kind: "sync",
      caller: {
        fingerprint: fixture.fingerprint,
        label: "pressure-test",
        keyGeneration: 0,
        validUntilMs: Date.now() + 60_000,
      },
      actor: {
        dashboardId: SYNC_WS_KEEPALIVE_DASHBOARD_ID,
      },
      readOnly: false,
      scope: {
        dashboardId: SYNC_WS_KEEPALIVE_DASHBOARD_ID,
        workerFps: new Set(),
        sessionIds: new Set(["flow-session"]),
        workspaceIds: new Set(),
      },
      sinceEventId: 0,
      viewerKey: null,
      feed: null,
      keepaliveTimer: null,
      reauthAtMs: null,
      reauthTimer: null,
      pressureTimer: null,
      pressureFrame: null,
      pressureClosing: false,
      flowControl,
      lastSentDeliverySeq: 0n,
      ackDeliverySeq: 0n,
      unackedEncodedBytes: 0,
      deliveryQueue: [],
      deliveryTimer: null,
      deliveryWaiters: new Set(),
    };
  }

  send(payload: unknown): number {
    if (!(payload instanceof Uint8Array)) throw new TypeError("expected binary Sync frame");
    const frame = fromBinary(FirehoseFrameSchema, payload);
    const frameKind = frame.frame.case ?? "unknown";
    this.lastFrameKind = frameKind;
    this.frameKinds.push(frameKind);
    this.deliverySeqs.push(frame.deliverySeq);
    this.sendCount += 1;
    if (this.sendError) throw this.sendError;
    this.onSend?.(frameKind);
    return frameKind === "keepalive" ? this.keepaliveSendResult : this.dataSendResult;
  }

  getBufferedAmount(): number {
    if (this.bufferedAmountError) throw this.bufferedAmountError;
    return this.lastFrameKind === "keepalive"
      ? this.keepaliveBufferedBytes
      : this.dataBufferedBytes;
  }

  close(code?: number, reason?: string): void {
    this.closes.push([code, reason]);
  }
}

export async function openPressureSocket(
  fixture: SyncWsKeepaliveCoordFixture,
  keepaliveSendResult: number,
  keepaliveBufferedBytes: number,
): Promise<PressureHarness> {
  const clock = new PressureClock();
  const handler = makeSyncWsHandler(fixture.deps, {
    keepaliveMs: 5,
    deadlineClock: clock,
    backpressureLimitBytes: 100,
    backpressureTimeoutMs: 50,
  });
  const socket = new PressureSocket(fixture, keepaliveSendResult, keepaliveBufferedBytes);
  const serverSocket = socket as unknown as ServerWebSocket<SyncWsData>;
  const { promise: sent, resolve: resolveSent } = Promise.withResolvers<void>();
  socket.onSend = (frameKind) => {
    if (frameKind !== "keepalive") return;
    clearInterval(socket.data.keepaliveTimer ?? undefined);
    socket.data.keepaliveTimer = null;
    resolveSent();
  };
  // Real interval is intentional: this proves production keepalives use the
  // guarded sender. Completion waits on the send callback, never a guessed sleep.
  handler.open(serverSocket);
  await sent;
  expect(socket.frameKinds).toContain("keepalive");
  return { clock, handler, socket, serverSocket };
}

export function sendAck(
  handler: PressureHandler,
  serverSocket: ServerWebSocket<SyncWsData>,
  deliverySeq: bigint,
): void {
  handler.message(
    serverSocket,
    toBinary(SyncClientFrameSchema, create(SyncClientFrameSchema, {
      ackDeliverySeq: deliverySeq,
    })) as unknown as Buffer,
  );
}

/** Generic traffic generator: one unfiltered per-session Sync frame whose
 *  payload size is caller-controlled. Nothing here is title-specific — these
 *  tests are about delivery-queue, ACK, backpressure and keepalive mechanics. */
export function publishSessionTitle(label: string, title = label): void {
  titleBus.publish({
    session_id: "flow-session",
    title,
    _dashboard_id: SYNC_WS_KEEPALIVE_DASHBOARD_ID,
  });
}

export async function openFlowSocket(
  fixture: SyncWsKeepaliveCoordFixture,
  flowControl = true,
): Promise<PressureHarness> {
  const clock = new PressureClock();
  const handler = makeSyncWsHandler(fixture.deps, {
    keepaliveMs: 60_000,
    deadlineClock: clock,
    backpressureLimitBytes: 8 * 1024 * 1024,
    backpressureTimeoutMs: 10_000,
  });
  const socket = new PressureSocket(fixture, 1, 0, flowControl);
  const serverSocket = socket as unknown as ServerWebSocket<SyncWsData>;
  const { promise: pairSeed, resolve: resolvePairSeed } = Promise.withResolvers<void>();
  socket.onSend = (frameKind) => {
    if (flowControl) {
      queueMicrotask(() => {
        if (
          !socket.data.pressureClosing
          && socket.data.lastSentDeliverySeq > socket.data.ackDeliverySeq
        ) {
          sendAck(handler, serverSocket, socket.data.lastSentDeliverySeq);
        }
      });
    }
    if (frameKind === "workerRoutable") resolvePairSeed();
  };
  handler.open(serverSocket);
  const retainedSeed = socket.data.feed?.seeded;
  await pairSeed;
  if (retainedSeed) await retainedSeed;
  clearInterval(socket.data.keepaliveTimer ?? undefined);
  socket.data.keepaliveTimer = null;
  socket.onSend = null;
  if (flowControl) {
    sendAck(handler, serverSocket, socket.data.lastSentDeliverySeq);
    expect(socket.data.deliveryQueue).toEqual([]);
    expect(socket.data.deliveryTimer).toBeNull();
  }
  socket.frameKinds.length = 0;
  socket.deliverySeqs.length = 0;
  return { clock, handler, socket, serverSocket };
}

export function encodedTitleFrameBytes(textLength: number, deliverySeq: bigint): number {
  return toBinary(FirehoseFrameSchema, create(FirehoseFrameSchema, {
    deliverySeq,
    frame: {
      case: "terminalTitle",
      value: create(TerminalTitleFrameSchema, {
        sessionId: "flow-session",
        title: "x".repeat(textLength),
      }),
    },
  })).byteLength;
}
