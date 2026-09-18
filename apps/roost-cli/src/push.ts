// `roost push` publishes one clean commit and rolls the local POSIX coordinator
// plus every reachable POSIX worker already on the prior SHA as one journaled
// fleet, deferring the machines it cannot converge to their own catch-up.
// No participant drops rollback state before the durable global commit decision.

import { resolve } from "node:path";
import {
  DeployFailure,
  failDeploy,
  resolveGitPublishTargetOrDie,
  resolveLocalGitShaOrDie,
  resolvePublishedGitShaOrDie,
  runOrDie,
} from "./deploy-exec.ts";
import {
  coordinatorReportIsOperational,
  recoverCoordinatorDeploy,
} from "./coordinator-deploy-recovery.ts";
import { loadCoordinatorDeployJournal } from "./coordinator-deploy-journal.ts";
import {
  acquireFleetPushTransaction,
  deployLocalCoordinatorHeld,
  prepareCoordinatorDeployLocation,
  type CoordinatorDeployLocation,
} from "./push-coordinator.ts";
import {
  convergeAtomicFleet,
  finishAtomicFleetFinalization,
  interruptedFleetRecoveryAction,
  rollbackAtomicFleet,
  type FleetRolloutPlan,
  type FleetRolloutTarget,
} from "./push-fleet-rollout.ts";
import {
  _atomicFleetConvergenceProblems,
  _partitionFleetForRollout,
  _resolveAtomicFleetWorkers,
  fleetWorkerIdentityProblems,
  planFromJournal,
  sameRolloutTarget,
  type DeferredFleetWorker,
} from "./push-fleet-plan.ts";
import { fleetRuntime } from "./push-rollout-runtime.ts";
import {
  routableWorkerFingerprints,
  statusReport,
  workerInventoryForUpdateAdmission,
} from "./status.ts";
import {
  classifyFleetKeeperUpdates,
  loadSourceKeeperContract,
  probeTargetKeeperContract,
} from "./push-keeper-admission.ts";
import type { KeeperContractV1 } from "@roost/shared/keeper-update";
import { POSIX_FULL_GIT_SHA_RE } from "./posix-deploy-journal.ts";
import { tryCoordinatorSelfUpdate } from "./deploy-windows-channel.ts";
import { parseWindowsReleaseManifest } from "./windows/windows-update-journal.ts";
import { fetchAndVerifyReleaseAsset, WINDOWS_RELEASE_MANIFEST_ASSET } from "./update.ts";

export {
  foreignWorkerDeployJournalForCoordinator,
  preserveWebDistForNoBuild,
} from "./push-coordinator.ts";
export {
  _atomicFleetConvergenceProblems,
  _partitionFleetForRollout,
  _resolveAtomicFleetWorkers,
  fleetWorkerIdentityProblems,
};
export {
  ambiguousPushTargets,
  resolvePushTargets,
  workerConvergenceThresholds,
  workerVersionProblems,
} from "./push-fleet-plan.ts";

const REPO_ROOT = resolve(import.meta.dir, "..", "..", "..");

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

