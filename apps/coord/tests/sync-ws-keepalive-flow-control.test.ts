/**
 * Owns Sync WebSocket application-window, cumulative-ACK, and flow-control cleanup contracts.
 * Bun discovers this module directly and runs the real feed through deterministic fake sockets.
 * It depends on isolated coordinator state, retained UI seeds, title traffic, and deadline clocks.
 */
import { afterAll, beforeAll, expect, test } from "bun:test";
import type { ServerWebSocket } from "bun";
import { create, toBinary } from "@bufbuild/protobuf";
import {
  SyncClientFrameSchema,
  UiReportStateRequestSchema,
} from "@roost/shared/proto/sync_pb";
import { makeSyncWsHandler, type SyncWsData } from "../src/connect/sync-ws-handler.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { _uiStatesByTab } from "../src/connect/handlers-ui.ts";
import {
  APPLICATION_MAX_UNACKED_BYTES,
  APPLICATION_MAX_UNACKED_FRAMES,
} from "../src/connect/sync-ws-v1-delivery.ts";
import {
  createSyncWsKeepaliveCoordFixture,
  SYNC_WS_KEEPALIVE_DASHBOARD_ID,
  type SyncWsKeepaliveCoordFixture,
} from "./sync-ws-keepalive-coord-fixture.ts";
import {
  encodedTitleFrameBytes,
  openFlowSocket as openFlowSocketWithFixture,
  PressureClock,
  PressureSocket as FixturePressureSocket,
  publishSessionTitle,
  sendAck,
} from "./sync-ws-keepalive-pressure-fixture.ts";

let fixture: SyncWsKeepaliveCoordFixture;
let deps: ConnectDeps;
let fingerprint: string;
const dashboardId = SYNC_WS_KEEPALIVE_DASHBOARD_ID;

class PressureSocket extends FixturePressureSocket {
  constructor(
    keepaliveSendResult: number,
    keepaliveBufferedBytes: number,
    flowControl = false,
  ) {
    super(fixture, keepaliveSendResult, keepaliveBufferedBytes, flowControl);
  }
}

function openFlowSocket(flowControl = true) {
  return openFlowSocketWithFixture(fixture, flowControl);
}

beforeAll(async () => {
  fixture = await createSyncWsKeepaliveCoordFixture();
  ({ deps, fingerprint } = fixture);
});

afterAll(async () => {
  await fixture?.close();
});

