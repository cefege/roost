import { createHash, randomBytes, randomUUID, timingSafeEqual } from "node:crypto";
import type { KyselyDB } from "../db/connection.ts";
import type { CoordConfig } from "@roost/shared/config";
import type { CoordKey } from "../coord-key.ts";
import { COORD_GIT_SHA } from "../git-sha.ts";
import { HandoffStateStore, isTerminalPhase, type HandoffState, type MovePhase } from "./state.ts";
import { CoordinatorWriteGate } from "./write-gate.ts";
import type { CoordinatorMoveRuntime, MoveSnapshot, MoveWorker } from "./runtime.ts";

export type MoveBlockerCode =
  | "move_in_progress" | "public_url_unavailable" | "target_same_as_source"
  | "target_offline" | "target_not_darwin" | "target_address_missing"
  | "target_version_mismatch" | "worker_offline" | "worker_version_mismatch"
  | "target_coord_active" | "target_prepare_failed" | "insufficient_disk";

export interface MoveBlocker { code: MoveBlockerCode; message: string; workerFp?: string }
export interface MovePreflight { eligible: boolean; sourceUrl: string; targetUrl: string; blockers: MoveBlocker[] }
export interface CoordinatorMoveService {
  preflight(targetWorkerFp: string): Promise<MovePreflight>;
  start(targetWorkerFp: string): Promise<string>;
  status(handoffId: string): HandoffState | null;
  current(): HandoffState | null;
  recover(): Promise<void>;
  internalStatus(handoffId: string, secret: string): HandoffState;
  internalCommit(handoffId: string, secret: string): Promise<void>;
  internalAbort(handoffId: string, secret: string): Promise<void>;
  readonly gate: CoordinatorWriteGate;
}

const SHA8 = COORD_GIT_SHA.slice(0, 8);

function blocker(code: MoveBlockerCode, message: string, workerFp?: string): MoveBlocker {
  return workerFp ? { code, message, workerFp } : { code, message };
}

function publicTargetUrl(worker: MoveWorker): string {
  return worker.reachableAddr ? `https://${worker.reachableAddr}:4102` : "";
}

export class CoordinatorMoveOrchestrator implements CoordinatorMoveService {
  readonly gate: CoordinatorWriteGate;
  #run: Promise<void> | null = null;

  constructor(
    private readonly options: {
      db: KyselyDB;
      cfg: CoordConfig;
      coordKey: CoordKey;
      store: HandoffStateStore;
      runtime: CoordinatorMoveRuntime;
      workers: () => Promise<MoveWorker[]>;
      gate?: CoordinatorWriteGate;
    },
  ) {
    this.gate = options.gate ?? new CoordinatorWriteGate();
  }

  async preflight(targetWorkerFp: string): Promise<MovePreflight> {
    const sourceUrl = this.options.cfg.publicUrl ?? "";
    const workers = await this.options.workers();
    const target = workers.find((worker) => worker.fp === targetWorkerFp);
    const targetUrl = target ? publicTargetUrl(target) : "";
    const blockers: MoveBlocker[] = [];
    const previous = this.options.store.load();
    if (previous && !isTerminalPhase(previous.phase)) {
      blockers.push(blocker("move_in_progress", "Another coordinator move is already in progress."));
    }
    if (!sourceUrl) {
      blockers.push(blocker("public_url_unavailable", "Set ROOST_COORDINATOR_PUBLIC_URL on the current coordinator and restart it."));
    }
    if (!target) {
      blockers.push(blocker("target_offline", "Bring the selected machine online before moving the coordinator.", targetWorkerFp));
    } else {
      const sourceHost = sourceUrl ? new URL(sourceUrl).hostname.toLowerCase() : "";
      if (sourceHost && target.reachableAddr?.toLowerCase() === sourceHost) {
        blockers.push(blocker("target_same_as_source", "This machine already hosts the coordinator.", target.fp));
      }
      if (!target.online) blockers.push(blocker("target_offline", `Bring ${target.label} online before moving the coordinator.`, target.fp));
      if (target.os !== "darwin") blockers.push(blocker("target_not_darwin", "Automatic coordinator moves currently require macOS.", target.fp));
      if (!target.reachableAddr) blockers.push(blocker("target_address_missing", `${target.label} has not reported a Tailscale address.`, target.fp));
      if (target.gitSha !== COORD_GIT_SHA) blockers.push(blocker("target_version_mismatch", `Deploy coordinator version ${SHA8} to ${target.label} first.`, target.fp));
      if (target.online && target.os === "darwin" && target.reachableAddr && target.gitSha === COORD_GIT_SHA) {
        const check = await this.options.runtime.checkTarget(target, COORD_GIT_SHA, await this.estimateDbSize());
        if (check) blockers.push(blocker("target_prepare_failed", `${target.label} could not prepare: ${check}`, target.fp));
      }
    }
    for (const worker of workers) {
      if (!worker.online) blockers.push(blocker("worker_offline", `Bring ${worker.label} online or remove it from Machines before moving.`, worker.fp));
      else if (worker.gitSha !== COORD_GIT_SHA) blockers.push(blocker("worker_version_mismatch", `Deploy coordinator version ${SHA8} to ${worker.label} first.`, worker.fp));
    }
    return { eligible: blockers.length === 0, sourceUrl, targetUrl, blockers };
  }

