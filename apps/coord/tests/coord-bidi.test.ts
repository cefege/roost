// T3.2 part 2 — bidi e2e shape verification via createCoord.
//
// The full spawn → input → bytes → kill loop requires:
//   1. A live coord (createCoord ✓)
//   2. A connected worker over the raw-WS transport (worker-ws-handler.ts)
//   3. A real PTY subprocess on the worker
//
// (2) + (3) live outside this harness (Bun.serve WSS + node-pty); the
// smoke under /roost-smoke covers the full flow against the deployed
// worker. This file proves the SPA-facing routing surface end-to-end:
// auth-gated entry, downstream frame composition, error semantics when
// the worker leg isn't attached. Together with apps/shared/tests/
// event-proto.test.ts (proto round-trip) and coord-e2e.test.ts
// (factory + auth surface), the wire shape is covered without
// requiring a Bun.serve + node-pty harness in unit tests.

import { describe, test, expect, beforeAll, afterAll } from "bun:test";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { newJwtCache, signJwt, fingerprintOf } from "../src/jwt.ts";
import { createCoord, type CoordHandle } from "../src/coord-factory.ts";
import { globalPresenceBus } from "../src/buses.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-service.ts";
import type { CoordConfig } from "@roost/shared/config";

let workdir: string;
let coord: CoordHandle;
let cleanup: () => void;
let browserJwt: string;
let browserFp: string;
let db: import("../src/db/connection.ts").KyselyDB;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-coord-bidi-"));
  const dbPath = join(workdir, "test.db");
  const keyPath = join(workdir, "test.key");
  const authPath = join(workdir, "authorized_keys");
  writeFileSync(authPath, "");

  const opened = openDb(dbPath);
  db = opened.db;
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
    relaxedCsp: false,
    corsAllowedOrigins: [],
    logDir: workdir,
    publicUrl: undefined,
    handoffPath: join(workdir, "coord-handoff.json"),
  };
  coord = createCoord({ db, coordKey, cfg, jwtCache });

  // Mint a browser keypair, authorize it loopback-only, sign a JWT.
  const browserKeys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  const rawPub = new Uint8Array(await crypto.subtle.exportKey("raw", browserKeys.publicKey));
  browserFp = await fingerprintOf(rawPub);
  await db.insertInto("authorized_keys").values({
    fingerprint: browserFp,
    public_key: rawPub,
    label: "test-browser",
    added_at: Date.now(),
  }).execute();
  const now = Math.floor(Date.now() / 1000);
  browserJwt = await signJwt(
    { aud: "roost-coordinator", sub: browserFp, iat: now, exp: now + 60 },
    browserKeys.privateKey,
    browserFp,
  );

  cleanup = () => {
    coord.dispose();
    try { sqlite.close(); } catch { /* ignore */ }
    if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
  };
});

afterAll(() => cleanup?.());

function authedFetch(path: string, body: unknown, tabId?: string): Promise<Response> {
  const headers: Record<string, string> = {
    "content-type": "application/json",
    authorization: `Bearer ${browserJwt}`,
  };
  if (tabId) headers["x-roost-tab-id"] = tabId;
  return coord.fetch(new Request(`http://t${path}`, {
    method: "POST",
    headers,
    body: JSON.stringify(body),
  }));
}

// _publishViewers is async (awaits label + tailnet-hostname lookup) and
// withdraws are deferred VIEWER_WITHDRAW_GRACE_MS (hysteresis), so tests
// must let those land before snapshotting. ms defaults to a microtask-ish
// flush for synchronous claims; pass GRACE+margin after a withdraw.
const settle = (ms = 60) => new Promise((r) => setTimeout(r, ms));
const AFTER_WITHDRAW_MS = 1000; // > VIEWER_WITHDRAW_GRACE_MS (800) + margin

// Subscribe to the viewer-presence fanout. Captures every published
// `{kind: "viewers", fps: [...]}` for a given session_id until unsub().
function captureViewers(targetSid: string): { stop(): string[][] } {
  const snapshots: string[][] = [];
  const unsub = globalPresenceBus.subscribe(({ session_id, data }) => {
    if (session_id !== targetSid) return;
    const d = data as { kind?: string; fps?: string[] };
    if (d.kind === "viewers") snapshots.push([...(d.fps ?? [])]);
  });
  return { stop(): string[][] { unsub(); return snapshots; } };
}

