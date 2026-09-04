// The TARGET half of a coordinator move, plus the plumbing both halves share
// (handoff lookup, snapshot projection, background-run bookkeeping).
//
// Split out of orchestrator.ts purely to keep either file under the line cap.
// It is a base class rather than a collaborator on purpose: `run` is a single
// mutex that `start()` (source) and `internalCommit()` (target) both read, and
// the auto-commit/retry timers are cleared from `internalAbort`. One instance
// owning one set of fields is the behaviour that was there before the split.

import { createHash, timingSafeEqual } from "node:crypto";
import { log } from "@roost/shared/log";
import type { CoordConfig } from "@roost/shared/config";
import type { CoordKey } from "../coord-key.ts";
import { HandoffStateStore, isTerminalPhase, type HandoffState, type MovePhase } from "./state.ts";
import { CoordinatorWriteGate } from "./write-gate.ts";
import type { CoordinatorMoveRuntime, MoveSnapshot, MoveWorker } from "./runtime.ts";

export interface CoordinatorMoveOptions {
  cfg: CoordConfig;
  coordKey: CoordKey;
  store: HandoffStateStore;
  runtime: CoordinatorMoveRuntime;
  workers: (dashboardId: string) => Promise<MoveWorker[]>;
  gate?: CoordinatorWriteGate;
}

export abstract class CoordinatorMoveTargetRole {
  readonly gate: CoordinatorWriteGate;
  /** The single in-flight background run, source-side or target-side. */
  protected run: Promise<void> | null = null;
  protected targetAutoCommitTimer: ReturnType<typeof setTimeout> | null = null;
  protected commitRetryTimer: ReturnType<typeof setTimeout> | null = null;
  protected targetCompleteSetSinceMs: number | null = null;

  constructor(protected readonly options: CoordinatorMoveOptions) {
    this.gate = options.gate ?? new CoordinatorWriteGate();
  }

  protected handoff(handoffId: string): HandoffState | null {
    const state = this.options.store.load();
    return state?.handoff_id === handoffId ? state : null;
  }

  status(dashboardId: string, handoffId: string): HandoffState | null {
    const state = this.handoff(handoffId);
    return state?.dashboard_id === dashboardId ? state : null;
  }

  async statusForWorker(handoffId: string, workerFp: string): Promise<HandoffState | null> {
    const state = this.handoff(handoffId);
    if (!state || !state.expected_worker_fps.includes(workerFp)) return null;
    const workers = await this.options.workers(state.dashboard_id);
    return workers.some((worker) => worker.fp === workerFp) ? state : null;
  }

  current(): HandoffState | null {
    return this.options.store.load();
  }

  /**
   * Persisting a terminal phase, changing the write gate, and announcing a
   * retired source are one lifecycle commit. The store callback runs after
   * durable replacement but before completion observers can resume.
   */
  protected writeTerminalState(state: HandoffState): Promise<HandoffState> {
    if (!isTerminalPhase(state.phase)) throw new Error(`coordinator move phase ${state.phase} is not terminal`);
    const retiresSource = state.role === "SOURCE" && state.phase === "COMMITTED";
    log.info("coord-move", "move_terminal", { handoff_id: state.handoff_id, role: state.role, phase: state.phase });
    return this.options.store.writeDurable(state, (persisted) => {
      this.gate.setMode(retiresSource ? "retired" : "active");
      if (retiresSource) this.options.runtime.publishRelocation(this.snapshot(persisted, "COMMITTED"));
    });
  }

  async internalStatus(handoffId: string, secret: string): Promise<HandoffState & { connected_worker_fps: string[] }> {
    const state = this.handoff(handoffId);
    if (!state || !this.secretMatches(state, secret)) throw new Error("handoff not found");
    const workers = await this.options.workers(state.dashboard_id);
    // Never echo the plaintext handoff secret back over the wire.
    const { secret: _secret, ...safe } = state;
    return { ...safe, connected_worker_fps: workers.filter((worker) => worker.online).map((worker) => worker.fp) };
  }

  async internalCommit(handoffId: string, secret: string): Promise<void> {
    const { connected_worker_fps: _connectedWorkerFps, ...state } = await this.internalStatus(handoffId, secret);
    if (state.role !== "TARGET") throw new Error("handoff target role required");
    if (state.phase === "COMMITTED") return;
    if (state.phase !== "COMMITTING") await this.options.store.writeDurable({ ...state, phase: "COMMITTING" });
    this.gate.setMode("active");
    if (this.run) return;
    this.runCommitTargetWorkers(handoffId);
  }

