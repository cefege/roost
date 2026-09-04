// `roost push` publishes one clean commit and rolls the local POSIX
// coordinator plus every registered POSIX worker as one journaled fleet.
// No participant drops rollback state before the durable global commit decision.

import { resolve } from "node:path";
import { DeployFailure, failDeploy, resolveGitPublishTargetOrDie, resolveLocalGitShaOrDie, resolvePublishedGitShaOrDie, runOrDie } from "./deploy-exec.ts";
import { deploy } from "./deploy.ts";
import {
  beginCoordinatorDeployFinalization,
  coordinatorReportIsOperational,
  finalizeCoordinatorDeploy,
  recoverCoordinatorDeploy,
  VERIFY_POLL_MS,
  VERIFY_TIMEOUT_MS,
} from "./coordinator-deploy-recovery.ts";
import {
  loadCoordinatorDeployJournal,
  type CoordinatorDeployJournalV2,
} from "./coordinator-deploy-journal.ts";
import {
  acquireFleetPushTransaction,
  deployLocalCoordinatorHeld,
  prepareCoordinatorDeployLocation,
  type CoordinatorDeployLocation,
} from "./push-coordinator.ts";
import {
  convergeAtomicFleet,
  interruptedFleetRecoveryAction,
  finishAtomicFleetFinalization,
  rollbackAtomicFleet,
  type AtomicFleetRolloutDeps,
  type FleetRolloutPlan,
  type FleetRolloutWorker,
} from "./push-fleet-rollout.ts";
import {
  statusReport,
  workerInventory,
  type WorkerStatus,
} from "./status.ts";
import type { WorkerRolloutAction, WorkerRolloutDirective } from "./worker-deploy-rollout.ts";
import { POSIX_FULL_GIT_SHA_RE } from "./posix-deploy-journal.ts";
import { normalizedHost, tryCoordinatorSelfUpdate } from "./deploy-windows-channel.ts";
import { parseWindowsReleaseManifest } from "./windows/windows-update-journal.ts";
import { fetchAndVerifyReleaseAsset, WINDOWS_RELEASE_MANIFEST_ASSET } from "./update.ts";

export {
  foreignWorkerDeployJournalForCoordinator,
  preserveWebDistForNoBuild,
} from "./push-coordinator.ts";

const REPO_ROOT = resolve(import.meta.dir, "..", "..", "..");
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
    } else if (worker.keeperState === "unknown") {
      problems.push(`${worker.label}: keeper build proof is unavailable`);
    } else if (worker.keeperState === "stale") {
      problems.push(`${worker.label}: keeper reports stale build ${worker.keeperBuild ?? "unknown"}`);
    } else if (worker.lastSeenMs <= (deployedAfter.get(normalizedHost(target)) ?? 0)) {
      problems.push(`${worker.label}: awaiting a post-deploy heartbeat`);
    }
  }
  return problems;
}

export function workerConvergenceThresholds(
  targets: readonly string[],
  activatedAt: number,
): Map<string, number> {
  return new Map(targets.map((target) => [normalizedHost(target), activatedAt]));
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
    } else if (worker.keeperState !== "current") {
      problems.push(`${worker.label}: keeper is ${worker.keeperState} before rollout`);
    }
  }
  return problems;
}

function workerConvergenceProblem(
  worker: WorkerStatus,
  expectedSha: string,
  action: WorkerRolloutAction,
  heartbeatBoundary: number | null,
): string | null {
  if (worker.stale) return `${worker.label}: stale`;
  if (worker.gitSha !== expectedSha) {
    return `${worker.label}: reports ${worker.gitSha ?? "no SHA"}, expected ${expectedSha}`;
  }
  if (worker.keeperState !== "current") return `${worker.label}: keeper is ${worker.keeperState}`;
  if (heartbeatBoundary !== null && worker.lastSeenMs <= heartbeatBoundary) {
    return `${worker.label}: awaiting a post-${action} heartbeat`;
  }
  return null;
}

