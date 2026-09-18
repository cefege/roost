// Converges a machine that was unreachable while the fleet moved: when a worker
// behind the coordinator's own SHA attaches, the coordinator starts the same
// POSIX `roost deploy <host>` job the SPA button drives, so a laptop that slept
// through `roost push` catches up by itself instead of staying behind forever.
// Called by main.ts's onWorkerConnected hook. Depends on deploy-jobs.ts for the
// job registry and @roost/shared/fleet-update for the one SHA comparison.

import { existsSync } from "node:fs";
import { join } from "node:path";
import { signal } from "@roost/shared/diag";
import { workerUpdateState } from "@roost/shared/fleet-update";
import { log } from "@roost/shared/log";
import { roostServiceDir } from "@roost/shared/paths";
import { workerDeployHost } from "./connect/handlers-workers-deploy.ts";
import type { KyselyDB } from "./db/connection.ts";
import {
  _deployJobs,
  type DeployStartResult,
  deployOutput,
  startDeploy,
} from "./deploy-jobs.ts";
import { COORD_GIT_SHA } from "./git-sha.ts";

/** A catch-up that settled must not re-arm on the next reconnect of a flapping
 *  worker: the same failure would repeat every few seconds. */
export const CATCH_UP_COOLDOWN_MS = 10 * 60 * 1000;

// Mirrors startDeploy's own host allowlist, so an unusable address is refused
// here with a reason instead of surfacing as a job that never spawns.
const DEPLOY_HOST_RE = /^[A-Za-z0-9.-]+$/;
// Both the sha1 and sha256 object-id widths git can report. A short stamp or
// the "dev" sentinel is NOT a release identity: it compares unequal to every
// real commit, so acting on one would redeploy the machine on every attach.
const FULL_GIT_SHA_RE = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/;

/** The registry columns a catch-up decision reads, renamed to camelCase at this
 *  boundary so the decision never depends on the SQLite row shape. */
export interface CatchUpWorkerRow {
  readonly fp: string;
  readonly os: string | null;
  readonly label: string;
  readonly reachableAddr: string | null;
  readonly gitSha: string | null;
  /** The keeper proof the worker last reported, null until its first
   *  post-reconcile heartbeat. Its epoch and channel count are what a keeper
   *  block is pinned to, so the block clears when the situation changes. */
  readonly keeperRuntimeJson: string | null;
}

/** One host resolution for the decision and the block registry: keying a block
 *  by a differently-resolved host would silently never match. */
function workerCatchUpHost(worker: CatchUpWorkerRow): string {
  return workerDeployHost(
    {
      fp: worker.fp,
      os: worker.os,
      label: worker.label,
      reachable_addr: worker.reachableAddr,
    },
    // An attach carries no operator-requested host, so there is nothing to fall
    // back to: an empty resolution means nothing addressable is known.
    "",
  );
}

export interface CatchUpDeployDecisionInputs {
  readonly worker: CatchUpWorkerRow;
  /** The running coordinator's SHA — the fleet's desired release. */
  readonly coordGitSha: string | null;
  /** Hosts with a deploy job running right now, operator-driven ones included. */
  readonly hostsWithDeployInFlight: ReadonlySet<string>;
  /** host → cooldown expiry, keyed by the deploy host this function resolves. */
  readonly cooldownUntilMsByHost: ReadonlyMap<string, number>;
  /** `roost push` owns the fleet right now (its journal is on disk). */
  readonly operatorRolloutActive: boolean;
  /** This host's keeper was already refused at this exact keeper signature. A
   *  keeper holding sessions the release cannot adopt fails the SAME admission
   *  every time, so retrying each attach would fill `roost doctor` with a
   *  recurring failure for a machine only `roost keeper-refresh` can unblock. */
  readonly keeperUpdateBlocked: boolean;
  readonly nowMs: number;
}

export type CatchUpDeployDecision =
  | { readonly start: true; readonly host: string }
  | { readonly start: false; readonly reason: string };