test("ACK-paced retained seed crosses 512 frames and a stalled seed exits at 3 seconds", async () => {
  const seedKeys: string[] = [];
  const seedPrefix = `retained-flow-${crypto.randomUUID()}`;
  const now = Date.now();
  for (let index = 0; index < 520; index += 1) {
    const tabId = `${seedPrefix}-${index}`;
    const key = `${dashboardId}:${fingerprint}:${tabId}`;
    const state = create(UiReportStateRequestSchema, {
      tabId,
      activePath: "/",
      folderKey: "",
      layoutJson: "{}",
      focusedPaneId: "",
      visibleSessionIds: [],
    });
    _uiStatesByTab.set(key, { dashboardId, fp: fingerprint, tabId, lastMs: now, state });
    seedKeys.push(key);
  }

  const clock = new PressureClock();
  const handler = makeSyncWsHandler(deps, {
    keepaliveMs: 60_000,
    deadlineClock: clock,
    backpressureLimitBytes: 8 * 1024 * 1024,
    backpressureTimeoutMs: 10_000,
  });

  try {
    const healthy = new PressureSocket(1, 0, true);
    const healthyWs = healthy as unknown as ServerWebSocket<SyncWsData>;
    let publishedDuringSeed = false;
    healthy.onSend = (frameKind) => {
      if (!publishedDuringSeed && frameKind === "workerRoutable") {
        publishedDuringSeed = true;
        publishSessionTitle("live-during-retained-seed");
      }
      queueMicrotask(() => {
        if (
          !healthy.data.pressureClosing
          && healthy.data.lastSentDeliverySeq > healthy.data.ackDeliverySeq
        ) {
          sendAck(handler, healthyWs, healthy.data.lastSentDeliverySeq);
        }
      });
    };

    handler.open(healthyWs);
    const healthySeeded = healthy.data.feed?.seeded;
    if (!healthySeeded) throw new Error("healthy retained seed did not start");
    await healthySeeded;

    expect(healthy.sendCount).toBeGreaterThan(512);
    expect(healthy.frameKinds.filter((kind) => kind === "uiState")).toHaveLength(520);
    expect(healthy.frameKinds.at(-1)).toBe("terminalTitle");
    expect(healthy.closes).toEqual([]);
    expect(healthy.data.deliveryQueue).toEqual([]);
    expect(healthy.data.deliveryWaiters.size).toBe(0);
    handler.close(healthyWs);

    const bounded = new PressureSocket(1, 0, true);
    const boundedWs = bounded as unknown as ServerWebSocket<SyncWsData>;
    handler.open(boundedWs);
    const boundedSeeded = bounded.data.feed?.seeded;
    if (!boundedSeeded) throw new Error("bounded retained seed did not start");
    await Promise.resolve();
    for (let index = 0; index < APPLICATION_MAX_UNACKED_FRAMES; index += 1) {
      publishSessionTitle(`queued-live-${index}`);
      sendAck(handler, boundedWs, bounded.data.lastSentDeliverySeq);
      await Promise.resolve();
      await Promise.resolve();
    }
    expect(bounded.closes).toEqual([]);
    expect(bounded.sendCount).toBe(APPLICATION_MAX_UNACKED_FRAMES + 1);
    publishSessionTitle("queued-live-overflow");
    await boundedSeeded;
    expect(bounded.closes).toEqual([[1013, "sync backpressure"]]);
    expect(bounded.data.feed).toBeNull();
    expect(bounded.data.deliveryQueue).toEqual([]);
    expect(bounded.data.unackedEncodedBytes).toBe(0);
    const boundedSendCount = bounded.sendCount;
    publishSessionTitle("after-buffer-cleanup");
    expect(bounded.sendCount).toBe(boundedSendCount);

    const byteBounded = new PressureSocket(1, 0, true);
    const byteBoundedWs = byteBounded as unknown as ServerWebSocket<SyncWsData>;
    handler.open(byteBoundedWs);
    const byteBoundedSeeded = byteBounded.data.feed?.seeded;
    if (!byteBoundedSeeded) throw new Error("byte-bounded retained seed did not start");
    await Promise.resolve();
    publishSessionTitle("queued-live-byte-overflow", "x".repeat(APPLICATION_MAX_UNACKED_BYTES));
    await byteBoundedSeeded;
    expect(byteBounded.closes).toEqual([[1013, "sync backpressure"]]);
    expect(byteBounded.sendCount).toBe(1);
    expect(byteBounded.data.feed).toBeNull();
    expect(byteBounded.data.deliveryQueue).toEqual([]);
    expect(byteBounded.data.unackedEncodedBytes).toBe(0);

    const stalled = new PressureSocket(1, 0, true);
    const stalledWs = stalled as unknown as ServerWebSocket<SyncWsData>;
    handler.open(stalledWs);
    const stalledSeeded = stalled.data.feed?.seeded;
    if (!stalledSeeded) throw new Error("stalled retained seed did not start");
    await Promise.resolve();
    expect(stalled.sendCount).toBe(1);

    clock.advance(2_999);
    expect(stalled.closes).toEqual([]);
    clock.advance(1);
    await stalledSeeded;

    expect(stalled.closes).toEqual([[1013, "sync backpressure"]]);
    expect(stalled.sendCount).toBe(1);
    expect(stalled.data.feed).toBeNull();
    expect(stalled.data.deliveryQueue).toEqual([]);
    expect(stalled.data.deliveryWaiters.size).toBe(0);
    expect(clock.timers.size).toBe(0);
  } finally {
    for (const key of seedKeys) _uiStatesByTab.delete(key);
  }
});

test("application window accepts 512 frames and rejects the guarded relocation candidate", async () => {
  const harness = await openFlowSocket();
  for (let i = 0; i < 512; i += 1) publishSessionTitle(`frame-${i}`);

  expect(harness.socket.closes).toEqual([]);
  expect(harness.socket.data.deliveryQueue).toHaveLength(512);
  const sentBeforeRelocation = harness.socket.sendCount;
  harness.handler.publishRelocation("handoff", "https://source.test", "https://target.test");

  expect(harness.socket.sendCount).toBe(sentBeforeRelocation);
  expect(harness.socket.frameKinds).not.toContain("coordinatorRelocation");
  expect(harness.socket.closes).toEqual([[1013, "sync backpressure"]]);
  expect(harness.socket.data.deliveryQueue).toEqual([]);
  expect(harness.socket.data.unackedEncodedBytes).toBe(0);
  harness.handler.close(harness.serverSocket);
});

