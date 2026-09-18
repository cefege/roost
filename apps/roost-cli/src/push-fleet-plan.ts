// Fleet worker selection, rollout partition, and convergence proofs for `roost push`.
// The push owner uses this module to split the registered workers into the
// participants one journaled rollout converges now and the machines it defers,
// and to validate worker state supplied to journaled rollout execution.

import { failDeploy } from "./deploy-exec.ts";
import { normalizedHost } from "./deploy-windows-channel.ts";
import { keeperUpdateConvergenceProblem } from "./keeper-update-convergence.ts";
import type {
  FleetRolloutPlan,
  FleetRolloutTarget,
  FleetRolloutWorker,
} from "./push-fleet-rollout.ts";
import type { CoordinatorDeployJournalV2 } from "./coordinator-deploy-journal.ts";
import type { WorkerStatus } from "./status.ts";
import type { WorkerRolloutAction } from "./worker-deploy-rollout.ts";

const FULL_WORKER_FINGERPRINT_RE = /^[0-9a-f]{64}$/;

function hostLabel(value: string): string {
  return normalizedHost(value).split(".")[0] ?? "";
}

function assertSafeSshTarget(value: string): string {
  const target = value.trim();
  if (!/^[A-Za-z0-9][A-Za-z0-9._:-]*$/.test(target)) {
    throw new Error(`invalid SSH deployment target: ${JSON.stringify(value)}`);
  }
  return target;
}

function resolveWorkerTarget(
  workers: readonly WorkerStatus[],
  target: string,
): { worker: WorkerStatus | null; ambiguous: boolean } {
  const normalizedTarget = normalizedHost(target);
  const fingerprintMatches = workers.filter(
    (worker) => normalizedHost(worker.fingerprint) === normalizedTarget,
  );
  if (fingerprintMatches.length > 0) {
    return {
      worker: fingerprintMatches.length === 1 ? fingerprintMatches[0]! : null,
      ambiguous: fingerprintMatches.length > 1,
    };
  }
  const exact = workers.filter((worker) =>
    [worker.label, worker.reachableAddr]
      .filter((value): value is string => Boolean(value))
      .some((value) => normalizedHost(value) === normalizedTarget)
  );
  if (exact.length > 0) {
    return { worker: exact.length === 1 ? exact[0]! : null, ambiguous: exact.length > 1 };
  }
  const targetLabel = hostLabel(normalizedTarget);
  const aliases = workers.filter((worker) =>
    [worker.label, worker.reachableAddr]
      .filter((value): value is string => Boolean(value))
      .some((value) => hostLabel(normalizedHost(value)) === targetLabel)
  );
  return { worker: aliases.length === 1 ? aliases[0]! : null, ambiguous: aliases.length > 1 };
}

export function resolvePushTargets(
  configured: string | undefined,
  workers: readonly WorkerStatus[],
): string[] {
  const requested = (configured ?? "").split(",").map((value) => value.trim()).filter(Boolean);
  const source = requested.length > 0
    ? requested
    : workers.map((worker) => worker.os === "win32"
      ? worker.fingerprint
      : (worker.reachableAddr || worker.label));
  const seen = new Set<string>();
  const targets: string[] = [];
  for (const value of source) {
    const resolved = resolveWorkerTarget(workers, value);
    const canonical = resolved.worker
      ? (resolved.worker.os === "win32"
        ? resolved.worker.fingerprint
        : (resolved.worker.reachableAddr || resolved.worker.label))
      : value;
    const target = assertSafeSshTarget(canonical);
    const key = normalizedHost(target);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    targets.push(target);
  }
  return targets;
}

export function ambiguousPushTargets(
  targets: readonly string[],
  workers: readonly WorkerStatus[],
): string[] {
  return targets.filter((target) => resolveWorkerTarget(workers, target).ambiguous);
}

export function workerVersionProblems(
  targets: readonly string[],
  workers: readonly WorkerStatus[],
  expectedSha: string,
  deployedAfter: ReadonlyMap<string, number> = new Map(),
): string[] {
  const problems: string[] = [];
  for (const target of targets) {
    const resolved = resolveWorkerTarget(workers, target);
    const worker = resolved.worker;
    if (resolved.ambiguous) problems.push(`${target}: ambiguous coordinator worker identity`);
    else if (!worker) problems.push(`${target}: missing from coordinator worker inventory`);
    else if (worker.stale) problems.push(`${worker.label}: stale`);
    else if (worker.gitSha !== expectedSha) {
      problems.push(`${worker.label}: reports ${worker.gitSha ?? "no SHA"}, expected ${expectedSha}`);
    } else if (!worker.keeperRuntime) {
      problems.push(`${worker.label}: keeper runtime proof is unavailable`);
    } else if (worker.lastSeenMs <= (deployedAfter.get(normalizedHost(target)) ?? 0)) {
      problems.push(`${worker.label}: awaiting a post-deploy heartbeat`);
    }
  }
  return problems;
}