  async internalAbort(handoffId: string, secret: string): Promise<void> {
    const { connected_worker_fps: _connectedWorkerFps, ...state } = await this.internalStatus(handoffId, secret);
    if (state.role !== "TARGET" || state.phase === "COMMITTING" || state.phase === "COMMITTED") {
      throw new Error("handoff can no longer be aborted");
    }
    // The auto-commit timer reads phase, then awaits workers(); without this it
    // can write COMMITTING over the ROLLING_BACK below and re-open the gate.
    this.clearTargetAutoCommit();
    // Record the rollback before asking the target's own worker to uninstall
    // this pending coordinator. Once it does, this process can disappear.
    await this.options.store.writeDurable({ ...state, phase: "ROLLING_BACK" });
    this.gate.setMode("target_pending");
    // runTargetAbort always lands terminal — ROLLED_BACK, or FAILED plus a
    // rethrow. Either way the gate is ours to restore, and `active` is exactly
    // what recover() picks for a terminal non-COMMITTED target. Without this
    // the abort path itself is what wedges the coordinator.
    try {
      await this.runTargetAbort(state);
    } finally {
      this.gate.setMode("active");
    }
  }

  /** The TARGET branch of recover(); the SOURCE branch stays in the subclass. */
  protected async recoverTarget(state: HandoffState, terminal: boolean): Promise<void> {
    // A TARGET that already reached ROLLED_BACK/FAILED owns nothing; without
    // this it boots into target_pending forever and rejects every write.
    if (terminal && state.phase !== "COMMITTED") {
      this.gate.setMode("active");
      return;
    }
    if (state.phase === "COMMITTED") {
      this.gate.setMode("active");
      void this.replayCommittedWorkers(state).catch((error) => this.recordRunError(state.handoff_id, error));
    } else if (state.phase === "COMMITTING") {
      this.gate.setMode("active");
      this.runCommitTargetWorkers(state.handoff_id);
    } else if (state.phase === "ROLLING_BACK") {
      // internalAbort rethrew mid-rollback and never wrote a terminal phase.
      this.gate.setMode("target_pending");
      void this.retryTargetAbort(state);
    } else {
      this.gate.setMode("target_pending");
      if (state.phase === "WAITING_FOR_WORKERS") this.scheduleTargetAutoCommit(state);
    }
  }

  /** ROLLING_BACK is not terminal: without a terminal write here a restart
   *  re-enters target_pending forever and rejects every write. */
  private async runTargetAbort(state: HandoffState): Promise<void> {
    const workers = await this.options.workers(state.dashboard_id);
    const staged = workers.filter((worker) => state.expected_worker_fps.includes(worker.fp) && worker.online);
    const results = await Promise.allSettled(staged.map((worker) =>
      this.options.runtime.abortWorker(worker, this.snapshot({ ...state, secret: "target" }, "ROLLING_BACK")),
    ));
    const failed = results.find((result) => result.status === "rejected");
    const latest = this.options.store.load() ?? state;
    if (failed?.status === "rejected") {
      await this.writeTerminalState({ ...latest, phase: "FAILED", error: String(failed.reason) });
      throw failed.reason;
    }
    await this.writeTerminalState({ ...latest, phase: "ROLLED_BACK" });
  }

  private async retryTargetAbort(state: HandoffState): Promise<void> {
    await this.runTargetAbort(state).catch((error) => this.recordRunError(state.handoff_id, error));
  }

  protected snapshot(state: HandoffState, phase: MovePhase): MoveSnapshot {
    return {
      handoffId: state.handoff_id, phase, sourceUrl: state.source_url, targetUrl: state.target_url,
      targetWorkerFp: state.target_worker_fp, dashboardId: state.dashboard_id, expectedWorkerFps: state.expected_worker_fps,
      expectedCoordKid: state.expected_coord_kid, expectedGitSha: state.expected_git_sha,
      secret: state.secret!, secretSha256: state.secret_sha256,
    };
  }

  /** Surface a background-run failure on the handoff state so the UI can show
   *  it. `void promise` attaches no rejection handler and coord installs no
   *  unhandledRejection hook. */
  protected async recordRunError(handoffId: string, error: unknown): Promise<void> {
    const current = this.handoff(handoffId);
    if (current) await this.options.store.writeDurable({ ...current, error: String((error as Error)?.message ?? error) });
    log.error("coord-move", "run_failed", { handoff_id: handoffId, error: String(error) });
  }

  protected secretMatches(state: HandoffState, secret: string): boolean {
    const expected = Buffer.from(state.secret_sha256, "hex");
    const actual = createHash("sha256").update(secret).digest();
    return expected.length === actual.length && timingSafeEqual(expected, actual);
  }

  private runCommitTargetWorkers(handoffId: string): void {
    this.run = this.commitTargetWorkers(handoffId)
      .catch((error) => this.scheduleCommitRetry(handoffId, error))
      .finally(() => { this.run = null; });
  }

  protected clearTargetAutoCommit(): void {
    clearTimeout(this.targetAutoCommitTimer ?? undefined);
    this.targetAutoCommitTimer = null;
    clearTimeout(this.commitRetryTimer ?? undefined);
    this.commitRetryTimer = null;
  }

