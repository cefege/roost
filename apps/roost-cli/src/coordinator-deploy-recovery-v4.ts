// Crash recovery for participant-free coordinator journal V4. Database restore
// is single-shot: prior-restored is checkpointed before the prior definition or
// service can admit writes, and that phase never restores the snapshot again.

import { readFileSync } from "node:fs";
import { dirname } from "node:path";
import { coordServiceLabel } from "@roost/shared/paths";
import {
  durableRemove,
  durableWriteFile,
  flushDurablePath,
} from "@roost/shared/durability";
import type { CoordinatorDeployJournalV4 } from "@roost/shared/coordinator-deploy-state";
import { DeployFailure, run } from "./deploy-exec.ts";
import type { CoordinatorDeployJournalContext } from "./coordinator-deploy-journal.ts";
import {
  checkpointCoordinatorDeployPhaseV4,
  loadCoordinatorDeployJournalV4,
} from "./coordinator-deploy-journal-v4.ts";
import {
  coordinatorInstallEnvironment,
  coordinatorRepoFromService,
  coordinatorRestartCommand,
  coordinatorStopCommand,
} from "./coordinator-service-definition.ts";
import {
  removeStagedCoordinatorRelease,
  retirePriorCoordinatorRelease,
} from "./coordinator-deploy-release.ts";
import { restoreCoordinatorDatabaseFromSnapshot } from "./coordinator-deploy-snapshot.ts";
import { statusReport } from "./status.ts";

const RECOVERY_TIMEOUT_MS = 60_000;
const RECOVERY_POLL_MS = 250;
export type CoordinatorDeployRecoveryV4Outcome =
  | "none"
  | "prepared-cleaned"
  | "prior-restored"
  | "target-finalized";


export async function recoverCoordinatorDeployV4(
  journalPath: string,
  context: CoordinatorDeployJournalContext,
): Promise<CoordinatorDeployRecoveryV4Outcome> {
  let journal = loadCoordinatorDeployJournalV4(journalPath, context);
  if (!journal) return "none";
  if (journal.phase === "prepared") {
    await cleanupUnactivated(journalPath, context, journal);
    return "prepared-cleaned";
  }
  if (journal.phase === "finalizing") {
    await proveTarget(journal);
    await finishTarget(journalPath, context, journal);
    return "target-finalized";
  }
  if (journal.phase === "snapshotting") {
    journal = await checkpointCoordinatorDeployPhaseV4(
      journalPath,
      journal,
      "prior-restored",
    );
    await restorePriorService(context, journal);
    await cleanupPriorRestored(journalPath, context, journal);
    return "prior-restored";
  }
  if (journal.phase === "activating") {
    const report = await statusReport();
    if (report.coord.reachable && report.coord.gitSha === journal.priorSha) {
      throw new DeployFailure(
        8,
        "prior coordinator is already admitting writes while activation remains rollback-eligible; journal retained",
      );
    }
    journal = await checkpointCoordinatorDeployPhaseV4(
      journalPath,
      journal,
      "rolling-back",
    );
  }
  if (journal.phase === "rolling-back") {
    await stopCurrentCoordinator(context, journal);
    await restoreCoordinatorDatabaseFromSnapshot(
      journal.databasePath,
      journal.databaseSnapshotPath,
      requiredSnapshotDigest(journal),
      async () => {},
    );
    journal = await checkpointCoordinatorDeployPhaseV4(
      journalPath,
      journal,
      "prior-restored",
    );
  }
  if (journal.phase === "prior-restored") {
    await restorePriorService(context, journal);
    await cleanupPriorRestored(journalPath, context, journal);
    return "prior-restored";
  }
  throw new DeployFailure(8, `unsupported coordinator V4 recovery phase ${journal.phase}`);
}

