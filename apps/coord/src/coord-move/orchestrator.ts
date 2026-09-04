// Source coordinator moves are sequenced here so one owner controls phase, gate, and recovery order.
// RPC handlers call this orchestrator; target-only plumbing remains in target-orchestrator.ts.
// It depends on durable handoff state, the move runtime, and source preflight checks.
// Phase transitions, write-gate changes, rollback, and publication must keep their established ordering.

import { createHash, randomBytes, randomUUID } from "node:crypto";
import { log } from "@roost/shared/log";
import { COORD_GIT_SHA } from "../git-sha.ts";
import { isTerminalPhase, type CoordinatorMoveTransaction, type HandoffState, type MovePhase } from "./state.ts";
import type { CoordinatorWriteGate } from "./write-gate.ts";
import type { MoveWorker } from "./runtime.ts";
import { preflightSourceMove, type MoveBlocker, type MoveBlockerCode, type MovePreflight } from "./source-preflight.ts";
import { CoordinatorMoveTargetRole } from "./target-orchestrator.ts";

export type { MoveBlocker, MoveBlockerCode, MovePreflight };

export interface CoordinatorMoveService {
  preflight(dashboardId: string, targetWorkerFp: string): Promise<MovePreflight>;
  start(dashboardId: string, targetWorkerFp: string): Promise<string>;
  status(dashboardId: string, handoffId: string): HandoffState | null;
  statusForWorker(handoffId: string, workerFp: string): Promise<HandoffState | null>;
  current(): HandoffState | null;
  recover(): Promise<void>;
  internalStatus(handoffId: string, secret: string): Promise<HandoffState & { connected_worker_fps: string[] }>;
  internalCommit(handoffId: string, secret: string): Promise<void>;
  internalAbort(handoffId: string, secret: string): Promise<void>;
  readonly gate: CoordinatorWriteGate;
}

/** Long enough for a large snapshot plus a slow target install; short enough
 *  that a wedged cluster self-heals. Never unbounded — the write gate is held. */
const FINISH_COMMIT_TIMEOUT_MS = 600_000;

/** Phases where the target may already have self-committed under us. */
const ROLLBACK_RACE_PHASES = new Set<MovePhase>(["DRAINING_SOURCE", "COPYING_STATE", "WAITING_FOR_WORKERS", "COMMITTING"]);

/** The SOURCE half. The TARGET half and the plumbing both roles share live in
 *  the base class — see target-orchestrator.ts for why it is a base class and
 *  not a collaborator. */
export class CoordinatorMoveOrchestrator extends CoordinatorMoveTargetRole implements CoordinatorMoveService {
  #starting = false;

  async preflight(dashboardId: string, targetWorkerFp: string): Promise<MovePreflight> {
    const transaction = await this.options.store.acquireTransaction();
    try {
      return await preflightSourceMove(this.options, dashboardId, targetWorkerFp);
    } finally {
      await transaction.release();
    }
  }