  private scheduleTargetAutoCommit(state: HandoffState): void {
    if (this.targetAutoCommitTimer) return;
    const attempt = async (): Promise<void> => {
      this.targetAutoCommitTimer = null;
      const current = this.handoff(state.handoff_id);
      if (!current || current.role !== "TARGET" || current.phase !== "WAITING_FOR_WORKERS") return;
      const workers = await this.options.workers(current.dashboard_id);
      // internalAbort can have written ROLLING_BACK during that await; writing
      // COMMITTING over it would re-open the gate on a dead handoff.
      const fresh = this.handoff(state.handoff_id);
      if (!fresh || fresh.role !== "TARGET" || fresh.phase !== "WAITING_FOR_WORKERS") return;
      if (!fresh.expected_worker_fps.every((fp) => workers.some((worker) => worker.fp === fp && worker.online))) {
        this.targetCompleteSetSinceMs = null;
        this.targetAutoCommitTimer = setTimeout(() => void attempt(), 5_000);
        return;
      }
      this.targetCompleteSetSinceMs ??= Date.now();
      const remainingMs = this.targetCompleteSetSinceMs + 60_000 - Date.now();
      if (remainingMs > 0) {
        this.targetAutoCommitTimer = setTimeout(() => void attempt(), remainingMs);
        return;
      }
      await this.options.store.writeDurable({ ...fresh, phase: "COMMITTING" });
      this.gate.setMode("active");
      this.runCommitTargetWorkers(fresh.handoff_id);
    };
    this.targetAutoCommitTimer = setTimeout(() => void attempt(), 0);
  }

  /** A target restarted at COMMITTING must keep trying: until every expected
   *  worker acks, commit_acked_worker_fps stays partial and
   *  authRedeemCoordinatorRelocation rejects every relocated browser. */
  private async scheduleCommitRetry(handoffId: string, error: unknown): Promise<void> {
    await this.recordRunError(handoffId, error);
    const state = this.handoff(handoffId);
    if (!state || state.role !== "TARGET" || state.phase !== "COMMITTING") return;
    this.commitRetryTimer = setTimeout(() => {
      this.commitRetryTimer = null;
      const current = this.handoff(handoffId);
      if (!current || current.role !== "TARGET" || current.phase !== "COMMITTING") return;
      this.run = this.commitTargetWorkers(handoffId)
        .catch((cause) => this.scheduleCommitRetry(handoffId, cause))
        .finally(() => { this.run = null; });
    }, 5_000);
  }

  private async commitTargetWorkers(handoffId: string): Promise<void> {
    const state = this.handoff(handoffId);
    if (!state || state.role !== "TARGET") return;
    const workers = await this.options.workers(state.dashboard_id);
    const pending = state.expected_worker_fps
      .filter((workerFp) => !state.commit_acked_worker_fps.includes(workerFp))
      .map((workerFp) => workers.find((worker) => worker.fp === workerFp))
      .filter((worker): worker is MoveWorker => worker !== undefined);
    if (pending.length !== state.expected_worker_fps.length - state.commit_acked_worker_fps.length ||
      pending.some((worker) => !worker.online)) {
      throw new Error("expected workers are not connected to target");
    }
    // target_pending withholds event acknowledgements. Closing these sockets
    // makes each preserved unacked event replay against the now-active target.
    await this.options.runtime.reconnectWorkers(state.dashboard_id, pending, 30_000);
    const reconnected = await this.options.workers(state.dashboard_id);
    for (const workerFp of state.expected_worker_fps) {
      if (state.commit_acked_worker_fps.includes(workerFp)) continue;
      const worker = reconnected.find((candidate) => candidate.fp === workerFp);
      if (!worker?.online) throw new Error(`worker ${workerFp} is not connected to target`);
      await this.options.runtime.commitWorker(worker, this.snapshot({ ...state, secret: "target" }, "COMMITTING"));
      const current = this.handoff(handoffId);
      if (!current) throw new Error("target handoff disappeared");
      await this.options.store.writeDurable({ ...current, commit_acked_worker_fps: [...current.commit_acked_worker_fps, workerFp] });
    }
    const committed = this.handoff(handoffId);
    if (committed && committed.commit_acked_worker_fps.length === committed.expected_worker_fps.length) {
      await this.writeTerminalState({ ...committed, phase: "COMMITTED" });
      await this.replayCommittedWorkers(this.handoff(handoffId)!);
    }
  }

  private async replayCommittedWorkers(state: HandoffState): Promise<void> {
    const workers = await this.options.workers(state.dashboard_id);
    await Promise.allSettled(state.expected_worker_fps.map(async (workerFp) => {
      const worker = workers.find((candidate) => candidate.fp === workerFp);
      if (worker?.online) await this.options.runtime.commitWorker(worker, this.snapshot({ ...state, secret: "target" }, "COMMITTED"));
    }));
  }
}