export function workerConvergenceThresholds(
  targets: readonly string[],
  workers: readonly WorkerStatus[],
): Map<string, number> {
  const thresholds = new Map<string, number>();
  for (const target of targets) {
    const resolved = resolveWorkerTarget(workers, target);
    if (!resolved.ambiguous && resolved.worker) {
      thresholds.set(normalizedHost(target), resolved.worker.lastSeenMs);
    }
  }
  return thresholds;
}

/** Identity integrity over the WHOLE registry. A duplicate or malformed
 *  fingerprint makes every per-worker proof ambiguous, so it refuses the push
 *  outright; version skew is not an identity defect and only defers a machine. */
export function fleetWorkerIdentityProblems(
  workers: readonly WorkerStatus[],
): string[] {
  const problems: string[] = [];
  const seen = new Set<string>();
  for (const worker of workers) {
    if (seen.has(worker.fingerprint)) {
      problems.push(`${worker.fingerprint}: duplicate coordinator worker identity`);
      continue;
    }
    seen.add(worker.fingerprint);
    if (!FULL_WORKER_FINGERPRINT_RE.test(worker.fingerprint)) {
      problems.push(`${worker.label}: invalid worker fingerprint`);
    }
  }
  return problems;
}

/** A registered worker this rollout skips, with the operator-facing reason. */
export interface DeferredFleetWorker {
  readonly fingerprint: string;
  readonly label: string;
  readonly reason: string;
}

export interface FleetRolloutPartition {
  readonly participants: FleetRolloutTarget[];
  readonly deferred: DeferredFleetWorker[];
}

/** Split the rollout candidates into the machines this push converges now and
 *  the machines it defers to their own catch-up. A participant must be
 *  reachable, heartbeat-fresh and already on `priorSha`: every per-worker
 *  `hold` deploy proves the installed service against the rollout's prior SHA
 *  and refuses anything else, so admitting a drifted machine would drag the
 *  whole fleet into rollback. */
export function _partitionFleetForRollout(
  candidates: readonly FleetRolloutTarget[],
  inventory: readonly WorkerStatus[],
  routableFingerprints: ReadonlySet<string>,
  priorSha: string,
): FleetRolloutPartition {
  const participants: FleetRolloutTarget[] = [];
  const deferred: DeferredFleetWorker[] = [];
  for (const candidate of candidates) {
    const worker = inventory.find(
      registered => registered.fingerprint === candidate.fingerprint,
    );
    const reason = !worker
      ? "no longer registered"
      : !routableFingerprints.has(candidate.fingerprint)
        ? "not reachable"
        : worker.stale
          ? "stale"
          : worker.gitSha !== priorSha
            ? `reports ${worker.gitSha?.slice(0, 8) ?? "no SHA"}, prior is ${
              priorSha.slice(0, 8)
            }`
            : null;
    if (reason === null) {
      participants.push(candidate);
      continue;
    }
    deferred.push({
      fingerprint: candidate.fingerprint,
      label: worker?.label ?? candidate.host,
      reason,
    });
  }
  return { participants, deferred };
}

function workerConvergenceProblem(
  worker: WorkerStatus,
  target: FleetRolloutWorker,
  expectedSha: string,
  action: WorkerRolloutAction,
  coordinatorHeartbeatBaselineMs?: number,
): string | null {
  if (worker.gitSha !== expectedSha) {
    return `${worker.label}: reports ${worker.gitSha ?? "no SHA"}, expected ${expectedSha}`;
  }
  return keeperUpdateConvergenceProblem(
    worker,
    target.keeperUpdate,
    action === "rollback" ? "source" : "target",
    coordinatorHeartbeatBaselineMs,
    undefined,
    action !== "rollback",
    false,
  );
}