/** The whole admission rule, pure so every refusal is testable without a
 *  coordinator, a database, or a subprocess. Evaluated only from the attach
 *  hook, which is why the worker counts as online. */
export function _catchUpDeployDecision(
  inputs: CatchUpDeployDecisionInputs,
): CatchUpDeployDecision {
  const { worker } = inputs;
  // The signed Windows updater owns its own admission, journal and rollback;
  // a POSIX source deploy aimed at that machine would fight all three.
  if (worker.os === "win32") {
    return { start: false, reason: "windows_broker_owned" };
  }
  // A rollout holds per-host journals and the machine lease for its whole
  // duration; a second deploy landing inside that window corrupts both.
  if (inputs.operatorRolloutActive) {
    return { start: false, reason: "operator_rollout_in_progress" };
  }
  if (!inputs.coordGitSha || !FULL_GIT_SHA_RE.test(inputs.coordGitSha)) {
    return { start: false, reason: "coordinator_sha_unknown" };
  }
  if (!worker.gitSha || !FULL_GIT_SHA_RE.test(worker.gitSha)) {
    return { start: false, reason: "worker_sha_unknown" };
  }
  if (inputs.keeperUpdateBlocked) {
    return { start: false, reason: "keeper_update_blocked" };
  }
  const host = workerCatchUpHost(worker);
  if (!DEPLOY_HOST_RE.test(host)) {
    return { start: false, reason: "no_reachable_host" };
  }
  const state = workerUpdateState({
    workerGitSha: worker.gitSha,
    coordGitSha: inputs.coordGitSha,
    // The attach hook is the only caller, so the transport is live by
    // construction; `update-deferred` belongs to the surfaces that render it.
    online: true,
    deployInFlight: inputs.hostsWithDeployInFlight.has(host),
  });
  switch (state) {
    case "update-available":
      break;
    case "updating":
      return { start: false, reason: "deploy_in_flight" };
    case "up-to-date":
      return { start: false, reason: "up_to_date" };
    // Both SHAs are full object ids by here and the attach transport is live,
    // so no other state is reachable; refuse rather than guess at one.
    default:
      return { start: false, reason: `update_state_${state}` };
  }
  const cooldownUntilMs = inputs.cooldownUntilMsByHost.get(host);
  if (cooldownUntilMs !== undefined && inputs.nowMs < cooldownUntilMs) {
    return { start: false, reason: "failure_cooldown" };
  }
  return { start: true, host };
}

// The one owner of every mutable catch-up fact, so a host that stopped catching
// up is one grep away: host → this module's in-flight job, host → cooldown end.
const catchUpDeploys = {
  jobIdByHost: new Map<string, string>(),
  cooldownUntilMsByHost: new Map<string, number>(),
  // host → the keeper signature that was refused. A keeper holding sessions the
  // release cannot adopt fails the SAME admission on every attempt, so retrying
  // it each attach would put a recurring failure in `roost doctor` for a machine
  // only `roost keeper-refresh` can unblock. Keyed by signature, not host alone,
  // so the block clears by itself once that keeper's epoch or channel count
  // moves — the sessions ended, and the deploy can be admitted again.
  keeperBlockedSignatureByHost: new Map<string, string>(),
};

/** Matches the refusals that mean "this machine's keeper cannot be updated now",
 *  as printed by `roost deploy`'s own keeper admission. */
const KEEPER_BLOCKED_OUTPUT_RE =
  /keeper update (?:is )?(?:blocked|unproven|incompatible)|keeper replacement blocked/i;

/** The keeper identity a block is pinned to: a new epoch or a changed channel
 *  count is a different keeper situation and deserves a fresh attempt. */
export function _keeperSignature(keeperRuntimeJson: string | null): string {
  if (!keeperRuntimeJson) return "none";
  try {
    const parsed: unknown = JSON.parse(keeperRuntimeJson);
    if (!parsed || typeof parsed !== "object") return "unparsed";
    const runtime = parsed as { keeper_epoch?: unknown; channel_count?: unknown };
    return `${String(runtime.keeper_epoch)}:${String(runtime.channel_count)}`;
  } catch {
    return "unparsed";
  }
}

