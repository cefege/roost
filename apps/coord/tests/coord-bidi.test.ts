// T3.2 part 2 — bidi e2e shape verification via createCoord.
//
// The full spawn → input → bytes → kill loop requires:
//   1. A live coord (createCoord ✓)
//   2. A connected worker over the raw-WS transport (worker-ws-handler.ts)
//   3. A real PTY subprocess on the worker
//
// (2) + (3) live outside this harness (Bun.serve WSS + Bun.Terminal); the
// smoke under /roost-smoke covers the full flow against the deployed worker.
// This file proves auth-gated unary setup plus the canonical Sync v2 terminal
// control hook: typed-frame composition, three-way acknowledged outcomes, and
// transactional provisional membership. Shared proto round trips and
// coord-e2e.test.ts cover the remaining wire shape without a real PTY here.

import { create } from "@bufbuild/protobuf";
import { describe, test, expect, beforeAll, afterAll, vi } from "bun:test";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ResizeCause } from "@roost/shared/proto/coordinator_pb";
import {
  TerminalViewportStatus,
  TerminalWritePhase,
  WViewportResultSchema,
  type CoordWorkerDown,
  type DViewportRequest,
} from "@roost/shared/proto/worker_transport_pb";
import { VIEWER_WITHDRAW_GRACE_MS } from "@roost/shared/viewport";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { newJwtCache, signJwt } from "../src/jwt.ts";
import { createCoord, type CoordHandle } from "../src/coord-factory.ts";
import { globalPresenceBus } from "../src/buses.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-service.ts";
import { primeChannelMap } from "../src/byte-hub.ts";
import type { CoordConfig } from "@roost/shared/config";
import { isSubscribed } from "../src/connect/cell-subscriptions.ts";
import { _viewersBySession } from "../src/connect/viewer-tracker.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import {
  makeSyncTerminalControlHooks,
  type SyncTerminalCommand,
  type SyncTerminalControlHooks,
  type SyncTerminalResultControl,
} from "../src/connect/sync-terminal-controls.ts";
import { resolvePendingRpc } from "../src/router/pending-rpcs.ts";