export function _atomicFleetConvergenceProblems(
  workers: readonly WorkerStatus[],
  targets: readonly FleetRolloutWorker[],
  expectedSha: string,
  action: WorkerRolloutAction,
  heartbeatBoundaries: ReadonlyMap<string, number>,
): string[] {
  const actual = [...workers].sort((left, right) =>
    left.fingerprint.localeCompare(right.fingerprint));
  const expected = [...targets].sort((left, right) =>
    left.fingerprint.localeCompare(right.fingerprint));
  const problems: string[] = [];
  if (action === "hold" && (
    actual.length !== expected.length
    || actual.some((worker, index) => worker.fingerprint !== expected[index]?.fingerprint)
  )) {
    problems.push("registered worker set does not exactly match the rollout journal");
  }
  for (const target of expected) {
    const worker = actual.find((candidate) => candidate.fingerprint === target.fingerprint);
    if (!worker) {
      problems.push(`${target.fingerprint}: missing from coordinator worker inventory`);
    } else {
      const problem = workerConvergenceProblem(
        worker,
        expectedSha,
        action,
        heartbeatBoundaries.get(worker.fingerprint) ?? 0,
      );
      if (problem) problems.push(problem);
    }
  }
  if (action === "rollback") {
    const participantFingerprints = new Set(expected.map((target) => target.fingerprint));
    for (const worker of actual) {
      if (participantFingerprints.has(worker.fingerprint)) continue;
      const problem = workerConvergenceProblem(worker, expectedSha, action, null);
      if (problem) problems.push(problem);
    }
  }
  return problems;
}

export async function preflightWindowsFleetRelease(
  expectedSha: string,
  fetchImpl: typeof fetch = fetch,
): Promise<{ manifestSha256: string }> {
  const release = await fetchAndVerifyReleaseAsset(WINDOWS_RELEASE_MANIFEST_ASSET, {
    fetchImpl,
    subject: "Windows fleet manifest",
    timeoutMs: 30_000,
    checksumTimeoutMs: 30_000,
    fail: (message) => new DeployFailure(8, message),
  });
  const manifest = parseWindowsReleaseManifest(release.bytes);
  if (manifest.build !== expectedSha) {
    throw new DeployFailure(
      8,
      `Windows release manifest reports ${manifest.build}, expected source commit ${expectedSha}`,
    );
  }
  return { manifestSha256: release.sha256 };
}

export async function deployCoordinatorForPlatform(
  expectedSha: string,
  buildWeb: boolean,
  deps: {
    windows?: (sha: string) => Promise<boolean | null>;
    posix?: (sha: string, build: boolean) => Promise<void>;
  } = {},
): Promise<"windows" | "posix"> {
  const windowsUpdated = await (deps.windows ?? tryCoordinatorSelfUpdate)(expectedSha);
  if (windowsUpdated !== null) return "windows";
  if (!deps.posix) {
    failDeploy(2, "POSIX coordinator deployment requires an atomic fleet rollout context");
  }
  await deps.posix(expectedSha, buildWeb);
  return "posix";
}

