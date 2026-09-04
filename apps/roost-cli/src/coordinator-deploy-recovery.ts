// Runtime recovery drivers for the coordinator self-update deploy journal:
// staged-release removal/flush, service restart + health proof, rollback to
// the prior definition, target retirement, and the interrupted-deploy state
// machine that maps a loaded journal onto clean/commit/rollback actions.
// inline) and by deploy-windows-channel.ts for the self-update health gate.
// Schema/parse live in coordinator-deploy-journal.ts.

import { coordServiceLabel, workerServicePath } from "@roost/shared/paths";
import { posixShellQuote } from "@roost/shared/shell-quote";

import { existsSync, lstatSync, readdirSync, readFileSync, realpathSync, rmSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { durableRemove, durableWriteFile, flushDurablePath } from "@roost/shared/durability";
import { DeployFailure, run } from "./deploy-exec.ts";
import { statusReport, type StatusReport } from "./status.ts";
import {
  coordinatorDeployRecoveryAction,
  coordinatorInstallEnvironment,
  coordinatorReleasePathIsConfined,
  coordinatorRepoFromService,
  coordinatorRestartCommand,
  coordinatorStagedReleasePathIsSafe,
  loadCoordinatorDeployJournal,
  writeCoordinatorDeployPhase,
  type CoordinatorDeployJournalContext,
  type CoordinatorDeployJournalV1,
} from "./coordinator-deploy-journal.ts";

export const VERIFY_TIMEOUT_MS = 60_000;
export const VERIFY_POLL_MS = 1_000;

export function coordinatorReportIsHealthy(
  report: StatusReport,
  expectedSha: string,
): boolean {
  return coordinatorReportIsOperational(report)
    && report.coord.gitSha === expectedSha;
}

export function coordinatorReportIsOperational(report: StatusReport): boolean {
  return report.coord.reachable
    && report.coordAgentLoaded
    && report.tlsMode !== "missing";
}

export async function removeStagedCoordinatorRelease(
  releaseRoot: string,
  stagingRepoPath: string,
  stagedReleasePath: string,
  targetSha: string,
): Promise<void> {
  if (!coordinatorStagedReleasePathIsSafe(releaseRoot, stagedReleasePath, targetSha)
    || realpathSync(releaseRoot) !== releaseRoot) {
    throw new Error(`refusing to remove unsafe staged coordinator path ${stagedReleasePath}`);
  }
  if (!existsSync(stagedReleasePath)) {
    await flushDurablePath(releaseRoot);
    return;
  }

  const stagedEntry = lstatSync(stagedReleasePath);
  if (stagedEntry.isSymbolicLink() || !stagedEntry.isDirectory()) {
    rmSync(stagedReleasePath, { force: true });
    await flushDurablePath(releaseRoot);
    return;
  }
  const canonicalStage = realpathSync(stagedReleasePath);
  if (canonicalStage !== stagedReleasePath
    || !coordinatorStagedReleasePathIsSafe(releaseRoot, canonicalStage, targetSha)) {
    throw new Error(`refusing to remove staged coordinator path through a symbolic link`);
  }

  let removedByGit = false;
  if (existsSync(join(stagingRepoPath, ".git"))
    && realpathSync(stagingRepoPath) === stagingRepoPath) {
    const removed = await run(["git", "worktree", "remove", "--force", stagedReleasePath], {
      cwd: stagingRepoPath,
      quiet: true,
    });
    removedByGit = removed.exit === 0;
  }
  if (!removedByGit) rmSync(stagedReleasePath, { recursive: true, force: true });
  await flushDurablePath(releaseRoot);
}

export async function flushCoordinatorReleaseTree(
  releaseRoot: string,
  stagedReleasePath: string,
  targetSha: string,
): Promise<void> {
  if (!coordinatorStagedReleasePathIsSafe(releaseRoot, stagedReleasePath, targetSha)
    || realpathSync(releaseRoot) !== releaseRoot
    || realpathSync(stagedReleasePath) !== stagedReleasePath
    || !lstatSync(stagedReleasePath).isDirectory()) {
    throw new Error("staged coordinator release is not a confined real directory");
  }
  const pending = [stagedReleasePath];
  const directories: string[] = [];
  while (pending.length > 0) {
    const directory = pending.pop()!;
    directories.push(directory);
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      const metadata = lstatSync(path);
      if (metadata.isSymbolicLink()) continue;
      if (metadata.isDirectory()) {
        pending.push(path);
      } else if (metadata.isFile()) {
        await flushDurablePath(path);
      } else {
        throw new Error(`staged coordinator release contains unsupported entry ${path}`);
      }
    }
  }
  for (let index = directories.length - 1; index >= 0; index--) {
    await flushDurablePath(directories[index]!);
  }
  await flushDurablePath(releaseRoot);
}

