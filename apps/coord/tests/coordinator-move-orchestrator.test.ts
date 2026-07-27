import { afterEach, describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { CoordinatorMoveOrchestrator } from "../src/coord-move/orchestrator.ts";
import { HandoffStateStore } from "../src/coord-move/state.ts";
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
    jwtMaxAgeSecs: 300, relaxedCsp: false, corsAllowedOrigins: [],
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
      db: f.db, cfg: f.cfg, coordKey: f.coordKey,
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
      db: f.db, cfg: f.cfg, coordKey: f.coordKey, store: state, runtime: targetRuntime,
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
});