export function _atomicFleetConvergenceProblems(
  workers: readonly WorkerStatus[],
  targets: readonly FleetRolloutWorker[],
  expectedSha: string,
  action: WorkerRolloutAction,
  heartbeatBoundaries: ReadonlyMap<string, number>,
  _admissionRecordedAtMs = 0,
  routableFingerprints: ReadonlySet<string> | null = null,
  /** A rollback proves the fleet at `expectedSha`, the rollout's prior SHA, so
   *  a deferred machine legitimately reports either end of the rollout and only
   *  a third SHA is evidence of an unjournaled mutation. */
  rolloutTargetSha: string | null = null,
): string[] {
  const actual = [...workers].sort((left, right) =>
    left.fingerprint.localeCompare(right.fingerprint));
  const expected = [...targets].sort((left, right) =>
    left.fingerprint.localeCompare(right.fingerprint));
  const problems: string[] = [];
  // A worker outside the journal is a deferred machine, never a participant: one
  // that registers or returns mid-rollout must not abort the rollout, so the
  // per-participant proofs below are the only set comparison.
  for (const target of expected) {
    const worker = actual.find(
      candidate => candidate.fingerprint === target.fingerprint,
    );
    if (!worker) {
      problems.push(`${target.fingerprint}: missing from coordinator worker inventory`);
      continue;
    }
    if (routableFingerprints
      && !routableFingerprints.has(target.fingerprint)) {
      problems.push(`${worker.label}: worker is not coordinator-routable`);
      continue;
    }
    const problem = workerConvergenceProblem(
      worker,
      target,
      expectedSha,
      action,
      heartbeatBoundaries.get(worker.fingerprint),
    );
    if (problem) problems.push(problem);
  }
  if (action === "rollback") {
    const participants = new Set(expected.map(target => target.fingerprint));
    const rolloutTargets = rolloutTargetSha
      ? [expectedSha, rolloutTargetSha]
      : [expectedSha];
    for (const worker of actual) {
      if (participants.has(worker.fingerprint)) continue;
      // A machine that never reported a SHA is unproven, not mutated, and a
      // deferred machine keeps its own SHA: only a third one proves this rollout
      // dragged a worker it never journaled.
      if (worker.gitSha && !rolloutTargets.includes(worker.gitSha)) {
        problems.push(
          `${worker.label}: unjournaled worker reports ${
            worker.gitSha.slice(0, 8)
          }, outside this rollout`,
        );
      }
    }
  }
  return problems;
}

/** Resolve the rollout candidates. A named subset is legal: `_partitionFleetForRollout`
 *  decides which candidates this rollout can actually converge. */
export function _resolveAtomicFleetWorkers(
  configured: string | undefined,
  inventory: readonly WorkerStatus[],
): FleetRolloutTarget[] {
  const windows = inventory.filter((worker) => worker.os === "win32");
  if (windows.length > 0) {
    failDeploy(
      2,
      `atomic push is unavailable while Windows workers are registered: ` +
        windows.map((worker) => worker.label).join(", "),
    );
  }
  const targets = resolvePushTargets(configured, inventory);
  const ambiguous = ambiguousPushTargets(targets, inventory);
  if (ambiguous.length > 0) {
    failDeploy(2, `ambiguous push targets: ${ambiguous.join(", ")}; use exact full addresses`);
  }
  if (targets.length === 0) failDeploy(2, "atomic push requires at least one registered worker");
  return targets.map((host) => {
    const match = resolveWorkerTarget(inventory, host).worker;
    if (!match) failDeploy(2, `${host}: missing from coordinator worker inventory`);
    if (!FULL_WORKER_FINGERPRINT_RE.test(match.fingerprint)) {
      failDeploy(2, `${match.label}: coordinator reported an invalid worker fingerprint`);
    }
    return { fingerprint: match.fingerprint, host };
  });
}

export function planFromJournal(
  journal: CoordinatorDeployJournalV2,
  inventory: readonly WorkerStatus[],
): FleetRolloutPlan {
  const workers = journal.targetWorkerFingerprints.map((fingerprint) => {
    const matches = inventory.filter((worker) => worker.fingerprint === fingerprint);
    if (matches.length !== 1) {
      failDeploy(8, `cannot resolve journal worker ${fingerprint} to one current deployment target`);
    }
    const worker = matches[0]!;
    const keeperPlan = journal.workerKeeperPlans.find(
      candidate => candidate.fingerprint === fingerprint,
    );
    if (!keeperPlan) {
      failDeploy(8, `coordinator journal has no keeper plan for ${fingerprint}`);
    }
    if (worker.os === "win32") failDeploy(8, "a POSIX fleet journal contains a Windows worker");
    return {
      fingerprint,
      host: assertSafeSshTarget(worker.reachableAddr || worker.label),
      keeperUpdate: keeperPlan.keeperUpdate,
    };
  });
  return {
    rolloutId: journal.rolloutId,
    admissionRecordedAtMs: journal.admissionRecordedAtMs,
    priorSha: journal.priorSha,
    targetSha: journal.targetSha,
    workers,
  };
}

/** Does an interrupted journal describe the rollout this push is asking for?
 *  The journal holds that rollout's PARTICIPANTS, a subset of today's
 *  candidates, so a machine deferred then — or one that has since returned —
 *  must not turn a resume of the same commit into a fleet-wide rollback. */
export function sameRolloutTarget(
  plan: FleetRolloutPlan,
  targetSha: string,
  candidates: readonly FleetRolloutTarget[],
): boolean {
  if (plan.targetSha !== targetSha || plan.workers.length === 0) return false;
  const requested = new Set(candidates.map((candidate) => candidate.fingerprint));
  return plan.workers.every((worker) => requested.has(worker.fingerprint));
}
