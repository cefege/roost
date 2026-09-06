// Phase checkpoints for the coordinator self-update deploy journal.
// Schema validation and loading live in the focused schema module.
// Recovery and finalization consume this stable public journal surface.

import { durableWriteFile } from "@roost/shared/durability";
import type {
  CoordinatorDeployJournalV2,
  CoordinatorDeployPhase,
  CoordinatorDeployJournalContext,
  CoordinatorWorkerKeeperPlanV1,
} from "./coordinator-deploy-journal-schema.ts";

export {
  COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION,
  canonicalCoordinatorTargetWorkers,
  coordinatorDatabaseSnapshotPath,
  coordinatorDeployJournalPath,
  coordinatorReleasePathIsConfined,
  coordinatorStagedReleasePathIsSafe,
  loadCoordinatorDeployJournal,
  parseCoordinatorDeployJournal,
} from "./coordinator-deploy-journal-schema.ts";
export type {
  CoordinatorDeployJournalContext,
  CoordinatorDeployJournalV2,
  CoordinatorDeployPhase,
  CoordinatorWorkerKeeperPlanV1,
};
export {
  coordinatorInstallEnvironment,
  coordinatorRepoFromService,
  coordinatorRestartCommand,
  coordinatorStopCommand,
} from "./coordinator-service-definition.ts";
export type CoordinatorDeployRecoveryAction =
  | "clean-prepared"
  | "rollback-prior"
  | "finish-finalize";

export function coordinatorDeployRecoveryAction(
  phase: CoordinatorDeployPhase,
): CoordinatorDeployRecoveryAction {
  if (phase === "prepared") return "clean-prepared";
  if (phase === "finalizing") return "finish-finalize";
  return "rollback-prior";
}

export async function writeCoordinatorDeployJournal(
  journalPath: string,
  journal: CoordinatorDeployJournalV2,
): Promise<void> {
  await durableWriteFile(journalPath, `${JSON.stringify(journal)}\n`, { mode: 0o600 });
}

export async function writeCoordinatorDeployPhase(
  journalPath: string,
  journal: CoordinatorDeployJournalV2,
  phase: "activating" | "fleet-converging",
): Promise<CoordinatorDeployJournalV2> {
  const validTransition = (journal.phase === "prepared" && phase === "activating")
    || (journal.phase === "activating" && phase === "fleet-converging");
  if (!validTransition) {
    throw new Error(`invalid coordinator deploy phase transition ${journal.phase} -> ${phase}`);
  }
  const next = { ...journal, phase };
  await writeCoordinatorDeployJournal(journalPath, next);
  return next;
}

export async function checkpointCoordinatorFinalizationDecision(
  journalPath: string,
  journal: CoordinatorDeployJournalV2,
): Promise<CoordinatorDeployJournalV2> {
  if (journal.phase !== "fleet-converging") {
    throw new Error(`cannot finalize coordinator deploy from ${journal.phase}`);
  }
  const next = { ...journal, phase: "finalizing" as const };
  await writeCoordinatorDeployJournal(journalPath, next);
  return next;
}


