// Two real CoordinatorMoveOrchestrator instances in one process — a SOURCE and
// a TARGET, each with its own temp dir, DB and HandoffStateStore — wired to
// each other through a CoordinatorMoveRuntime. Every per-defect unit test pins
// one branch; this pins the property that matters across both halves: a move
// ends with each side terminal and neither write gate stuck closed.

import { afterEach, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { createBunCoordinatorMoveRuntime } from "../src/coord-move/bun-runtime.ts";
import { CoordinatorMoveOrchestrator } from "../src/coord-move/orchestrator.ts";
import { HandoffStateStore } from "../src/coord-move/state.ts";
import { COORD_GIT_SHA } from "../src/git-sha.ts";
import type { CoordConfig } from "@roost/shared/config";
import type { Database } from "bun:sqlite";
import type { CoordKey } from "../src/coord-key.ts";
import type { CoordinatorMoveRuntime, MoveSnapshot, MoveWorker } from "../src/coord-move/runtime.ts";

interface MoveSide {
  dir: string;
  cfg: CoordConfig;
  sqlite: Database;
  store: HandoffStateStore;
  coordKey: CoordKey;
}

const workdirs: string[] = [];
const closers: (() => Promise<void>)[] = [];
afterEach(async () => {
  for (const close of closers.splice(0)) await close();
  for (const dir of workdirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

async function side(name: string, publicUrl: string): Promise<MoveSide> {
  const dir = mkdtempSync(join(tmpdir(), `roost-coord-move-e2e-${name}-`));
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
  publicUrl, tlsCertPath: undefined, tlsKeyPath: undefined,
  jwtMaxAgeSecs: 300, auditRetentionDays: 90, relaxedCsp: false, corsAllowedOrigins: [], }
  return { dir, cfg, sqlite: opened.sqlite, store: new HandoffStateStore(cfg.handoffPath), coordKey: await loadOrCreateCoordKey(keyPath) };
}

function worker(fp: string): MoveWorker {
  return { fp, label: fp, os: "darwin", gitSha: COORD_GIT_SHA, reachableAddr: `${fp}.ts.net`, online: true };
}

const SOURCE_URL = "https://source.ts.net:4102";
const DASHBOARD_ID = "coordinator-move-e2e-dashboard";
const TARGET_URL = "https://target.ts.net:4102";

/** The target half of the pair: its own orchestrator plus the runtime it uses
 *  to reach the two workers that followed the move. */
function targetSide(
  fixture: MoveSide,
  workers: MoveWorker[],
  calls: string[],
): CoordinatorMoveOrchestrator {
  const record = (label: string) => async (target: MoveWorker) => { calls.push(`${label}:${target.fp}`); };
  const runtime: CoordinatorMoveRuntime = {
    checkTarget: async () => null,
    prepareTarget: async () => {},
    stageWorker: record("target-stage"),
    activateWorker: record("target-activate"),
    commitWorker: record("target-commit-worker"),
    abortWorker: record("target-abort-worker"),
    copySnapshot: async () => {},
    reconnectWorkers: async () => { calls.push("target-reconnect"); },
    waitForWorkers: async () => {},
    targetStatus: async () => null,
    commitTarget: async () => {},
    abortTarget: async () => {},
    targetHealthy: async () => {},
    publishRelocation: () => {},
  };
  return new CoordinatorMoveOrchestrator({
    cfg: fixture.cfg, coordKey: fixture.coordKey, store: fixture.store, runtime,
    workers: async () => workers,
  });
}

/** The source's runtime, with every target-facing call routed into the real
 *  target orchestrator instead of a stub. */
function sourceRuntime(options: {
  target: CoordinatorMoveOrchestrator;
  targetStore: HandoffStateStore;
  targetCoordKid: string;
  workers: MoveWorker[];
  calls: string[];
  published: string[];
  onWaitForWorkers?: () => Promise<void>;
}): CoordinatorMoveRuntime {
  const { target, targetStore, calls } = options;
  const record = (label: string) => async (moveWorker: MoveWorker) => { calls.push(`${label}:${moveWorker.fp}`); };
  const phaseOf = async (state: MoveSnapshot) =>
    (await target.internalStatus(state.handoffId, state.secret).catch(() => null))?.phase ?? null;
  return {
    checkTarget: async () => null,
    prepareTarget: async () => { calls.push("prepare-target"); },
    stageWorker: record("stage"),
    activateWorker: record("activate"),
    commitWorker: record("commit-worker"),
    abortWorker: record("abort-worker"),
    copySnapshot: async (state) => {
      calls.push("copy-state");
      // What the real transfer's final rename does on the target box: the
      // TARGET-role handoff record lands on the target's disk. A TARGET record
      // must not carry the plaintext secret (state.ts's schema refinement) —
      // only the digest the source will authenticate against.
      targetStore.write({
        version: 1, handoff_id: state.handoffId, role: "TARGET", phase: "WAITING_FOR_WORKERS",
        dashboard_id: state.dashboardId,
        source_url: state.sourceUrl, target_url: state.targetUrl, target_worker_fp: "target-pending",
        expected_worker_fps: state.expectedWorkerFps, commit_acked_worker_fps: [],
        expected_coord_kid: options.targetCoordKid, expected_git_sha: state.expectedGitSha,
        secret_sha256: state.secretSha256, started_at_ms: Date.now(), updated_at_ms: Date.now(),
      });
    },
    reconnectWorkers: async () => { calls.push("reconnect-workers"); },
    waitForWorkers: async () => {
      calls.push("wait-workers");
      await options.onWaitForWorkers?.();
    },
    targetStatus: phaseOf,
    commitTarget: async (state) => {
      calls.push("commit-target");
      if (target.status(state.dashboardId, state.handoffId)?.phase === "COMMITTED") return;
      // internalCommit returns as soon as it has kicked its background
      // commitTargetWorkers, exactly like the real RPC ack. Await the durable
      // COMMITTED write so the source's next targetStatus is not racing
      // finishCommit's 5s retry — the real coord just polls again.
      const committed = whenPhase(targetStore, "COMMITTED");
      await target.internalCommit(state.handoffId, state.secret);
      await committed;
    },
    abortTarget: async (state) => {
      calls.push("abort-target");
      await target.internalAbort(state.handoffId, state.secret);
    },
    targetHealthy: async () => { calls.push("target-healthy"); },
    publishRelocation: (state) => { options.published.push(state.targetUrl); },
  };
}

function whenPhase(store: HandoffStateStore, phase: string): Promise<void> {
  let resolve!: () => void;
  const reached = new Promise<void>((done) => { resolve = done; });
  const write = store.write.bind(store);
  store.write = (next) => {
    const persisted = write(next);
    if (persisted.phase === phase) resolve();
    return persisted;
  };
  return reached;
}

test("a full source-to-target move retires the source and leaves the target serving", async () => {
  const source = await side("source", SOURCE_URL);
  const target = await side("target", TARGET_URL);
  const workers = [worker("target"), worker("worker-a")];
  const calls: string[] = [];
  const published: string[] = [];

  const targetMove = targetSide(target, workers, calls);
  const sourceMove = new CoordinatorMoveOrchestrator({
    cfg: source.cfg, coordKey: source.coordKey, store: source.store,
    runtime: sourceRuntime({
      target: targetMove, targetStore: target.store,
      targetCoordKid: source.coordKey.verifyingKeyKid(), workers, calls, published,
    }),
    workers: async () => workers,
  });

  const retired = whenPhase(source.store, "COMMITTED");
  const handoffId = await sourceMove.start(DASHBOARD_ID, "target");
  await retired;

  expect(source.store.load()?.phase).toBe("COMMITTED");
  expect(sourceMove.gate.mode).toBe("retired");
  expect(target.store.load()?.phase).toBe("COMMITTED");
  // The target is the live coordinator now — a gate left closed here rejects
  // every mutation and every browser firehose upgrade on the new host.
  expect(targetMove.gate.mode).toBe("active");
  expect(targetMove.status(DASHBOARD_ID, handoffId)?.commit_acked_worker_fps).toEqual(["target", "worker-a"]);
  expect(published).toEqual([TARGET_URL]);
});

test("a move that fails mid-flight rolls both halves back to a terminal phase with open gates", async () => {
  const source = await side("source", SOURCE_URL);
  const target = await side("target", TARGET_URL);
  const workers = [worker("target"), worker("worker-a")];
  const calls: string[] = [];
  const published: string[] = [];

  const targetMove = targetSide(target, workers, calls);
  const sourceMove = new CoordinatorMoveOrchestrator({
    cfg: source.cfg, coordKey: source.coordKey, store: source.store,
    runtime: sourceRuntime({
      target: targetMove, targetStore: target.store,
      targetCoordKid: source.coordKey.verifyingKeyKid(), workers, calls, published,
      // The workers never all reach the target. The source rolls back a
      // coordinator whose state has already been copied across.
      onWaitForWorkers: async () => { throw new Error("workers never reached the target"); },
    }),
    workers: async () => workers,
  });

  const failed = whenPhase(source.store, "FAILED");
  await sourceMove.start(DASHBOARD_ID, "target");
  await failed;

  expect(source.store.load()?.phase).toBe("FAILED");
  expect(target.store.load()?.phase).toBe("ROLLED_BACK");
  // Neither side owns the cluster, so both must accept writes again. This is
  // the assertion that fails if internalAbort or execute returns with the gate
  // still held — the failure mode where a rolled-back cluster stays read-only
  // until someone restarts a coordinator.
  expect(sourceMove.gate.mode).toBe("active");
  expect(targetMove.gate.mode).toBe("active");
  expect(published).toEqual([]);
  expect(calls).toContain("abort-target");
  expect(calls.filter((call) => call.startsWith("abort-worker:"))).toEqual(["abort-worker:target", "abort-worker:worker-a"]);
});

test("a failed snapshot transfer does not leave the vacuumed database copy behind", async () => {
  // copySnapshot VACUUM INTOs the whole coordinator DB before it sends a byte.
  // Nothing else deletes that copy, and estimateDbSize sizes only the live DB,
  // so a leak here silently defeats the target's insufficient_disk preflight.
  const source = await side("leak", SOURCE_URL);
  const handoffId = "00000000-0000-4000-8000-000000000009";
  const runtime = createBunCoordinatorMoveRuntime({
    sqlite: source.sqlite, dbPath: source.cfg.dbPath, coordKeyPath: source.cfg.coordKeyPath,
    authorizedKeysPath: source.cfg.authorizedKeysPath, handoffPath: source.cfg.handoffPath,
    publishRelocation: () => {},
  });

  // No handle in the worker registry, so sendCoordinatorSnapshotStart throws
  // after the vacuum — the exact path that used to leak.
  await expect(runtime.copySnapshot({
    handoffId, phase: "COPYING_STATE", sourceUrl: SOURCE_URL, targetUrl: TARGET_URL,
    targetWorkerFp: "no-such-worker", expectedWorkerFps: ["no-such-worker"],
    dashboardId: DASHBOARD_ID,
    expectedCoordKid: source.coordKey.verifyingKeyKid(), expectedGitSha: COORD_GIT_SHA,
    secret: "secret", secretSha256: createHash("sha256").update("secret").digest("hex"),
  })).rejects.toThrow("worker unavailable");

  expect(existsSync(join(source.dir, "handoffs", handoffId, "coordinator_v2.snapshot"))).toBeFalse();
});