export interface CatchUpDeployOptions {
  /** Defaults to the real POSIX deploy job; a test substitutes a recorder. */
  readonly deployStarter?: (host: string, expectedGitSha?: string) => DeployStartResult;
  readonly nowMs?: number;
  /** The fleet's desired release. Production always leaves this unset: the
   *  desired SHA is the running coordinator's own, never a second record. */
  readonly coordGitSha?: string;
}

/** Evaluates admission against live coordinator state and starts the job when
 *  it is admitted. Returns the decision, or `start_failed` when the job itself
 *  could not be spawned. */
export function _startCatchUpDeployForWorker(
  worker: CatchUpWorkerRow,
  options: CatchUpDeployOptions = {},
): CatchUpDeployDecision {
  const nowMs = options.nowMs ?? Date.now();
  const deployStarter = options.deployStarter ?? startDeploy;
  const coordGitSha = options.coordGitSha ?? COORD_GIT_SHA;
  const keeperSignature = _keeperSignature(worker.keeperRuntimeJson);
  const decision = _catchUpDeployDecision({
    worker,
    coordGitSha,
    hostsWithDeployInFlight: hostsWithDeployInFlight(),
    cooldownUntilMsByHost: catchUpDeploys.cooldownUntilMsByHost,
    operatorRolloutActive: operatorFleetRolloutActive(),
    keeperUpdateBlocked:
      catchUpDeploys.keeperBlockedSignatureByHost.get(
        workerCatchUpHost(worker),
      ) === keeperSignature,
    nowMs,
  });
  if (!decision.start) {
    // A silent no-op here is unexplainable in production: the skip reason is
    // the only record of why a behind machine stayed behind.
    log.info("deploy", "catchup_skipped", {
      worker_fp: worker.fp,
      reason: decision.reason,
      worker_git_sha: worker.gitSha,
      coord_git_sha: coordGitSha,
    });
    return decision;
  }
  // Pinned, not implicit: without the SHA the job deploys the coordinator's
  // checkout HEAD, which is only the fleet's desired release by coincidence.
  const result = deployStarter(decision.host, coordGitSha);
  if (!result.ok || !result.jobId) {
    const error = result.error ?? "deploy job did not start";
    catchUpDeploys.cooldownUntilMsByHost.set(
      decision.host,
      nowMs + CATCH_UP_COOLDOWN_MS,
    );
    // No job exists, so deploy-jobs' own emitDone signal never fires for this
    // failure and doctor would otherwise never see an unstartable catch-up.
    signal("deploy.failed", {
      host: decision.host,
      reason: error,
      cooldownKey: decision.host,
    });
    log.warn("deploy", "catchup_start_failed", {
      worker_fp: worker.fp,
      host: decision.host,
      error,
    });
    return { start: false, reason: "start_failed" };
  }
  catchUpDeploys.jobIdByHost.set(decision.host, result.jobId);
  log.info("deploy", "catchup_started", {
    worker_fp: worker.fp,
    host: decision.host,
    job_id: result.jobId,
    worker_git_sha: worker.gitSha,
    coord_git_sha: coordGitSha,
  });
  void _watchCatchUpDeployOutcome(decision.host, result.jobId, keeperSignature);
  return decision;
}

/** The attach hook. Never throws: a worker that reconnected is worth more than
 *  the deploy it might have needed, so every failure degrades to a logged skip. */
export async function startCatchUpDeployOnAttach(
  db: KyselyDB,
  workerFp: string,
  options: CatchUpDeployOptions = {},
): Promise<void> {
  try {
    const row = await db
      .selectFrom("workers")
      .select(["fp", "os", "label", "git_sha", "reachable_addr", "keeper_runtime_json"])
      .where("fp", "=", workerFp)
      .where("deleted_at_ms", "is", null)
      .executeTakeFirst();
    if (!row) {
      log.info("deploy", "catchup_skipped", {
        worker_fp: workerFp,
        reason: "worker_not_registered",
      });
      return;
    }
    _startCatchUpDeployForWorker({
      fp: row.fp,
      os: row.os,
      label: row.label,
      reachableAddr: row.reachable_addr,
      gitSha: row.git_sha,
      keeperRuntimeJson: row.keeper_runtime_json,
    }, options);
  } catch (error) {
    log.warn("deploy", "catchup_attach_failed", {
      worker_fp: workerFp,
      error: String(error),
    });
  }
}

