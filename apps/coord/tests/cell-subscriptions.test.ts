// Per-tab cell and cursor-presence fanout. Cell grids are subscription-filtered;
// compact terminal-link mappings are intentionally delivered to every Sync
// browser so its registry can retain mappings for offscreen panes.
//
// Integration: real Bun.serve + real JWT auth + the real startSyncFeed wiring,
// driving globalCellBus, coordinator-internal globalBytesBus, terminalLinkBus,
// and globalPresenceBus. Raw PTY bytes must never cross Sync.
//
// Frames are collected up to a BARRIER frame on an unfiltered bus rather than a
// sleep: one socket delivers in order, so once the barrier lands every frame
// published before it has either arrived or been filtered out.

import { test, expect, beforeAll, afterAll, setSystemTime } from "bun:test";
import type { Server } from "bun";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { create, fromBinary } from "@bufbuild/protobuf";
import { FirehoseFrameSchema } from "@roost/shared/proto/sync_pb";
import { PbCellGridFrameSchema } from "@roost/shared/proto/cell_pb";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { newJwtCache, signJwt, fingerprintOf } from "../src/jwt.ts";
import { handleSyncWsUpgrade, makeSyncWsHandler } from "../src/connect/sync-ws-handler.ts";
import {
  _reapCellSubscriptions,
  mutateCellSubscription,
  subscribeCells,
  unsubscribeCells,
  isSubscribed,
} from "../src/connect/cell-subscriptions.ts";
import { globalCellBus, globalBytesBus, globalPresenceBus, terminalLinkBus, workerRoutableBus } from "../src/buses.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import type { SyncWsData } from "../src/connect/sync-ws-handler.ts";
import type { CoordConfig } from "@roost/shared/config";
import { VIEWER_CLAIM_TTL_MS } from "@roost/shared/viewport";

const TAB_ID = "tab-alpha";
const WATCHED = "11111111-1111-1111-1111-111111111111";
const OTHER = "22222222-2222-2222-2222-222222222222";
const BARRIER_FP = "barrier-sentinel";

let workdir: string;
let cleanup: () => void;
let server: Server<SyncWsData>;
let jwt: string;
let fingerprint: string;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-cell-subs-"));
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
  const cfg: CoordConfig = { trustProxy: false, bind: "127.0.0.1:0",
  dbPath, coordKeyPath: keyPath, authorizedKeysPath: authPath,
  webDistPath: "",
  tlsCertPath: undefined, tlsKeyPath: undefined,
  jwtMaxAgeSecs: 300,
  auditRetentionDays: 90,
  relaxedCsp: false,
  corsAllowedOrigins: [],
  logDir: workdir,
  publicUrl: undefined,
  handoffPath: join(workdir, "coord-handoff.json"), }
  const deps: ConnectDeps = { db, sqlite, coordKey, jwtCache, cfg };

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

  // Keepalive pushed far out so it can never be mistaken for a data frame.
  server = Bun.serve<SyncWsData, never>({
    hostname: "127.0.0.1", port: 0,
    async fetch(req, srv) {
      const up = await handleSyncWsUpgrade(req, srv, deps);
      if (up !== null) return up;
      return new Response("not found", { status: 404 });
    },
    websocket: makeSyncWsHandler(deps, { keepaliveMs: 60_000 }),
  });

  cleanup = () => {
    try { server.stop(true); } catch { /* ignore */ }
    try { sqlite.close(); } catch { /* ignore */ }
    if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
  };
});

afterAll(() => cleanup?.());

/** Open a socket, publish per-session cell/link frames plus coordinator-internal
 * raw bytes, and return the frame ids that cross Sync. */
