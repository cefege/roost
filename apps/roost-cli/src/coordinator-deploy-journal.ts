// Schema, confinement, parsing, and phase bookkeeping for the coordinator
// self-update deploy journal (coordinator-deploy.json under transactions/).
// Version 2 binds one rollout to its exact worker set and live SQLite rollback
// snapshot. Runtime rollback/finalization lives in coordinator-deploy-recovery.ts.
import { existsSync, lstatSync, readFileSync, realpathSync } from "node:fs";

import { dirname, isAbsolute, join, relative } from "node:path";
import { durableWriteFile } from "@roost/shared/durability";
import { DeployFailure } from "./deploy-exec.ts";
import {
  coordinatorInstallEnvironment,
  coordinatorRepoFromService,
} from "./coordinator-service-definition.ts";
export {
  coordinatorInstallEnvironment,
  coordinatorRepoFromService,
  coordinatorRestartCommand,
  coordinatorStopCommand,
} from "./coordinator-service-definition.ts";
import {
  POSIX_FULL_GIT_SHA_RE,
  POSIX_RELEASE_ID_SUFFIX_RE,
  isResolvedCanonicalAbsolutePath,
  posixJournalObjectValue,
} from "./posix-deploy-journal.ts";


export const COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION = 2 as const;
const COORDINATOR_DEPLOY_JOURNAL_MAX_BYTES = 2 * 1024 * 1024;
const COORDINATOR_SERVICE_DEFINITION_MAX_BYTES = 1024 * 1024;
const SNAPSHOT_SHA256_RE = /^[0-9a-f]{64}$/;
const WORKER_FINGERPRINT_RE = /^[0-9a-f]{64}$/;

export type CoordinatorDeployPhase =
  | "prepared"
  | "activating"
  | "fleet-converging"
  | "finalizing";

export interface CoordinatorDeployJournalV2 {
  schemaVersion: typeof COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION;
  phase: CoordinatorDeployPhase;
  rolloutId: string;
  targetWorkerFingerprints: string[];
  priorDefinitionBase64: string;
  priorDefinitionMode: number;
  priorSha: string;
  targetSha: string;
  servicePath: string;
  sourceReleasePath: string;
  stagingRepoPath: string;
  stagedReleasePath: string;
  databasePath: string;
  databaseSnapshotPath: string;
  databaseSnapshotSha256: string;
}

export interface CoordinatorDeployJournalContext {
  servicePath: string;
  releaseRoot: string;
  transactionRoot: string;
  platform: "darwin" | "linux";
}

export type CoordinatorDeployRecoveryAction =
  | "clean-prepared"
  | "rollback-prior"
  | "finish-finalize";

function journalString(
  value: Partial<Record<keyof CoordinatorDeployJournalV2, unknown>>,
  key: keyof CoordinatorDeployJournalV2,
): string {
  const field = value[key];
  if (typeof field !== "string" || field.length === 0 || field.includes("\0")) {
    throw new Error(`${key} must be a non-empty string`);
  }
  return field;
}

function canonicalAbsolutePath(value: string, field: string): string {
  if (!isResolvedCanonicalAbsolutePath(value)) {
    throw new Error(`${field} must be a canonical absolute path`);
  }
  return value;
}

export function coordinatorReleasePathIsConfined(
  releaseRoot: string,
  candidate: string,
): boolean {
  if (!isResolvedCanonicalAbsolutePath(releaseRoot)
    || !isResolvedCanonicalAbsolutePath(candidate)) {
    return false;
  }
  const child = relative(releaseRoot, candidate);
  return child.length > 0
    && !isAbsolute(child)
    && child !== ".."
    && !child.startsWith("../")
    && !child.startsWith("..\\");
}

export function coordinatorStagedReleasePathIsSafe(
  releaseRoot: string,
  stagedReleasePath: string,
  targetSha: string,
): boolean {
  if (!coordinatorReleasePathIsConfined(releaseRoot, stagedReleasePath)
    || dirname(stagedReleasePath) !== releaseRoot) {
    return false;
  }
  const releaseId = relative(releaseRoot, stagedReleasePath);
  return releaseId.startsWith(`${targetSha}-`)
    && POSIX_RELEASE_ID_SUFFIX_RE.test(releaseId.slice(targetSha.length + 1));
}