export async function coordinatorStartupPolicyIsEnabled(
  platform: "darwin" | "linux",
  label: string = coordServiceLabel(),
): Promise<boolean> {
  if (platform === "linux") {
    const unit = label.endsWith(".service") ? label : `${label}.service`;
    const result = await run(["bash", "-lc",
      `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"; ` +
        `systemctl --user is-enabled ${posixShellQuote(unit)}`,
    ], { quiet: true });
    return result.exit === 0 && result.stdout.trim() === "enabled";
  }
  const result = await run(["bash", "-lc", "launchctl print-disabled gui/$(id -u)"], {
    quiet: true,
  });
  return result.exit === 0
    && !new RegExp(`"${label.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}"\\s*=>\\s*true`)
      .test(result.stdout);
}

export async function restorePriorCoordinator(
  journalPath: string,
  releaseRoot: string,
  journal: CoordinatorDeployJournalV1,
): Promise<string | null> {
  try {
    await durableWriteFile(
      journal.servicePath,
      Buffer.from(journal.priorDefinitionBase64, "base64"),
      { mode: journal.priorDefinitionMode },
    );
    const result = await run(["bash", "-lc", coordinatorRestartCommand(journal.servicePath)], {
      cwd: releaseRoot,
      quiet: true,
    });
    if (result.exit !== 0) {
      return `coordinator rollback restart failed (exit ${result.exit})\n${result.stdout}\n${result.stderr}`;
    }
    const deadline = Date.now() + 30_000;
    let report = await statusReport();
    while (!coordinatorReportIsHealthy(report, journal.priorSha)) {
      if (Date.now() >= deadline) {
        return `restored coordinator did not prove healthy (reported ${report.coord.gitSha ?? "no SHA"}, ` +
          `expected ${journal.priorSha ?? "unknown"}, TLS=${report.tlsMode})`;
      }
      await Bun.sleep(VERIFY_POLL_MS);
      report = await statusReport();
    }
    await removeStagedCoordinatorRelease(
      releaseRoot,
      journal.stagingRepoPath,
      journal.stagedReleasePath,
      journal.targetSha,
    );
    await durableRemove(journalPath);
    return null;
  } catch (error) {
    return `coordinator rollback failed: ${error instanceof Error ? error.message : String(error)}`;
  }
}

export function currentServiceTargetsRelease(
  journal: CoordinatorDeployJournalV1,
  platform: "darwin" | "linux",
): boolean {
  if (!existsSync(journal.servicePath) || !existsSync(journal.stagedReleasePath)) return false;
  try {
    const definition = readFileSync(journal.servicePath, "utf8");
    const repo = coordinatorRepoFromService(definition, platform);
    if (!repo || !isAbsolute(repo) || resolve(repo) !== journal.stagedReleasePath) return false;
    const environment = coordinatorInstallEnvironment(definition, platform);
    return (environment.ROOST_GIT_SHA ?? environment.GIT_SHA) === journal.targetSha;
  } catch {
    return false;
  }
}