async function fanoutFor(query: string): Promise<string[]> {
  const ws = new WebSocket(
    `ws://127.0.0.1:${server.port}/ws/coord-sync${query}`,
    ["roost-auth", jwt],
  );
  ws.binaryType = "arraybuffer";
  const seen: string[] = [];
  const { promise: barrierSeen, resolve: sawBarrier } = Promise.withResolvers<void>();
  ws.onmessage = (ev) => {
    const frame = fromBinary(FirehoseFrameSchema, new Uint8Array(ev.data as ArrayBuffer));
    if (frame.frame.case === "cellGrid") seen.push(`cell:${frame.frame.value.sessionId}`);
    else if (frame.frame.case === "terminalLink") seen.push(`link:${frame.frame.value.sessionId}`);
    else if (frame.frame.case === "workerRoutable" && frame.frame.value.fps.includes(BARRIER_FP)) sawBarrier();
  };
  const { promise: open, resolve: opened } = Promise.withResolvers<void>();
  ws.onopen = () => opened();
  await open;
  globalCellBus.publish(create(PbCellGridFrameSchema, { sessionId: WATCHED, seq: 1n }));
  globalCellBus.publish(create(PbCellGridFrameSchema, { sessionId: OTHER, seq: 1n }));
  globalBytesBus.publish({ session_id: WATCHED, bytes: new Uint8Array([1]) });
  globalBytesBus.publish({ session_id: OTHER, bytes: new Uint8Array([2]) });
  terminalLinkBus.publish({ session_id: WATCHED, text: "watched", uri: "https://example.test/watched" });
  terminalLinkBus.publish({ session_id: OTHER, text: "other", uri: "https://example.test/other" });
  workerRoutableBus.publish({ fps: [BARRIER_FP] });
  await barrierSeen;
  try { ws.close(); } catch { /* ignore */ }
  return seen.sort();
}

type TestPresencePayload = {
  kind: "presence-delta" | "viewers";
  viewer_id?: string;
  cursor_col?: number;
  cursor_row?: number;
  fps?: string[];
};

async function presenceFanoutFor(query: string): Promise<TestPresencePayload[]> {
  const ws = new WebSocket(
    `ws://127.0.0.1:${server.port}/ws/coord-sync${query}`,
    ["roost-auth", jwt],
  );
  ws.binaryType = "arraybuffer";
  const seen: TestPresencePayload[] = [];
  const { promise: barrierSeen, resolve: sawBarrier } = Promise.withResolvers<void>();
  ws.onmessage = (ev) => {
    const frame = fromBinary(FirehoseFrameSchema, new Uint8Array(ev.data as ArrayBuffer));
    if (frame.frame.case === "sessionPresence" && frame.frame.value.sessionId === WATCHED) {
      seen.push(JSON.parse(frame.frame.value.payloadJson) as TestPresencePayload);
    } else if (
      frame.frame.case === "workerRoutable"
      && frame.frame.value.fps.includes(BARRIER_FP)
    ) {
      sawBarrier();
    }
  };
  const { promise: open, resolve: opened } = Promise.withResolvers<void>();
  ws.onopen = () => opened();
  await open;

  const alpha = `${fingerprint}:${TAB_ID}`;
  const beta = `${fingerprint}:tab-beta`;
  globalPresenceBus.publish({
    session_id: WATCHED,
    data: { kind: "presence-delta", viewer_id: alpha, cursor_col: 1, cursor_row: 2 },
  });
  globalPresenceBus.publish({
    session_id: WATCHED,
    data: { kind: "presence-delta", viewer_id: beta, cursor_col: 4, cursor_row: 5 },
  });
  globalPresenceBus.publish({
    session_id: WATCHED,
    data: { kind: "viewers", fps: [alpha, beta] },
  });
  workerRoutableBus.publish({ fps: [BARRIER_FP] });
  await barrierSeen;
  try { ws.close(); } catch { /* ignore */ }
  return seen;
}

test("a tab filters cells but retains links for every session", async () => {
  subscribeCells(`${fingerprint}:${TAB_ID}`, WATCHED);
  expect(await fanoutFor(`?tab=${TAB_ID}`)).toEqual([
    `cell:${WATCHED}`, `link:${WATCHED}`, `link:${OTHER}`,
  ]);
});

test("a withdrawn session stops arriving", async () => {
  unsubscribeCells(`${fingerprint}:${TAB_ID}`, WATCHED);
  expect(isSubscribed(`${fingerprint}:${TAB_ID}`, WATCHED)).toBe(false);
  expect(await fanoutFor(`?tab=${TAB_ID}`)).toEqual([`link:${WATCHED}`, `link:${OTHER}`]);
});

