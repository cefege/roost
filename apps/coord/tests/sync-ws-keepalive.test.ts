// Sync WebSocket transport contracts: keepalive liveness, native Bun buffer
// pressure, and opted-in application-consumption flow control. Integration
// cases use a real Bun server/JWT; window/ACK cases use a deterministic clock
// and fake socket while retaining the real startSyncFeed wiring.

import { test, expect, beforeAll, afterAll } from "bun:test";
import type { ServerWebSocket } from "bun";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncClientFrameSchema,
  TerminalTitleFrameSchema,
  UiReportStateRequestSchema,
} from "@roost/shared/proto/sync_pb";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { invalidateJwtKey, newJwtCache, signJwt } from "../src/jwt.ts";
import {
  handleSyncWsUpgrade,
  makeSyncWsHandler,
  type SyncWsData,
} from "../src/connect/sync-ws-handler.ts";
import type { SyncDeadlineClock } from "../src/connect/sync-ws-deadline.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import type { CoordConfig } from "@roost/shared/config";
import { titleBus } from "../src/buses.ts";
import { _uiStatesByTab } from "../src/connect/handlers-ui.ts";

let workdir: string;
let cleanup: () => void;
let server: ReturnType<typeof Bun.serve>;
let jwt: string;
let deps: ConnectDeps;
let fingerprint: string;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-sync-keepalive-"));
  const dbPath = join(workdir, "test.db");
  const keyPath = join(workdir, "test.key");
  const authPath = join(workdir, "authorized_keys");
  writeFileSync(authPath, "");

  const opened = openDb(dbPath);
  const db = opened.db;
  const sqlite = opened.sqlite;
  await runMigrations(sqlite);
  const coordKey = await loadOrCreateCoordKey(keyPath);
  const jwtCache = newJwtCache();
  const cfg: CoordConfig = {
    bind: "127.0.0.1:0",
    dbPath, coordKeyPath: keyPath, authorizedKeysPath: authPath,
    webDistPath: "",
    tlsCertPath: undefined, tlsKeyPath: undefined,
    jwtMaxAgeSecs: 300,
    auditRetentionDays: 90,
    relaxedCsp: false,
    trustProxy: false,
    corsAllowedOrigins: [],
    logDir: workdir,
    webPublicUrl: "https://public.example",
    publicUrl: undefined,
    handoffPath: join(workdir, "coord-handoff.json"),
  };
  deps = { db, sqlite, coordKey, jwtCache, cfg };

  // Mint a keypair, authorize it, sign a JWT (same shape as the browser's
  // web-key.ts JWT — sub == fingerprint, verified by verifyJwt at upgrade).
  const keys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const rawPub = new Uint8Array(await crypto.subtle.exportKey("raw", keys.publicKey));
  fingerprint = await fingerprintOf(rawPub);
  await db.insertInto("authorized_keys").values({
    fingerprint, public_key: rawPub, label: "test-web", added_at: Date.now(),
  }).execute();
  const now = Math.floor(Date.now() / 1000);
  jwt = await signJwt(
    { aud: "roost-coordinator", sub: fingerprint, iat: now, exp: now + 60 },
    keys.privateKey, fingerprint,
  );

  // Boot the real Sync WS endpoint, same wiring as main.ts but with a 100ms
  // keepalive (scaled-down from the production 30s) for a fast test.
  server = Bun.serve({
    hostname: "127.0.0.1", port: 0,
    async fetch(req, srv) {
      const up = await handleSyncWsUpgrade(req, srv, deps);
      if (up !== null) return up;
      return new Response("not found", { status: 404 });
    },
    websocket: makeSyncWsHandler(deps, { keepaliveMs: 100 }),
  });

  cleanup = () => {
    try { server.stop(true); } catch { /* ignore */ }
    try { sqlite.close(); } catch { /* ignore */ }
    if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
  };
});

afterAll(() => cleanup?.());

test("open → server sends KeepaliveFrame within keepalive interval", async () => {
  const ws = new WebSocket(
    `ws://127.0.0.1:${server.port}/ws/coord-sync`,
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
  try { ws.close(); } catch { /* ignore */ }
}, 5_000);

test("rejects missing or malformed auth subprotocols before upgrade", async () => {
  const fakeServer = {
    requestIP: () => ({ address: "127.0.0.1" }),
    upgrade: () => {
      throw new Error("upgrade must not run");
    },
  };
  for (const protocol of [undefined, "wrong-marker, credential", "roost-auth", "roost-auth,"]) {
    const headers = new Headers();
    if (protocol !== undefined) headers.set("sec-websocket-protocol", protocol);
    const response = await handleSyncWsUpgrade(
      new Request("https://coord.example/ws/coord-sync", { headers }),
      fakeServer,
      deps,
    );
    expect(response?.status, protocol ?? "missing").toBe(401);
  }
});

test("rejects foreign Origin and negotiates roost-auth for an allowed origin", async () => {
  let upgradeHeaders: HeadersInit | undefined;
  const fakeServer = {
    requestIP: () => ({ address: "127.0.0.1" }),
    upgrade: (_req: Request, opts: { headers?: HeadersInit }) => {
      upgradeHeaders = opts.headers;
      return true;
    },
  };
  const foreign = await handleSyncWsUpgrade(new Request(
    "https://public.example/ws/coord-sync",
    {
      headers: {
        origin: "https://attacker.example",
        "sec-websocket-protocol": `roost-auth, ${jwt}`,
      },
    },
  ), fakeServer, deps);
  expect(foreign?.status).toBe(403);

  const allowed = await handleSyncWsUpgrade(new Request(
    "https://public.example/ws/coord-sync",
    {
      headers: {
        origin: "https://public.example",
        "sec-websocket-protocol": `roost-auth, ${jwt}`,
      },
    },
  ), fakeServer, deps);
  expect(allowed).toBeUndefined();
  expect(new Headers(upgradeHeaders).get("sec-websocket-protocol")).toBe("roost-auth");
});

test("only exact flow=1 enables the application window", async () => {
  for (const [query, expected] of [
    ["", false],
    ["?flow=0", false],
    ["?flow=true", false],
    ["?flow=01", false],
    ["?flow=1", true],
  ] as const) {
    const upgradedData: SyncWsData[] = [];
    const fakeServer = {
      requestIP: () => ({ address: "127.0.0.1" }),
      upgrade: (_req: Request, opts: { data: SyncWsData }) => {
        upgradedData.push(opts.data);
        return true;
      },
    };
    const result = await handleSyncWsUpgrade(new Request(
      `https://public.example/ws/coord-sync${query}`,
      {
        headers: {
          origin: "https://public.example",
          "sec-websocket-protocol": `roost-auth, ${jwt}`,
        },
      },
    ), fakeServer, deps);
    expect(result).toBeUndefined();
    expect(upgradedData[0]?.flowControl, query || "absent").toBe(expected);
  }
});

interface PressureTimer {
  at: number;
  callback: () => void;
}

class PressureClock implements SyncDeadlineClock {
  nowMs = 0;
  nextId = 1;
  timers = new Map<number, PressureTimer>();
  now(): number { return this.nowMs; }
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
        .sort((a, b) => a[1].at - b[1].at)[0];
      if (!due) break;
      this.timers.delete(due[0]);
      this.nowMs = due[1].at;
      due[1].callback();
    }
    this.nowMs = target;
  }
}