let workdir: string;
let coord: CoordHandle;
let cleanup: () => Promise<void>;
let browserJwt: string;
let browserFp: string;
let db: import("../src/db/connection.ts").KyselyDB;
let terminalControlHooks: SyncTerminalControlHooks;

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
  terminalControlHooks = makeSyncTerminalControlHooks(deps);

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
  command: SyncTerminalCommand,
  remoteAddress = "127.0.0.1",
): Promise<SyncTerminalResultControl> {
  const { promise, resolve } = Promise.withResolvers<SyncTerminalResultControl>();
  let replied = false;
  terminalControlHooks.onV2Command({
    caller: { fingerprint: browserFp, label: "test-browser" },
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

interface ViewportCommand {
  sessionId: string;
  cols: number;
  rows: number;
  clientSeq: bigint;
  cause: ResizeCause;
  heldCellSeq?: bigint;
}

function dispatchSyncViewport(
  tabId: string,
  command: ViewportCommand,
  remoteAddress?: string,
): Promise<SyncTerminalResultControl> {
  return dispatchSyncTerminalCommand(tabId, {
    case: "viewport",
    value: {
      ...command,
      heldCellSeq: command.heldCellSeq ?? 0n,
      domainGeneration: TERMINAL_DOMAIN_GENERATION,
    },
  }, remoteAddress);
}

async function acceptSyncViewport(
  tabId: string,
  command: ViewportCommand,
  remoteAddress?: string,
): Promise<Extract<SyncTerminalResultControl, { case: "viewportAccepted" }>["value"]> {
  const result = await dispatchSyncViewport(tabId, command, remoteAddress);
  expect(result.case).toBe("viewportAccepted");
  if (result.case !== "viewportAccepted") {
    throw new Error(`expected viewport acceptance, received ${result.case}`);
  }
  expect(result.value.domainGeneration).toBe(TERMINAL_DOMAIN_GENERATION);
  return result.value;
}

async function rejectSyncViewport(
  tabId: string,
  command: ViewportCommand,
  remoteAddress?: string,
): Promise<Extract<SyncTerminalResultControl, { case: "viewportRejected" }>["value"]> {
  const result = await dispatchSyncViewport(tabId, command, remoteAddress);
  expect(result.case).toBe("viewportRejected");
  if (result.case !== "viewportRejected") {
    throw new Error(`expected viewport rejection, received ${result.case}`);
  }
  expect(result.value.domainGeneration).toBe(TERMINAL_DOMAIN_GENERATION);
  return result.value;
}

async function ambiguateSyncViewport(
  tabId: string,
  command: ViewportCommand,
  remoteAddress?: string,
): Promise<Extract<SyncTerminalResultControl, { case: "viewportAmbiguous" }>["value"]> {
  const result = await dispatchSyncViewport(tabId, command, remoteAddress);
  expect(result.case).toBe("viewportAmbiguous");
  if (result.case !== "viewportAmbiguous") {
    throw new Error(`expected viewport ambiguity, received ${result.case}`);
  }
  expect(result.value.domainGeneration).toBe(TERMINAL_DOMAIN_GENERATION);
  return result.value;
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
      value: {
        sessionId: "00000000-0000-0000-0000-000000000000",
        inputSeq: 1n,
        data: Uint8Array.of(0x6c, 0x73, 0x0d),
        domainGeneration: TERMINAL_DOMAIN_GENERATION,
      },
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

// Sync v2 owns browser viewport claims. Its socket identity supplies the
// `${fp}:${tabId}` viewer key. Cell/viewer membership is provisional once the
// worker transport admits a command: typed commit accepts, typed rejection
// rolls back, and unknown completion remains TTL-backed for monotonic repair.
// The composite key also keeps one tab's withdraw isolated from sibling tabs.
describe("per-tab viewer identity — resize and cursor presence", () => {
  const FAKE_WORKER_FP = "deadbeef".repeat(8); // 64 hex chars
  let workerSends: CoordWorkerDown[] = [];
  let nextChannelResizeSeq = 0n;

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

  function captureAndAcknowledgeWorkerSend(frame: CoordWorkerDown): number {
    workerSends.push(frame);
    if (frame.frame.case === "viewportRequest") {
      const request = frame.frame.value;
      nextChannelResizeSeq += 1n;
      resolvePendingRpc(request.requestId, create(WViewportResultSchema, {
        requestId: request.requestId,
        sessionId: request.sessionId,
        clientSeq: request.clientSeq,
        status: TerminalViewportStatus.COMMITTED,
        channelResizeSeq: nextChannelResizeSeq,
        cols: request.cols,
        rows: request.rows,
        resized: true,
      }));
    }
    return 1;
  }

  function attachAcknowledgingWorker(
    send: (frame: CoordWorkerDown) => number = captureAndAcknowledgeWorkerSend,
  ): void {
    __setConnectWorkerForTest(FAKE_WORKER_FP, {
      workerFp: FAKE_WORKER_FP,
      send,
    });
  }

  function viewportRequests(): DViewportRequest[] {
    const requests: DViewportRequest[] = [];
    for (const sent of workerSends) {
      if (sent.frame.case === "viewportRequest") requests.push(sent.frame.value);
    }
    return requests;
  }

  beforeAll(() => {
    attachAcknowledgingWorker();
  });
  afterAll(() => { __setConnectWorkerForTest(FAKE_WORKER_FP, null); });

  test("two tabs from same fp register as TWO viewers (composite key prevents collapse)", async () => {
    const sid = "11111111-1111-1111-1111-111111111111";
    await seedSession(sid);
    await acceptSyncViewport("tab-A", {
      sessionId: sid, cols: 80, rows: 24,
      clientSeq: 1n, cause: ResizeCause.INITIAL,
    });
    await acceptSyncViewport("tab-B", {
      sessionId: sid, cols: 100, rows: 30,
      clientSeq: 1n, cause: ResizeCause.INITIAL,
    });

    const final = [...(_viewersBySession.get(sid)?.keys() ?? [])].sort();
    expect(final).toEqual([syncViewerKey("tab-A"), syncViewerKey("tab-B")].sort());
  });

  test("withdraw (cols=0) for tab-A keeps tab-B alive (composite isolation)", async () => {
    vi.useFakeTimers();
    try {
      const sid = "22222222-2222-2222-2222-222222222222";
      await seedSession(sid);
      await acceptSyncViewport("tab-A", {
        sessionId: sid, cols: 80, rows: 24,
        clientSeq: 1n, cause: ResizeCause.INITIAL,
      });
      await acceptSyncViewport("tab-B", {
        sessionId: sid, cols: 100, rows: 30,
        clientSeq: 1n, cause: ResizeCause.INITIAL,
      });
      await acceptSyncViewport("tab-A", {
        sessionId: sid, cols: 0, rows: 0,
        clientSeq: 2n, cause: ResizeCause.WITHDRAW,
      });

      expect(isSubscribed(syncViewerKey("tab-A"), sid)).toBe(false);
      expect(isSubscribed(syncViewerKey("tab-B"), sid)).toBe(true);
      vi.advanceTimersByTime(VIEWER_WITHDRAW_GRACE_MS);
      expect([...(_viewersBySession.get(sid)?.keys() ?? [])]).toEqual([
        syncViewerKey("tab-B"),
      ]);
    } finally {
      vi.useRealTimers();
    }
  });

  test("remote-address changes do not split one Sync tab identity", async () => {
    const sid = "33333333-3333-3333-3333-333333333333";
    const tabId = "tab-STABLE";
    await seedSession(sid);
    await acceptSyncViewport(tabId, {
      sessionId: sid, cols: 80, rows: 24,
      clientSeq: 1n, cause: ResizeCause.INITIAL,
    }, "100.64.0.10");
    await acceptSyncViewport(tabId, {
      sessionId: sid, cols: 100, rows: 30,
      clientSeq: 2n, cause: ResizeCause.VIEWPORT,
    }, "100.64.0.11");

    expect([...(_viewersBySession.get(sid)?.keys() ?? [])]).toEqual([
      syncViewerKey(tabId),
    ]);
  });

  test("rejected withdraw rolls subscription and viewer presence back atomically", async () => {
    const sid = "44444444-4444-4444-4444-444444444444";
    const tabId = "tab-Z";
    const viewerKey = syncViewerKey(tabId);
    await seedSession(sid);
    await acceptSyncViewport(tabId, {
      sessionId: sid, cols: 80, rows: 24,
      clientSeq: 1n, cause: ResizeCause.INITIAL,
    });

    __setConnectWorkerForTest(FAKE_WORKER_FP, null);
    try {
      const rejected = await rejectSyncViewport(tabId, {
        sessionId: sid, cols: 0, rows: 0,
        clientSeq: 2n, cause: ResizeCause.WITHDRAW,
      });
      expect(rejected.reason).toMatch(/worker unavailable/);
    } finally {
      attachAcknowledgingWorker();
    }

    expect(isSubscribed(viewerKey, sid)).toBe(true);
    expect(_viewersBySession.get(sid)?.has(viewerKey)).toBe(true);
  });

  test("matching typed worker rejection rolls provisional membership back", async () => {
    const sid = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const tabId = "tab-TYPED-REJECT";
    const viewerKey = syncViewerKey(tabId);
    await seedSession(sid);
    workerSends = [];
    attachAcknowledgingWorker((frame) => {
      workerSends.push(frame);
      if (frame.frame.case === "viewportRequest") {
        const request = frame.frame.value;
        // Both coordinator projections are provisional before transport
        // completion, then the matching typed rejection must undo both.
        expect(isSubscribed(viewerKey, sid)).toBe(true);
        expect(_viewersBySession.get(sid)?.has(viewerKey)).toBe(true);
        resolvePendingRpc(request.requestId, create(WViewportResultSchema, {
          requestId: request.requestId,
          sessionId: request.sessionId,
          clientSeq: request.clientSeq,
          status: TerminalViewportStatus.REJECTED,
          phase: TerminalWritePhase.PRE_WRITE,
          reason: "keeper declined viewport",
        }));
      }
      return 1;
    });

    try {
      const rejected = await rejectSyncViewport(tabId, {
        sessionId: sid, cols: 80, rows: 24,
        clientSeq: 41n, cause: ResizeCause.INITIAL,
      });
      expect(rejected.reason).toBe("keeper declined viewport");
      expect(isSubscribed(viewerKey, sid)).toBe(false);
      expect(_viewersBySession.get(sid)?.has(viewerKey) ?? false).toBe(false);
    } finally {
      attachAcknowledgingWorker();
    }
  });

  test("typed worker ambiguity retains TTL-backed provisional membership", async () => {
    const sid = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    const tabId = "tab-TYPED-AMBIGUOUS";
    const viewerKey = syncViewerKey(tabId);
    await seedSession(sid);
    workerSends = [];
    attachAcknowledgingWorker((frame) => {
      workerSends.push(frame);
      if (frame.frame.case === "viewportRequest") {
        const request = frame.frame.value;
        resolvePendingRpc(request.requestId, create(WViewportResultSchema, {
          requestId: request.requestId,
          sessionId: request.sessionId,
          clientSeq: request.clientSeq,
          status: TerminalViewportStatus.AMBIGUOUS,
          reason: "keeper completion unknown",
        }));
      }
      return 1;
    });

    try {
      const ambiguous = await ambiguateSyncViewport(tabId, {
        sessionId: sid, cols: 91, rows: 27,
        clientSeq: 51n, cause: ResizeCause.INITIAL,
      });
      expect(ambiguous.reason).toBe("keeper completion unknown");
      expect(isSubscribed(viewerKey, sid)).toBe(true);
      expect(_viewersBySession.get(sid)?.get(viewerKey)).toMatchObject({
        cols: 91,
        rows: 27,
      });
    } finally {
      attachAcknowledgingWorker();
    }
  });

  test("a mismatched typed rejection is ambiguous and cannot roll membership back", async () => {
    const sid = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
    const tabId = "tab-MISMATCHED-RESULT";
    const viewerKey = syncViewerKey(tabId);
    await seedSession(sid);
    workerSends = [];
    attachAcknowledgingWorker((frame) => {
      workerSends.push(frame);
      if (frame.frame.case === "viewportRequest") {
        const request = frame.frame.value;
        resolvePendingRpc(request.requestId, create(WViewportResultSchema, {
          requestId: request.requestId,
          sessionId: request.sessionId,
          clientSeq: request.clientSeq + 1n,
          status: TerminalViewportStatus.REJECTED,
          phase: TerminalWritePhase.PRE_WRITE,
          reason: "belongs to another attempt",
        }));
      }
      return 1;
    });

    try {
      const ambiguous = await ambiguateSyncViewport(tabId, {
        sessionId: sid, cols: 96, rows: 32,
        clientSeq: 56n, cause: ResizeCause.INITIAL,
      });
      expect(ambiguous.reason).toBe("mismatched worker viewport result");
      expect(isSubscribed(viewerKey, sid)).toBe(true);
      expect(_viewersBySession.get(sid)?.get(viewerKey)).toMatchObject({
        cols: 96,
        rows: 32,
      });
    } finally {
      attachAcknowledgingWorker();
    }
  });

  test("a newer zero-sequence ambiguity invalidates an older committed replay", async () => {
    const sid = "ffffffff-ffff-4fff-8fff-ffffffffffff";
    const tabId = "tab-CACHE-FENCE";
    const viewerKey = syncViewerKey(tabId);
    await seedSession(sid);
    await acceptSyncViewport(tabId, {
      sessionId: sid, cols: 84, rows: 26,
      clientSeq: 70n, cause: ResizeCause.INITIAL,
    });

    workerSends = [];
    attachAcknowledgingWorker((frame) => {
      workerSends.push(frame);
      if (frame.frame.case === "viewportRequest") {
        const request = frame.frame.value;
        resolvePendingRpc(request.requestId, create(WViewportResultSchema, {
          requestId: request.requestId,
          sessionId: request.sessionId,
          clientSeq: request.clientSeq,
          status: TerminalViewportStatus.AMBIGUOUS,
          reason: "withdraw completion unknown",
        }));
      }
      return 1;
    });

    try {
      await ambiguateSyncViewport(tabId, {
        sessionId: sid, cols: 0, rows: 0,
        clientSeq: 0n, cause: ResizeCause.WITHDRAW,
      });
      expect(isSubscribed(viewerKey, sid)).toBe(false);
      expect(viewportRequests()).toHaveLength(1);

      attachAcknowledgingWorker();
      const staleReplay = await rejectSyncViewport(tabId, {
        sessionId: sid, cols: 84, rows: 26,
        clientSeq: 70n, cause: ResizeCause.INITIAL,
      });
      expect(staleReplay.reason).toMatch(/stale|conflicting/);
      expect(viewportRequests()).toHaveLength(1);
      expect(isSubscribed(viewerKey, sid)).toBe(false);
    } finally {
      attachAcknowledgingWorker();
    }
  });

  test("lost admitted result keeps membership and a late old rejection cannot undo its successor", async () => {
    const sid = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    const tabId = "tab-LOST-RESULT";
    const viewerKey = syncViewerKey(tabId);
    await seedSession(sid);
    workerSends = [];
    const firstSent = Promise.withResolvers<DViewportRequest>();
    let viewportAttempt = 0;
    attachAcknowledgingWorker((frame) => {
      workerSends.push(frame);
      if (frame.frame.case !== "viewportRequest") return 1;
      const request = frame.frame.value;
      viewportAttempt += 1;
      if (viewportAttempt === 1) {
        firstSent.resolve(request);
        return 1;
      }
      nextChannelResizeSeq += 1n;
      resolvePendingRpc(request.requestId, create(WViewportResultSchema, {
        requestId: request.requestId,
        sessionId: request.sessionId,
        clientSeq: request.clientSeq,
        status: TerminalViewportStatus.COMMITTED,
        channelResizeSeq: nextChannelResizeSeq,
        cols: request.cols,
        rows: request.rows,
        resized: true,
      }));
      return 1;
    });

    vi.useFakeTimers();
    try {
      const firstResult = dispatchSyncViewport(tabId, {
        sessionId: sid, cols: 80, rows: 24,
        clientSeq: 61n, cause: ResizeCause.INITIAL,
      });
      const oldRequest = await firstSent.promise;
      expect(isSubscribed(viewerKey, sid)).toBe(true);
      expect(_viewersBySession.get(sid)?.has(viewerKey)).toBe(true);

      vi.advanceTimersByTime(8_000);
      const ambiguous = await firstResult;
      expect(ambiguous.case).toBe("viewportAmbiguous");
      if (ambiguous.case !== "viewportAmbiguous") {
        throw new Error(`expected viewport ambiguity, received ${ambiguous.case}`);
      }
      expect(ambiguous.value.reason).toMatch(/did not reply within \d+ms/);
      expect(isSubscribed(viewerKey, sid)).toBe(true);
      expect(_viewersBySession.get(sid)?.has(viewerKey)).toBe(true);

      await acceptSyncViewport(tabId, {
        sessionId: sid, cols: 104, rows: 38,
        clientSeq: 62n, cause: ResizeCause.VIEWPORT,
      });
      expect(_viewersBySession.get(sid)?.get(viewerKey)).toMatchObject({
        cols: 104,
        rows: 38,
      });

      // The first correlation was retired by its deadline. Its late typed
      // rejection settles nothing and therefore cannot trigger old rollback.
      expect(resolvePendingRpc(oldRequest.requestId, create(WViewportResultSchema, {
        requestId: oldRequest.requestId,
        sessionId: oldRequest.sessionId,
        clientSeq: oldRequest.clientSeq,
        status: TerminalViewportStatus.REJECTED,
        phase: TerminalWritePhase.PRE_WRITE,
        reason: "late old rejection",
      }))).toBe(false);
      expect(isSubscribed(viewerKey, sid)).toBe(true);
      expect(_viewersBySession.get(sid)?.get(viewerKey)).toMatchObject({
        cols: 104,
        rows: 38,
      });
    } finally {
      vi.useRealTimers();
      attachAcknowledgingWorker();
    }
  });

  test("worker receives composite viewer_id in the typed viewport request", async () => {
    const sid = "55555555-5555-5555-5555-555555555555";
    await seedSession(sid);
    workerSends = [];
    await acceptSyncViewport("tab-WIRE", {
      sessionId: sid, cols: 80, rows: 24,
      clientSeq: 1n, cause: ResizeCause.INITIAL,
    });

    const requests = viewportRequests();
    expect(requests).toHaveLength(1);
    expect(requests[0]!.viewerId).toBe(syncViewerKey("tab-WIRE"));
  });

  test("stale resize intents neither change membership nor reach the worker", async () => {
    const sid = "77777777-7777-4777-8777-777777777777";
    const tabId = "tab-ORDER";
    const viewerKey = syncViewerKey(tabId);
    await seedSession(sid);
    workerSends = [];

    await acceptSyncViewport(tabId, {
      sessionId: sid, cols: 80, rows: 24,
      clientSeq: 1n, cause: ResizeCause.INITIAL,
    });
    await acceptSyncViewport(tabId, {
      sessionId: sid, cols: 0, rows: 0,
      clientSeq: 2n, cause: ResizeCause.WITHDRAW,
    });
    await acceptSyncViewport(tabId, {
      sessionId: sid, cols: 80, rows: 24,
      clientSeq: 3n, cause: ResizeCause.TAB_VISIBLE,
    });
    expect(viewportRequests()).toHaveLength(3);
    expect(isSubscribed(viewerKey, sid)).toBe(true);

    // An old withdraw arriving after the newer reclaim is explicitly rejected;
    // neither coordinator nor worker may apply it.
    const staleWithdraw = await rejectSyncViewport(tabId, {
      sessionId: sid, cols: 0, rows: 0,
      clientSeq: 2n, cause: ResizeCause.WITHDRAW,
    });
    expect(staleWithdraw.reason).toMatch(/stale|conflicting/);
    expect(viewportRequests()).toHaveLength(3);
    expect(isSubscribed(viewerKey, sid)).toBe(true);

    await acceptSyncViewport(tabId, {
      sessionId: sid, cols: 0, rows: 0,
      clientSeq: 4n, cause: ResizeCause.WITHDRAW,
    });
    expect(viewportRequests()).toHaveLength(4);
    expect(isSubscribed(viewerKey, sid)).toBe(false);

    // The old claim cannot revive coordinator delivery or reach the worker.
    const staleClaim = await rejectSyncViewport(tabId, {
      sessionId: sid, cols: 80, rows: 24,
      clientSeq: 3n, cause: ResizeCause.TAB_VISIBLE,
    });
    expect(staleClaim.reason).toMatch(/stale|conflicting/);
    expect(viewportRequests()).toHaveLength(4);
    expect(isSubscribed(viewerKey, sid)).toBe(false);
  });

  test("equal current heartbeat refreshes and reaches the worker exactly once", async () => {
    const sid = "88888888-8888-4888-8888-888888888888";
    const tabId = "tab-HEARTBEAT";
    const viewerKey = syncViewerKey(tabId);
    await seedSession(sid);
    workerSends = [];

    await acceptSyncViewport(tabId, {
      sessionId: sid, cols: 90, rows: 30,
      clientSeq: 20n, cause: ResizeCause.INITIAL,
    });
    await acceptSyncViewport(tabId, {
      sessionId: sid, cols: 90, rows: 30, heldCellSeq: 17n,
      clientSeq: 20n, cause: ResizeCause.HEARTBEAT,
    });
    expect(viewportRequests()).toHaveLength(2);
    expect(isSubscribed(viewerKey, sid)).toBe(true);

    await rejectSyncViewport(tabId, {
      sessionId: sid, cols: 91, rows: 31,
      clientSeq: 20n, cause: ResizeCause.VIEWPORT,
    });
    await rejectSyncViewport(tabId, {
      sessionId: sid, cols: 90, rows: 30,
      clientSeq: 19n, cause: ResizeCause.HEARTBEAT,
    });
    await rejectSyncViewport(tabId, {
      sessionId: sid, cols: 0, rows: 0,
      clientSeq: 20n, cause: ResizeCause.WITHDRAW,
    });
    expect(viewportRequests()).toHaveLength(2);
    expect(isSubscribed(viewerKey, sid)).toBe(true);
  });

  test("definite send failure rolls back and permits a same-sequence retry", async () => {
    const sid = "99999999-9999-4999-8999-999999999999";
    const tabId = "tab-RETRY";
    const viewerKey = syncViewerKey(tabId);
    await seedSession(sid);
    workerSends = [];
    let sendAttempts = 0;
    attachAcknowledgingWorker((frame) => {
      sendAttempts += 1;
      if (sendAttempts === 1) throw new Error("injected first send failure");
      return captureAndAcknowledgeWorkerSend(frame);
    });

    try {
      const command: ViewportCommand = {
        sessionId: sid, cols: 80, rows: 24,
        clientSeq: 30n, cause: ResizeCause.INITIAL,
      };
      const failed = await rejectSyncViewport(tabId, command);
      expect(failed.reason).toMatch(/worker unavailable/);
      expect(isSubscribed(viewerKey, sid)).toBe(false);
      expect(_viewersBySession.get(sid)?.has(viewerKey) ?? false).toBe(false);

      await acceptSyncViewport(tabId, command);
      expect(sendAttempts).toBe(2);
      expect(viewportRequests()).toHaveLength(1);
      expect(isSubscribed(viewerKey, sid)).toBe(true);
      expect(_viewersBySession.get(sid)?.has(viewerKey)).toBe(true);
      expect(viewportRequests()[0]!.clientSeq).toBe(30n);
    } finally {
      attachAcknowledgingWorker();
    }
  });

  test("zero-sequence compatibility commands forward the coordinator effective sequence", async () => {
    const sid = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const tabId = "tab-MIXED-SEQUENCE";
    const viewerKey = syncViewerKey(tabId);
    await seedSession(sid);
    workerSends = [];

    await acceptSyncViewport(tabId, {
      sessionId: sid, cols: 80, rows: 24,
      clientSeq: 80n, cause: ResizeCause.INITIAL,
    });
    await acceptSyncViewport(tabId, {
      sessionId: sid, cols: 81, rows: 25,
      clientSeq: 0n, cause: ResizeCause.VIEWPORT,
    });
    // The zero-sequence claim advanced both sides to 81, so ordered equality
    // is rejected without another worker request.
    await rejectSyncViewport(tabId, {
      sessionId: sid, cols: 82, rows: 26,
      clientSeq: 81n, cause: ResizeCause.VIEWPORT,
    });
    // A zero-sequence withdraw preserves the effective watermark because the
    // worker removes the claim before inspecting client_seq.
    await acceptSyncViewport(tabId, {
      sessionId: sid, cols: 0, rows: 0,
      clientSeq: 0n, cause: ResizeCause.WITHDRAW,
    });
    await acceptSyncViewport(tabId, {
      sessionId: sid, cols: 83, rows: 27,
      clientSeq: 82n, cause: ResizeCause.TAB_VISIBLE,
    });

    const sentResizes = viewportRequests().map((request) => ({
      clientSeq: request.clientSeq,
      cols: request.cols,
      rows: request.rows,
    }));
    expect(sentResizes).toEqual([
      { clientSeq: 80n, cols: 80, rows: 24 },
      { clientSeq: 81n, cols: 81, rows: 25 },
      { clientSeq: 81n, cols: 0, rows: 0 },
      { clientSeq: 82n, cols: 83, rows: 27 },
    ]);
    expect(isSubscribed(viewerKey, sid)).toBe(true);
  });

  test("cursor presence and worker command use the composite tab identity", async () => {
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
    expect(last.frame.value.viewerId).toBe(`${browserFp}:tab-CURSOR`);
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
