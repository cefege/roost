import { createHash, randomBytes, randomUUID } from "node:crypto";
import * as fs from "node:fs";
import { COORD_GIT_SHA } from "../git-sha.ts";
import { isTerminalPhase, type HandoffState, type MovePhase } from "./state.ts";
import type { CoordinatorWriteGate } from "./write-gate.ts";
import type { MoveWorker } from "./runtime.ts";
import { CoordinatorMoveTargetRole } from "./target-orchestrator.ts";

export type MoveBlockerCode =
  | "move_in_progress" | "public_url_unavailable" | "target_same_as_source"
  | "target_offline" | "target_address_missing"
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
  internalStatus(handoffId: string, secret: string): Promise<HandoffState & { connected_worker_fps: string[] }>;
  internalCommit(handoffId: string, secret: string): Promise<void>;
  internalAbort(handoffId: string, secret: string): Promise<void>;
  readonly gate: CoordinatorWriteGate;
}

const SHA8 = COORD_GIT_SHA.slice(0, 8);

/** Long enough for a large snapshot plus a slow target install; short enough
 *  that a wedged cluster self-heals. Never unbounded — the write gate is held. */
const FINISH_COMMIT_TIMEOUT_MS = 600_000;

/** Phases where the target may already have self-committed under us. */
const ROLLBACK_RACE_PHASES = new Set<MovePhase>([
  "DRAINING_SOURCE", "COPYING_STATE", "WAITING_FOR_WORKERS", "COMMITTING",
]);

function blocker(code: MoveBlockerCode, message: string, workerFp?: string): MoveBlocker {
  return workerFp ? { code, message, workerFp } : { code, message };
}

function publicTargetUrl(worker: MoveWorker): string {
  return worker.reachableAddr ? `https://${worker.reachableAddr}:4102` : "";
}

function targetProbeBlocker(target: MoveWorker, detail: string): MoveBlocker {
  if (detail.includes("target already has an active coordinator")) {
    return blocker("target_coord_active", `${target.label} already runs a different active coordinator.`, target.fp);
  }
  if (detail.includes("target URL does not match this worker's Tailscale address")) {
    return blocker(
      "target_address_missing",
      `${target.label} does not recognise ${publicTargetUrl(target)} as its own Tailscale address.`,
      target.fp,
    );
  }
  const disk = /insufficient disk: required (\d+), available (\d+)/.exec(detail);
  if (disk) {
    return blocker(
      "insufficient_disk",
      `${target.label} needs at least ${disk[1]} bytes free; ${disk[2]} bytes are available.`,
      target.fp,
    );
  }
  return blocker("target_prepare_failed", `${target.label} could not prepare: ${detail}`, target.fp);
}

/** The SOURCE half. The TARGET half and the plumbing both roles share live in
 *  the base class — see target-orchestrator.ts for why it is a base class and
 *  not a collaborator. */