async function stopCurrentCoordinator(
  context: CoordinatorDeployJournalContext,
  journal: CoordinatorDeployJournalV4,
): Promise<void> {
  const stopped = await run(["bash", "-lc", coordinatorStopCommand(
    context.platform,
    coordServiceLabel(process.env, context.platform),
  )], { quiet: true });
  if (stopped.exit !== 0) {
    throw new DeployFailure(8, `coordinator rollback stop failed (exit ${stopped.exit})`);
  }
  const deadline = Date.now() + RECOVERY_TIMEOUT_MS;
  while ((await statusReport()).coord.reachable) {
    if (Date.now() >= deadline) {
      throw new DeployFailure(8, "coordinator target did not stop before database restore");
    }
    await Bun.sleep(RECOVERY_POLL_MS);
  }
  const installed = readFileSync(journal.servicePath, "utf8");
  const root = coordinatorRepoFromService(installed, context.platform);
  const environment = coordinatorInstallEnvironment(installed, context.platform);
  const sha = environment.ROOST_GIT_SHA ?? environment.GIT_SHA;
  if ((root !== journal.stagedReleasePath && root !== journal.sourceReleasePath)
    || (sha !== journal.targetSha && sha !== journal.priorSha)) {
    throw new DeployFailure(8, "foreign coordinator service definition blocks rollback");
  }
}

async function restorePriorService(
  context: CoordinatorDeployJournalContext,
  journal: CoordinatorDeployJournalV4,
): Promise<void> {
  const installed = readFileSync(journal.servicePath, "utf8");
  const installedRoot = coordinatorRepoFromService(installed, context.platform);
  if (installedRoot !== journal.stagedReleasePath
    && installedRoot !== journal.sourceReleasePath) {
    throw new DeployFailure(8, "foreign coordinator service definition blocks prior restore");
  }
  await durableWriteFile(
    journal.servicePath,
    Buffer.from(journal.priorDefinitionBase64, "base64"),
    { mode: journal.priorDefinitionMode },
  );
  const restarted = await run(["bash", "-lc", coordinatorRestartCommand(
    journal.servicePath,
    context.platform,
    coordServiceLabel(process.env, context.platform),
  )], { quiet: true });
  if (restarted.exit !== 0) {
    throw new DeployFailure(8, `prior coordinator restart failed (exit ${restarted.exit})`);
  }
  const deadline = Date.now() + RECOVERY_TIMEOUT_MS;
  for (;;) {
    const report = await statusReport();
    if (report.coord.reachable && report.coord.gitSha === journal.priorSha) return;
    if (Date.now() >= deadline) {
      throw new DeployFailure(8, `prior coordinator did not prove ${journal.priorSha}`);
    }
    await Bun.sleep(RECOVERY_POLL_MS);
  }
}

async function proveTarget(journal: CoordinatorDeployJournalV4): Promise<void> {
  const report = await statusReport();
  if (!report.coord.reachable
    || report.coord.gitSha !== journal.targetSha
    || report.coord.updateReady !== true
    || report.coord.updateTransactionId !== journal.rolloutId) {
    throw new DeployFailure(8, "finalizing coordinator target identity is unavailable");
  }
}

async function cleanupUnactivated(
  journalPath: string,
  context: CoordinatorDeployJournalContext,
  journal: CoordinatorDeployJournalV4,
): Promise<void> {
  await removeStagedCoordinatorRelease(
    context.releaseRoot,
    journal.stagingRepoPath,
    journal.stagedReleasePath,
    journal.targetSha,
  );
  await durableRemove(journal.databaseSnapshotPath).catch(() => undefined);
  await durableRemove(journalPath);
}

async function cleanupPriorRestored(
  journalPath: string,
  context: CoordinatorDeployJournalContext,
  journal: CoordinatorDeployJournalV4,
): Promise<void> {
  await removeStagedCoordinatorRelease(
    context.releaseRoot,
    journal.stagingRepoPath,
    journal.stagedReleasePath,
    journal.targetSha,
  );
  await durableRemove(journal.databaseSnapshotPath).catch(() => undefined);
  await durableRemove(journalPath);
}

async function finishTarget(
  journalPath: string,
  context: CoordinatorDeployJournalContext,
  journal: CoordinatorDeployJournalV4,
): Promise<void> {
  await flushDurablePath(journal.servicePath);
  await flushDurablePath(dirname(journal.servicePath));
  await retirePriorCoordinatorRelease(context.releaseRoot, journal, context.platform);
  await durableRemove(journal.databaseSnapshotPath);
  await durableRemove(journalPath);
}

function requiredSnapshotDigest(journal: CoordinatorDeployJournalV4): string {
  if (!journal.databaseSnapshotSha256) {
    throw new DeployFailure(8, "coordinator rollback snapshot digest is absent");
  }
  return journal.databaseSnapshotSha256;
}
