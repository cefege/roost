// T3.2 part 2 — bidi e2e shape verification via createCoord.
//
// The full spawn → input → bytes → kill loop requires:
//   1. A live coord (createCoord ✓)
//   2. A connected worker over the raw-WS transport (worker-ws-handler.ts)
//   3. A real PTY subprocess on the worker
//
// (2) + (3) live outside this harness (Bun.serve WSS + Bun.Terminal); the
// terminal tier (smoke/terminal/) covers the full flow against a real worker.
// This file proves auth-gated unary setup plus the canonical Sync v2 terminal
// input hook and its typed acknowledged rejection. Shared proto round trips and
// coord-e2e.test.ts cover the remaining wire shape without a real PTY here.

import { create } from "@bufbuild/protobuf";
import { describe, test, expect, beforeAll, afterAll } from "bun:test";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { CoordWorkerDown } from "@roost/shared/proto/worker_transport_pb";
import { InputCommandSchema } from "@roost/shared/proto/sync_pb";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { newJwtCache, signJwt } from "../src/jwt.ts";
import { createCoord, type CoordHandle } from "../src/coord-factory.ts";
import { globalPresenceBus } from "../src/buses.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import { primeChannelMap } from "../src/byte-hub.ts";
import type { CoordConfig } from "@roost/shared/config";
import type { ConnectDeps } from "../src/connect/router.ts";
import {
  makeSyncTerminalControlHooks,
  type SyncTerminalControlHooks,
} from "../src/connect/sync-terminal-controls.ts";
import type {
  SyncV2CommandContext,
  SyncV2ResultControl,
} from "../src/connect/sync-ws-v2-commands.ts";
import { TerminalViewHub } from "../src/connect/terminal-view-hub.ts";

let workdir: string;
let coord: CoordHandle;
let cleanup: () => Promise<void>;
let browserJwt: string;
let browserFp: string;
let db: import("../src/db/connection.ts").KyselyDB;
let terminalControlHooks: SyncTerminalControlHooks;
let terminalViews: TerminalViewHub;

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
  const deps: ConnectDeps = { db, sqlite, coordKey, cfg, jwtCache };
  coord = createCoord(deps);
  terminalViews = new TerminalViewHub({ db });
  terminalControlHooks = makeSyncTerminalControlHooks(deps, terminalViews);

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

  cleanup = async () => {
    terminalViews.dispose();
    coord.dispose();
    try { await opened.close(); } finally { if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true }); }
  };
});

afterAll(async () => { await cleanup?.(); });

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

const TERMINAL_DOMAIN_GENERATION = 7n;

function syncViewerKey(tabId: string): string {
  return `${browserFp}:${tabId}`;
}

function dispatchSyncTerminalCommand(
  tabId: string,
  command: SyncV2CommandContext["command"],
  remoteAddress = "127.0.0.1",
): Promise<SyncV2ResultControl> {
  const { promise, resolve } = Promise.withResolvers<SyncV2ResultControl>();
  let replied = false;
  terminalControlHooks.onV2Command({
    caller: { fingerprint: browserFp, label: "test-browser", keyGeneration: 1 },
    viewerKey: syncViewerKey(tabId),
    remoteAddress,
    socketId: `coord-bidi:${tabId}`,
    command,
    reply(control): boolean {
      if (replied) return false;
      replied = true;
      resolve(control);
      return true;
    },
  });
  return promise;
}

describe("coord-bidi spawn → Sync input → kill routing", () => {
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

  test("Sync input on an unknown session returns an acknowledged rejection", async () => {
    const result = await dispatchSyncTerminalCommand("tab-UNKNOWN-INPUT", {
      case: "input",
      value: create(InputCommandSchema, {
        sessionId: "00000000-0000-0000-0000-000000000000",
        inputSeq: 1n,
        data: Uint8Array.of(0x6c, 0x73, 0x0d),
        domainGeneration: TERMINAL_DOMAIN_GENERATION,
      }),
    });
    expect(result.case).toBe("inputRejected");
    if (result.case !== "inputRejected") {
      throw new Error(`expected input rejection, received ${result.case}`);
    }
    expect(result.value).toMatchObject({
      sessionId: "00000000-0000-0000-0000-000000000000",
      inputSeq: 1n,
      domainGeneration: TERMINAL_DOMAIN_GENERATION,
    });
    expect(result.value.reason).toMatch(/unknown session/);
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

describe("cursor presence and worker command routing", () => {
  const FAKE_WORKER_FP = "deadbeef".repeat(8);
  let workerSends: CoordWorkerDown[] = [];

  async function seedSession(sid: string): Promise<void> {
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
      send(frame: CoordWorkerDown): number {
        workerSends.push(frame);
        return 1;
      },
    });
  });

  afterAll(() => {
    __setConnectWorkerForTest(FAKE_WORKER_FP, null);
  });

  test("cursor presence uses tab identity while the legacy worker envelope uses sender identity", async () => {
    const sid = "66666666-6666-6666-6666-666666666666";
    await seedSession(sid);
    primeChannelMap([{ id: sid, worker_fp: FAKE_WORKER_FP, channel: 1 }]);
    const deltas: Array<{ kind?: string; viewer_id?: string }> = [];
    const unsub = globalPresenceBus.subscribe(({ session_id, data }) => {
      const payload = data as { kind?: string; viewer_id?: string };
      if (session_id === sid && payload.kind === "presence-delta") deltas.push(payload);
    });
    workerSends = [];
    try {
      await authedFetch("/roost.v1.CoordinatorService/SessionsCursorPos",
        { sessionId: sid, col: 17, row: 9 }, "tab-CURSOR");
    } finally {
      unsub();
    }

    expect(deltas).toHaveLength(1);
    expect(deltas[0]!.viewer_id).toBe(`${browserFp}:tab-CURSOR`);
    expect(workerSends.length).toBeGreaterThan(0);
    const last = workerSends[workerSends.length - 1]!;
    expect(last.frame.case).toBe("browserCommand");
    if (last.frame.case !== "browserCommand") {
      throw new Error(`expected browser command, received ${last.frame.case}`);
    }
    expect(last.frame.value.viewerId).toBe(browserFp);
  });

  test("cursor presence and worker command keep the legacy bare fingerprint", async () => {
    const sid = "77777777-7777-7777-7777-777777777777";
    await seedSession(sid);
    primeChannelMap([{ id: sid, worker_fp: FAKE_WORKER_FP, channel: 1 }]);
    const deltas: Array<{ kind?: string; viewer_id?: string }> = [];
    const unsub = globalPresenceBus.subscribe(({ session_id, data }) => {
      const payload = data as { kind?: string; viewer_id?: string };
      if (session_id === sid && payload.kind === "presence-delta") deltas.push(payload);
    });
    workerSends = [];
    try {
      await authedFetch("/roost.v1.CoordinatorService/SessionsCursorPos",
        { sessionId: sid, col: 23, row: 4 });
    } finally {
      unsub();
    }

    expect(deltas).toHaveLength(1);
    expect(deltas[0]!.viewer_id).toBe(browserFp);
    expect(workerSends.length).toBeGreaterThan(0);
    const last = workerSends[workerSends.length - 1]!;
    expect(last.frame.case).toBe("browserCommand");
    if (last.frame.case !== "browserCommand") {
      throw new Error(`expected browser command, received ${last.frame.case}`);
    }
    expect(last.frame.value.viewerId).toBe(browserFp);
  });
});
