import { afterEach, describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { CoordinatorMoveOrchestrator } from "../src/coord-move/orchestrator.ts";
import { HandoffStateStore, type HandoffState } from "../src/coord-move/state.ts";
import { CoordinatorWriteGate } from "../src/coord-move/write-gate.ts";
import { COORD_GIT_SHA } from "../src/git-sha.ts";
import type { CoordConfig } from "@roost/shared/config";
import type { CoordinatorMoveRuntime, MoveSnapshot, MoveWorker } from "../src/coord-move/runtime.ts";

const workdirs: string[] = [];
afterEach(() => {
  for (const dir of workdirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

async function fixture() {
  const dir = mkdtempSync(join(tmpdir(), "roost-coord-move-"));
  workdirs.push(dir);
  const dbPath = join(dir, "coord.db");
  const keyPath = join(dir, "coord.key");
  const authorizedKeysPath = join(dir, "authorized_keys.roost");
  writeFileSync(authorizedKeysPath, "");
  const opened = openDb(dbPath);
  await runMigrations(opened.sqlite);
  const cfg: CoordConfig = {
    bind: "127.0.0.1:4102", dbPath, coordKeyPath: keyPath, authorizedKeysPath,
    handoffPath: join(dir, "coord-handoff.json"), webDistPath: "", logDir: dir,
    publicUrl: "https://source.ts.net:4102", tlsCertPath: undefined, tlsKeyPath: undefined,
    jwtMaxAgeSecs: 300, auditRetentionDays: 90, relaxedCsp: false, corsAllowedOrigins: [],
  };
  return { dir, cfg, db: opened.db, sqlite: opened.sqlite, coordKey: await loadOrCreateCoordKey(keyPath) };
}

function worker(fp: string): MoveWorker {
  return { fp, label: fp, os: "darwin", gitSha: COORD_GIT_SHA, reachableAddr: `${fp}.ts.net`, online: true };
}

function runtime(calls: string[], target: { committed: boolean }, onPublished?: () => void, onCommitWorker?: (count: number) => void): CoordinatorMoveRuntime {
  const record = (name: string) => async (_value: MoveWorker | MoveSnapshot) => { calls.push(name); };
  return {
    checkTarget: async () => null,
    prepareTarget: record("prepare-target"),
    stageWorker: record("stage-worker"),
    activateWorker: record("activate-worker"),
    commitWorker: async () => {
      calls.push("commit-worker");
      onCommitWorker?.(calls.filter((call) => call === "commit-worker").length);
    },
    abortWorker: record("abort-worker"),
    copySnapshot: record("copy-state"),
    reconnectWorkers: async () => { calls.push("reconnect-workers"); },
    waitForWorkers: record("wait-workers"),
    targetStatus: async () => target.committed ? "COMMITTED" : "COMMITTING",
    commitTarget: async () => { calls.push("commit-target"); target.committed = true; },
    abortTarget: record("abort-target"),
    targetHealthy: record("target-healthy"),
    publishRelocation: (snapshot) => { calls.push(`relocate:${snapshot.handoffId}`); onPublished?.(); },
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

/** The private half of the orchestrator, for branches with no public entry. */
interface MoveInternals { execute(state: HandoffState): Promise<void> }


describe("CoordinatorMoveOrchestrator", () => {
  test("durably stages, drains, transfers, and retires the source only after target commit", async () => {
    const f = await fixture();
    const calls: string[] = [];
    const target = { committed: false };
    const workers = [worker("target"), worker("worker-a")];
    const store = new HandoffStateStore(f.cfg.handoffPath);
    let published!: () => void;
    const relocationPublished = new Promise<void>((done) => { published = done; });
    const move = new CoordinatorMoveOrchestrator({
      cfg: f.cfg, coordKey: f.coordKey,
      store, runtime: runtime(calls, target, published),
      workers: async () => workers,
    });

    const handoffId = await move.start("target");
    await relocationPublished;

    expect(move.gate.mode).toBe("retired");
    expect(calls).toEqual([
      "prepare-target", "stage-worker", "stage-worker", "copy-state",
      "activate-worker", "activate-worker", "wait-workers", "commit-target",
      "target-healthy", `relocate:${handoffId}`,
    ]);
    f.sqlite.close();
  });

  test("target commit authenticates the handoff and opens writes only after every worker acknowledges", async () => {
    const f = await fixture();
    const calls: string[] = [];
    const state = new HandoffStateStore(f.cfg.handoffPath);
    state.write({
      version: 1, handoff_id: "00000000-0000-4000-8000-000000000001", role: "TARGET", phase: "WAITING_FOR_WORKERS",
      source_url: "https://source.ts.net:4102", target_url: "https://target.ts.net:4102", target_worker_fp: "target",
      expected_worker_fps: ["target", "worker-a"], commit_acked_worker_fps: [],
      expected_coord_kid: f.coordKey.verifyingKeyKid(), expected_git_sha: COORD_GIT_SHA,
      secret_sha256: "f".repeat(64), started_at_ms: Date.now(), updated_at_ms: Date.now(),
    });
    let replayed!: () => void;
    const replaySent = new Promise<void>((done) => { replayed = done; });
    const targetRuntime = runtime(calls, { committed: false }, undefined, (count) => {
      if (count === 4) replayed();
    });
    const move = new CoordinatorMoveOrchestrator({
      cfg: f.cfg, coordKey: f.coordKey, store: state, runtime: targetRuntime,
      workers: async () => [worker("target"), worker("worker-a")],
    });

    const handoffId = "00000000-0000-4000-8000-000000000001";
    await expect(move.internalCommit(handoffId, "wrong-secret")).rejects.toThrow("handoff not found");
    state.write({ ...state.load()!, secret_sha256: createHash("sha256").update("secret").digest("hex") });
    const committed = whenPhase(state, "COMMITTED");
    await move.internalCommit(handoffId, "secret");
    await committed;
    await replaySent;
    expect(move.gate.mode).toBe("active");
    expect(move.status(handoffId)?.commit_acked_worker_fps).toEqual(["target", "worker-a"]);
    // First round obtains durable commit acknowledgements; the second round
    // tells workers to reconnect and replay events held during target_pending.
    expect(calls.filter((call) => call === "commit-worker")).toHaveLength(4);
    f.sqlite.close();
  });

  test("write drain waits for in-flight mutations and rejects new ones", async () => {
    const gate = new CoordinatorWriteGate();
    const lease = gate.acquire();
    const draining = gate.beginDrain(100);
    expect(gate.mode).toBe("source_draining");
    expect(() => gate.acquire()).toThrow("coordinator move in progress");
    lease.release();
    await draining;
  });

  test("recovers a source crash during rollback by aborting the staged target", async () => {
    const f = await fixture();
    const calls: string[] = [];
    const store = new HandoffStateStore(f.cfg.handoffPath);
    store.write({
      version: 1, handoff_id: "00000000-0000-4000-8000-000000000003", role: "SOURCE", phase: "ROLLING_BACK",
      source_url: "https://source.ts.net:4102", target_url: "https://target.ts.net:4102", target_worker_fp: "target",
      expected_worker_fps: ["target"], commit_acked_worker_fps: [], expected_coord_kid: f.coordKey.verifyingKeyKid(),
      expected_git_sha: COORD_GIT_SHA, secret_sha256: createHash("sha256").update("secret").digest("hex"), secret: "secret",
      started_at_ms: Date.now(), updated_at_ms: Date.now(), error: "source crashed",
    });
    const move = new CoordinatorMoveOrchestrator({
      cfg: f.cfg, coordKey: f.coordKey, store, runtime: runtime(calls, { committed: false }),
      workers: async () => [worker("target")],
    });

    await move.recover();

    expect(calls).toEqual(["abort-worker", "abort-target"]);
    expect(store.load()?.phase).toBe("ROLLED_BACK");
    expect(move.gate.mode).toBe("active");
    f.sqlite.close();
  });

  test("recovered snapshot failure aborts every durably staged worker", async () => {
    const f = await fixture();
    const calls: string[] = [];
    const store = new HandoffStateStore(f.cfg.handoffPath);
    store.write({
      version: 1, handoff_id: "00000000-0000-4000-8000-000000000004", role: "SOURCE", phase: "COPYING_STATE",
      source_url: "https://source.ts.net:4102", target_url: "https://target.ts.net:4102", target_worker_fp: "target",
      expected_worker_fps: ["target", "worker-a"], commit_acked_worker_fps: [], expected_coord_kid: f.coordKey.verifyingKeyKid(),
      expected_git_sha: COORD_GIT_SHA, secret_sha256: createHash("sha256").update("secret").digest("hex"), secret: "secret",
      started_at_ms: Date.now(), updated_at_ms: Date.now(),
    });
    const recoveredRuntime = runtime(calls, { committed: false });
    recoveredRuntime.targetStatus = async () => null;
    recoveredRuntime.copySnapshot = async () => {
      calls.push("copy-state");
      throw new Error("snapshot transfer failed");
    };
    const failed = whenPhase(store, "FAILED");
    const move = new CoordinatorMoveOrchestrator({
      cfg: f.cfg, coordKey: f.coordKey, store, runtime: recoveredRuntime,
      workers: async () => [worker("target"), worker("worker-a")],
    });

    await move.recover();
    await failed;

    expect(calls).toEqual(["copy-state", "abort-worker", "abort-worker", "abort-target"]);
    expect(move.gate.mode).toBe("active");
    f.sqlite.close();
  });

  // The gate is read by auth-interceptor, worker-conn and sync-ws-handler; a
  // non-active mode with no terminal phase left to drive it rejects every
  // mutation and every firehose upgrade with nothing left to reopen it.
  for (const phase of ["FAILED", "ROLLED_BACK"] as const) {
    test(`recovering a target handoff already ${phase} reopens writes`, async () => {
      const f = await fixture();
      const calls: string[] = [];
      const store = new HandoffStateStore(f.cfg.handoffPath);
      const handoffId = "00000000-0000-4000-8000-000000000005";
      store.write({
        version: 1, handoff_id: handoffId, role: "TARGET", phase,
        source_url: "https://source.ts.net:4102", target_url: "https://target.ts.net:4102", target_worker_fp: "target",
        expected_worker_fps: ["target"], commit_acked_worker_fps: [], expected_coord_kid: f.coordKey.verifyingKeyKid(),
        expected_git_sha: COORD_GIT_SHA, secret_sha256: "f".repeat(64),
        started_at_ms: Date.now(), updated_at_ms: Date.now(), error: "rolled back before restart",
      });
      const move = new CoordinatorMoveOrchestrator({
        cfg: f.cfg, coordKey: f.coordKey, store, runtime: runtime(calls, { committed: false }),
        workers: async () => [worker("target")],
      });

      await move.recover();

      expect(move.gate.mode).toBe("active");
      // A terminal target owns nothing: no commit replay, no rollback retry.
      expect(calls).toEqual([]);
      expect(store.load()?.phase).toBe(phase);
      f.sqlite.close();
    });
  }

  test("a target abort that fails to reach a worker restores writes instead of holding the gate", async () => {
    const f = await fixture();
    const calls: string[] = [];
    const store = new HandoffStateStore(f.cfg.handoffPath);
    const handoffId = "00000000-0000-4000-8000-000000000006";
    store.write({
      version: 1, handoff_id: handoffId, role: "TARGET", phase: "WAITING_FOR_WORKERS",
      source_url: "https://source.ts.net:4102", target_url: "https://target.ts.net:4102", target_worker_fp: "target",
      expected_worker_fps: ["target"], commit_acked_worker_fps: [], expected_coord_kid: f.coordKey.verifyingKeyKid(),
      expected_git_sha: COORD_GIT_SHA, secret_sha256: createHash("sha256").update("secret").digest("hex"),
      started_at_ms: Date.now(), updated_at_ms: Date.now(),
    });
    const targetRuntime = runtime(calls, { committed: false });
    targetRuntime.abortWorker = async () => {
      calls.push("abort-worker");
      throw new Error("worker abort failed");
    };
    const move = new CoordinatorMoveOrchestrator({
      cfg: f.cfg, coordKey: f.coordKey, store, runtime: targetRuntime,
      workers: async () => [worker("target")],
    });

    await expect(move.internalAbort(handoffId, "secret")).rejects.toThrow("worker abort failed");

    // The abort path is the recovery mechanism; wedging here is unrecoverable.
    expect(move.gate.mode).toBe("active");
    expect(store.load()?.phase).toBe("FAILED");
    expect(calls).toEqual(["abort-worker"]);
    f.sqlite.close();
  });

  test("a source that throws at COMMITTING still reaches a terminal phase without a restart", async () => {
    const f = await fixture();
    const calls: string[] = [];
    const store = new HandoffStateStore(f.cfg.handoffPath);
    const seeded = store.write({
      version: 1, handoff_id: "00000000-0000-4000-8000-000000000007", role: "SOURCE", phase: "COMMITTING",
      source_url: "https://source.ts.net:4102", target_url: "https://target.ts.net:4102", target_worker_fp: "target",
      expected_worker_fps: ["target", "worker-a"], commit_acked_worker_fps: [], expected_coord_kid: f.coordKey.verifyingKeyKid(),
      expected_git_sha: COORD_GIT_SHA, secret_sha256: createHash("sha256").update("secret").digest("hex"), secret: "secret",
      started_at_ms: Date.now(), updated_at_ms: Date.now(),
    });
    const target = { committed: false };
    const sourceRuntime = runtime(calls, target);
    let commitAttempts = 0;
    sourceRuntime.commitTarget = async () => {
      calls.push("commit-target");
      commitAttempts += 1;
      if (commitAttempts === 1) throw new Error("target commit rejected");
      target.committed = true;
    };
    const move = new CoordinatorMoveOrchestrator({
      cfg: f.cfg, coordKey: f.coordKey, store, runtime: sourceRuntime,
      // worker-a has dropped out of the registry, so advance() throws while the
      // durable phase is already COMMITTING.
      workers: async () => [worker("target")],
    });

    // recover() intercepts a COMMITTING source before execute() ever runs, so
    // this catch branch has no public entry point to drive it from.
    const internals = move as unknown as MoveInternals;
    await internals.execute(seeded);

    // COMMITTING is not terminal: returning here would leave the source retired
    // with no in-process retry, recoverable only by a coord restart.
    expect(store.load()?.phase).toBe("COMMITTED");
    expect(move.gate.mode).toBe("retired");
    expect(commitAttempts).toBe(2);
    expect(calls).toContain(`relocate:${seeded.handoff_id}`);
    f.sqlite.close();
  }, 20_000);
});
