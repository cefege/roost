// Per-tab cell/byte fanout. Before this filter, every open Sync socket received
// — and re-serialized — every cell frame and every PTY byte of every session on
// the fleet, so N tabs cost N× the work regardless of what they were looking at.
//
// Integration: real Bun.serve + real JWT auth + the real startSyncFeed wiring,
// driving the actual globalCellBus/globalBytesBus. Asserts the three cases that
// matter: a subscribed session arrives, an unsubscribed one does not, and a
// client that sends no `tab=` still receives everything (fail-open for the CLI
// and older SPA builds).
//
// Frames are collected up to a BARRIER frame on an unfiltered bus rather than a
// sleep: one socket delivers in order, so once the barrier lands every frame
// published before it has either arrived or been filtered out.

import { test, expect, beforeAll, afterAll } from "bun:test";
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
import { subscribeCells, unsubscribeCells, isSubscribed } from "../src/connect/cell-subscriptions.ts";
import { globalCellBus, globalBytesBus, workerRoutableBus } from "../src/buses.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import type { SyncWsData } from "../src/connect/sync-ws-handler.ts";
import type { CoordConfig } from "@roost/shared/config";

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
  const cfg: CoordConfig = {
    bind: "127.0.0.1:0",
    dbPath, coordKeyPath: keyPath, authorizedKeysPath: authPath,
    webDistPath: "",
    tlsCertPath: undefined, tlsKeyPath: undefined,
    jwtMaxAgeSecs: 300,
    auditRetentionDays: 90,
    relaxedCsp: false,
    corsAllowedOrigins: [],
    logDir: workdir,
    publicUrl: undefined,
    handoffPath: join(workdir, "coord-handoff.json"),
  };
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
    websocket: makeSyncWsHandler(deps, 60_000),
  });

  cleanup = () => {
    try { server.stop(true); } catch { /* ignore */ }
    try { sqlite.close(); } catch { /* ignore */ }
    if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
  };
});

afterAll(() => cleanup?.());

/** Open a socket, publish the per-session frames, and return the ids that made
 *  it through the fanout filter. */
async function fanoutFor(query: string): Promise<string[]> {
  const ws = new WebSocket(`ws://127.0.0.1:${server.port}/ws/coord-sync?token=${encodeURIComponent(jwt)}${query}`);
  ws.binaryType = "arraybuffer";
  const seen: string[] = [];
  const { promise: barrierSeen, resolve: sawBarrier } = Promise.withResolvers<void>();
  ws.onmessage = (ev) => {
    const frame = fromBinary(FirehoseFrameSchema, new Uint8Array(ev.data as ArrayBuffer));
    if (frame.frame.case === "cellGrid") seen.push(`cell:${frame.frame.value.sessionId}`);
    else if (frame.frame.case === "bytes") seen.push(`bytes:${frame.frame.value.sessionId}`);
    else if (frame.frame.case === "workerRoutable" && frame.frame.value.fps.includes(BARRIER_FP)) sawBarrier();
  };
  const { promise: open, resolve: opened } = Promise.withResolvers<void>();
  ws.onopen = () => opened();
  await open;
  globalCellBus.publish(create(PbCellGridFrameSchema, { sessionId: WATCHED, seq: 1n }));
  globalCellBus.publish(create(PbCellGridFrameSchema, { sessionId: OTHER, seq: 1n }));
  globalBytesBus.publish({ session_id: WATCHED, bytes: new Uint8Array([1]) });
  globalBytesBus.publish({ session_id: OTHER, bytes: new Uint8Array([2]) });
  workerRoutableBus.publish({ fps: [BARRIER_FP] });
  await barrierSeen;
  try { ws.close(); } catch { /* ignore */ }
  return seen.sort();
}

test("a tab receives only the sessions it claimed", async () => {
  subscribeCells(`${fingerprint}:${TAB_ID}`, WATCHED);
  expect(await fanoutFor(`&tab=${TAB_ID}`)).toEqual([`bytes:${WATCHED}`, `cell:${WATCHED}`]);
});

test("a withdrawn session stops arriving", async () => {
  unsubscribeCells(`${fingerprint}:${TAB_ID}`, WATCHED);
  expect(isSubscribed(`${fingerprint}:${TAB_ID}`, WATCHED)).toBe(false);
  expect(await fanoutFor(`&tab=${TAB_ID}`)).toEqual([]);
});

test("a client that sends no tab= still receives every session (fail open)", async () => {
  expect(await fanoutFor("")).toEqual([
    `bytes:${WATCHED}`, `bytes:${OTHER}`, `cell:${WATCHED}`, `cell:${OTHER}`,
  ]);
});
