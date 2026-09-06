// Fleet worker selection and convergence proofs for `roost push`.
// The push owner uses this module to build exact fingerprint plans and
// validate worker state supplied to journaled rollout execution.

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

export function atomicFleetPriorProblems(
  workers: readonly WorkerStatus[],
  priorSha: string,
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
    } else if (worker.stale) {
      problems.push(`${worker.label}: stale before rollout`);
    } else if (worker.gitSha !== priorSha) {
      problems.push(`${worker.label}: reports ${worker.gitSha ?? "no SHA"}, prior is ${priorSha}`);
    }
  }
  return problems;
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
): string[] {
  const actual = [...workers].sort((left, right) =>
    left.fingerprint.localeCompare(right.fingerprint));
  const expected = [...targets].sort((left, right) =>
    left.fingerprint.localeCompare(right.fingerprint));
  const problems: string[] = [];
  if (action === "hold" && (
    actual.length !== expected.length
    || actual.some((worker, index) =>
      worker.fingerprint !== expected[index]?.fingerprint)
  )) {
    problems.push("registered worker set does not exactly match the rollout journal");
  }
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
    for (const worker of actual) {
      if (participants.has(worker.fingerprint)) continue;
      if (worker.stale || worker.gitSha !== expectedSha) {
        problems.push(
          `${worker.label}: unjournaled worker did not remain at ${expectedSha}`,
        );
      }
    }
  }
  return problems;
}

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
  const resolved = targets.map((host) => {
    const match = resolveWorkerTarget(inventory, host).worker;
    if (!match) failDeploy(2, `${host}: missing from coordinator worker inventory`);
    if (!FULL_WORKER_FINGERPRINT_RE.test(match.fingerprint)) {
      failDeploy(2, `${match.label}: coordinator reported an invalid worker fingerprint`);
    }
    return { fingerprint: match.fingerprint, host };
  });
  const selected = [...new Set(resolved.map((worker) => worker.fingerprint))].sort();
  const registered = [...new Set(inventory.map((worker) => worker.fingerprint))].sort();
  if (selected.length !== registered.length
    || selected.some((fingerprint, index) => fingerprint !== registered[index])) {
    failDeploy(2, "atomic push requires --targets to identify the exact registered worker set");
  }
  return resolved;
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

export function sameRolloutTarget(
  plan: FleetRolloutPlan,
  targetSha: string,
  workers: readonly FleetRolloutTarget[],
): boolean {
  if (plan.targetSha !== targetSha || plan.workers.length !== workers.length) return false;
  const expected = plan.workers.map((worker) => worker.fingerprint).sort();
  const requested = workers.map((worker) => worker.fingerprint).sort();
  return expected.every((fingerprint, index) => fingerprint === requested[index]);
}
