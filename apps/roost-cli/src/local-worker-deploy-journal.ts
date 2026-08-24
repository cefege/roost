// Schema, confinement, parsing, and recovery decision for the localhost
// worker deploy journal (worker-deploy.json beside the machine transaction).
// Pure logic plus service-definition inspection only: journal file IO,
// lifecycle probes, and rollback commands stay in deploy-local.ts, which is
// this module's sole runtime caller (tests pin the parse/decision surface).
// Built on the shared posix-deploy-journal core.

import { dirname, join, resolve } from "node:path";
import { roostServiceDir } from "@roost/shared/paths";
import { parsePosixServiceEnvironment } from "./deploy-plist-env.ts";
import {
  isResolvedCanonicalAbsolutePath,
  posixDeployJournalDecision,
  posixJournalObjectValue,
} from "./posix-deploy-journal.ts";

export const LOCAL_WORKER_DEPLOY_JOURNAL_SCHEMA_VERSION = 1;
const LOCAL_WORKER_DEPLOY_JOURNAL_FILE = "worker-deploy.json";

export type LocalWorkerDeployPhase = "prepared" | "activating" | "activated";
export type LocalWorkerLifecycle = "running" | "stopped" | "unknown";

export interface LocalWorkerServiceSnapshot {
  definitionBase64: string;
  mode: number;
}

export interface LocalWorkerDeployJournalV1 {
  schemaVersion: 1;
  phase: LocalWorkerDeployPhase;
  os: "linux" | "darwin";
  sourceRoot: string;
  releaseRoot: string;
  stagedReleasePath: string;
  targetSha: string;
  priorService: LocalWorkerServiceSnapshot | null;
  priorWasRunning: boolean;
  priorWorkingDirectory: string | null;
  priorGitSha: string | null;
  targetService: LocalWorkerServiceSnapshot | null;
}

export interface LocalWorkerDeployConfinement {
  os: "linux" | "darwin";
  sourceRoot: string;
  releaseRoot: string;
}

export interface LocalWorkerDeployRecoveryDeps {
  readService: (
    journal: Readonly<LocalWorkerDeployJournalV1>,
  ) => LocalWorkerServiceSnapshot | null | Promise<LocalWorkerServiceSnapshot | null>;
  probeLifecycle: (
    journal: Readonly<LocalWorkerDeployJournalV1>,
  ) => LocalWorkerLifecycle | Promise<LocalWorkerLifecycle>;
  restorePrior: (journal: Readonly<LocalWorkerDeployJournalV1>) => Promise<void>;
  cleanupStage: (journal: Readonly<LocalWorkerDeployJournalV1>) => Promise<void>;
  commitTarget: (journal: Readonly<LocalWorkerDeployJournalV1>) => Promise<void>;
  clearJournal: () => Promise<void>;
  sleep?: (milliseconds: number) => Promise<void>;
  proofAttempts?: number;
}

export type LocalWorkerDeployRecoveryDecision =
  | "prepared-cleaned"
  | "target-committed"
  | "prior-restored";

function objectValue(value: unknown, label: string): Record<string, unknown> {
  return posixJournalObjectValue(value, `${label} must be an object`);
}

function nullableString(value: unknown, label: string): string | null {
  if (value === null) return null;
  if (typeof value !== "string") throw new Error(`${label} must be a string or null`);
  return value;
}

function parseServiceSnapshot(
  value: unknown,
  label: string,
): LocalWorkerServiceSnapshot | null {
  if (value === null) return null;
  const snapshot = objectValue(value, label);
  if (typeof snapshot.definitionBase64 !== "string") {
    throw new Error(`${label}.definitionBase64 must be a string`);
  }
  if (snapshot.definitionBase64.length > 2 * 1024 * 1024) {
    throw new Error(`${label}.definitionBase64 is too large`);
  }
  const decoded = Buffer.from(snapshot.definitionBase64, "base64");
  if (decoded.toString("base64") !== snapshot.definitionBase64) {
    throw new Error(`${label}.definitionBase64 is not canonical base64`);
  }
  if (!Number.isInteger(snapshot.mode) || (snapshot.mode as number) < 0 || (snapshot.mode as number) > 0o777) {
    throw new Error(`${label}.mode is invalid`);
  }
  return {
    definitionBase64: snapshot.definitionBase64,
    mode: snapshot.mode as number,
  };
}

export function decodeServiceSnapshot(snapshot: Readonly<LocalWorkerServiceSnapshot>): Buffer {
  return Buffer.from(snapshot.definitionBase64, "base64");
}

export function serviceSnapshotMatches(
  actual: Readonly<LocalWorkerServiceSnapshot> | null,
  expected: Readonly<LocalWorkerServiceSnapshot> | null,
): boolean {
  return actual === null
    ? expected === null
    : expected !== null
      && actual.mode === expected.mode
      && actual.definitionBase64 === expected.definitionBase64;
}

