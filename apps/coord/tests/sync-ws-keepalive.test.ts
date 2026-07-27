// Regression: coord sends a FirehoseFrame{keepalive} on each Sync WS so the
// browser stale-link watchdog (apps/web/src/store/sync-watchdog.ts) can tell
// a half-open connection from a merely idle session. Mirrors worker-conn.ts.
// Integration test: real Bun.serve + real JWT auth + real startSyncFeed
// (empty DB → no backfill frames → the keepalive is the only thing that
// arrives). Real-clock keepalive interval IS the behavior under test — the
// ts-no-test-timers exception (real platform-clock timer behavior) applies.

import { test, expect, beforeAll, afterAll } from "bun:test";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fromBinary } from "@bufbuild/protobuf";
import { FirehoseFrameSchema } from "@roost/shared/proto/sync_pb";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { newJwtCache, signJwt, fingerprintOf } from "../src/jwt.ts";
import { handleSyncWsUpgrade, makeSyncWsHandler } from "../src/connect/sync-ws-handler.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import type { CoordConfig } from "@roost/shared/config";

let workdir: string;
let cleanup: () => void;
let server: ReturnType<typeof Bun.serve>;
let jwt: string;

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
    corsAllowedOrigins: [],
    logDir: workdir,
    publicUrl: undefined,
    handoffPath: join(workdir, "coord-handoff.json"),
  };
  const deps: ConnectDeps = { db, coordKey, jwtCache, cfg };

  // Mint a keypair, authorize it, sign a JWT (same shape as the browser's
  // web-key.ts JWT — sub == fingerprint, verified by verifyJwt at upgrade).
  const keys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const rawPub = new Uint8Array(await crypto.subtle.exportKey("raw", keys.publicKey));
  const fp = await fingerprintOf(rawPub);
  await db.insertInto("authorized_keys").values({
    fingerprint: fp, public_key: rawPub, label: "test-web", added_at: Date.now(),
  }).execute();
  const now = Math.floor(Date.now() / 1000);
  jwt = await signJwt(
    { aud: "roost-coordinator", sub: fp, iat: now, exp: now + 60 },
    keys.privateKey, fp,
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
    websocket: makeSyncWsHandler(deps, 100),
  });

  cleanup = () => {
    try { server.stop(true); } catch { /* ignore */ }
    try { sqlite.close(); } catch { /* ignore */ }
    if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
  };
});

afterAll(() => cleanup?.());

test("open → server sends KeepaliveFrame within keepalive interval", async () => {
  const ws = new WebSocket(`ws://127.0.0.1:${server.port}/ws/coord-sync?token=${encodeURIComponent(jwt)}`);
  ws.binaryType = "arraybuffer";
  const { promise: gotKeepalive, resolve: sawKeepalive } = Promise.withResolvers<void>();
  // Filter for keepalive frames — the empty DB sends no backfill, so the
  // keepalive is the only frame, but filter defensively.
  ws.onmessage = (ev) => {
    const frame = fromBinary(FirehoseFrameSchema, new Uint8Array(ev.data as ArrayBuffer));
    if (frame.frame.case === "keepalive") sawKeepalive();
  };
  // If the keepalive never fires, this hangs and bun:test fails on timeout.
  await gotKeepalive;
  expect(true).toBe(true); // gotKeepalive resolved → keepalive received
  try { ws.close(); } catch { /* ignore */ }
}, 5_000);