  async start(targetWorkerFp: string): Promise<string> {
    if (this.#run) throw new Error("Another coordinator move is already in progress.");
    const preflight = await this.preflight(targetWorkerFp);
    if (!preflight.eligible) throw new Error(preflight.blockers[0]!.message);
    const existing = this.options.store.load();
    if (existing) this.options.store.archiveTerminal(existing);
    const workers = await this.options.workers();
    const secret = randomBytes(32).toString("base64url");
    const now = Date.now();
    const state = this.options.store.write({
      version: 1,
      handoff_id: randomUUID(),
      role: "SOURCE",
      phase: "PREPARING_TARGET",
      source_url: preflight.sourceUrl,
      target_url: preflight.targetUrl,
      target_worker_fp: targetWorkerFp,
      expected_worker_fps: workers.map((worker) => worker.fp),
      commit_acked_worker_fps: [],
      expected_coord_kid: this.options.coordKey.verifyingKeyKid(),
      expected_git_sha: COORD_GIT_SHA,
      secret_sha256: createHash("sha256").update(secret).digest("hex"),
      secret,
      started_at_ms: now,
      updated_at_ms: now,
    });
    this.#run = this.execute(state).finally(() => { this.#run = null; });
    void this.#run;
    return state.handoff_id;
  }

  status(handoffId: string): HandoffState | null {
    const state = this.options.store.load();
    return state?.handoff_id === handoffId ? state : null;
  }

  current(): HandoffState | null {
    return this.options.store.load();
  }

  internalStatus(handoffId: string, secret: string): HandoffState {
    const state = this.status(handoffId);
    if (!state || !this.secretMatches(state, secret)) throw new Error("handoff not found");
    return state;
  }

  async internalCommit(handoffId: string, secret: string): Promise<void> {
    const state = this.internalStatus(handoffId, secret);
    if (state.role !== "TARGET") throw new Error("handoff target role required");
    if (state.phase === "COMMITTED") return;
    if (state.phase !== "COMMITTING") this.options.store.write({ ...state, phase: "COMMITTING" });
    this.gate.setMode("target_pending");
    if (this.#run) return;
    this.#run = this.commitTargetWorkers(handoffId).finally(() => { this.#run = null; });
    void this.#run;
  }

  async internalAbort(handoffId: string, secret: string): Promise<void> {
    const state = this.internalStatus(handoffId, secret);
    if (state.role !== "TARGET" || state.phase === "COMMITTING" || state.phase === "COMMITTED") {
      throw new Error("handoff can no longer be aborted");
    }
    this.options.store.write({ ...state, phase: "ROLLED_BACK" });
    this.gate.setMode("target_pending");
  }

  async recover(): Promise<void> {
    const state = this.options.store.load();
    if (!state) return;
    if (state.role === "TARGET") {
      this.gate.setMode(state.phase === "COMMITTED" ? "active" : "target_pending");
      return;
    }
    if (isTerminalPhase(state.phase)) {
      this.gate.setMode(state.phase === "COMMITTED" ? "retired" : "active");
      return;
    }
    const targetPhase = await this.options.runtime.targetStatus(this.snapshot(state, state.phase));
    if (targetPhase === "COMMITTED") {
      this.options.store.write({ ...state, phase: "COMMITTED" });
      this.gate.setMode("retired");
      return;
    }
    if (state.phase === "COMMITTING") {
      this.gate.setMode("retired");
      this.#run = this.finishCommit(state).finally(() => { this.#run = null; });
      void this.#run;
      return;
    }
    this.#run = this.rollbackRecovered(state).finally(() => { this.#run = null; });
    void this.#run;
  }

  private async execute(initial: HandoffState): Promise<void> {
    const workers = await this.options.workers();
    const snapshot = (phase: MovePhase): MoveSnapshot => this.snapshot(initial, phase);
    const staged: MoveWorker[] = [];
    try {
      await this.options.runtime.prepareTarget(snapshot("PREPARING_TARGET"));
      this.transition(initial, "STAGING_WORKERS");
      for (const worker of workers) { await this.options.runtime.stageWorker(worker, snapshot("STAGING_WORKERS")); staged.push(worker); }
      this.transition(initial, "DRAINING_SOURCE");
      await this.gate.beginDrain();
      this.transition(initial, "COPYING_STATE");
      await this.options.runtime.copySnapshot(snapshot("COPYING_STATE"));
      this.transition(initial, "WAITING_FOR_WORKERS");
      for (const worker of workers) await this.options.runtime.activateWorker(worker, snapshot("WAITING_FOR_WORKERS"));
      await this.options.runtime.waitForWorkers(snapshot("WAITING_FOR_WORKERS"), 60_000);
      this.transition(initial, "COMMITTING");
      await this.finishCommit(this.options.store.load()!);
    } catch (error) {
      const current = this.options.store.load()!;
      if (current.phase === "COMMITTING") {
        this.options.store.write({ ...current, error: (error as Error).message });
        this.gate.setMode("retired");
        return;
      }
      this.options.store.write({ ...current, phase: "ROLLING_BACK", error: (error as Error).message });
      await Promise.allSettled(staged.map((worker) => this.options.runtime.abortWorker(worker, snapshot("ROLLING_BACK"))));
      await this.options.runtime.abortTarget(snapshot("ROLLING_BACK"));
      this.options.store.write({ ...this.options.store.load()!, phase: "FAILED" });
      this.gate.setMode("active");
    }
  }

  private snapshot(state: HandoffState, phase: MovePhase): MoveSnapshot {
    return {
      handoffId: state.handoff_id, phase, sourceUrl: state.source_url, targetUrl: state.target_url,
      targetWorkerFp: state.target_worker_fp, expectedWorkerFps: state.expected_worker_fps,
      expectedCoordKid: state.expected_coord_kid, expectedGitSha: state.expected_git_sha,
      secret: state.secret!, secretSha256: state.secret_sha256,
    };
  }

  private async finishCommit(state: HandoffState): Promise<void> {
    const snapshot = this.snapshot(state, "COMMITTING");
    for (;;) {
      try {
        await this.options.runtime.commitTarget(snapshot);
        if (await this.options.runtime.targetStatus(snapshot) === "COMMITTED") {
          await this.options.runtime.targetHealthy(snapshot);
          this.options.store.write({ ...this.options.store.load()!, phase: "COMMITTED", error: undefined });
          this.gate.setMode("retired");
          this.options.runtime.publishRelocation(this.snapshot(state, "COMMITTED"));
          return;
        }
      } catch (error) {
        this.options.store.write({ ...this.options.store.load()!, error: (error as Error).message });
      }
      await Bun.sleep(5_000);
    }
  }

  private async rollbackRecovered(state: HandoffState): Promise<void> {
    const snapshot = this.snapshot(state, "ROLLING_BACK");
    this.options.store.write({ ...state, phase: "ROLLING_BACK" });
    try {
      await this.options.runtime.abortTarget(snapshot);
      this.options.store.write({ ...this.options.store.load()!, phase: "ROLLED_BACK" });
    } catch (error) {
      this.options.store.write({ ...this.options.store.load()!, phase: "FAILED", error: (error as Error).message });
    }
    this.gate.setMode("active");
  }

  private transition(initial: HandoffState, phase: MovePhase): void {
    const current = this.options.store.load();
    if (!current || current.handoff_id !== initial.handoff_id) throw new Error("coordinator handoff state disappeared");
    this.options.store.write({ ...current, phase });
  }

  private secretMatches(state: HandoffState, secret: string): boolean {
    const expected = Buffer.from(state.secret_sha256, "hex");
    const actual = createHash("sha256").update(secret).digest();
    return expected.length === actual.length && timingSafeEqual(expected, actual);
  }

  private async commitTargetWorkers(handoffId: string): Promise<void> {
    const state = this.status(handoffId);
    if (!state || state.role !== "TARGET") return;
    const workers = await this.options.workers();
    for (const workerFp of state.expected_worker_fps) {
      if (state.commit_acked_worker_fps.includes(workerFp)) continue;
      const worker = workers.find((candidate) => candidate.fp === workerFp);
      if (!worker?.online) throw new Error(`worker ${workerFp} is not connected to target`);
      await this.options.runtime.commitWorker(worker, this.snapshot({ ...state, secret: "target" }, "COMMITTING"));
      const current = this.status(handoffId);
      if (!current) throw new Error("target handoff disappeared");
      this.options.store.write({ ...current, commit_acked_worker_fps: [...current.commit_acked_worker_fps, workerFp] });
    }
    const committed = this.status(handoffId);
    if (committed && committed.commit_acked_worker_fps.length === committed.expected_worker_fps.length) {
      this.options.store.write({ ...committed, phase: "COMMITTED" });
      this.gate.setMode("active");
    }
  }

  private async estimateDbSize(): Promise<number> {
    const row = await this.options.db.selectFrom("events").select((expressionBuilder) => expressionBuilder.fn.countAll<number>().as("count")).executeTakeFirst();
    return Math.max(1, Number(row?.count ?? 0)) * 1024;
  }
}