describe("coord-bidi spawn → input → kill routing", () => {
  test("SessionsSpawn with no worker attached → FAILED_PRECONDITION", async () => {
    const resp = await authedFetch("/roost.v1.CoordinatorService/SessionsSpawn", {
      workerFp: "deadbeef".repeat(8),
      kind: "shell",
      folder: "/tmp",
      cols: 80, rows: 24,
    });
    expect(resp.status).toBe(400);
    const body = await resp.json();
    // Connect maps failed_precondition → HTTP 400 + code in body.
    expect(body.code).toBe("failed_precondition");
    expect(body.message).toMatch(/not connected/);
  });

  // proto3 JSON omits default values (booleans default false, empty
  // repeated). Treat absent === default.
  test("SessionsKill on unknown session → accepted falsy (idempotent)", async () => {
    const resp = await authedFetch("/roost.v1.CoordinatorService/SessionsKill", {
      sessionId: "00000000-0000-0000-0000-000000000000",
    });
    expect(resp.status).toBe(200);
    const body = await resp.json();
    expect(body.accepted ?? false).toBe(false);
  });

  test("SessionsInput on unknown session → accepted falsy (no crash)", async () => {
    const resp = await authedFetch("/roost.v1.CoordinatorService/SessionsInput", {
      sessionId: "00000000-0000-0000-0000-000000000000",
      bytes: btoa("ls\r"),
    });
    expect(resp.status).toBe(200);
    const body = await resp.json();
    expect(body.accepted ?? false).toBe(false);
  });

  test("SessionsList authenticated → 200 + sessions array (may be omitted when empty)", async () => {
    const resp = await authedFetch("/roost.v1.CoordinatorService/SessionsList", {});
    expect(resp.status).toBe(200);
    const body = await resp.json();
    const sessions = body.sessions ?? [];
    expect(Array.isArray(sessions)).toBe(true);
  });

  test("WorkersList authenticated → 200 + workers array (may be omitted when empty)", async () => {
    const resp = await authedFetch("/roost.v1.CoordinatorService/WorkersList", {});
    expect(resp.status).toBe(200);
    const body = await resp.json();
    const workers = body.workers ?? [];
    expect(Array.isArray(workers)).toBe(true);
  });
});