async function recoveredTargetIsHealthy(
  journal: CoordinatorDeployJournalV1,
  platform: "darwin" | "linux",
): Promise<boolean> {
  const deadline = Date.now() + VERIFY_TIMEOUT_MS;
  for (;;) {
    try {
      const report = await statusReport();
      if (currentServiceTargetsRelease(journal, platform)
        && coordinatorReportIsHealthy(report, journal.targetSha)) {
        return true;
      }
    } catch {
      // The recovery decision remains rollback unless the target proves healthy.
    }
    if (Date.now() >= deadline) return false;
    await Bun.sleep(VERIFY_POLL_MS);
  }
}

async function retirePriorCoordinatorRelease(
  releaseRoot: string,
  journal: CoordinatorDeployJournalV1,
): Promise<void> {
  const source = journal.sourceReleasePath;
  if (!coordinatorReleasePathIsConfined(releaseRoot, source)
    || dirname(source) !== releaseRoot
    || !existsSync(journal.stagedReleasePath)) {
    return;
  }
  if (!existsSync(source)) return;
  if (realpathSync(source) !== source) {
    throw new Error(`prior coordinator release is not canonical: ${source}`);
  }
  const workerPath = workerServicePath();
  let workerRepo: string | null;
  if (!existsSync(workerPath)) {
    workerRepo = null;
  } else {
    const parsed = coordinatorRepoFromService(
      readFileSync(workerPath, "utf8"),
      process.platform,
    );
    if (!parsed) {
      throw new Error("cannot prove whether the prior coordinator release is still used by the worker");
    }
    const resolvedWorkerRepo = resolve(parsed);
    workerRepo = existsSync(resolvedWorkerRepo)
      ? realpathSync(resolvedWorkerRepo)
      : resolvedWorkerRepo;
  }
  if (source === workerRepo) return;
  const removed = await run(["git", "worktree", "remove", "--force", source], {
    cwd: journal.stagedReleasePath,
    quiet: true,
  });
  if (removed.exit !== 0) {
    throw new Error(
      `cannot retire prior coordinator release ${source}: ${removed.stderr.trim() || `exit ${removed.exit}`}`,
    );
  }
  await flushDurablePath(releaseRoot);
}

export async function commitCoordinatorDeploy(
  journalPath: string,
  releaseRoot: string,
  journal: CoordinatorDeployJournalV1,
): Promise<void> {
  await flushDurablePath(journal.servicePath);
  await flushDurablePath(dirname(journal.servicePath));
  const activated = journal.phase === "activated"
    ? journal
    : await writeCoordinatorDeployPhase(journalPath, journal, "activated");
  await retirePriorCoordinatorRelease(releaseRoot, activated);
  await durableRemove(journalPath);
}

export async function recoverCoordinatorDeploy(
  journalPath: string,
  context: CoordinatorDeployJournalContext,
): Promise<void> {
  const journal = loadCoordinatorDeployJournal(journalPath, context);
  if (!journal) return;
  console.log(`\n>> recover interrupted coordinator deployment (${journal.phase})`);
  const targetHealthy = journal.phase === "prepared"
    ? false
    : await recoveredTargetIsHealthy(journal, context.platform);
  const action = coordinatorDeployRecoveryAction(journal.phase, targetHealthy);
  if (action === "clean-prepared") {
    await removeStagedCoordinatorRelease(
      context.releaseRoot,
      journal.stagingRepoPath,
      journal.stagedReleasePath,
      journal.targetSha,
    );
    await durableRemove(journalPath);
    return;
  }
  if (action === "commit-target") {
    await commitCoordinatorDeploy(journalPath, context.releaseRoot, journal);
    return;
  }
  const rollbackError = await restorePriorCoordinator(journalPath, context.releaseRoot, journal);
  if (rollbackError) {
    throw new DeployFailure(
      8,
      `interrupted coordinator deployment could not be recovered\n${rollbackError}`,
    );
  }
}