class PressureSocket {
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
  onSend: ((frameKind: string) => void) | null = null;
  constructor(
    keepaliveSendResult: number,
    keepaliveBufferedBytes: number,
    flowControl = false,
  ) {
    this.keepaliveSendResult = keepaliveSendResult;
    this.keepaliveBufferedBytes = keepaliveBufferedBytes;
    this.data = {
      kind: "sync",
      caller: { fingerprint, label: "pressure-test", keyGeneration: 0 },
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
    this.onSend?.(frameKind);
    return frameKind === "keepalive" ? this.keepaliveSendResult : this.dataSendResult;
  }
  getBufferedAmount(): number {
    return this.lastFrameKind === "keepalive" ? this.keepaliveBufferedBytes : this.dataBufferedBytes;
  }
  close(code?: number, reason?: string): void { this.closes.push([code, reason]); }
}

async function openPressureSocket(keepaliveSendResult: number, keepaliveBufferedBytes: number) {
  const clock = new PressureClock();
  const handler = makeSyncWsHandler(deps, {
    keepaliveMs: 5,
    deadlineClock: clock,
    backpressureLimitBytes: 100,
    backpressureTimeoutMs: 50,
  });
  const socket = new PressureSocket(keepaliveSendResult, keepaliveBufferedBytes);
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

interface AckHandler {
  message(ws: ServerWebSocket<SyncWsData>, message: string | Buffer): void;
}

function sendAck(
  handler: AckHandler,
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
function publishSessionTitle(label: string, title = label): void {
  titleBus.publish({ session_id: "flow-session", title });
}

async function openFlowSocket(flowControl = true) {
  const clock = new PressureClock();
  const handler = makeSyncWsHandler(deps, {
    keepaliveMs: 60_000,
    deadlineClock: clock,
    backpressureLimitBytes: 8 * 1024 * 1024,
    backpressureTimeoutMs: 10_000,
  });
  const socket = new PressureSocket(1, 0, flowControl);
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
    if (frameKind === "pairRequestDelta") resolvePairSeed();
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

test("ACK-paced retained seed crosses 512 frames and a stalled seed exits at 3 seconds", async () => {
  const seedKeys: string[] = [];
  const seedPrefix = `retained-flow-${crypto.randomUUID()}`;
  const now = Date.now();
  for (let index = 0; index < 520; index += 1) {
    const tabId = `${seedPrefix}-${index}`;
    const key = `${fingerprint}:${tabId}`;
    const state = create(UiReportStateRequestSchema, {
      tabId,
      activePath: "/",
      folderKey: "",
      layoutJson: "{}",
      focusedPaneId: "",
      visibleSessionIds: [],
    });
    _uiStatesByTab.set(key, { fp: fingerprint, tabId, lastMs: now, state });
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

function encodedTitleFrameBytes(textLength: number, deliverySeq: bigint): number {
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

test("revocation between accepted upgrade and open closes before feed registration", async () => {
  let acceptedData: SyncWsData | undefined;
  const fakeServer = {
    requestIP: () => ({ address: "127.0.0.1" }),
    upgrade: (_req: Request, options: { data: SyncWsData }) => {
      acceptedData = options.data;
      return true;
    },
  };
  const accepted = await handleSyncWsUpgrade(new Request(
    "https://public.example/ws/coord-sync",
    {
      headers: {
        origin: "https://public.example",
        "sec-websocket-protocol": `roost-auth, ${jwt}`,
      },
    },
  ), fakeServer, deps);
  expect(accepted).toBeUndefined();
  if (!acceptedData) throw new Error("upgrade data was not captured");
  invalidateJwtKey(deps.jwtCache, acceptedData.caller.fingerprint);
  let closed: [number, string] | undefined;
  const ws = {
    data: acceptedData,
    close: (code: number, reason: string) => { closed = [code, reason]; },
  };
  makeSyncWsHandler(deps).open(ws as never);
  expect(closed).toEqual([4001, "revoked"]);
  expect(acceptedData.feed).toBeNull();
});
