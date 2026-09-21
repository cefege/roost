// `roost push` publishes one clean commit, commits the coordinator through its
// participant-free V4 journal, releases coordinator preparation ownership, and
// submits one durable update job per selected worker. Per-worker failure or
// deferral never rolls back the coordinator or another worker.

import { resolve } from "node:path";
import { existsSync, readFileSync } from "node:fs";
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
  prepareCoordinatorDeployLocation,
  type CoordinatorDeployLocation,
} from "./push-coordinator.ts";
import { deployLocalCoordinator } from "./push-coordinator-v4.ts";
import { recoverCoordinatorDeployV4 } from "./coordinator-deploy-recovery-v4.ts";
import {
  printIndependentWorkerUpdateResults,
  submitIndependentWorkerUpdates,
} from "./push-worker-jobs.ts";
import {
  finishAtomicFleetFinalization,
  type FleetRolloutTarget,
} from "./push-fleet-rollout.ts";
import {
  _atomicFleetConvergenceProblems,
  _partitionFleetForRollout,
  _resolveAtomicFleetWorkers,
  fleetWorkerIdentityProblems,
  planFromJournal,
  type DeferredFleetWorker,
} from "./push-fleet-plan.ts";
import { fleetRuntime } from "./push-rollout-runtime.ts";
import {
  statusReport,
  workerInventoryForUpdateAdmission,
} from "./status.ts";
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


export async function push(args: string[]): Promise<void> {
  if (args.includes("--allow-dirty")) {
    failDeploy(1, "roost push never permits --allow-dirty");
  }
  if (args.includes("--no-coord")) {
    failDeploy(1, "roost push cannot skip the coordinator transaction");
  }
  const targetsArg = args.find((arg) => arg.startsWith("--targets="));
  const configured = targetsArg?.slice("--targets=".length)
    ?? process.env.ROOST_PUSH_TARGETS;
  const location = prepareCoordinatorDeployLocation();
  const transaction = await acquireFleetPushTransaction(location);
  let expectedSha: string;
  let targets: FleetRolloutTarget[];
  try {
    await recoverCoordinatorJournalBeforeForwardPush(location);
    expectedSha = resolveLocalGitShaOrDie(REPO_ROOT);
    if (expectedSha.endsWith("-dirty") || !POSIX_FULL_GIT_SHA_RE.test(expectedSha)) {
      failDeploy(7, "roost push requires a clean full Git commit");
    }
    const publishTarget = resolveGitPublishTargetOrDie(REPO_ROOT);
    if (!args.includes("--no-git")) {
      console.log(`>> git push ${publishTarget.remote} HEAD:${publishTarget.mergeRef}`);
      await runOrDie(
        ["git", "push", "--", publishTarget.remote, `HEAD:${publishTarget.mergeRef}`],
        "git push",
        { cwd: REPO_ROOT, echo: true },
      );
    }
    resolvePublishedGitShaOrDie(REPO_ROOT, expectedSha);
    const report = await statusReport();
    if (!coordinatorReportIsOperational(report)) {
      failDeploy(8, "coordinator must be operational before update");
    }
    targets = _resolveAtomicFleetWorkers(
      configured,
      workerInventoryForUpdateAdmission(),
    );
    if (report.coord.gitSha !== expectedSha) {
      const windowsUpdated = await tryCoordinatorSelfUpdate(expectedSha);
      if (windowsUpdated === null) {
        await deployLocalCoordinator({
          targetSha: expectedSha,
          rolloutId: crypto.randomUUID(),
          buildWeb: !args.includes("--no-web"),
        });
      }
    }
  } finally {
    await transaction.release();
  }

  const results = await submitIndependentWorkerUpdates(targets, expectedSha);
  printIndependentWorkerUpdateResults(results);
  const failures = results.filter(result => result.outcome === "failed");
  if (failures.length > 0) {
    throw new DeployFailure(
      8,
      `${failures.length} worker update${failures.length === 1 ? "" : "s"} failed;`
        + " successful workers and coordinator remain committed",
    );
  }
}

async function recoverCoordinatorJournalBeforeForwardPush(
  location: CoordinatorDeployLocation,
): Promise<void> {
  if (!existsSync(location.journalPath)) return;
  let schemaVersion: unknown;
  try {
    const parsed: unknown = JSON.parse(readFileSync(location.journalPath, "utf8"));
    schemaVersion = parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>).schemaVersion
      : undefined;
  } catch (error) {
    failDeploy(5, `coordinator deploy journal is malformed: ${String(error)}`);
  }
  if (schemaVersion === 4) {
    await recoverCoordinatorDeployV4(location.journalPath, location.context);
    return;
  }
  if (schemaVersion === 3) {
    await finishMandatoryCoordinatorRecovery(location);
    return;
  }
  failDeploy(5, `unsupported coordinator deploy journal schema ${String(schemaVersion)}`);
}