test("application byte preflight accepts through 4 MiB and rejects the next candidate", async () => {
  const harness = await openFlowSocket();
  const limit = 4 * 1024 * 1024;
  const seq = harness.socket.data.lastSentDeliverySeq + 1n;
  let lo = 0;
  let hi = limit;
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    if (encodedTitleFrameBytes(mid, seq) <= limit) lo = mid;
    else hi = mid - 1;
  }
  const acceptedBytes = encodedTitleFrameBytes(lo, seq);
  expect(acceptedBytes).toBeLessThanOrEqual(limit);
  expect(encodedTitleFrameBytes(lo + 1, seq)).toBeGreaterThan(limit);

  publishSessionTitle("large", "x".repeat(lo));
  expect(harness.socket.closes).toEqual([]);
  expect(harness.socket.data.unackedEncodedBytes).toBe(acceptedBytes);
  publishSessionTitle("overflow");

  expect(harness.socket.closes).toEqual([[1013, "sync backpressure"]]);
  expect(harness.socket.data.deliveryQueue).toEqual([]);
  expect(harness.socket.data.unackedEncodedBytes).toBe(0);
  harness.handler.close(harness.serverSocket);
});

test("oldest unacknowledged send closes at its original 3000 ms deadline", async () => {
  const harness = await openFlowSocket();
  publishSessionTitle("oldest");
  harness.clock.advance(1_000);
  publishSessionTitle("later");

  harness.clock.advance(1_999);
  expect(harness.socket.closes).toEqual([]);
  harness.clock.advance(1);
  expect(harness.socket.closes).toEqual([[1013, "sync backpressure"]]);
  harness.handler.close(harness.serverSocket);
});

test("cumulative ACK releases records and re-arms from the next original send time", async () => {
  const harness = await openFlowSocket();
  publishSessionTitle("first");
  const firstSeq = harness.socket.data.lastSentDeliverySeq;
  harness.clock.advance(1_000);
  publishSessionTitle("second");
  const secondSeq = harness.socket.data.lastSentDeliverySeq;
  harness.clock.advance(1_000);
  publishSessionTitle("third");
  const thirdSeq = harness.socket.data.lastSentDeliverySeq;
  const thirdBytes = harness.socket.data.deliveryQueue[2]!.encodedBytes;

  sendAck(harness.handler, harness.serverSocket, secondSeq);
  expect(harness.socket.data.ackDeliverySeq).toBe(secondSeq);
  expect(harness.socket.data.deliveryQueue).toEqual([{
    seq: thirdSeq,
    encodedBytes: thirdBytes,
    sentAtMs: 2_000,
  }]);
  expect(harness.socket.data.unackedEncodedBytes).toBe(thirdBytes);
  expect(firstSeq).toBeLessThan(secondSeq);

  harness.clock.advance(2_999);
  expect(harness.socket.closes).toEqual([]);
  harness.clock.advance(1);
  expect(harness.socket.closes).toEqual([[1013, "sync backpressure"]]);
  harness.handler.close(harness.serverSocket);
});

test("equal and stale cumulative ACKs are idempotent", async () => {
  const harness = await openFlowSocket();
  publishSessionTitle("one");
  const staleSeq = harness.socket.data.lastSentDeliverySeq;
  publishSessionTitle("two");
  const ackSeq = harness.socket.data.lastSentDeliverySeq;

  sendAck(harness.handler, harness.serverSocket, ackSeq);
  sendAck(harness.handler, harness.serverSocket, ackSeq);
  sendAck(harness.handler, harness.serverSocket, staleSeq);

  expect(harness.socket.closes).toEqual([]);
  expect(harness.socket.data.ackDeliverySeq).toBe(ackSeq);
  expect(harness.socket.data.deliveryQueue).toEqual([]);
  expect(harness.socket.data.deliveryTimer).toBeNull();
  harness.clock.advance(10_000);
  expect(harness.socket.closes).toEqual([]);
  harness.handler.close(harness.serverSocket);
});

