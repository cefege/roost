// Durable journal IO and lifecycle recovery for localhost worker deploys.
// deploy-local.ts owns the transaction and activation workflow around it.
// Service controls and confined worktree cleanup supply rollback operations.

import {
  existsSync,
  lstatSync,
  readFileSync,
  realpathSync,
  rmSync,
} from "node:fs";
import type { Stats } from "node:fs";
import { join } from "node:path";
import { durableRemove, durableWriteFile, flushDurablePath } from "@roost/shared/durability";
import { coordServicePath } from "@roost/shared/paths";
import {
  DeployFailure,
  POSIX_WORKER_DEPLOY_JOURNAL_PATHS,
  run,
  workerServiceIsRunning,
} from "./deploy-exec.ts";
import {
  startLocalWorkerForActivation,
  stopLocalWorkerForActivation,
} from "./deploy-local-activation.ts";
import type { JournaledKeeperUpdateCallbacks } from "./direct-keeper-update.ts";
import {
  probeLocalWorkerLifecycle,
  probeLocalWorkerStartupPolicy,
  restoreLocalWorkerPriorLifecycle,
  startRestoredLocalWorker,
} from "./deploy-local-service-lifecycle.ts";
import {
  decodeServiceSnapshot,
  localWorkerDeployStageIsConfined,
  parseLocalWorkerDeployJournal,
  serviceGitSha,
  serviceWorkingDirectory,
  _recoverLocalWorkerDeployJournal,
} from "./local-worker-deploy-journal.ts";
import type {
  LocalWorkerDeployConfinement,
  LocalWorkerDeployJournal,
  LocalWorkerDeployRecoveryDeps,
  LocalWorkerLifecycle,
  LocalWorkerServiceSnapshot,
} from "./local-worker-deploy-journal.ts";
import { coordinatorJournalAllowsLocalWorkerRollout } from "./local-worker-rollout-coordinator.ts";
import { verifyWorkerCmd, WORKER_AGENT, WORKER_UNIT } from "./service-ctl.ts";
import type { WorkerRolloutDirective } from "./worker-deploy-rollout.ts";

function lstatIfPresent(path: string): Stats | null {
  try {
    return lstatSync(path);
  } catch (error) {
    if (
      error instanceof Error
      && "code" in error
      && (error as NodeJS.ErrnoException).code === "ENOENT"
    ) {
      return null;
    }
    throw error;
  }
}

export function readLocalWorkerServiceSnapshot(
  servicePath: string,
): LocalWorkerServiceSnapshot | null {
  const entry = lstatIfPresent(servicePath);
  if (!entry) return null;
  if (!entry.isFile() || entry.isSymbolicLink()) {
    throw new Error("worker service definition must be a regular file");
  }
  return {
    definitionBase64: readFileSync(servicePath).toString("base64"),
    mode: entry.mode & 0o777,
  };
}

export async function checkpointLocalWorkerDeployJournal(
  journalPath: string,
  journal: Readonly<LocalWorkerDeployJournal>,
  confinement: Readonly<LocalWorkerDeployConfinement>,
): Promise<void> {
  const serialized = `${JSON.stringify(journal)}\n`;
  parseLocalWorkerDeployJournal(serialized, confinement);
  await durableWriteFile(journalPath, serialized, { mode: 0o600 });
}

function readLocalWorkerDeployJournal(journalPath: string): string | null {
  const entry = lstatIfPresent(journalPath);
  if (!entry) return null;
  if (!entry.isFile() || entry.isSymbolicLink()) {
    throw new Error("worker deploy journal must be a regular file");
  }
  try {
    return readFileSync(journalPath, "utf8");
  } catch (error) {
    if (
      error instanceof Error
      && "code" in error
      && (error as NodeJS.ErrnoException).code === "ENOENT"
    ) {
      return null;
    }
    throw error;
  }
}