export function normalizedMetadataPath(value: string | null): string | null {
  return value && isResolvedCanonicalAbsolutePath(value) ? value : null;
}

function assertNormalizedAbsolutePath(value: string, label: string): void {
  if (!isResolvedCanonicalAbsolutePath(value)) {
    throw new Error(`${label} must be a normalized absolute path`);
  }
}

export function serviceGitSha(
  definition: string,
  os: "linux" | "darwin",
): string | null {
  const environment = parsePosixServiceEnvironment(definition, os);
  const value = environment.GIT_SHA ?? environment.ROOST_GIT_SHA;
  return value === undefined || value.length === 0 ? null : value;
}

export function localWorkerDeployStageIsConfined(
  releaseRoot: string,
  stagedReleasePath: string,
): boolean {
  return isResolvedCanonicalAbsolutePath(releaseRoot)
    && isResolvedCanonicalAbsolutePath(stagedReleasePath)
    && dirname(stagedReleasePath) === releaseRoot;
}

export function parseLocalWorkerDeployJournal(
  raw: string,
  confinement: Readonly<LocalWorkerDeployConfinement>,
): LocalWorkerDeployJournalV1 {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new Error(`worker deploy journal is malformed JSON: ${String(error)}`);
  }
  const value = objectValue(parsed, "worker deploy journal");
  if (value.schemaVersion !== LOCAL_WORKER_DEPLOY_JOURNAL_SCHEMA_VERSION) {
    throw new Error("worker deploy journal schema version is unsupported");
  }
  if (value.phase !== "prepared" && value.phase !== "activating" && value.phase !== "activated") {
    throw new Error("worker deploy journal phase is invalid");
  }
  if (value.os !== "linux" && value.os !== "darwin") {
    throw new Error("worker deploy journal OS is invalid");
  }
  const sourceRoot = value.sourceRoot;
  const releaseRoot = value.releaseRoot;
  const stagedReleasePath = value.stagedReleasePath;
  if (typeof sourceRoot !== "string") throw new Error("journal.sourceRoot must be a string");
  if (typeof releaseRoot !== "string") throw new Error("journal.releaseRoot must be a string");
  if (typeof stagedReleasePath !== "string") {
    throw new Error("journal.stagedReleasePath must be a string");
  }
  assertNormalizedAbsolutePath(sourceRoot, "journal.sourceRoot");
  assertNormalizedAbsolutePath(releaseRoot, "journal.releaseRoot");
  assertNormalizedAbsolutePath(stagedReleasePath, "journal.stagedReleasePath");
  assertNormalizedAbsolutePath(confinement.sourceRoot, "expected source root");
  assertNormalizedAbsolutePath(confinement.releaseRoot, "expected release root");
  if (value.os !== confinement.os) throw new Error("worker deploy journal OS does not match this host");
  if (sourceRoot !== confinement.sourceRoot) {
    throw new Error("worker deploy journal source root does not match this deployment");
  }
  if (releaseRoot !== confinement.releaseRoot) {
    throw new Error("worker deploy journal release root does not match this deployment");
  }
  if (!localWorkerDeployStageIsConfined(releaseRoot, stagedReleasePath)) {
    throw new Error("worker deploy journal staged release path is unsafe");
  }
  if (typeof value.targetSha !== "string" || !/^[0-9a-f]{40}$/i.test(value.targetSha)) {
    throw new Error("worker deploy journal target SHA is invalid");
  }
  const stagedReleaseId = stagedReleasePath.slice(releaseRoot.length + 1);
  if (!new RegExp(
    `^${value.targetSha}-[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$`,
    "i",
  ).test(stagedReleaseId)) {
    throw new Error("worker deploy journal staged release identifier is invalid");
  }
  if (typeof value.priorWasRunning !== "boolean") {
    throw new Error("worker deploy journal prior running state is invalid");
  }
  const priorService = parseServiceSnapshot(value.priorService, "journal.priorService");
  const targetService = parseServiceSnapshot(value.targetService, "journal.targetService");
  const priorWorkingDirectory = nullableString(
    value.priorWorkingDirectory,
    "journal.priorWorkingDirectory",
  );
  const priorGitSha = nullableString(value.priorGitSha, "journal.priorGitSha");
  if (!priorService && value.priorWasRunning) {
    throw new Error("worker deploy journal cannot mark an absent prior service as running");
  }
  if (!priorService && (priorWorkingDirectory !== null || priorGitSha !== null)) {
    throw new Error("worker deploy journal has metadata for an absent prior service");
  }
  if (priorService) {
    const priorDefinition = decodeServiceSnapshot(priorService).toString("utf8");
    const expectedWorkingDirectory = normalizedMetadataPath(
      serviceWorkingDirectory(priorDefinition, value.os),
    );
    const expectedGitSha = serviceGitSha(priorDefinition, value.os);
    if (priorWorkingDirectory !== expectedWorkingDirectory || priorGitSha !== expectedGitSha) {
      throw new Error("worker deploy journal prior service metadata does not match its definition");
    }
  }
  if (value.phase === "prepared" && targetService) {
    throw new Error("prepared worker deploy journal cannot contain a target service definition");
  }
  if (value.phase === "activated" && !targetService) {
    throw new Error("activated worker deploy journal is missing its target service definition");
  }
  if (targetService) {
    const targetDefinition = decodeServiceSnapshot(targetService).toString("utf8");
    if (!localWorkerReleaseMatches(
      targetDefinition,
      value.os,
      stagedReleasePath,
      value.targetSha,
    )) {
      throw new Error("worker deploy journal target service definition does not match its release");
    }
  }
  return {
    schemaVersion: LOCAL_WORKER_DEPLOY_JOURNAL_SCHEMA_VERSION,
    phase: value.phase,
    os: value.os,
    sourceRoot,
    releaseRoot,
    stagedReleasePath,
    targetSha: value.targetSha,
    priorService,
    priorWasRunning: value.priorWasRunning,
    priorWorkingDirectory,
    priorGitSha,
    targetService,
  };
}