  async start(dashboardId: string, targetWorkerFp: string): Promise<string> {
    if (this.run || this.#starting) throw new Error("Another coordinator move is already in progress.");
    this.#starting = true;
    let transaction: CoordinatorMoveTransaction | null = null;
    let transactionOwnedByRun = false;
    try {
      transaction = await this.options.store.acquireTransaction();
      const preflight = await preflightSourceMove(this.options, dashboardId, targetWorkerFp);
      if (!preflight.eligible) throw new Error(preflight.blockers[0]!.message);
      const existing = this.options.store.load();
      if (existing) await this.options.store.archiveTerminalDurable(existing);
      const workers = await this.options.workers(dashboardId);
      const secret = randomBytes(32).toString("base64url");
      const now = Date.now();
      const state = await this.options.store.writeDurable({
        version: 1,
        handoff_id: randomUUID(),
        role: "SOURCE",
        dashboard_id: dashboardId,
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
      transactionOwnedByRun = true;
      this.run = this.execute(state)
        .catch((error) => this.recordRunError(state.handoff_id, error))
        .finally(async () => {
          this.run = null;
          await transaction!.release();
        });
      return state.handoff_id;
    } finally {
      this.#starting = false;
      if (transaction && !transactionOwnedByRun) await transaction.release();
    }
  }

  async recover(): Promise<void> {
    const state = this.options.store.load();
    if (!state) return;
    if (state.role === "TARGET") {
      await this.recoverTarget(state, isTerminalPhase(state.phase));
      return;
    }
    if (isTerminalPhase(state.phase)) {
      if (state.phase === "COMMITTED") {
        this.gate.setMode("retired");
        this.options.runtime.publishRelocation(this.snapshot(state, "COMMITTED"));
      } else {
        this.gate.setMode("active");
      }
      return;
    }
    const transaction = await this.options.store.acquireTransaction();
    let transactionOwnedByRun = false;
    try {
      await this.#awaitExpectedWorkers(state.dashboard_id, state.expected_worker_fps);
      if (state.phase === "ROLLING_BACK") {
        await this.rollbackRecovered(state);
        return;
      }
      const targetPhase = await this.options.runtime.targetStatus(this.snapshot(state, state.phase));
      if (targetPhase === "ROLLED_BACK" || targetPhase === "FAILED") {
        await this.rollbackRecovered(state);
        return;
      }
      if (targetPhase === "COMMITTED") {
        await this.#retire(state, "COMMITTED");
        return;
      }
      if (targetPhase === "COMMITTING" || state.phase === "COMMITTING") {
        this.gate.setMode("retired");
        transactionOwnedByRun = true;
        this.run = this.finishCommit(state)
          .catch((error) => this.recordRunError(state.handoff_id, error))
          .finally(async () => {
            this.run = null;
            await transaction.release();
          });
        return;
      }
      if (targetPhase === "WAITING_FOR_WORKERS" && state.phase !== "WAITING_FOR_WORKERS") {
        await this.options.store.writeDurable({ ...state, phase: "WAITING_FOR_WORKERS" });
      }
      const resumed = this.options.store.load();
      if (!resumed) return;
      transactionOwnedByRun = true;
      this.run = this.execute(resumed)
        .catch((error) => this.recordRunError(resumed.handoff_id, error))
        .finally(async () => {
          this.run = null;
          await transaction.release();
        });
    } finally {
      if (!transactionOwnedByRun) await transaction.release();
    }
  }
  private async execute(initial: HandoffState): Promise<void> {
    const staged: MoveWorker[] = [];
    try {
      await this.advance(initial, staged);
    } catch (error) {
      const current = this.options.store.load() ?? initial;
      try {
        if (current.phase === "COMMITTING") {
          await this.options.store.writeDurable({ ...current, error: (error as Error).message });
          this.gate.setMode("retired");
          // COMMITTING is not terminal: returning leaves the source retired
          // with no in-process retry; finishCommit is bounded and always ends
          // terminal (#retire, or FAILED + gate active).
          await this.finishCommit({ ...current, secret: initial.secret });
          return;
        }
        // The target self-commits 60s after the expected workers connect and
        // our own waitForWorkers deadline is the same 60s, so losing that race
        // is routine. Rolling back then would uninstall a live coordinator.
        if (ROLLBACK_RACE_PHASES.has(current.phase)) {
          const targetPhase = await this.options.runtime
            .targetStatus(this.snapshot({ ...current, secret: initial.secret }, current.phase)).catch(() => null);
          if (targetPhase === "COMMITTING" || targetPhase === "COMMITTED") {
            await this.#retire({ ...current, secret: initial.secret }, "COMMITTED");
            return;
          }
        }
        const rollbackReason = (error as Error).message;
        await this.options.store.writeDurable({ ...current, phase: "ROLLING_BACK", error: rollbackReason });
        log.info("coord-move", "rollback_started", { handoff_id: current.handoff_id, reason: rollbackReason });
        // A crash after durable staging but before `staged` was populated is
        // recovered by including every expected source-connected worker:
        // ABORT is idempotent.
        const connected = await this.options.workers(current.dashboard_id).catch(() => []);
        const rollbackWorkers = new Map(staged.map((worker) => [worker.fp, worker]));
        for (const worker of connected) {
          if (current.expected_worker_fps.includes(worker.fp)) rollbackWorkers.set(worker.fp, worker);
        }
        await Promise.allSettled([...rollbackWorkers.values()].map((worker) =>
          this.options.runtime.abortWorker(worker, this.snapshot({ ...current, secret: initial.secret }, "ROLLING_BACK")),
        ));
        let rollbackError: unknown;
        try {
          await this.options.runtime.abortTarget(this.snapshot({ ...current, secret: initial.secret }, "ROLLING_BACK"));
        } catch (cause) {
          rollbackError = cause;
        }
        const latest = this.options.store.load() ?? current;
        await this.writeTerminalState({
          ...latest,
          phase: "FAILED",
          error: rollbackError
            ? `${(error as Error).message}; rollback failed: ${String(rollbackError)}`
            : (error as Error).message,
        });
      } finally {
        // Without this a throw inside the rollback (a vanished handoff file is
        // exactly the case that made `advance` throw) leaves every write RPC
        // returning Unavailable until coord restarts.
        if (this.gate.mode !== "retired") this.gate.setMode("active");
      }
    }
  }

  private async advance(initial: HandoffState, staged: MoveWorker[]): Promise<void> {
    let state = this.options.store.load();
    if (!state || state.handoff_id !== initial.handoff_id) throw new Error("coordinator handoff state disappeared");
    // Stage and activate EXACTLY the set snapshotted at start(). A worker that
    // paired since would be staged here yet skipped by the target's
    // commitTargetWorkers and by the rollback filter, both of which iterate
    // expected_worker_fps.
    const workers = await this.#expectedWorkers(state.dashboard_id, state.expected_worker_fps);

    if (state.phase === "PREPARING_TARGET") {
      await this.options.runtime.prepareTarget(this.snapshot(state, "PREPARING_TARGET"));
      await this.transition(initial, "STAGING_WORKERS");
      state = this.options.store.load()!;
    }
    if (state.phase === "STAGING_WORKERS") {
      for (const worker of workers) {
        await this.options.runtime.stageWorker(worker, this.snapshot(state, "STAGING_WORKERS"));
        staged.push(worker);
      }
      await this.transition(initial, "DRAINING_SOURCE");
      state = this.options.store.load()!;
    }
    if (state.phase === "DRAINING_SOURCE") {
      await this.gate.beginDrain();
      await this.transition(initial, "COPYING_STATE");
      state = this.options.store.load()!;
    }
    if (state.phase === "COPYING_STATE") {
      await this.gate.beginDrain();
      await this.options.runtime.copySnapshot(this.snapshot(state, "COPYING_STATE"));
      await this.transition(initial, "WAITING_FOR_WORKERS");
      state = this.options.store.load()!;
    }
    if (state.phase === "WAITING_FOR_WORKERS") {
      await this.gate.beginDrain();
      for (const worker of workers) await this.options.runtime.activateWorker(worker, this.snapshot(state, "WAITING_FOR_WORKERS"));
      await this.options.runtime.waitForWorkers(this.snapshot(state, "WAITING_FOR_WORKERS"), 60_000);
      await this.transition(initial, "COMMITTING");
      state = this.options.store.load()!;
    }
    if (state.phase === "COMMITTING") await this.finishCommit(state);
  }

  /** Bounded: never leave this loop with the gate still `source_draining`. */
  private async finishCommit(state: HandoffState, deadlineMs = FINISH_COMMIT_TIMEOUT_MS): Promise<void> {
    const snapshot = this.snapshot(state, "COMMITTING");
    const deadline = Date.now() + deadlineMs;
    let lastError: string | undefined;
    for (;;) {
      try {
        await this.options.runtime.commitTarget(snapshot);
        if (await this.options.runtime.targetStatus(snapshot) === "COMMITTED") {
          await this.options.runtime.targetHealthy(snapshot);
          await this.#retire(state, "COMMITTED");
          return;
        }
      } catch (error) {
        lastError = (error as Error).message;
        await this.options.store.writeDurable({ ...(this.options.store.load() ?? state), error: lastError });
      }
      if (Date.now() >= deadline) {
        const phase = await this.options.runtime.targetStatus(snapshot).catch(() => null);
        if (phase === "COMMITTING" || phase === "COMMITTED") {
          // The target owns the cluster even though this side never got a
          // clean confirmation — retiring is the only correct end state.
          await this.#retire(state, "COMMITTED");
          return;
        }
        await this.writeTerminalState({
          ...(this.options.store.load() ?? state),
          phase: "FAILED",
          error: lastError ?? "coordinator move timed out waiting for the target to commit",
        });
        return;
      }
      await Bun.sleep(5_000);
    }
  }

  /** The single post-commit exit: durable COMMITTED, gate retired, browsers told. */
  async #retire(state: HandoffState, phase: "COMMITTED"): Promise<void> {
    await this.writeTerminalState({ ...(this.options.store.load() ?? state), phase, error: undefined });
  }

  /** Staged set = exactly the start() snapshot, resolved against the live
   *  registry; a missing fp is a hard error, not a silent skip. */
  async #expectedWorkers(dashboardId: string, fps: string[]): Promise<MoveWorker[]> {
    const live = await this.options.workers(dashboardId);
    const byFp = new Map(live.map((worker) => [worker.fp, worker]));
    const missing = fps.filter((fp) => !byFp.has(fp));
    if (missing.length > 0) throw new Error(`expected workers are no longer known to the coordinator: ${missing.join(", ")}`);
    return fps.map((fp) => byFp.get(fp)!);
  }

  /** Recovery runs before workers re-attach: an empty registry means a blind
   *  rollback, so wait for the expected set. Never throws on timeout — the
   *  rollback path handles it. */
  async #awaitExpectedWorkers(dashboardId: string, fps: string[], timeoutMs = 60_000): Promise<void> {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const online = new Set((await this.options.workers(dashboardId)).filter((worker) => worker.online).map((worker) => worker.fp));
      if (fps.every((fp) => online.has(fp))) return;
      if (Date.now() >= deadline) return;
      await Bun.sleep(500);
    }
  }

  private async rollbackRecovered(state: HandoffState): Promise<void> {
    const snapshot = this.snapshot(state, "ROLLING_BACK");
    await this.options.store.writeDurable({ ...state, phase: "ROLLING_BACK" });
    log.info("coord-move", "rollback_started", { handoff_id: state.handoff_id, reason: `recovered in ${snapshot.phase}` });
    const workers = await this.options.workers(state.dashboard_id);
    await Promise.allSettled(workers
      .filter((worker) => state.expected_worker_fps.includes(worker.fp))
      .map((worker) => this.options.runtime.abortWorker(worker, snapshot)));
    try {
      await this.options.runtime.abortTarget(snapshot);
      await this.writeTerminalState({ ...(this.options.store.load() ?? state), phase: "ROLLED_BACK" });
    } catch (error) {
      await this.writeTerminalState({ ...(this.options.store.load() ?? state), phase: "FAILED", error: (error as Error).message });
    }
  }

  private async transition(initial: HandoffState, phase: MovePhase): Promise<void> {
    const current = this.options.store.load();
    if (!current || current.handoff_id !== initial.handoff_id) throw new Error("coordinator handoff state disappeared");
    await this.options.store.writeDurable({ ...current, phase });
    log.info("coord-move", "phase_transition", { handoff_id: current.handoff_id, from: current.phase, to: phase });
  }
}