function localCoordinatorWorkingDirectory(): string | null {
  const servicePath = coordServicePath();
  if (!existsSync(servicePath)) return null;
  try {
    const workingDirectory = serviceWorkingDirectory(
      readFileSync(servicePath, "utf8"),
      process.platform as "linux" | "darwin",
    );
    if (!workingDirectory) throw new Error("missing WorkingDirectory");
    return workingDirectory;
  } catch (error) {
    throw new Error(
      `cannot prove coordinator release use before worker cleanup: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
}

async function cleanupLocalWorkerStage(
  journal: Readonly<LocalWorkerDeployJournal>,
  confinement: Readonly<LocalWorkerDeployConfinement>,
): Promise<void> {
  if (
    journal.sourceRoot !== confinement.sourceRoot
    || journal.releaseRoot !== confinement.releaseRoot
    || !localWorkerDeployStageIsConfined(journal.releaseRoot, journal.stagedReleasePath)
  ) {
    throw new Error("refusing to clean an unconfined worker deploy stage");
  }
  const entry = lstatIfPresent(journal.stagedReleasePath);
  if (!entry) return;
  let removedByGit = false;
  if (entry.isDirectory() && !entry.isSymbolicLink()) {
    const canonicalRoot = realpathSync(journal.releaseRoot);
    const canonicalStage = realpathSync(journal.stagedReleasePath);
    if (
      canonicalRoot !== journal.releaseRoot
      || !localWorkerDeployStageIsConfined(canonicalRoot, canonicalStage)
    ) {
      throw new Error("refusing to clean a worker deploy stage outside its release root");
    }
    try {
      const removed = await run(
        ["git", "worktree", "remove", "--force", journal.stagedReleasePath],
        { cwd: journal.sourceRoot, quiet: true },
      );
      removedByGit = removed.exit === 0;
    } catch {
      // Recursive removal below remains confined and handles a missing source checkout.
    }
  }
  if (!removedByGit) {
    rmSync(journal.stagedReleasePath, { recursive: true, force: true });
  }
  await flushDurablePath(journal.releaseRoot);
}

export async function _removeManagedPriorRelease(
  sourceRepo: string,
  releaseRoot: string,
  priorWorkingDirectory: string | null,
): Promise<void> {
  if (!priorWorkingDirectory || !localWorkerDeployStageIsConfined(releaseRoot, priorWorkingDirectory)) {
    return;
  }
  const entry = lstatIfPresent(priorWorkingDirectory);
  if (!entry) return;
  if (entry) {
    if (entry.isSymbolicLink()) {
      throw new Error("refusing to remove a symlinked prior worker release");
    }
    const canonicalRoot = realpathSync(releaseRoot);
    const canonicalPrior = realpathSync(priorWorkingDirectory);
    if (
      canonicalRoot !== releaseRoot
      || !localWorkerDeployStageIsConfined(canonicalRoot, canonicalPrior)
    ) {
      throw new Error("refusing to remove a prior worker release outside its release root");
    }
  }
  if (localCoordinatorWorkingDirectory() === priorWorkingDirectory) return;
  // A release staged by rsync — every installed release — is an ordinary
  // directory, so `git worktree remove` fails it with "is not a working tree"
  // and settlement dies AFTER the new release is already serving. Ask git
  // first and fall back to a plain recursive remove; the symlink and
  // release-root confinement proofs above are what make that safe.
  const registered = await run(["git", "worktree", "list", "--porcelain"], {
    cwd: sourceRepo,
    quiet: true,
  });
  const isWorktree = registered.exit === 0
    && registered.stdout.split("\n").some((line) =>
      line === `worktree ${priorWorkingDirectory}`
    );
  const gitMetadata = lstatIfPresent(join(priorWorkingDirectory, ".git"));
  const hasInvalidGitMetadata = gitMetadata !== null && !gitMetadata.isFile();
  let removedByGit = false;
  if (isWorktree) {
    const removed = await run(["git", "worktree", "remove", "--force", priorWorkingDirectory], {
      cwd: sourceRepo,
      quiet: true,
    });
    if (removed.exit !== 0 && !hasInvalidGitMetadata) {
      throw new Error(
        `cannot retire prior worker release ${priorWorkingDirectory}: ${removed.stderr.trim() || `exit ${removed.exit}`}`,
      );
    }
    removedByGit = removed.exit === 0;
  }
  if (!removedByGit) {
    rmSync(priorWorkingDirectory, { recursive: true, force: true });
    if (isWorktree) {
      const pruned = await run(["git", "worktree", "prune"], { cwd: sourceRepo, quiet: true });
      if (pruned.exit !== 0) {
        throw new Error(`cannot prune retired worker release ${priorWorkingDirectory}: ${pruned.stderr.trim() || `exit ${pruned.exit}`}`);
      }
    }
  }
  if (existsSync(releaseRoot)) await flushDurablePath(releaseRoot);
}

export function createLocalWorkerDeployRecoveryDeps(
  servicePath: string,
  journalPath: string,
  confinement: Readonly<LocalWorkerDeployConfinement>,
  keeperCallbacks: Readonly<JournaledKeeperUpdateCallbacks>,
): LocalWorkerDeployRecoveryDeps {
  return {
    readService: () => readLocalWorkerServiceSnapshot(servicePath),
    probeLifecycle: async journal =>
      await readLocalWorkerServiceSnapshot(servicePath) === null
        ? "unloaded"
        : await probeLocalWorkerLifecycle(journal.os),
    probeStartupPolicy: async journal =>
      await readLocalWorkerServiceSnapshot(servicePath) === null
        ? "absent"
        : await probeLocalWorkerStartupPolicy(journal.os),
    checkpointRollback: journal =>
      checkpointLocalWorkerDeployJournal(journalPath, journal, confinement),
    checkpointCommit: journal =>
      checkpointLocalWorkerDeployJournal(journalPath, journal, confinement),
    restorePriorDefinition: async journal => {
      if (journal.priorService) {
        await durableWriteFile(servicePath, decodeServiceSnapshot(journal.priorService), {
          mode: journal.priorService.mode,
        });
      } else if (lstatIfPresent(servicePath)) {
        await durableRemove(servicePath);
      }
    },
    stopWorker: async journal => {
      const stopped = await stopLocalWorkerForActivation(journal.os, journal.sourceRoot);
      if (stopped.exit !== 0) throw new Error(
        `worker stop failed (exit ${stopped.exit})\n${stopped.stdout}\n${stopped.stderr}`,
      );
    },
    applyKeeperUpdate: keeperCallbacks.apply,
    proveKeeperUpdate: keeperCallbacks.prove,
    startPrior: journal => startRestoredLocalWorker(journal, servicePath),
    restorePriorLifecycle: restoreLocalWorkerPriorLifecycle,
    activateTarget: async journal => {
      const activated = await startLocalWorkerForActivation(
        journal.os,
        servicePath,
        journal.sourceRoot,
        "worker target recovery",
      );
      if (activated.exit !== 0) throw new Error(
        `target reactivation failed (exit ${activated.exit})\n${activated.stdout}\n${activated.stderr}`,
      );
    },
    cleanupStage: journal => cleanupLocalWorkerStage(journal, confinement),
    commitTarget: journal => _removeManagedPriorRelease(
      journal.sourceRoot, journal.releaseRoot, journal.priorWorkingDirectory,
    ),
    clearJournal: async () => {
      if (lstatIfPresent(journalPath)) await durableRemove(journalPath);
    },
  };
}

async function localWorkerIsRunningAtSha(
  servicePath: string, os: "linux" | "darwin", expectedSha: string,
): Promise<boolean> {
  const service = readLocalWorkerServiceSnapshot(servicePath);
  if (!service || serviceGitSha(decodeServiceSnapshot(service).toString("utf8"), os) !== expectedSha) {
    return false;
  }
  const status = await run(["bash", "-lc", verifyWorkerCmd(os)], { quiet: true });
  return status.exit === 0 && workerServiceIsRunning(status.stdout, os);
}

interface LocalWorkerJournalSettlementOptions {
  serviceDir: string;
  journalPath: string;
  confinement: Readonly<LocalWorkerDeployConfinement>;
  recoveryDeps: Readonly<LocalWorkerDeployRecoveryDeps>;
  rollout: Readonly<WorkerRolloutDirective> | null;
  servicePath: string;
  os: "linux" | "darwin";
}
export async function settleLocalWorkerDeployJournal(
  options: Readonly<LocalWorkerJournalSettlementOptions>,
): Promise<boolean> {
  const {
    serviceDir,
    journalPath,
    confinement,
    recoveryDeps,
    rollout,
    servicePath,
    os,
  } = options;
  for (const relative of [
    POSIX_WORKER_DEPLOY_JOURNAL_PATHS.linux,
    POSIX_WORKER_DEPLOY_JOURNAL_PATHS.darwin,
    POSIX_WORKER_DEPLOY_JOURNAL_PATHS.coordinator,
  ]) {
    const foreignJournal = join(serviceDir, relative);
    if (!lstatIfPresent(foreignJournal)) continue;
    if (
      relative === POSIX_WORKER_DEPLOY_JOURNAL_PATHS.coordinator
      && rollout
      && coordinatorJournalAllowsLocalWorkerRollout(serviceDir, os, rollout)
    ) {
      continue;
    }
    throw new DeployFailure(
      5,
      `cannot mutate past unsettled foreign worker deploy journal: ${foreignJournal}`,
    );
  }
  const existingJournal = readLocalWorkerDeployJournal(journalPath);
  if (existingJournal) {
    try {
      let loaded = parseLocalWorkerDeployJournal(existingJournal, confinement);
      if (loaded.rolloutId !== null && loaded.rolloutId !== rollout?.rolloutId) {
        throw new Error("another fleet rollout still owns the local worker deploy journal");
      }
      const decision = await _recoverLocalWorkerDeployJournal(
        existingJournal,
        confinement,
        recoveryDeps,
        rollout ?? undefined,
      );
      if (decision === "target-held") {
        if (rollout?.action !== "hold") {
          throw new Error("a fleet-held local worker requires its owning rollout");
        }
        if (loaded.phase === "activating") {
          loaded = { ...loaded, phase: "activated" };
          await checkpointLocalWorkerDeployJournal(journalPath, loaded, confinement);
        }
        console.log(`>> local worker target already held for fleet rollout ${rollout.rolloutId}`);
        return true;
      }
      console.log(`>> recovered interrupted local worker deploy (${decision})`);
      if (rollout?.action === "finalize") {
        if (decision !== "target-committed") {
          throw new Error("local worker target was not finalized");
        }
        return true;
      }
      if (rollout?.action === "rollback") {
        if (decision !== "prior-restored" && decision !== "prepared-cleaned") {
          throw new Error("local worker prior state was not restored");
        }
        return true;
      }
    } catch (error) {
      throw new DeployFailure(
        5,
        `cannot settle local worker deploy: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  } else if (rollout?.action === "finalize" || rollout?.action === "rollback") {
    const expectedSha = rollout.action === "finalize" ? rollout.targetSha : rollout.priorSha;
    if (!await localWorkerIsRunningAtSha(servicePath, os, expectedSha)) {
      throw new DeployFailure(5, `local worker has no journal and does not prove ${expectedSha}`);
    }
    console.log(`>> local worker already ${rollout.action === "finalize" ? "finalized" : "rolled back"}`);
    return true;
  }
  return false;
}