// SessionsResize is the wire-level entry for multi-viewer viewport
// claims. Coord composes `${fp}:${tabId}` from the JWT caller +
// `x-roost-tab-id` header, forwards as a ClientControlFrame to the
// worker, and bumps the per-session viewer map. Phase-t1.1 fix
// verification — without the composite key, two tabs from the same
// browser would collapse into one entry, and one tab's pagehide
// would drop the other tab's claim.
describe("sessionsResize — tab-id composite + viewer bookkeeping", () => {
  const FAKE_WORKER_FP = "deadbeef".repeat(8); // 64 hex chars
  let workerSends: unknown[] = [];

  async function seedSession(sid: string): Promise<void> {
    // Ensure worker row exists (FK), then insert/upsert the session row.
    await db.insertInto("workers").values({
      fp: FAKE_WORKER_FP, label: "fake", os: "darwin",
      reachable_addr: "127.0.0.1", git_sha: null, host_metrics_json: null,
      registered_at_ms: Date.now(), last_seen_ms: Date.now(),
    }).onConflict((oc) => oc.column("fp").doNothing()).execute();
    await db.insertInto("sessions").values({
      id: sid, worker_fp: FAKE_WORKER_FP, channel: 1, kind: "shell",
      cwd: "/tmp", status: "open", created_at: Date.now(),
    }).onConflict((oc) => oc.column("id").doNothing()).execute();
  }

  beforeAll(() => {
    __setConnectWorkerForTest(FAKE_WORKER_FP, {
      workerFp: FAKE_WORKER_FP,
      send: (frame: unknown) => workerSends.push(frame),
    });
  });
  afterAll(() => { __setConnectWorkerForTest(FAKE_WORKER_FP, null); });

  test("two tabs from same fp register as TWO viewers (composite key prevents collapse)", async () => {
    const sid = "11111111-1111-1111-1111-111111111111";
    await seedSession(sid);
    const cap = captureViewers(sid);
    await authedFetch("/roost.v1.CoordinatorService/SessionsResize",
      { sessionId: sid, cols: 80, rows: 24 }, "tab-A");
    await authedFetch("/roost.v1.CoordinatorService/SessionsResize",
      { sessionId: sid, cols: 100, rows: 30 }, "tab-B");
    await settle();
    const snaps = cap.stop();
    expect(snaps.length).toBeGreaterThanOrEqual(2);
    const final = snaps[snaps.length - 1]!.slice().sort();
    expect(final).toEqual([`${browserFp}:tab-A`, `${browserFp}:tab-B`].sort());
  });

  test("withdraw (cols=0) for tab-A keeps tab-B alive (composite isolation)", async () => {
    const sid = "22222222-2222-2222-2222-222222222222";
    await seedSession(sid);
    await authedFetch("/roost.v1.CoordinatorService/SessionsResize",
      { sessionId: sid, cols: 80, rows: 24 }, "tab-A");
    await authedFetch("/roost.v1.CoordinatorService/SessionsResize",
      { sessionId: sid, cols: 100, rows: 30 }, "tab-B");
    const cap = captureViewers(sid);
    await authedFetch("/roost.v1.CoordinatorService/SessionsResize",
      { sessionId: sid, cols: 0, rows: 0 }, "tab-A");
    await settle(AFTER_WITHDRAW_MS); // withdraw is deferred (hysteresis)
    const snaps = cap.stop();
    expect(snaps.length).toBeGreaterThan(0);
    const final = snaps[snaps.length - 1]!;
    expect(final).toEqual([`${browserFp}:tab-B`]);
  });

  test("missing x-roost-tab-id → bare fingerprint key (no composite)", async () => {
    const sid = "33333333-3333-3333-3333-333333333333";
    await seedSession(sid);
    const cap = captureViewers(sid);
    await authedFetch("/roost.v1.CoordinatorService/SessionsResize",
      { sessionId: sid, cols: 80, rows: 24 }); // no x-roost-tab-id
    await settle();
    const snaps = cap.stop();
    expect(snaps.length).toBeGreaterThanOrEqual(1);
    const final = snaps[snaps.length - 1]!;
    expect(final).toEqual([browserFp]);
  });

  test("withdraw without worker attached still clears the viewer map", async () => {
    const sid = "44444444-4444-4444-4444-444444444444";
    await seedSession(sid);
    // Seed a live claim through the worker.
    await authedFetch("/roost.v1.CoordinatorService/SessionsResize",
      { sessionId: sid, cols: 80, rows: 24 }, "tab-Z");
    // Detach the worker mid-session.
    __setConnectWorkerForTest(FAKE_WORKER_FP, null);
    const cap = captureViewers(sid);
    const r = await authedFetch("/roost.v1.CoordinatorService/SessionsResize",
      { sessionId: sid, cols: 0, rows: 0 }, "tab-Z");
    const body = await r.json();
    expect(body.accepted ?? false).toBe(false);   // forward failed
    await settle(AFTER_WITHDRAW_MS); // withdraw is deferred (hysteresis)
    const snaps = cap.stop();
    expect(snaps[snaps.length - 1]).toEqual([]);  // but withdraw cleared the map
    // Re-attach the fake worker for any later tests.
    __setConnectWorkerForTest(FAKE_WORKER_FP, {
      workerFp: FAKE_WORKER_FP,
      send: (frame: unknown) => workerSends.push(frame),
    });
  });

  test("worker receives composite viewer_id in browser-command frame", async () => {
    const sid = "55555555-5555-5555-5555-555555555555";
    await seedSession(sid);
    workerSends = [];
    await authedFetch("/roost.v1.CoordinatorService/SessionsResize",
      { sessionId: sid, cols: 80, rows: 24 }, "tab-WIRE");
    // The fake worker captured the CoordWorkerDown frame. Extract viewerId.
    expect(workerSends.length).toBeGreaterThan(0);
    const last = workerSends[workerSends.length - 1] as {
      frame: { case: string; value: { viewerId: string } };
    };
    expect(last.frame.case).toBe("browserCommand");
    expect(last.frame.value.viewerId).toBe(`${browserFp}:tab-WIRE`);
  });
});
