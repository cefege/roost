// Coordinator-only deploy journal V4 parsing, confinement, and checkpoints.
// Legacy schema-3 fleet recovery remains in coordinator-deploy-journal.ts;
// new forward updates use only this participant-free transaction.

import { existsSync, lstatSync, readFileSync, realpathSync } from "node:fs";
import { dirname, resolve } from "node:path";
import {
  CoordinatorDeployJournalV4Schema,
  type CoordinatorDeployJournalV4,
  type CoordinatorDeployPhaseV4,
} from "@roost/shared/coordinator-deploy-state";
import { durableWriteFile } from "@roost/shared/durability";
import { DeployFailure } from "./deploy-exec.ts";
import {
  coordinatorDatabaseSnapshotPath,
  coordinatorStagedReleasePathIsSafe,
  type CoordinatorDeployJournalContext,
} from "./coordinator-deploy-journal.ts";

const MAX_JOURNAL_BYTES = 2 * 1024 * 1024;

export function parseCoordinatorDeployJournalV4(
  serialized: string,
  context: CoordinatorDeployJournalContext,
): CoordinatorDeployJournalV4 {
  if (Buffer.byteLength(serialized) > MAX_JOURNAL_BYTES) {
    throw new Error("journal exceeds the maximum size");
  }
  const journal = CoordinatorDeployJournalV4Schema.parse(JSON.parse(serialized));
  const canonicalServicePath = realCanonicalPath(context.servicePath, "servicePath", false);
  const releaseRoot = realCanonicalPath(context.releaseRoot, "releaseRoot", true);
  const transactionRoot = realCanonicalPath(context.transactionRoot, "transactionRoot", true);
  if (journal.servicePath !== canonicalServicePath) {
    throw new Error("servicePath does not match the installed coordinator service");
  }
  if (!coordinatorStagedReleasePathIsSafe(
    releaseRoot,
    journal.stagedReleasePath,
    journal.targetSha,
  )) {
    throw new Error("stagedReleasePath is outside the coordinator release root");
  }
  if (journal.databaseSnapshotPath !== coordinatorDatabaseSnapshotPath(
    transactionRoot,
    journal.rolloutId,
  )) {
    throw new Error("databaseSnapshotPath does not match the rollout transaction");
  }
  for (const [field, value] of [
    ["sourceReleasePath", journal.sourceReleasePath],
    ["stagingRepoPath", journal.stagingRepoPath],
    ["databasePath", journal.databasePath],
  ] as const) {
    if (resolve(value) !== value || value.includes("\0")) {
      throw new Error(`${field} must be a canonical absolute path`);
    }
  }
  if (dirname(journal.databaseSnapshotPath) !== transactionRoot) {
    throw new Error("database snapshot is outside the transaction root");
  }
  return journal;
}

export function loadCoordinatorDeployJournalV4(
  journalPath: string,
  context: CoordinatorDeployJournalContext,
): CoordinatorDeployJournalV4 | null {
  try {
    if (!existsSync(journalPath)) return null;
    const metadata = lstatSync(journalPath);
    if (!metadata.isFile() || metadata.isSymbolicLink()
      || metadata.size > MAX_JOURNAL_BYTES) {
      throw new Error("journal must be a bounded regular file");
    }
    return parseCoordinatorDeployJournalV4(readFileSync(journalPath, "utf8"), context);
  } catch (error) {
    throw new DeployFailure(
      5,
      `coordinator deploy journal V4 is malformed or unsafe: ${String(error)}; refusing recovery`,
    );
  }
}

export async function writeCoordinatorDeployJournalV4(
  journalPath: string,
  journal: CoordinatorDeployJournalV4,
): Promise<void> {
  const checked = CoordinatorDeployJournalV4Schema.parse(journal);
  await durableWriteFile(journalPath, `${JSON.stringify(checked)}\n`, { mode: 0o600 });
}

export async function checkpointCoordinatorDeployPhaseV4(
  journalPath: string,
  journal: CoordinatorDeployJournalV4,
  phase: CoordinatorDeployPhaseV4,
  patch: Partial<Pick<CoordinatorDeployJournalV4, "databaseSnapshotSha256">> = {},
): Promise<CoordinatorDeployJournalV4> {
  if (!validTransition(journal.phase, phase)) {
    throw new Error(`invalid coordinator deploy V4 transition ${journal.phase} -> ${phase}`);
  }
  const next = CoordinatorDeployJournalV4Schema.parse({ ...journal, ...patch, phase });
  await writeCoordinatorDeployJournalV4(journalPath, next);
  return next;
}

function validTransition(
  from: CoordinatorDeployPhaseV4,
  to: CoordinatorDeployPhaseV4,
): boolean {
  return (from === "prepared" && to === "snapshotting")
    || (from === "snapshotting" && (to === "activating" || to === "prior-restored"))
    || (from === "activating" && (to === "finalizing" || to === "rolling-back"))
    || (from === "rolling-back" && to === "prior-restored");
}

function realCanonicalPath(value: string, field: string, directory: boolean): string {
  const canonical = resolve(value);
  if (canonical !== value || realpathSync(value) !== canonical) {
    throw new Error(`${field} must be a canonical real path`);
  }
  const metadata = lstatSync(canonical);
  if (metadata.isSymbolicLink() || (directory ? !metadata.isDirectory() : !metadata.isFile())) {
    throw new Error(`${field} has the wrong file type`);
  }
  return canonical;
}