test("a client that sends no tab= still receives every session (fail open)", async () => {
  expect(await fanoutFor("")).toEqual([
    `cell:${WATCHED}`, `cell:${OTHER}`, `link:${WATCHED}`, `link:${OTHER}`,
  ]);
});

test("a tab omits its own cursor but receives another tab and viewer snapshots", async () => {
  const alpha = `${fingerprint}:${TAB_ID}`;
  const beta = `${fingerprint}:tab-beta`;
  expect(await presenceFanoutFor(`?tab=${TAB_ID}`)).toEqual([
    { kind: "presence-delta", viewer_id: beta, cursor_col: 4, cursor_row: 5 },
    { kind: "viewers", fps: [alpha, beta] },
  ]);
});

test("a client without tab= receives both cursor deltas", async () => {
  const alpha = `${fingerprint}:${TAB_ID}`;
  const beta = `${fingerprint}:tab-beta`;
  expect(await presenceFanoutFor("")).toEqual([
    { kind: "presence-delta", viewer_id: alpha, cursor_col: 1, cursor_row: 2 },
    { kind: "presence-delta", viewer_id: beta, cursor_col: 4, cursor_row: 5 },
    { kind: "viewers", fps: [alpha, beta] },
  ]);
});

test("a newer reclaim survives a delayed old withdraw", () => {
  const viewer = "ordered-viewer:reclaim";
  const sessionId = "ordered-session-reclaim";

  expect(subscribeCells(viewer, sessionId, 1n)).toBe(true);
  expect(unsubscribeCells(viewer, sessionId, 2n)).toBe(true);
  expect(subscribeCells(viewer, sessionId, 3n)).toBe(true);

  expect(unsubscribeCells(viewer, sessionId, 2n)).toBe(false);
  expect(isSubscribed(viewer, sessionId)).toBe(true);
});

test("a newer withdraw survives a delayed old claim", () => {
  const viewer = "ordered-viewer:withdraw";
  const sessionId = "ordered-session-withdraw";

  expect(subscribeCells(viewer, sessionId, 10n)).toBe(true);
  expect(unsubscribeCells(viewer, sessionId, 11n)).toBe(true);

  expect(subscribeCells(viewer, sessionId, 10n)).toBe(false);
  expect(isSubscribed(viewer, sessionId)).toBe(false);
});

test("legacy sequence zero keeps arrival-order membership semantics", () => {
  const viewer = "legacy-viewer";
  const sessionId = "legacy-session";

  expect(subscribeCells(viewer, sessionId, 7n)).toBe(true);
  expect(unsubscribeCells(viewer, sessionId, 0n)).toBe(true);
  expect(isSubscribed(viewer, sessionId)).toBe(false);
  expect(subscribeCells(viewer, sessionId, 0n)).toBe(true);
  expect(isSubscribed(viewer, sessionId)).toBe(true);
});

test("only an accepted equal heartbeat refreshes claim TTL", () => {
  const heartbeatViewer = "ordered-viewer:heartbeat";
  const staleViewer = "ordered-viewer:stale";
  const sessionId = "ordered-session-heartbeat";
  const start = new Date("2030-01-01T00:00:00.000Z");

  setSystemTime(start);
  try {
    expect(subscribeCells(heartbeatViewer, sessionId, 20n)).toBe(true);
    expect(subscribeCells(staleViewer, sessionId, 20n)).toBe(true);

    setSystemTime(new Date(start.getTime() + VIEWER_CLAIM_TTL_MS - 1));
    expect(subscribeCells(heartbeatViewer, sessionId, 20n, true)).toBe(true);
    expect(unsubscribeCells(heartbeatViewer, sessionId, 20n)).toBe(false);
    expect(subscribeCells(staleViewer, sessionId, 19n)).toBe(false);

    _reapCellSubscriptions(start.getTime() + VIEWER_CLAIM_TTL_MS + 1);
    expect(isSubscribed(heartbeatViewer, sessionId)).toBe(true);
    expect(isSubscribed(staleViewer, sessionId)).toBe(false);
  } finally {
    setSystemTime();
  }
});

