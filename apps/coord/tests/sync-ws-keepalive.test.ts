// Regression: coord sends a FirehoseFrame{keepalive} on each Sync WS so the
// browser stale-link watchdog (apps/web/src/store/sync-watchdog.ts) can tell
// a half-open connection from a merely idle session. Mirrors worker-conn.ts.
// Integration test: real Bun.serve + real JWT auth + real startSyncFeed
// (empty DB → no backfill frames → the keepalive is the only thing that
// arrives). Real-clock keepalive interval IS the behavior under test — the
// ts-no-test-timers exception (real platform-clock timer behavior) applies.

import { test, expect, beforeAll, afterAll } from "bun:test";
import type { ServerWebSocket } from "bun";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fromBinary } from "@bufbuild/protobuf";
import { FirehoseFrameSchema } from "@roost/shared/proto/sync_pb";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { fingerprintOf, invalidateJwtKey, newJwtCache, signJwt } from "../src/jwt.ts";
import {
  handleSyncWsUpgrade,
  makeSyncWsHandler,
  type SyncDeadlineClock,
  type SyncWsData,
} from "../src/connect/sync-ws-handler.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import type { CoordConfig } from "@roost/shared/config";

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
  onSend: ((frameKind: string) => void) | null = null;
  constructor(keepaliveSendResult: number, keepaliveBufferedBytes: number) {
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
    };
  }
  send(payload: unknown): number {
    if (!(payload instanceof Uint8Array)) throw new TypeError("expected binary Sync frame");
    const frameKind = fromBinary(FirehoseFrameSchema, payload).frame.case ?? "unknown";
    this.lastFrameKind = frameKind;
    this.frameKinds.push(frameKind);
    this.sendCount += 1;
    this.onSend?.(frameKind);
    return frameKind === "keepalive" ? this.keepaliveSendResult : 1;
  }
  getBufferedAmount(): number {
    return this.lastFrameKind === "keepalive" ? this.keepaliveBufferedBytes : 0;
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