export function _resolveAtomicFleetWorkers(
  configured: string | undefined,
  inventory: readonly WorkerStatus[],
): FleetRolloutWorker[] {
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

function planFromJournal(
  journal: CoordinatorDeployJournalV2,
  inventory: readonly WorkerStatus[],
): FleetRolloutPlan {
  const workers = journal.targetWorkerFingerprints.map((fingerprint) => {
    const matches = inventory.filter((worker) => worker.fingerprint === fingerprint);
    if (matches.length !== 1) {
      failDeploy(8, `cannot resolve journal worker ${fingerprint} to one current deployment target`);
    }
    const worker = matches[0]!;
    if (worker.os === "win32") failDeploy(8, "a POSIX fleet journal contains a Windows worker");
    return {
      fingerprint,
      host: assertSafeSshTarget(worker.reachableAddr || worker.label),
    };
  });
  return {
    rolloutId: journal.rolloutId,
    priorSha: journal.priorSha,
    targetSha: journal.targetSha,
    workers,
  };
}

function sameRolloutTarget(
  plan: FleetRolloutPlan,
  targetSha: string,
  workers: readonly FleetRolloutWorker[],
): boolean {
  if (plan.targetSha !== targetSha || plan.workers.length !== workers.length) return false;
  const expected = plan.workers.map((worker) => worker.fingerprint).sort();
  const requested = workers.map((worker) => worker.fingerprint).sort();
  return expected.every((fingerprint, index) => fingerprint === requested[index]);
}

function fleetRuntime(
  location: CoordinatorDeployLocation,
  plan: FleetRolloutPlan,
): AtomicFleetRolloutDeps {
  const heartbeatBoundaries = new Map<WorkerRolloutAction, Map<string, number>>();
  const deployWorker = async (
    worker: FleetRolloutWorker,
    directive: WorkerRolloutDirective,
  ): Promise<void> => {
    let boundaries = heartbeatBoundaries.get(directive.action);
    if (!boundaries) {
      boundaries = new Map();
      heartbeatBoundaries.set(directive.action, boundaries);
    }
    boundaries.set(worker.fingerprint, Date.now());
    console.log(`\n>> ${directive.action} worker ${worker.host}`);
    await deploy([
      worker.host,
      `--source-root=${REPO_ROOT}`,
      `--expected-sha=${directive.targetSha}`,
    ], { rollout: directive });
  };
  const proveFleet = async (
    expectedSha: string,
    action: WorkerRolloutAction,
  ): Promise<readonly string[]> => {
    const deadline = Date.now() + VERIFY_TIMEOUT_MS;
    let problems: string[] = [];
    do {
      try {
        const report = await statusReport();
        problems = _atomicFleetConvergenceProblems(
          report.workers,
          plan.workers,
          expectedSha,
          action,
          heartbeatBoundaries.get(action) ?? new Map(),
        );
        if (problems.length === 0) return [];
      } catch (error) {
        problems = [`coordinator status failed: ${error instanceof Error ? error.message : String(error)}`];
      }
      if (Date.now() < deadline) await Bun.sleep(VERIFY_POLL_MS);
    } while (Date.now() < deadline);
    return problems;
  };
  return {
    deployWorker,
    proveFleet,
    beginCoordinatorFinalization: async () => {
      await beginCoordinatorDeployFinalization(location.journalPath, location.context);
    },
    coordinatorCanRollback: async () =>
      loadCoordinatorDeployJournal(location.journalPath, location.context)?.phase === "fleet-converging",
    finalizeCoordinator: async (finishWorkers) => {
      await finalizeCoordinatorDeploy(
        location.journalPath,
        location.context,
        async () => finishWorkers(),
      );
    },
    rollbackCoordinator: async (rollbackWorkers) => {
      await recoverCoordinatorDeploy(location.journalPath, location.context, {
        rollbackFleet: async () => rollbackWorkers(),
      });
    },
  };
}

async function recoverOrResumeCoordinatorRollout(
  location: CoordinatorDeployLocation,
  requestedSha: string,
  requestedWorkers: readonly FleetRolloutWorker[],
): Promise<boolean> {
  const journal = loadCoordinatorDeployJournal(location.journalPath, location.context);
  if (!journal) return false;
  if (journal.phase === "prepared" || journal.phase === "activating") {
    await recoverCoordinatorDeploy(location.journalPath, location.context);
    return false;
  }
  const plan = planFromJournal(journal, workerInventory());
  const runtime = fleetRuntime(location, plan);
  const targetMatches = sameRolloutTarget(plan, requestedSha, requestedWorkers);
  const recoveryAction = interruptedFleetRecoveryAction(journal.phase, targetMatches);
  if (recoveryAction === "finish-target") {
    await finishAtomicFleetFinalization(plan, runtime);
    return targetMatches;
  }
  if (recoveryAction === "converge-target") {
    await convergeAtomicFleet(plan, runtime);
    return true;
  }
  await rollbackAtomicFleet(plan, runtime);
  return false;
}
async function executePushUnderLease(
  args: readonly string[],
  configured: string | undefined,
  expectedSha: string,
  location: CoordinatorDeployLocation,
  requestedTargets: readonly FleetRolloutWorker[],
): Promise<void> {
  if (await recoverOrResumeCoordinatorRollout(
    location,
    expectedSha,
    requestedTargets,
  )) {
    console.log(`\n>> push complete — recovered exact fleet rollout ${expectedSha}`);
    return;
  }

  const initial = await statusReport();
  if (!coordinatorReportIsOperational(initial) || !initial.coord.gitSha) {
    failDeploy(8, "coordinator must be operational before fleet mutation begins");
  }
  const priorSha = initial.coord.gitSha;
  if (!POSIX_FULL_GIT_SHA_RE.test(priorSha)) {
    failDeploy(8, "coordinator did not report a full prior Git SHA");
  }
  const targets = _resolveAtomicFleetWorkers(configured, initial.workers);
  const priorProblems = atomicFleetPriorProblems(initial.workers, priorSha);
  if (priorProblems.length > 0) {
    failDeploy(8, `fleet is not wholly converged on prior SHA ${priorSha}:\n${priorProblems.join("\n")}`);
  }
  if (priorSha === expectedSha) {
    console.log(`\n>> push complete — coordinator and ${targets.length} workers already report ${expectedSha}`);
    return;
  }

  const rolloutId = crypto.randomUUID();
  const held = await deployLocalCoordinatorHeld({
    targetSha: expectedSha,
    priorSha,
    rolloutId,
    targetWorkerFingerprints: targets.map((worker) => worker.fingerprint),
    buildWeb: !args.includes("--no-web"),
  });
  const plan: FleetRolloutPlan = {
    rolloutId,
    priorSha,
    targetSha: expectedSha,
    workers: targets,
  };
  await convergeAtomicFleet(plan, fleetRuntime(held, plan));
  console.log(`\n>> push complete — coordinator and ${targets.length} workers report ${expectedSha}`);
}


async function finishMandatoryCoordinatorRecovery(
  location: CoordinatorDeployLocation,
): Promise<void> {
  const journal = loadCoordinatorDeployJournal(location.journalPath, location.context);
  if (!journal) return;
  if (journal.phase === "finalizing") {
    const plan = planFromJournal(journal, workerInventory());
    await finishAtomicFleetFinalization(plan, fleetRuntime(location, plan));
  } else if (journal.phase === "prepared" || journal.phase === "activating") {
    await recoverCoordinatorDeploy(location.journalPath, location.context);
  }
}

async function rollbackHeldCoordinatorIfPresent(
  location: CoordinatorDeployLocation,
): Promise<void> {
  const journal = loadCoordinatorDeployJournal(location.journalPath, location.context);
  if (!journal || journal.phase !== "fleet-converging") return;
  const plan = planFromJournal(journal, workerInventory());
  await rollbackAtomicFleet(plan, fleetRuntime(location, plan));
}

export async function push(args: string[]): Promise<void> {
  if (args.includes("--allow-dirty")) {
    failDeploy(1, "roost push never permits --allow-dirty");
  }
  if (args.includes("--no-coord")) {
    failDeploy(1, "atomic roost push cannot skip the coordinator transaction");
  }
  const targetsArg = args.find((arg) => arg.startsWith("--targets="));
  const configured = targetsArg?.slice("--targets=".length) ?? process.env.ROOST_PUSH_TARGETS;
  let targets: FleetRolloutWorker[];
  const location = prepareCoordinatorDeployLocation();
  const fleetTransaction = await acquireFleetPushTransaction(location);
  try {
    await finishMandatoryCoordinatorRecovery(location);
    try {
      targets = _resolveAtomicFleetWorkers(configured, workerInventory());
    } catch (error) {
      await rollbackHeldCoordinatorIfPresent(location);
      throw error;
    }
    let expectedSha: string;
    try {
      expectedSha = resolveLocalGitShaOrDie(REPO_ROOT);
      if (expectedSha.endsWith("-dirty") || !POSIX_FULL_GIT_SHA_RE.test(expectedSha)) {
        failDeploy(7, "roost push requires a clean full Git commit");
      }
    } catch (error) {
      await rollbackHeldCoordinatorIfPresent(location);
      throw error;
    }

    try {
      const publishTarget = resolveGitPublishTargetOrDie(REPO_ROOT);
      if (!args.includes("--no-git")) {
        console.log(`>> git push ${publishTarget.remote} HEAD:${publishTarget.mergeRef}`);
        await runOrDie(
          ["git", "push", "--", publishTarget.remote, `HEAD:${publishTarget.mergeRef}`],
          "git push",
          { cwd: REPO_ROOT, echo: true },
        );
      } else {
        console.log(">> skipping git transmission (--no-git); verifying remote identity");
      }
      resolvePublishedGitShaOrDie(REPO_ROOT, expectedSha);
    } catch (error) {
      await rollbackHeldCoordinatorIfPresent(location);
      throw error;
    }
    await executePushUnderLease(args, configured, expectedSha, location, targets);
  } finally {
    await fleetTransaction.release();
  }
}
