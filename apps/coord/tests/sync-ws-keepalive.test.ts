/**
 * Owns real keepalive emission, native pressure, and socket-lifecycle contracts.
 * Bun discovers this module directly and gives it coordinator and pressure fixtures.
 * A real server covers interval wiring while fake sockets expose guarded-send state.
 */
import { afterAll, beforeAll, expect, test } from "bun:test";
import type { Server, ServerWebSocket } from "bun";
import { fromBinary } from "@bufbuild/protobuf";
import { FirehoseFrameSchema } from "@roost/shared/proto/sync_pb";
import {
  handleSyncWsUpgrade,
  makeSyncWsHandler,
  type SyncWsData,
} from "../src/connect/sync-ws-handler.ts";
import { createSyncV2SocketState } from "../src/connect/sync-ws-v2-state.ts";
import type { TerminalViewHub } from "../src/connect/terminal-view-hub.ts";
import {
  createSyncWsKeepaliveCoordFixture,
  SYNC_WS_KEEPALIVE_DASHBOARD_ID,
  type SyncWsKeepaliveCoordFixture,
} from "./sync-ws-keepalive-coord-fixture.ts";
import {
  openPressureSocket as openPressureSocketWithFixture,
  PressureSocket as FixturePressureSocket,
  publishSessionTitle,
} from "./sync-ws-keepalive-pressure-fixture.ts";

let fixture: SyncWsKeepaliveCoordFixture;
let deps: SyncWsKeepaliveCoordFixture["deps"];
let jwt: string;
let server: Server<SyncWsData>;
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

function openPressureSocket(
  keepaliveSendResult: number,
  keepaliveBufferedBytes: number,
) {
  return openPressureSocketWithFixture(
    fixture,
    keepaliveSendResult,
    keepaliveBufferedBytes,
  );
}

beforeAll(async () => {
  fixture = await createSyncWsKeepaliveCoordFixture();
  deps = fixture.deps;
  jwt = fixture.jwt;

  // Boot the real Sync WS endpoint, same wiring as main.ts but with a 100ms
  // keepalive (scaled-down from the production 30s) for a fast test.
  server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request, bunServer) {
      const upgradeResponse = await handleSyncWsUpgrade(request, bunServer, deps);
      if (upgradeResponse !== null) return upgradeResponse;
      return new Response("not found", { status: 404 });
    },
    websocket: makeSyncWsHandler(deps, { keepaliveMs: 100 }),
  });
});

afterAll(async () => {
  try {
    server?.stop(true);
  } catch {
    // Cleanup still has to release the database and work directory.
  }
  await fixture?.close();
});

test("open → server sends KeepaliveFrame within keepalive interval", async () => {
  const ws = new WebSocket(
    `ws://127.0.0.1:${server.port}/ws/coord-sync?dashboard=${dashboardId}`,
    ["roost-auth", jwt],
  );
  ws.binaryType = "arraybuffer";
  const { promise: gotKeepalive, resolve: sawKeepalive } = Promise.withResolvers<void>();
  const { promise: opened, resolve: sawOpen } = Promise.withResolvers<void>();
  ws.onopen = () => {
    expect(ws.protocol).toBe("roost-auth");
    sawOpen();
  };
  // Filter for keepalive frames — the empty DB sends no backfill, so the
  // keepalive is the only frame, but filter defensively.
  ws.onmessage = (ev) => {
    const frame = fromBinary(FirehoseFrameSchema, new Uint8Array(ev.data as ArrayBuffer));
    if (frame.frame.case === "keepalive") sawKeepalive();
  };
  // If the keepalive never fires, this hangs and bun:test fails on timeout.
  await Promise.all([opened, gotKeepalive]);
  try {
    ws.close();
  } catch {
    // The server may already have observed the peer close.
  }
}, 5_000);

test("live terminal lease expiry closes the owning v2 socket once", () => {
  let expire = (
    _socketId: string,
    _viewId: string,
    _sessionId: string,
  ): void => {
    throw new Error("terminal lease callback was not installed");
  };
  const terminalViews = {
    setOnLiveViewExpired(handler: typeof expire) {
      expire = handler;
    },
    registerSocket() {},
    closeSocket() {},
  } as unknown as TerminalViewHub;
  const handler = makeSyncWsHandler(deps, {
    keepaliveMs: 60_000,
    terminalViews,
  });
  const socket = new PressureSocket(1, 0, true);
  socket.data.v2 = createSyncV2SocketState();
  const serverSocket = socket as unknown as ServerWebSocket<SyncWsData>;
  handler.open(serverSocket);
  const socketId = socket.data.v2.socketId;

  expire(socketId, "view-live", "flow-session");
  expire(socketId, "view-duplicate", "flow-session");
  expect(socket.closes).toEqual([[1013, "terminal view lease expired"]]);
  expect(socket.data.keepaliveTimer).toBeNull();
});

test("guarded keepalive send=0 closes with retryable backpressure", async () => {
  const harness = await openPressureSocket(0, 0);
  expect(harness.socket.closes).toEqual([[1013, "sync backpressure"]]);
  harness.handler.close(harness.serverSocket);
});

test("queued keepalive drain clears pressure without closing", async () => {
  const harness = await openPressureSocket(-1, 25);
  expect(harness.clock.timers.size).toBe(1);
  harness.handler.drain(harness.serverSocket);
  expect(harness.clock.timers.size).toBe(0);
  harness.clock.advance(100);
  expect(harness.socket.closes).toEqual([]);
  harness.handler.close(harness.serverSocket);
});

test("queued keepalive without drain closes after the pressure deadline", async () => {
  const harness = await openPressureSocket(-1, 25);
  harness.clock.advance(49);
  expect(harness.socket.closes).toEqual([]);
  harness.clock.advance(1);
  expect(harness.socket.closes).toEqual([[1013, "sync backpressure"]]);
  harness.handler.close(harness.serverSocket);
});

test("buffered high-water closes immediately", async () => {
  const harness = await openPressureSocket(1, 101);
  expect(harness.socket.closes).toEqual([[1013, "sync backpressure"]]);
  harness.handler.close(harness.serverSocket);
});

test("v1 live-feed send and buffer-probe exceptions close with 1013", () => {
  for (const failure of ["send", "buffer"] as const) {
    const handler = makeSyncWsHandler(deps, { keepaliveMs: 60_000 });
    const socket = new PressureSocket(1, 0, false);
    const serverSocket = socket as unknown as ServerWebSocket<SyncWsData>;
    handler.open(serverSocket);
    if (failure === "send") socket.sendError = new Error("v1 send failed");
    else socket.bufferedAmountError = new Error("v1 buffer probe failed");

    publishSessionTitle(`v1-${failure}-throw`);
    expect(socket.closes, failure).toEqual([[1013, "sync backpressure"]]);
    expect(socket.data.feed).toBeNull();
  }
});
