// Disposable coordinator-owned worker catch-up scheduler. Worker-ready events
// and thirty-second sweeps converge routable POSIX machines on the committed
// coordinator SHA through the same durable WorkerUpdateOwner as manual Retry.

import { log } from "@roost/shared/log";
import { workerUpdateState } from "@roost/shared/fleet-update";
import type { KyselyDB } from "./db/connection.ts";
import { workerDeployHost } from "./connect/handlers-workers-deploy.ts";
import { listRoutableFps } from "./connect/worker-registry.ts";
import { COORD_GIT_SHA } from "./git-sha.ts";
import type { WorkerUpdateOwner } from "./worker-update-owner.ts";

export const CATCH_UP_COOLDOWN_MS = 10 * 60 * 1_000;
export const CATCH_UP_SWEEP_MS = 30_000;
const DEPLOY_HOST_RE = /^[A-Za-z0-9.-]+$/;
const FULL_GIT_SHA_RE = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/;

export interface CatchUpWorkerRow {
  readonly fp: string;
  readonly os: string | null;
  readonly label: string;
  readonly reachableAddr: string | null;
  readonly gitSha: string | null;
  readonly keeperRuntimeJson: string | null;
}

export interface CatchUpDeployDecisionInputs {
  readonly worker: CatchUpWorkerRow;
  readonly coordGitSha: string | null;
  readonly routable: boolean;
  readonly updateInFlight: boolean;
  readonly retryAfterMs: number | null;
  readonly nowMs: number;
}

export type CatchUpDeployDecision =
  | { readonly start: true; readonly host: string }
  | { readonly start: false; readonly reason: string };

export function _catchUpDeployDecision(
  inputs: CatchUpDeployDecisionInputs,
): CatchUpDeployDecision {
  const { worker } = inputs;
  if (worker.os === "win32") return { start: false, reason: "unsupported_platform" };
  if (!inputs.coordGitSha || !FULL_GIT_SHA_RE.test(inputs.coordGitSha)) {
    return { start: false, reason: "coordinator_sha_unknown" };
  }
  if (!worker.gitSha || !FULL_GIT_SHA_RE.test(worker.gitSha)) {
    return { start: false, reason: "worker_sha_unknown" };
  }
  if (!inputs.routable) return { start: false, reason: "offline" };
  const host = workerDeployHost({
    fp: worker.fp,
    os: worker.os,
    label: worker.label,
    reachable_addr: worker.reachableAddr,
  }, "");
  if (!DEPLOY_HOST_RE.test(host)) return { start: false, reason: "no_reachable_host" };
  const state = workerUpdateState({
    workerGitSha: worker.gitSha,
    coordGitSha: inputs.coordGitSha,
    online: true,
    deployInFlight: inputs.updateInFlight,
  });
  if (state === "up-to-date") return { start: false, reason: "up_to_date" };
  if (state === "updating") return { start: false, reason: "deploy_in_flight" };
  if (state !== "update-available") return { start: false, reason: `update_state_${state}` };
  if (inputs.retryAfterMs !== null && inputs.nowMs < inputs.retryAfterMs) {
    return { start: false, reason: "failure_cooldown" };
  }
  return { start: true, host };
}

export interface WorkerCatchUpScheduler {
  onWorkerReady(workerFp: string): void;
  sweep(): Promise<void>;
  dispose(): void;
}

export interface WorkerCatchUpSchedulerDeps {
  db: KyselyDB;
  sourceRoot: string;
  coordGitSha?: string | null;
  now?: () => number;
  setInterval?: typeof globalThis.setInterval;
  clearInterval?: typeof globalThis.clearInterval;
  startTimer?: boolean;
  updateOwner: Pick<WorkerUpdateOwner, "readSummary" | "startDeploy" | "sweep">;
}

export function createWorkerCatchUpScheduler(
  deps: WorkerCatchUpSchedulerDeps,
): WorkerCatchUpScheduler {
  let disposed = false;
  const pending = new Set<string>();
  const now = deps.now ?? Date.now;
  const coordGitSha = deps.coordGitSha ?? COORD_GIT_SHA;
  const evaluate = async (workerFp: string, workerReady = false): Promise<void> => {
    if (disposed || pending.has(workerFp)) return;
    pending.add(workerFp);
    try {
      const row = await deps.db.selectFrom("workers")
        .select(["fp", "os", "label", "git_sha", "reachable_addr", "keeper_runtime_json"])
        .where("fp", "=", workerFp)
        .where("deleted_at_ms", "is", null)
        .executeTakeFirst();
      if (!row) return;
      const latest = deps.updateOwner.readSummary(workerFp);
      const resumeOfflineWaiting = workerReady
        && latest?.status === "waiting"
        && latest.reasonCode === "offline";
      const updateInFlight = latest !== null
        && latest.status !== "blocked"
        && latest.status !== "failed"
        && latest.status !== "succeeded"
        && !resumeOfflineWaiting;
      const retryAfterMs = resumeOfflineWaiting
        ? null
        : latest?.nextAttemptAtMs
          ?? (latest && (latest.status === "failed" || latest.status === "blocked")
            ? latest.updatedAtMs + CATCH_UP_COOLDOWN_MS
            : null);
      const decision = _catchUpDeployDecision({
        worker: {
          fp: row.fp,
          os: row.os,
          label: row.label,
          reachableAddr: row.reachable_addr,
          gitSha: row.git_sha,
          keeperRuntimeJson: row.keeper_runtime_json,
        },
        coordGitSha,
        routable: listRoutableFps().includes(row.fp),
        updateInFlight,
        retryAfterMs,
        nowMs: now(),
      });
      if (!decision.start) {
        log.info("deploy", "catchup_skipped", {
          worker_fp: row.fp,
          reason: decision.reason,
          worker_git_sha: row.git_sha,
          coord_git_sha: coordGitSha,
        });
        return;
      }
      const requestedTargetSha = resumeOfflineWaiting
        ? latest!.targetGitSha
        : coordGitSha!;
      const result = await deps.updateOwner.startDeploy({
        workerFp: row.fp,
        host: decision.host,
        expectedGitSha: requestedTargetSha,
        source: "catchup",
        sourceRoot: deps.sourceRoot,
        sourceMode: "coordinator-pinned",
      });
      log.info("deploy", result.ok ? "catchup_started" : "catchup_start_failed", {
        worker_fp: row.fp,
        host: decision.host,
        job_id: result.jobId ?? null,
        error: result.error ?? null,
      });
    } catch (error) {
      log.warn("deploy", "catchup_evaluate_failed", {
        worker_fp: workerFp,
        error: String(error),
      });
    } finally {
      pending.delete(workerFp);
    }
  };

  const sweep = async (): Promise<void> => {
    if (disposed) return;
    await deps.updateOwner.sweep();
    const rows = await deps.db.selectFrom("workers")
      .select("fp")
      .where("deleted_at_ms", "is", null)
      .execute();
    await Promise.all(rows.map(row => evaluate(row.fp)));
  };
  const interval = deps.startTimer === false
    ? null
    : (deps.setInterval ?? globalThis.setInterval)(() => void sweep(), CATCH_UP_SWEEP_MS);
  interval?.unref?.();
  return {
    onWorkerReady: workerFp => { void evaluate(workerFp, true); },
    sweep,
    dispose: () => {
      if (disposed) return;
      disposed = true;
      pending.clear();
      if (interval !== null) (deps.clearInterval ?? globalThis.clearInterval)(interval);
    },
  };
}