async function recoverOrResumeCoordinatorRollout(
  location: CoordinatorDeployLocation,
  requestedSha: string,
  requestedWorkers: readonly FleetRolloutTarget[],
): Promise<boolean> {
  const journal = loadCoordinatorDeployJournal(location.journalPath, location.context);
  if (!journal) return false;
  if (journal.phase === "prepared" || journal.phase === "activating") {
    await recoverCoordinatorDeploy(location.journalPath, location.context);
    return false;
  }
  const plan = planFromJournal(journal, workerInventoryForUpdateAdmission());
  const runtime = fleetRuntime(location, plan, _atomicFleetConvergenceProblems);
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

/** The operator's whole view of a partial fleet: which machines this push left
 *  alone, why, and how each one catches up. A deferred machine is not an error,
 *  so these lines print beside a successful push. */
export function _deferredFleetReportLines(
  deferred: readonly DeferredFleetWorker[],
): string[] {
  if (deferred.length === 0) return [];
  return [
    `\n>> ${deferred.length} machine${
      deferred.length === 1 ? "" : "s"
    } deferred — update pending:`,
    ...deferred.map((machine) => `   ${machine.label}: ${machine.reason}`),
    "   Each updates automatically when it next attaches to the coordinator,"
      + " or immediately with `roost deploy <host>`.",
  ];
}

async function executePushUnderLease(
  args: readonly string[],
  configured: string | undefined,
  expectedSha: string,
  location: CoordinatorDeployLocation,
  requestedTargets: readonly FleetRolloutTarget[],
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
  const inventory = workerInventoryForUpdateAdmission();
  const identityProblems = fleetWorkerIdentityProblems(inventory);
  if (identityProblems.length > 0) {
    failDeploy(
      8,
      `coordinator worker identity is not provable; zero mutation:\n${
        identityProblems.join("\n")
      }`,
    );
  }
  const candidates = _resolveAtomicFleetWorkers(configured, inventory);
  let routableFingerprints: ReadonlySet<string>;
  try {
    routableFingerprints = await routableWorkerFingerprints();
  } catch (error) {
    failDeploy(
      8,
      `fleet routability proof failed with zero mutation: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
  const { participants, deferred } = _partitionFleetForRollout(
    candidates,
    inventory,
    routableFingerprints,
    priorSha,
  );
  // The coordinator can only move as one journaled transaction with at least one
  // worker, so a fleet with nobody to converge is a refusal — unless the
  // coordinator already runs the target, where there is simply nothing to do.
  if (participants.length === 0 && priorSha !== expectedSha) {
    failDeploy(
      8,
      `no registered worker is reachable and on the prior SHA; zero mutation\n${
        deferred.map((machine) => `${machine.label}: ${machine.reason}`).join("\n")
      }`,
    );
  }
  // Probed per host, not as one Promise.all: a machine whose keeper contract
  // cannot be read is exactly a deferral, and rejecting the batch on the first
  // unreadable host is the wedge this model removes.
  const targetContracts = new Map<string, KeeperContractV1>();
  const probeDeferred: DeferredFleetWorker[] = [];
  let sourceKeeperContract: KeeperContractV1;
  try {
    sourceKeeperContract = await loadSourceKeeperContract(REPO_ROOT);
  } catch (error) {
    failDeploy(
      8,
      `fleet keeper source proof failed with zero mutation: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
  }
  await Promise.all(participants.map(async (target) => {
    try {
      targetContracts.set(
        target.fingerprint,
        await probeTargetKeeperContract(target.host, expectedSha, sourceKeeperContract),
      );
    } catch (error) {
      probeDeferred.push({
        fingerprint: target.fingerprint,
        label: target.host,
        reason: `keeper target proof failed: ${
          error instanceof Error ? error.message : String(error)
        }`,
      });
    }
  }));
  const admission = classifyFleetKeeperUpdates(
    participants.filter((target) => targetContracts.has(target.fingerprint)),
    inventory,
    targetContracts,
  );
  const allDeferred = [...deferred, ...probeDeferred, ...admission.deferred];
  if (admission.workers.length === 0 && priorSha !== expectedSha) {
    failDeploy(
      8,
      `no registered worker can be updated safely; zero mutation\n${
        allDeferred.map((machine) => `${machine.label}: ${machine.reason}`).join("\n")
      }`,
    );
  }

  // "Already satisfied" must mean every participant is provably on the target
  // with no keeper work left. A machine that came back behind the fleet is
  // deferred and reported, never counted as satisfied.
  const unconverged = admission.workers.filter((worker) =>
    worker.keeperUpdate.admission.required_action !== "preserve"
    || inventory.find(
      registered => registered.fingerprint === worker.fingerprint,
    )?.gitSha !== expectedSha);
  if (priorSha === expectedSha && unconverged.length === 0) {
    console.log(
      `\n>> push complete — coordinator, ${admission.workers.length} workers, and keepers already satisfy ${expectedSha}`,
    );
    for (const line of _deferredFleetReportLines(allDeferred)) console.log(line);
    return;
  }
  const rolloutId = crypto.randomUUID();
  const held = await deployLocalCoordinatorHeld({
    targetSha: expectedSha,
    priorSha,
    rolloutId,
    targetWorkerFingerprints: admission.workers.map((worker) => worker.fingerprint),
    workerKeeperPlans: admission.workers.map((worker) => ({
      fingerprint: worker.fingerprint,
      keeperUpdate: worker.keeperUpdate,
    })),
    buildWeb: !args.includes("--no-web"),
  });
  const plan: FleetRolloutPlan = {
    rolloutId,
    admissionRecordedAtMs: held.journal.admissionRecordedAtMs,
    priorSha,
    targetSha: expectedSha,
    workers: admission.workers,
  };
  await convergeAtomicFleet(
    plan,
    fleetRuntime(held, plan, _atomicFleetConvergenceProblems),
  );
  console.log(
    `\n>> push complete — coordinator and ${admission.workers.length} workers report ${expectedSha}`,
  );
  for (const line of _deferredFleetReportLines(allDeferred)) console.log(line);
}

async function finishMandatoryCoordinatorRecovery(
  location: CoordinatorDeployLocation,
): Promise<void> {
  const journal = loadCoordinatorDeployJournal(location.journalPath, location.context);
  if (!journal) return;
  if (journal.phase === "finalizing") {
    const plan = planFromJournal(journal, workerInventoryForUpdateAdmission());
    await finishAtomicFleetFinalization(
      plan,
      fleetRuntime(location, plan, _atomicFleetConvergenceProblems),
    );
  } else if (journal.phase === "prepared" || journal.phase === "activating") {
    await recoverCoordinatorDeploy(location.journalPath, location.context);
  }
}

async function rollbackHeldCoordinatorIfPresent(
  location: CoordinatorDeployLocation,
): Promise<void> {
  const journal = loadCoordinatorDeployJournal(location.journalPath, location.context);
  if (!journal || journal.phase !== "fleet-converging") return;
  const plan = planFromJournal(journal, workerInventoryForUpdateAdmission());
  await rollbackAtomicFleet(
    plan,
    fleetRuntime(location, plan, _atomicFleetConvergenceProblems),
  );
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
  let targets: FleetRolloutTarget[];
  const location = prepareCoordinatorDeployLocation();
  const fleetTransaction = await acquireFleetPushTransaction(location);
  try {
    await finishMandatoryCoordinatorRecovery(location);
    try {
      targets = _resolveAtomicFleetWorkers(
        configured,
        workerInventoryForUpdateAdmission(),
      );
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
