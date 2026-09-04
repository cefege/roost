// Security contract for the coordinator-move internal handoff endpoint
// (handleInternalHandoffRequest, dispatched above every other route in
// main.ts fetch). Drives the REAL handler against a REAL target-role
// orchestrator whose runtime seams are stubbed, mirroring
// coordinator-move-e2e.test.ts's construction of deps.

import { afterEach, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { CoordinatorMoveOrchestrator } from "../src/coord-move/orchestrator.ts";
import { handleInternalHandoffRequest } from "../src/coord-move/internal-http.ts";
import { HandoffStateStore, type MovePhase } from "../src/coord-move/state.ts";
import { COORD_GIT_SHA } from "../src/git-sha.ts";
import type { CoordConfig } from "@roost/shared/config";
import type { Database } from "bun:sqlite";
import type { CoordKey } from "../src/coord-key.ts";
import type { CoordinatorMoveService } from "../src/coord-move/orchestrator.ts";
import type { CoordinatorMoveRuntime, MoveWorker } from "../src/coord-move/runtime.ts";

const HANDOFF_ID = "11111111-2222-3333-4444-555555555555";
const DASHBOARD_ID = "coord-move-internal-http-dashboard";
const SECRET = "e2e-handoff-secret-value";
const SECRET_SHA256 = createHash("sha256").update(SECRET).digest("hex");
const PREFIX = "https://coord.test/internal/coord-handoff";

interface Fixture {
  sqlite: Database;
  store: HandoffStateStore;
  /** The real orchestrator, for gate and state assertions. */
  move: CoordinatorMoveOrchestrator;
  /** What the handler receives: the real orchestrator behind call counters. */
  service: CoordinatorMoveService;
  calls: { internalStatus: number; internalCommit: number; internalAbort: number };
}

const workdirs: string[] = [];
const closers: (() => Promise<void>)[] = [];
afterEach(async () => {
  for (const close of closers.splice(0)) await close();
  for (const dir of workdirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

function worker(fp: string): MoveWorker {
  return { fp, label: fp, os: "darwin", gitSha: COORD_GIT_SHA, reachableAddr: `${fp}.ts.net`, online: true };
}

/** Real target-role orchestrator over temp-dir deps, with a TARGET handoff
 *  record seeded the way the source's snapshot transfer lands it (digest
 *  only — the schema forbids plaintext secrets on TARGET records). */
async function fixture(): Promise<Fixture> {
  const dir = mkdtempSync(join(tmpdir(), "roost-coord-handoff-http-"));
  workdirs.push(dir);
  const dbPath = join(dir, "coord.db");
  const keyPath = join(dir, "coord.key");
  const authorizedKeysPath = join(dir, "authorized_keys.roost");
  writeFileSync(authorizedKeysPath, "");
  const opened = openDb(dbPath);
  await runMigrations(opened.sqlite);
  closers.push(() => opened.close());
  const cfg: CoordConfig = { trustProxy: false, bind: "127.0.0.1:4102", dbPath, coordKeyPath: keyPath, authorizedKeysPath,
  saasMode: false,
  managedContainer: false,
  pushAllowedOrigins: [],
  handoffPath: join(dir, "coord-handoff.json"), webDistPath: "", logDir: dir,
  publicUrl: "https://target.ts.net:4102", tlsCertPath: undefined, tlsKeyPath: undefined,
  jwtMaxAgeSecs: 300, auditRetentionDays: 90, relaxedCsp: false, corsAllowedOrigins: [] };
  const store = new HandoffStateStore(cfg.handoffPath);
  const coordKey: CoordKey = await loadOrCreateCoordKey(keyPath);
  const runtime: CoordinatorMoveRuntime = {
    checkTarget: async () => null,
    prepareTarget: async () => {},
    stageWorker: async () => {},
    activateWorker: async () => {},
    commitWorker: async () => {},
    abortWorker: async () => {},
    copySnapshot: async () => {},
    reconnectWorkers: async () => {},
    waitForWorkers: async () => {},
    targetStatus: async () => null,
    commitTarget: async () => {},
    abortTarget: async () => {},
    targetHealthy: async () => {},
    publishRelocation: () => {},
  };
  store.write({
    version: 1, handoff_id: HANDOFF_ID, role: "TARGET",
    phase: "WAITING_FOR_WORKERS",
    dashboard_id: DASHBOARD_ID,
    source_url: "https://source.ts.net:4102", target_url: cfg.publicUrl!, target_worker_fp: "target-pending",
    expected_worker_fps: ["worker-a"], commit_acked_worker_fps: [],
    expected_coord_kid: coordKey.verifyingKeyKid(), expected_git_sha: COORD_GIT_SHA,
    secret_sha256: SECRET_SHA256, started_at_ms: Date.now(), updated_at_ms: Date.now(),
  });
  const move = new CoordinatorMoveOrchestrator({
    cfg, coordKey, store, runtime,
    workers: async () => [worker("worker-a")],
  });
  // Count handler-visible entry points without replacing the real bodies:
  // the constant-time digest compare lives inside internalStatus.
  const calls = { internalStatus: 0, internalCommit: 0, internalAbort: 0 };
  const counted: CoordinatorMoveService = {
    preflight: (dashboardId, targetWorkerFp) => move.preflight(dashboardId, targetWorkerFp),
    start: (dashboardId, targetWorkerFp) => move.start(dashboardId, targetWorkerFp),
    status: (dashboardId, handoffId) => move.status(dashboardId, handoffId),
    statusForWorker: (handoffId, workerFp) => move.statusForWorker(handoffId, workerFp),
    current: () => move.current(),
    recover: () => move.recover(),
    internalStatus: async (handoffId, secret) => { calls.internalStatus += 1; return move.internalStatus(handoffId, secret); },
    internalCommit: async (handoffId, secret) => { calls.internalCommit += 1; await move.internalCommit(handoffId, secret); },
    internalAbort: async (handoffId, secret) => { calls.internalAbort += 1; await move.internalAbort(handoffId, secret); },
    gate: move.gate,
  };
  return { sqlite: opened.sqlite, store, move, service: counted, calls };
}

function handoffRequest(path: string, options: { method?: string; handoffId?: string | null; secret?: string | null } = {}): Request {
  const headers = new Headers();
  if (options.handoffId !== null) headers.set("x-roost-handoff-id", options.handoffId ?? HANDOFF_ID);
  if (options.secret !== null) headers.set("x-roost-handoff-secret", options.secret ?? SECRET);
  return new Request(`${PREFIX}/${path}`, { method: options.method ?? "GET", headers });
}

/** Resolve when a durable write lands the given phase — no wall-clock waits;
 *  every background run must reach its terminal phase for the test to end. */
function whenPhase(store: HandoffStateStore, phase: MovePhase): Promise<void> {
  const { promise, resolve } = Promise.withResolvers<void>();
  const write = store.write.bind(store);
  store.write = (next) => {
    const persisted = write(next);
    if (persisted.phase === phase) resolve();
    return persisted;
  };
  return promise;
}

test("missing or partial credentials reject with 401 before the service is touched", async () => {
  const fx = await fixture();
  const noHeaders = await handleInternalHandoffRequest(
    handoffRequest("status", { handoffId: null, secret: null }), fx.service);
  const idOnly = await handleInternalHandoffRequest(
    handoffRequest("status", { secret: null }), fx.service);
  const secretOnly = await handleInternalHandoffRequest(
    handoffRequest("status", { handoffId: null }), fx.service);
  expect(noHeaders?.status).toBe(401);
  expect(await noHeaders?.text()).toBe("unauthorized");
  expect(idOnly?.status).toBe(401);
  expect(secretOnly?.status).toBe(401);
  expect(fx.calls).toEqual({ internalStatus: 0, internalCommit: 0, internalAbort: 0 });
  expect(fx.store.load()?.phase).toBe("WAITING_FOR_WORKERS");
});

test("a wrong or empty secret fails the timing-safe digest compare and leaves zero side effects", async () => {
  const fx = await fixture();
  const committed = await handleInternalHandoffRequest(
    handoffRequest("commit", { method: "POST", secret: `${SECRET}-tampered` }), fx.service);
  expect(committed?.status).toBe(401);
  expect(await committed?.text()).toBe("handoff not found");
  const aborted = await handleInternalHandoffRequest(
    handoffRequest("abort", { method: "POST", secret: "" }), fx.service);
  expect(aborted?.status).toBe(401);
  expect(fx.store.load()?.phase).toBe("WAITING_FOR_WORKERS");
  expect(fx.store.load()?.commit_acked_worker_fps).toEqual([]);
  expect(fx.store.load()?.error).toBeUndefined();
});

test("an unknown handoff id reports 401 with the same message as a bad secret", async () => {
  const fx = await fixture();
  const response = await handleInternalHandoffRequest(
    handoffRequest("status", { handoffId: "99999999-2222-3333-4444-555555555555" }), fx.service);
  expect(response?.status).toBe(401);
  expect(await response?.text()).toBe("handoff not found");
});

test("a valid-secret status request returns the wire projection without the plaintext secret", async () => {
  const fx = await fixture();
  const response = await handleInternalHandoffRequest(handoffRequest("status"), fx.service);
  expect(response?.status).toBe(200);
  const body = await response?.json();
  expect(body).toEqual({
    phase: "WAITING_FOR_WORKERS",
    source_url: "https://source.ts.net:4102",
    target_url: "https://target.ts.net:4102",
    expected_worker_fps: ["worker-a"],
    connected_worker_fps: ["worker-a"],
    error: null,
  });
  expect(JSON.stringify(body)).not.toContain(SECRET);
});

test("a valid-secret commit runs internalCommit exactly once and answers 202 with an empty body", async () => {
  const fx = await fixture();
  const committed = whenPhase(fx.store, "COMMITTED");
  const response = await handleInternalHandoffRequest(
    handoffRequest("commit", { method: "POST" }), fx.service);
  expect(response?.status).toBe(202);
  expect(response?.body).toBeNull();
  expect(fx.calls.internalCommit).toBe(1);
  expect(fx.store.load()?.phase).toBe("COMMITTING");
  await committed;
  expect(fx.store.load()?.phase).toBe("COMMITTED");
  expect(fx.store.load()?.commit_acked_worker_fps).toEqual(["worker-a"]);
});

test("a valid-secret abort runs internalAbort and rolls the target back to ROLLED_BACK", async () => {
  const fx = await fixture();
  const rolledBack = whenPhase(fx.store, "ROLLED_BACK");
  const response = await handleInternalHandoffRequest(
    handoffRequest("abort", { method: "POST" }), fx.service);
  expect(response?.status).toBe(204);
  expect(response?.body).toBeNull();
  expect(fx.calls.internalAbort).toBe(1);
  await rolledBack;
  expect(fx.move.gate.mode).toBe("active");
});

test("method and path mismatches follow the real dispatch contract", async () => {
  const fx = await fixture();
  const getOnCommit = await handleInternalHandoffRequest(
    handoffRequest("commit", { method: "GET" }), fx.service);
  const postOnStatus = await handleInternalHandoffRequest(
    handoffRequest("status", { method: "POST" }), fx.service);
  const deleteOnAbort = await handleInternalHandoffRequest(
    handoffRequest("abort", { method: "DELETE" }), fx.service);
  const unknownSubpath = await handleInternalHandoffRequest(
    handoffRequest("bogus"), fx.service);
  expect(getOnCommit?.status).toBe(404);
  expect(postOnStatus?.status).toBe(404);
  expect(deleteOnAbort?.status).toBe(404);
  // Any path under the prefix demands auth first, then 404s unknown routes.
  const unauthenticatedUnknown = await handleInternalHandoffRequest(
    handoffRequest("bogus", { handoffId: null, secret: null }), fx.service);
  expect(unauthenticatedUnknown?.status).toBe(401);
  expect(unknownSubpath?.status).toBe(404);
  // Anything outside the prefix falls through to main.ts's later routes.
  const outside = new Request("https://coord.test/healthz");
  expect(await handleInternalHandoffRequest(outside, fx.service)).toBeNull();
});