export function __clearCatchUpDeployStateForTest(): void {
  catchUpDeploys.jobIdByHost.clear();
  catchUpDeploys.cooldownUntilMsByHost.clear();
  catchUpDeploys.keeperBlockedSignatureByHost.clear();
}

/** Awaits the started job's terminal frame and records the outcome, watching the
 *  output for a keeper refusal on the way: the job's terminal frame carries only
 *  an exit code, so the keeper reason exists nowhere else. The start path fires
 *  this and forgets it; a test awaits it for the settle transition. */
export async function _watchCatchUpDeployOutcome(
  host: string,
  jobId: string,
  keeperSignature = "none",
): Promise<void> {
  let keeperBlocked = false;
  try {
    for await (const message of deployOutput(jobId)) {
      if (message.kind === "line") {
        if (KEEPER_BLOCKED_OUTPUT_RE.test(message.text)) keeperBlocked = true;
        continue;
      }
      noteCatchUpDeploySettled(host, message.error, keeperBlocked ? keeperSignature : null);
      return;
    }
    noteCatchUpDeploySettled(host, "deploy output ended without a result", null);
  } catch (error) {
    noteCatchUpDeploySettled(host, String(error), null);
  }
}

// An operator-started deploy counts: the SPA button and a catch-up reach the
// same host through the same subprocess, so one must never overlap the other.
function hostsWithDeployInFlight(): ReadonlySet<string> {
  const hosts = new Set<string>(catchUpDeploys.jobIdByHost.keys());
  for (const job of _deployJobs.values()) {
    if (job.status === "running") hosts.add(job.host);
  }
  return hosts;
}

/** `roost push` writes this journal before it stages anything and removes it
 *  once the rollout finalizes, so its presence is the fleet-wide "hands off".
 *  Path-mirrored from the CLI's owner (push-coordinator-location.ts) rather
 *  than imported: the coordinator never depends on CLI modules. */
function operatorFleetRolloutActive(): boolean {
  try {
    return existsSync(
      join(roostServiceDir(), "transactions", "coordinator-deploy.json"),
    );
  } catch (error) {
    // An unreadable service directory is not proof the fleet is free.
    log.warn("deploy", "catchup_rollout_probe_failed", { error: String(error) });
    return true;
  }
}

/** Records the end of this module's catch-up for `host`. A settled job always
 *  arms the cooldown, not only a failed one: a deploy that exits 0 without
 *  moving the worker's reported SHA would otherwise re-arm on every attach.
 *  `keeperBlockedSignature` additionally pins the refusal to that keeper, so the
 *  machine is left alone until its keeper situation actually changes. */
function noteCatchUpDeploySettled(
  host: string,
  error: string | undefined,
  keeperBlockedSignature: string | null,
  nowMs = Date.now(),
): void {
  catchUpDeploys.jobIdByHost.delete(host);
  catchUpDeploys.cooldownUntilMsByHost.set(host, nowMs + CATCH_UP_COOLDOWN_MS);
  if (keeperBlockedSignature !== null) {
    catchUpDeploys.keeperBlockedSignatureByHost.set(host, keeperBlockedSignature);
  }
  if (error) {
    // deploy-jobs already signalled deploy.failed for the job itself; a second
    // signal for the same exit would only shorten doctor's cooldown window.
    log.warn("deploy", "catchup_failed", {
      host,
      error,
      keeper_blocked: keeperBlockedSignature !== null,
      cooldown_ms: CATCH_UP_COOLDOWN_MS,
    });
    return;
  }
  log.info("deploy", "catchup_settled", {
    host,
    cooldown_ms: CATCH_UP_COOLDOWN_MS,
  });
}