export async function priorServiceIsProven(
  journal: Readonly<LocalWorkerDeployJournalV1>,
  deps: Readonly<LocalWorkerDeployRecoveryDeps>,
): Promise<boolean> {
  const attempts = Math.max(1, deps.proofAttempts ?? 20);
  const sleep = deps.sleep ?? Bun.sleep;
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    try {
      const definition = await deps.readService(journal);
      const lifecycle = await deps.probeLifecycle(journal);
      if (
        serviceSnapshotMatches(definition, journal.priorService)
        && lifecycle === (journal.priorWasRunning ? "running" : "stopped")
      ) {
        return true;
      }
    } catch {
      // A transient read/probe failure is retried; exhaustion fails closed.
    }
    if (attempt + 1 < attempts) await sleep(250);
  }
  return false;
}

export async function _recoverLocalWorkerDeployJournal(
  raw: string,
  confinement: Readonly<LocalWorkerDeployConfinement>,
  deps: Readonly<LocalWorkerDeployRecoveryDeps>,
): Promise<LocalWorkerDeployRecoveryDecision> {
  const journal = parseLocalWorkerDeployJournal(raw, confinement);
  let targetIsProven = false;
  if (journal.targetService) {
    try {
      const activeService = await deps.readService(journal);
      if (serviceSnapshotMatches(activeService, journal.targetService)) {
        targetIsProven = await deps.probeLifecycle(journal) === "running";
      }
    } catch {
      // Any failure to prove the exact target falls through to rollback.
    }
  }

  // A prepared journal can never carry a target service (parse enforces it),
  // so no activation probe above can run before the prepared cleanup below.
  const decision = posixDeployJournalDecision(journal.phase, targetIsProven);
  if (decision === "clean-prepared") {
    await deps.cleanupStage(journal);
    await deps.clearJournal();
    return "prepared-cleaned";
  }
  if (decision === "commit") {
    await deps.commitTarget(journal);
    await deps.clearJournal();
    return "target-committed";
  }

  await deps.restorePrior(journal);
  if (!await priorServiceIsProven(journal, deps)) {
    throw new Error("prior worker service definition and lifecycle could not be proven");
  }
  await deps.cleanupStage(journal);
  await deps.clearJournal();
  return "prior-restored";
}

function unescapeXml(value: string): string {
  return value
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", "\"")
    .replaceAll("&apos;", "'")
    .replaceAll("&amp;", "&");
}

export function serviceWorkingDirectory(
  definition: string,
  os: "linux" | "darwin",
): string | null {
  if (os === "linux") {
    const match = /^WorkingDirectory=(?:"((?:\\.|[^"])*)"|([^\r\n]*))$/m.exec(definition);
    const value = match?.[1] ?? match?.[2];
    return value
      ? value.replace(/\\([\\\"nrt])/g, (_full, escaped: string) => {
        if (escaped === "n") return "\n";
        if (escaped === "r") return "\r";
        if (escaped === "t") return "\t";
        return escaped;
      }).trim() || null
      : null;
  }
  const value = /<key>WorkingDirectory<\/key>\s*<string>([^<]+)<\/string>/.exec(definition)?.[1];
  return value ? unescapeXml(value).trim() || null : null;
}

export function localWorkerReleaseMatches(
  definition: string,
  os: "linux" | "darwin",
  releaseDir: string,
  gitSha: string,
): boolean {
  const environment = parsePosixServiceEnvironment(definition, os);
  return serviceWorkingDirectory(definition, os) === releaseDir
    && (environment.GIT_SHA ?? environment.ROOST_GIT_SHA) === gitSha;
}

export function localWorkerDeployJournalPath(serviceDir: string = roostServiceDir()): string {
  return join(resolve(serviceDir), "transactions", LOCAL_WORKER_DEPLOY_JOURNAL_FILE);
}
