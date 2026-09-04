// Coordinator self-update rollback and interrupted rollout recovery.
// Fleet convergence/finalization, database snapshot mechanics, and immutable
// release cleanup live in focused siblings and are re-exported as one deploy API.

import { coordServiceLabel } from "@roost/shared/paths";
import { durableRemove, durableWriteFile } from "@roost/shared/durability";
import { DeployFailure } from "./deploy-exec.ts";
import {
  coordinatorDeployRecoveryAction,
  coordinatorRestartCommand,
  coordinatorStopCommand,
  loadCoordinatorDeployJournal,
  type CoordinatorDeployJournalContext,
  type CoordinatorDeployJournalV2,
} from "./coordinator-deploy-journal.ts";
import {
  flushCoordinatorReleaseTree,
  removeStagedCoordinatorRelease,
} from "./coordinator-deploy-release.ts";
import { restoreCoordinatorDatabaseFromSnapshot } from "./coordinator-deploy-snapshot.ts";
import {
  coordinatorDeployRuntime,
  finalizeCoordinatorDeploy,
  waitForCoordinatorProof,
  type CoordinatorDeployRuntimeOptions,
} from "./coordinator-deploy-finalization.ts";

export { flushCoordinatorReleaseTree, removeStagedCoordinatorRelease };
export {
  VERIFY_POLL_MS,
  VERIFY_TIMEOUT_MS,
  beginCoordinatorDeployFinalization,
  coordinatorFleetConvergenceProblems,
  coordinatorReportIsHealthy,
  coordinatorReportIsOperational,
  coordinatorStartupPolicyIsEnabled,
  currentServiceTargetsPriorRelease,
  currentServiceTargetsRelease,
  finalizeCoordinatorDeploy,
  markCoordinatorFleetConverging,
} from "./coordinator-deploy-finalization.ts";

export interface CoordinatorDeployRecoveryOptions extends CoordinatorDeployRuntimeOptions {
  rollbackFleet?: (journal: CoordinatorDeployJournalV2) => Promise<void>;
  finishFleetFinalization?: (journal: CoordinatorDeployJournalV2) => Promise<void>;
}


async function cleanupPreparedCoordinatorDeploy(
  journalPath: string,
  context: CoordinatorDeployJournalContext,
  journal: CoordinatorDeployJournalV2,
): Promise<void> {
  await removeStagedCoordinatorRelease(
    context.releaseRoot,
    journal.stagingRepoPath,
    journal.stagedReleasePath,
    journal.targetSha,
  );
  await durableRemove(journal.databaseSnapshotPath);
  await durableRemove(journalPath);
}

export async function rollbackCoordinatorDeploy(
  journalPath: string,
  context: CoordinatorDeployJournalContext,
  options: CoordinatorDeployRuntimeOptions = {},
): Promise<void> {
  const journal = loadCoordinatorDeployJournal(journalPath, context);
  if (!journal) return;
  if (journal.phase === "prepared") {
    await cleanupPreparedCoordinatorDeploy(journalPath, context, journal);
    return;
  }
  if (journal.phase === "finalizing") {
    throw new DeployFailure(8, "coordinator rollout is already durably finalizing");
  }
  const runtime = coordinatorDeployRuntime(options);
  await restoreCoordinatorDatabaseFromSnapshot(
    journal.databasePath,
    journal.databaseSnapshotPath,
    journal.databaseSnapshotSha256,
    async () => {
      const stopped = await runtime.runCommand(
        ["bash", "-lc", coordinatorStopCommand(
          context.platform,
          coordServiceLabel(process.env, context.platform),
        )],
        { cwd: context.releaseRoot, quiet: true },
      );
      if (stopped.exit !== 0) {
        throw new Error(`coordinator rollback stop failed (exit ${stopped.exit})`);
      }
    },
  );
  await durableWriteFile(
    journal.servicePath,
    Buffer.from(journal.priorDefinitionBase64, "base64"),
    { mode: journal.priorDefinitionMode },
  );
  const restarted = await runtime.runCommand(
    ["bash", "-lc", coordinatorRestartCommand(
      journal.servicePath,
      context.platform,
      coordServiceLabel(process.env, context.platform),
    )],
    { cwd: context.releaseRoot, quiet: true },
  );
  if (restarted.exit !== 0) {
    throw new DeployFailure(8, `coordinator rollback restart failed (exit ${restarted.exit})`);
  }
  if (!await waitForCoordinatorProof(journal, context, "prior", runtime)) {
    throw new DeployFailure(8, `restored coordinator did not prove prior SHA ${journal.priorSha}`);
  }
  await removeStagedCoordinatorRelease(
    context.releaseRoot,
    journal.stagingRepoPath,
    journal.stagedReleasePath,
    journal.targetSha,
  );
  await durableRemove(journalPath);
  await durableRemove(journal.databaseSnapshotPath).catch((error) => {
    console.warn(`warning: could not remove orphan coordinator snapshot: ${String(error)}`);
  });
}


export async function recoverCoordinatorDeploy(
  journalPath: string,
  context: CoordinatorDeployJournalContext,
  options: CoordinatorDeployRecoveryOptions = {},
): Promise<void> {
  const journal = loadCoordinatorDeployJournal(journalPath, context);
  if (!journal) return;
  console.log(`\n>> recover interrupted coordinator deployment (${journal.phase})`);
  const action = coordinatorDeployRecoveryAction(journal.phase);
  if (action === "clean-prepared") {
    await cleanupPreparedCoordinatorDeploy(journalPath, context, journal);
    return;
  }
  if (action === "finish-finalize") {
    if (!options.finishFleetFinalization) {
      throw new DeployFailure(8, "finalizing coordinator recovery requires fleet finalization");
    }
    await finalizeCoordinatorDeploy(
      journalPath,
      context,
      options.finishFleetFinalization,
      options,
    );
    return;
  }
  if (journal.phase === "fleet-converging" && !options.rollbackFleet) {
    throw new DeployFailure(8, "fleet-converging coordinator recovery requires fleet rollback");
  }
  if (options.rollbackFleet) await options.rollbackFleet(journal);
  try {
    await rollbackCoordinatorDeploy(journalPath, context, options);
  } catch (error) {
    throw new DeployFailure(
      8,
      `interrupted coordinator deployment could not be recovered\n` +
        `${error instanceof Error ? error.message : String(error)}`,
    );
  }
}