test("future and malformed ACKs close 1008 and clear accounting", async () => {
  const future = await openFlowSocket();
  publishSessionTitle("future");
  sendAck(
    future.handler,
    future.serverSocket,
    future.socket.data.lastSentDeliverySeq + 1n,
  );
  expect(future.socket.closes).toEqual([[1008, "invalid sync ack"]]);
  expect(future.socket.data.deliveryQueue).toEqual([]);
  expect(future.socket.data.deliveryTimer).toBeNull();
  future.handler.close(future.serverSocket);

  const malformedPayloads: Array<[string, Uint8Array]> = [
    ["invalid wire", new Uint8Array([0xff])],
    ["empty", new Uint8Array()],
    ["encoded zero", toBinary(SyncClientFrameSchema, create(SyncClientFrameSchema, {
      ackDeliverySeq: 0n,
    }))],
    ["unknown trailing field", new Uint8Array([0x08, 0x01, 0x10, 0x01])],
  ];
  for (const [label, payload] of malformedPayloads) {
    const malformed = await openFlowSocket();
    publishSessionTitle(`malformed-${label}`);
    malformed.handler.message(
      malformed.serverSocket,
      payload as unknown as Buffer,
    );
    expect(malformed.socket.closes, label).toEqual([[1008, "invalid sync ack"]]);
    expect(malformed.socket.data.deliveryQueue, label).toEqual([]);
    expect(malformed.socket.data.deliveryTimer, label).toBeNull();
    malformed.handler.close(malformed.serverSocket);
  }
});

test("native drain leaves application records and oldest-age timer intact", async () => {
  const harness = await openFlowSocket();
  harness.socket.dataSendResult = -1;
  harness.socket.dataBufferedBytes = 25;
  publishSessionTitle("native-queued");

  expect(harness.socket.data.pressureTimer).not.toBeNull();
  expect(harness.socket.data.deliveryTimer).not.toBeNull();
  expect(harness.socket.data.deliveryQueue).toHaveLength(1);
  harness.handler.drain(harness.serverSocket);
  expect(harness.socket.data.pressureTimer).toBeNull();
  expect(harness.socket.data.deliveryTimer).not.toBeNull();
  expect(harness.socket.data.deliveryQueue).toHaveLength(1);

  harness.clock.advance(3_000);
  expect(harness.socket.closes).toEqual([[1013, "sync backpressure"]]);
  harness.handler.close(harness.serverSocket);
});

test("normal close idempotently clears native and application state", async () => {
  const harness = await openFlowSocket();
  harness.socket.dataSendResult = -1;
  publishSessionTitle("cleanup");
  expect(harness.clock.timers.size).toBe(2);

  harness.handler.close(harness.serverSocket);
  harness.handler.close(harness.serverSocket);

  expect(harness.clock.timers.size).toBe(0);
  expect(harness.socket.data.pressureTimer).toBeNull();
  expect(harness.socket.data.deliveryTimer).toBeNull();
  expect(harness.socket.data.deliveryQueue).toEqual([]);
  expect(harness.socket.data.unackedEncodedBytes).toBe(0);
  expect(harness.socket.data.feed).toBeNull();
  expect(harness.socket.data.keepaliveTimer).toBeNull();
});

test("legacy sockets remain unsequenced and unenforced", async () => {
  const harness = await openFlowSocket(false);
  for (let i = 0; i < 513; i += 1) publishSessionTitle(`legacy-${i}`);
  harness.clock.advance(3_000);

  expect(harness.socket.closes).toEqual([]);
  expect(harness.socket.deliverySeqs.every((seq) => seq === 0n)).toBe(true);
  expect(harness.socket.data.deliveryQueue).toEqual([]);
  expect(harness.socket.data.deliveryTimer).toBeNull();
  harness.handler.close(harness.serverSocket);
});

test("accepted relocation is sequenced by the guarded sender", async () => {
  const harness = await openFlowSocket();
  harness.handler.publishRelocation("handoff", "https://source.test", "https://target.test");

  expect(harness.socket.frameKinds.at(-1)).toBe("coordinatorRelocation");
  expect(harness.socket.deliverySeqs.at(-1)).toBeGreaterThan(0n);
  expect(harness.socket.closes).toEqual([[undefined, undefined]]);
  harness.handler.close(harness.serverSocket);
  expect(harness.socket.data.deliveryQueue).toEqual([]);
});