export function canonicalCoordinatorTargetWorkers(
  workerFingerprints: readonly string[],
): string[] {
  if (workerFingerprints.length > 4_096) {
    throw new Error("targetWorkerFingerprints exceeds the maximum count");
  }
  const canonical = [...workerFingerprints];
  for (const fingerprint of canonical) {
    if (!WORKER_FINGERPRINT_RE.test(fingerprint)) {
      throw new Error("targetWorkerFingerprints contains an invalid worker fingerprint");
    }
  }
  canonical.sort();
  for (let index = 1; index < canonical.length; index++) {
    if (canonical[index] === canonical[index - 1]) {
      throw new Error("targetWorkerFingerprints contains a duplicate worker fingerprint");
    }
  }
  return canonical;
}

export function coordinatorDeployJournalPath(transactionRoot: string): string {
  return join(transactionRoot, "coordinator-deploy.json");
}

export function coordinatorDatabaseSnapshotPath(
  transactionRoot: string,
  rolloutId: string,
): string {
  return join(transactionRoot, `coordinator-deploy-${rolloutId}.db.gz`);
}


export function parseCoordinatorDeployJournal(
  serialized: string,
  context: CoordinatorDeployJournalContext,
): CoordinatorDeployJournalV2 {
  if (Buffer.byteLength(serialized) > COORDINATOR_DEPLOY_JOURNAL_MAX_BYTES) {
    throw new Error("journal exceeds the maximum size");
  }
  let value: unknown;
  try {
    value = JSON.parse(serialized);
  } catch (error) {
    throw new Error(
      `journal is not valid JSON: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  posixJournalObjectValue(value, "journal root must be an object");
  const fields = value as Partial<Record<keyof CoordinatorDeployJournalV2, unknown>>;
  if (fields.schemaVersion !== COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION) {
    throw new Error(`unsupported coordinator deploy journal schema ${String(fields.schemaVersion)}`);
  }
  const phase = fields.phase;
  if (phase !== "prepared"
    && phase !== "activating"
    && phase !== "fleet-converging"
    && phase !== "finalizing") {
    throw new Error(`invalid coordinator deploy phase ${String(phase)}`);
  }

  const expectedServicePath = canonicalAbsolutePath(context.servicePath, "expected servicePath");
  const releaseRoot = canonicalAbsolutePath(context.releaseRoot, "releaseRoot");
  const transactionRoot = canonicalAbsolutePath(context.transactionRoot, "transactionRoot");
  const servicePath = canonicalAbsolutePath(journalString(fields, "servicePath"), "servicePath");
  if (servicePath !== expectedServicePath) {
    throw new Error("servicePath does not match the installed coordinator service");
  }

  const rolloutId = journalString(fields, "rolloutId");
  if (!POSIX_RELEASE_ID_SUFFIX_RE.test(rolloutId)) {
    throw new Error("rolloutId is not a lowercase UUIDv4");
  }
  const providedWorkers = fields.targetWorkerFingerprints;
  if (!Array.isArray(providedWorkers)
    || !providedWorkers.every((fingerprint) => typeof fingerprint === "string")) {
    throw new Error("targetWorkerFingerprints must be an array of worker fingerprints");
  }
  const targetWorkerFingerprints = canonicalCoordinatorTargetWorkers(providedWorkers);
  if (providedWorkers.some((fingerprint, index) => fingerprint !== targetWorkerFingerprints[index])) {
    throw new Error("targetWorkerFingerprints must be sorted in canonical order");
  }

  const targetSha = journalString(fields, "targetSha");
  if (!POSIX_FULL_GIT_SHA_RE.test(targetSha)) throw new Error("targetSha is not a full Git SHA");
  const priorSha = journalString(fields, "priorSha");
  if (!POSIX_FULL_GIT_SHA_RE.test(priorSha)) throw new Error("priorSha is not a full Git SHA");

  const sourceReleasePath = canonicalAbsolutePath(
    journalString(fields, "sourceReleasePath"),
    "sourceReleasePath",
  );
  const stagingRepoPath = canonicalAbsolutePath(
    journalString(fields, "stagingRepoPath"),
    "stagingRepoPath",
  );
  const stagedReleasePath = canonicalAbsolutePath(
    journalString(fields, "stagedReleasePath"),
    "stagedReleasePath",
  );
  if (!coordinatorStagedReleasePathIsSafe(releaseRoot, stagedReleasePath, targetSha)) {
    throw new Error("stagedReleasePath is outside the coordinator release root or has an invalid release ID");
  }
  if (sourceReleasePath === stagedReleasePath || stagingRepoPath === stagedReleasePath) {
    throw new Error("source and staging repository paths must differ from stagedReleasePath");
  }

  const databasePath = canonicalAbsolutePath(
    journalString(fields, "databasePath"),
    "databasePath",
  );
  const databaseSnapshotPath = canonicalAbsolutePath(
    journalString(fields, "databaseSnapshotPath"),
    "databaseSnapshotPath",
  );
  const expectedSnapshotPath = coordinatorDatabaseSnapshotPath(transactionRoot, rolloutId);
  if (databaseSnapshotPath !== expectedSnapshotPath || databaseSnapshotPath === databasePath) {
    throw new Error("databaseSnapshotPath does not match the rollout transaction path");
  }
  const databaseSnapshotSha256 = journalString(fields, "databaseSnapshotSha256");
  if (!SNAPSHOT_SHA256_RE.test(databaseSnapshotSha256)) {
    throw new Error("databaseSnapshotSha256 is not a lowercase SHA-256 digest");
  }

  const priorDefinitionMode = fields.priorDefinitionMode;
  if (!Number.isInteger(priorDefinitionMode)
    || (priorDefinitionMode as number) < 0
    || (priorDefinitionMode as number) > 0o777) {
    throw new Error("priorDefinitionMode is invalid");
  }
  const priorDefinitionBase64 = journalString(fields, "priorDefinitionBase64");
  const priorDefinition = Buffer.from(priorDefinitionBase64, "base64");
  if (priorDefinition.length === 0
    || priorDefinition.length > COORDINATOR_SERVICE_DEFINITION_MAX_BYTES
    || priorDefinition.toString("base64") !== priorDefinitionBase64) {
    throw new Error("priorDefinitionBase64 is not a canonical service definition");
  }
  let priorDefinitionText: string;
  try {
    priorDefinitionText = new TextDecoder("utf-8", { fatal: true }).decode(priorDefinition);
  } catch {
    throw new Error("prior coordinator service definition is not valid UTF-8");
  }
  if (priorDefinitionText.includes("\0")) {
    throw new Error("prior coordinator service definition contains a NUL byte");
  }
  const definitionSource = coordinatorRepoFromService(priorDefinitionText, context.platform);
  if (!definitionSource
    || canonicalAbsolutePath(definitionSource, "prior definition WorkingDirectory")
      !== sourceReleasePath) {
    throw new Error("prior coordinator service definition does not match sourceReleasePath");
  }
  const definitionEnvironment = coordinatorInstallEnvironment(priorDefinitionText, context.platform);
  const definitionSha = definitionEnvironment.ROOST_GIT_SHA
    ?? definitionEnvironment.GIT_SHA
    ?? null;
  if (definitionSha !== priorSha) {
    throw new Error("prior coordinator service definition does not match priorSha");
  }
  const definitionDatabasePath = definitionEnvironment.ROOST_COORDINATOR_DB;
  if (!definitionDatabasePath
    || canonicalAbsolutePath(definitionDatabasePath, "prior definition ROOST_COORDINATOR_DB")
      !== databasePath) {
    throw new Error("prior coordinator service definition does not match databasePath");
  }

  return {
    schemaVersion: COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION,
    phase,
    rolloutId,
    targetWorkerFingerprints,
    priorDefinitionBase64,
    priorDefinitionMode: priorDefinitionMode as number,
    priorSha,
    targetSha,
    servicePath,
    sourceReleasePath,
    stagingRepoPath,
    stagedReleasePath,
    databasePath,
    databaseSnapshotPath,
    databaseSnapshotSha256,
  };
}

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

export function loadCoordinatorDeployJournal(
  journalPath: string,
  context: CoordinatorDeployJournalContext,
): CoordinatorDeployJournalV2 | null {
  try {
    const transactionRoot = canonicalAbsolutePath(context.transactionRoot, "transactionRoot");
    if (!existsSync(transactionRoot)
      || !lstatSync(transactionRoot).isDirectory()
      || lstatSync(transactionRoot).isSymbolicLink()
      || realpathSync(transactionRoot) !== transactionRoot) {
      throw new Error("transactionRoot must be a canonical real directory");
    }
    if (journalPath !== coordinatorDeployJournalPath(transactionRoot)) {
      throw new Error("journalPath does not match the coordinator transaction root");
    }
    if (!existsSync(journalPath)) return null;
    const metadata = lstatSync(journalPath);
    if (!metadata.isFile() || metadata.isSymbolicLink()
      || metadata.size > COORDINATOR_DEPLOY_JOURNAL_MAX_BYTES) {
      throw new Error("journal must be a bounded regular file");
    }
    return parseCoordinatorDeployJournal(readFileSync(journalPath, "utf8"), context);
  } catch (error) {
    throw new DeployFailure(
      5,
      `coordinator deploy journal is malformed or unsafe: ` +
        `${error instanceof Error ? error.message : String(error)}; refusing recovery`,
    );
  }
}