test("an exact rollback restores the prior membership and watermark", () => {
  const viewer = "rollback-viewer:prior";
  const sessionId = "rollback-session-prior";

  expect(subscribeCells(viewer, sessionId, 40n)).toBe(true);
  const failed = mutateCellSubscription(viewer, sessionId, false, 41n);
  expect(failed?.effectiveClientSeq).toBe(41n);
  expect(isSubscribed(viewer, sessionId)).toBe(false);

  expect(failed?.rollback()).toBe(true);
  expect(isSubscribed(viewer, sessionId)).toBe(true);
  // Sequence 41 is accepted again only if rollback restored watermark 40.
  const retry = mutateCellSubscription(viewer, sessionId, false, 41n);
  expect(retry?.effectiveClientSeq).toBe(41n);
  expect(isSubscribed(viewer, sessionId)).toBe(false);
});

test("rollback restores the prior claim expiry", () => {
  const viewer = "rollback-viewer:expiry";
  const sessionId = "rollback-session-expiry";
  const start = new Date("2031-01-01T00:00:00.000Z");

  setSystemTime(start);
  try {
    expect(subscribeCells(viewer, sessionId, 50n)).toBe(true);
    setSystemTime(new Date(start.getTime() + VIEWER_CLAIM_TTL_MS - 1));
    const failedHeartbeat = mutateCellSubscription(
      viewer,
      sessionId,
      true,
      50n,
      true,
    );
    expect(failedHeartbeat).not.toBeNull();
    expect(failedHeartbeat?.rollback()).toBe(true);

    _reapCellSubscriptions(start.getTime() + VIEWER_CLAIM_TTL_MS + 1);
    expect(isSubscribed(viewer, sessionId)).toBe(false);
  } finally {
    setSystemTime();
  }
});

test("an older rollback token cannot undo a newer mutation", () => {
  const viewer = "rollback-viewer:newer";
  const sessionId = "rollback-session-newer";

  const older = mutateCellSubscription(viewer, sessionId, true, 60n);
  const newer = mutateCellSubscription(viewer, sessionId, false, 61n);
  expect(older).not.toBeNull();
  expect(newer).not.toBeNull();
  expect(isSubscribed(viewer, sessionId)).toBe(false);

  expect(older?.rollback()).toBe(false);
  expect(isSubscribed(viewer, sessionId)).toBe(false);
  // Equality remains rejected, proving the newer watermark also survived.
  expect(mutateCellSubscription(viewer, sessionId, true, 61n)).toBeNull();
});

test("legacy claims advance while legacy withdraw preserves the effective sequence", () => {
  const viewer = "mixed-sequence-viewer";
  const sessionId = "mixed-sequence-session";

  const ordered = mutateCellSubscription(viewer, sessionId, true, 70n);
  const legacyClaim = mutateCellSubscription(viewer, sessionId, true, 0n);
  expect(ordered?.effectiveClientSeq).toBe(70n);
  expect(legacyClaim?.effectiveClientSeq).toBe(71n);

  // Worker receives 71 for the legacy claim, so coord must reject an ordered
  // 71 rather than forwarding an equality the worker would ignore.
  expect(mutateCellSubscription(viewer, sessionId, true, 71n)).toBeNull();
  expect(
    mutateCellSubscription(viewer, sessionId, true, 72n)?.effectiveClientSeq,
  ).toBe(72n);

  // Worker withdraws before consulting client_seq, so zero-withdraw must keep
  // 72 and allow the browser's next ordered intent, 73, through.
  const legacyWithdraw = mutateCellSubscription(viewer, sessionId, false, 0n);
  expect(legacyWithdraw?.effectiveClientSeq).toBe(72n);
  expect(isSubscribed(viewer, sessionId)).toBe(false);
  expect(
    mutateCellSubscription(viewer, sessionId, true, 73n)?.effectiveClientSeq,
  ).toBe(73n);
});