export class CoordinatorMoveOrchestrator extends CoordinatorMoveTargetRole implements CoordinatorMoveService {
  #starting = false;

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
      if (!target.reachableAddr) blockers.push(blocker("target_address_missing", `${target.label} has not reported a Tailscale address.`, target.fp));
      if (target.gitSha !== COORD_GIT_SHA) blockers.push(blocker("target_version_mismatch", `Deploy coordinator version ${SHA8} to ${target.label} first.`, target.fp));
      if (target.online && target.reachableAddr && target.gitSha === COORD_GIT_SHA) {
        const check = await this.options.runtime.checkTarget(target, COORD_GIT_SHA, await this.estimateDbSize());
        if (check) blockers.push(targetProbeBlocker(target, check));
      }
    }
    for (const worker of workers) {
      // The target's own offline/version state is already reported above with
      // a target-specific message; a second near-identical line just confuses.
      if (worker.fp === targetWorkerFp) continue;
      if (!worker.online) blockers.push(blocker("worker_offline", `Bring ${worker.label} online or remove it from Machines before moving.`, worker.fp));
      else if (worker.gitSha !== COORD_GIT_SHA) blockers.push(blocker("worker_version_mismatch", `Deploy coordinator version ${SHA8} to ${worker.label} first.`, worker.fp));
    }
    return { eligible: blockers.length === 0, sourceUrl, targetUrl, blockers };
  }

  async start(targetWorkerFp: string): Promise<string> {
    if (this.run || this.#starting) throw new Error("Another coordinator move is already in progress.");
    this.#starting = true;
    try {
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
      this.run = this.execute(state)
        .catch((error) => this.recordRunError(state.handoff_id, error))
        .finally(() => { this.run = null; });
      return state.handoff_id;
    } finally {
      this.#starting = false;
    }
  }

  async recover(): Promise<void> {
    const state = this.options.store.load();
    if (!state) return;
    if (state.role === "TARGET") {
      this.recoverTarget(state, isTerminalPhase(state.phase));
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
    await this.#awaitExpectedWorkers(state.expected_worker_fps);
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
      this.#retire(state, "COMMITTED");
      return;
    }
    if (targetPhase === "COMMITTING" || state.phase === "COMMITTING") {
      this.gate.setMode("retired");
      this.run = this.finishCommit(state)
        .catch((error) => this.recordRunError(state.handoff_id, error))
        .finally(() => { this.run = null; });
      return;
    }
    if (targetPhase === "WAITING_FOR_WORKERS" && state.phase !== "WAITING_FOR_WORKERS") {
      this.options.store.write({ ...state, phase: "WAITING_FOR_WORKERS" });
    }
    const resumed = this.options.store.load();
    if (!resumed) return;
    this.run = this.execute(resumed)
      .catch((error) => this.recordRunError(resumed.handoff_id, error))
      .finally(() => { this.run = null; });
  }
  private async execute(initial: HandoffState): Promise<void> {
    const staged: MoveWorker[] = [];
    try {
      await this.advance(initial, staged);
    } catch (error) {
      const current = this.options.store.load() ?? initial;
      try {
        if (current.phase === "COMMITTING") {
          this.options.store.write({ ...current, error: (error as Error).message });
          this.gate.setMode("retired");
          // COMMITTING is not terminal: returning here leaves the source
          // retired with no in-process retry. finishCommit is bounded and
          // always ends terminal (#retire, or FAILED + gate active).
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
            this.#retire({ ...current, secret: initial.secret }, "COMMITTED");
            return;
          }
        }
        this.options.store.write({ ...current, phase: "ROLLING_BACK", error: (error as Error).message });
        // A process can crash after durable staging but before this invocation
        // populated `staged`. ABORT is idempotent, so include every expected
        // source-connected worker when recovering that durable boundary.
        const connected = await this.options.workers().catch(() => []);
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
        this.options.store.write({
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
    const workers = await this.#expectedWorkers(state.expected_worker_fps);

    if (state.phase === "PREPARING_TARGET") {
      await this.options.runtime.prepareTarget(this.snapshot(state, "PREPARING_TARGET"));
      this.transition(initial, "STAGING_WORKERS");
      state = this.options.store.load()!;
    }
    if (state.phase === "STAGING_WORKERS") {
      for (const worker of workers) {
        await this.options.runtime.stageWorker(worker, this.snapshot(state, "STAGING_WORKERS"));
        staged.push(worker);
      }
      this.transition(initial, "DRAINING_SOURCE");
      state = this.options.store.load()!;
    }
    if (state.phase === "DRAINING_SOURCE") {
      await this.gate.beginDrain();
      this.transition(initial, "COPYING_STATE");
      state = this.options.store.load()!;
    }
    if (state.phase === "COPYING_STATE") {
      await this.gate.beginDrain();
      await this.options.runtime.copySnapshot(this.snapshot(state, "COPYING_STATE"));
      this.transition(initial, "WAITING_FOR_WORKERS");
      state = this.options.store.load()!;
    }
    if (state.phase === "WAITING_FOR_WORKERS") {
      await this.gate.beginDrain();
      for (const worker of workers) await this.options.runtime.activateWorker(worker, this.snapshot(state, "WAITING_FOR_WORKERS"));
      await this.options.runtime.waitForWorkers(this.snapshot(state, "WAITING_FOR_WORKERS"), 60_000);
      this.transition(initial, "COMMITTING");
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
          this.#retire(state, "COMMITTED");
          return;
        }
      } catch (error) {
        lastError = (error as Error).message;
        this.options.store.write({ ...(this.options.store.load() ?? state), error: lastError });
      }
      if (Date.now() >= deadline) {
        const phase = await this.options.runtime.targetStatus(snapshot).catch(() => null);
        if (phase === "COMMITTING" || phase === "COMMITTED") {
          // The target owns the cluster even though this side never got a
          // clean confirmation — retiring is the only correct end state.
          this.#retire(state, "COMMITTED");
          return;
        }
        this.options.store.write({
          ...(this.options.store.load() ?? state),
          phase: "FAILED",
          error: lastError ?? "coordinator move timed out waiting for the target to commit",
        });
        this.gate.setMode("active");
        return;
      }
      await Bun.sleep(5_000);
    }
  }

  /** The single post-commit exit: durable COMMITTED, gate retired, browsers told. */
  #retire(state: HandoffState, phase: "COMMITTED"): void {
    this.options.store.write({ ...(this.options.store.load() ?? state), phase, error: undefined });
    this.gate.setMode("retired");
    this.options.runtime.publishRelocation(this.snapshot(state, phase));
  }

  /** The staged/activated set is the one snapshotted at start(), resolved
   *  against the live registry. A missing fp is a hard error, not a silent
   *  skip that would later time out in waitForWorkers. */
  async #expectedWorkers(fps: string[]): Promise<MoveWorker[]> {
    const live = await this.options.workers();
    const byFp = new Map(live.map((worker) => [worker.fp, worker]));
    const missing = fps.filter((fp) => !byFp.has(fp));
    if (missing.length > 0) throw new Error(`expected workers are no longer known to the coordinator: ${missing.join(", ")}`);
    return fps.map((fp) => byFp.get(fp)!);
  }

  /** Recovery runs before any worker has re-attached to the fresh Bun.serve.
   *  Acting on an empty registry guarantees `worker offline` and a blind
   *  rollback, so give the expected set a chance to come back first. Returns
   *  (never throws) on timeout so the existing rollback path still runs. */
  async #awaitExpectedWorkers(fps: string[], timeoutMs = 60_000): Promise<void> {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const online = new Set((await this.options.workers()).filter((worker) => worker.online).map((worker) => worker.fp));
      if (fps.every((fp) => online.has(fp))) return;
      if (Date.now() >= deadline) return;
      await Bun.sleep(500);
    }
  }

  private async rollbackRecovered(state: HandoffState): Promise<void> {
    const snapshot = this.snapshot(state, "ROLLING_BACK");
    this.options.store.write({ ...state, phase: "ROLLING_BACK" });
    const workers = await this.options.workers();
    await Promise.allSettled(workers
      .filter((worker) => state.expected_worker_fps.includes(worker.fp))
      .map((worker) => this.options.runtime.abortWorker(worker, snapshot)));
    try {
      await this.options.runtime.abortTarget(snapshot);
      this.options.store.write({ ...(this.options.store.load() ?? state), phase: "ROLLED_BACK" });
    } catch (error) {
      this.options.store.write({ ...(this.options.store.load() ?? state), phase: "FAILED", error: (error as Error).message });
    }
    this.gate.setMode("active");
  }

  private transition(initial: HandoffState, phase: MovePhase): void {
    const current = this.options.store.load();
    if (!current || current.handoff_id !== initial.handoff_id) throw new Error("coordinator handoff state disappeared");
    this.options.store.write({ ...current, phase });
  }

  private async estimateDbSize(): Promise<number> {
    // PREPARE sends fs.statSync(dbPath).size and the target sizes its disk
    // check off that number; preflight must validate the same one.
    return fs.statSync(this.options.cfg.dbPath).size;
  }
}
