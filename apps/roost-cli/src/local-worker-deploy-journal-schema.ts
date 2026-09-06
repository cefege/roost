// Local worker deploy journal schema, confinement, and service metadata.
// local-worker-deploy-journal.ts exposes this API beside recovery decisions.
// POSIX service parsers and keeper-update schemas provide fail-closed proof.

import { dirname } from "node:path";
import { JournaledKeeperUpdateV1Schema } from "@roost/shared/keeper-update";
import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";
import { parsePosixServiceEnvironment } from "./deploy-plist-env.ts";
import {
  isResolvedCanonicalAbsolutePath,
  POSIX_FULL_GIT_SHA_RE,
  posixJournalObjectValue,
} from "./posix-deploy-journal.ts";
import {
  workerRolloutFingerprintOrNull,
  workerRolloutIdOrNull,
} from "./worker-deploy-rollout.ts";

export const LOCAL_WORKER_DEPLOY_JOURNAL_SCHEMA_VERSION = 4;
export type LocalWorkerDeployPhase =
  | "prepared"
  | "activating"
  | "activated"
  | "committing"
  | "rolling-back";
export type LocalWorkerLifecycle = "running" | "stopped" | "unloaded" | "unknown";
export type LocalWorkerStartupPolicy = "enabled" | "disabled" | "masked" | "absent";
export interface LocalWorkerServiceSnapshot {
  definitionBase64: string;
  mode: number;
}
export interface LocalWorkerDeployJournal {
  schemaVersion: 4;
  phase: LocalWorkerDeployPhase;
  os: "linux" | "darwin";
  sourceRoot: string;
  releaseRoot: string;
  stagedReleasePath: string;
  targetSha: string;
  rolloutId: string | null;
  workerFingerprint: string | null;
  keeperUpdate: JournaledKeeperUpdateV1 | null;
  priorService: LocalWorkerServiceSnapshot | null;
  priorLifecycle: Exclude<LocalWorkerLifecycle, "unknown">;
  priorStartupPolicy: LocalWorkerStartupPolicy;
  priorWorkingDirectory: string | null;
  priorGitSha: string | null;
  targetService: LocalWorkerServiceSnapshot | null;
}
export interface LocalWorkerDeployConfinement {
  os: "linux" | "darwin";
  sourceRoot: string;
  releaseRoot: string;
}

function objectValue(value: unknown, label: string): Record<string, unknown> {
  return posixJournalObjectValue(value, `${label} must be an object`);
}
function nullableString(value: unknown, label: string): string | null {
  if (value === null) return null;
  if (typeof value !== "string") throw new Error(`${label} must be a string or null`);
  return value;
}
function parseServiceSnapshot(value: unknown, label: string): LocalWorkerServiceSnapshot | null {
  if (value === null) return null;
  const snapshot = objectValue(value, label);
  const fields = ["definitionBase64", "mode"];
  if (Object.keys(snapshot).length !== fields.length
    || fields.some(field => !Object.hasOwn(snapshot, field))) {
    throw new Error(`${label} fields are incomplete or unexpected`);
  }
  if (typeof snapshot.definitionBase64 !== "string") throw new Error(`${label}.definitionBase64 must be a string`);
  if (snapshot.definitionBase64.length > 2 * 1024 * 1024) throw new Error(`${label}.definitionBase64 is too large`);
  const decoded = Buffer.from(snapshot.definitionBase64, "base64");
  if (decoded.toString("base64") !== snapshot.definitionBase64) {
    throw new Error(`${label}.definitionBase64 is not canonical base64`);
  }
  if (!Number.isInteger(snapshot.mode) || (snapshot.mode as number) < 0 || (snapshot.mode as number) > 0o777) {
    throw new Error(`${label}.mode is invalid`);
  }
  return { definitionBase64: snapshot.definitionBase64, mode: snapshot.mode as number };
}
export function decodeServiceSnapshot(snapshot: Readonly<LocalWorkerServiceSnapshot>): Buffer {
  return Buffer.from(snapshot.definitionBase64, "base64");
}
export function serviceSnapshotMatches(
  actual: Readonly<LocalWorkerServiceSnapshot> | null,
  expected: Readonly<LocalWorkerServiceSnapshot> | null,
): boolean {
  if (actual === null || expected === null) return actual === expected;
  return actual.mode === expected.mode
    && actual.definitionBase64 === expected.definitionBase64;
}

export function normalizedMetadataPath(value: string | null): string | null { return value && isResolvedCanonicalAbsolutePath(value) ? value : null; }

function assertNormalizedAbsolutePath(value: string, label: string): void {
  if (!isResolvedCanonicalAbsolutePath(value)) throw new Error(`${label} must be a normalized absolute path`);
}

export function serviceGitSha(definition: string, os: "linux" | "darwin"): string | null {
  const environment = parsePosixServiceEnvironment(definition, os);
  const value = environment.GIT_SHA ?? environment.ROOST_GIT_SHA;
  return value === undefined || value.length === 0 ? null : value;
}

export function localWorkerDeployStageIsConfined(releaseRoot: string, stagedReleasePath: string): boolean {
  return isResolvedCanonicalAbsolutePath(releaseRoot)
    && isResolvedCanonicalAbsolutePath(stagedReleasePath)
    && dirname(stagedReleasePath) === releaseRoot;
}

export function parseLocalWorkerDeployJournal(
  raw: string,
  confinement: Readonly<LocalWorkerDeployConfinement>,
): LocalWorkerDeployJournal {
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
  const fields = [
    "schemaVersion", "phase", "os", "sourceRoot", "releaseRoot", "stagedReleasePath",
    "targetSha", "rolloutId", "workerFingerprint", "keeperUpdate", "priorService",
    "priorLifecycle", "priorStartupPolicy", "priorWorkingDirectory", "priorGitSha",
    "targetService",
  ];
  if (Object.keys(value).length !== fields.length || fields.some(field => !Object.hasOwn(value, field))) {
    throw new Error("worker deploy journal fields are incomplete or unexpected");
  }
  if (value.phase !== "prepared" && value.phase !== "activating"
    && value.phase !== "activated" && value.phase !== "committing"
    && value.phase !== "rolling-back") {
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
  if (typeof value.targetSha !== "string" || !POSIX_FULL_GIT_SHA_RE.test(value.targetSha.toLowerCase())) {
    throw new Error("worker deploy journal target SHA is invalid");
  }
  const stagedReleaseId = stagedReleasePath.slice(releaseRoot.length + 1);
  if (!new RegExp(
    `^${value.targetSha}-[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$`,
    "i",
  ).test(stagedReleaseId)) {
    throw new Error("worker deploy journal staged release identifier is invalid");
  }
  const rolloutId = workerRolloutIdOrNull(value.rolloutId, "worker deploy journal rollout ID");
  if (value.rolloutId !== null && rolloutId === null) {
    throw new Error("worker deploy journal rollout ID must be a canonical UUID or null");
  }
  const workerFingerprint = workerRolloutFingerprintOrNull(
    value.workerFingerprint,
    "worker deploy journal fingerprint",
  );
  if (value.workerFingerprint !== null && workerFingerprint === null) {
    throw new Error("worker deploy journal fingerprint must be a full lowercase SHA-256 identity or null");
  }
  const keeperUpdate = value.keeperUpdate === null
    ? null
    : JournaledKeeperUpdateV1Schema.parse(value.keeperUpdate);
  if ((keeperUpdate === null) !== (workerFingerprint === null)) {
    throw new Error(
      "journal worker fingerprint must be null exactly when no keeper update is required",
    );
  }
  if (keeperUpdate === null && rolloutId !== null) {
    throw new Error("a fleet rollout journal requires a keeper update");
  }
  const priorLifecycle = value.priorLifecycle;
  if (priorLifecycle !== "running" && priorLifecycle !== "stopped"
    && priorLifecycle !== "unloaded") {
    throw new Error("worker deploy journal prior lifecycle is invalid");
  }
  const priorStartupPolicy = value.priorStartupPolicy;
  if (priorStartupPolicy !== "enabled" && priorStartupPolicy !== "disabled"
    && priorStartupPolicy !== "masked" && priorStartupPolicy !== "absent") {
    throw new Error("worker deploy journal prior startup policy is invalid");
  }
  const priorService = parseServiceSnapshot(value.priorService, "journal.priorService");
  const targetService = parseServiceSnapshot(value.targetService, "journal.targetService");
  const priorWorkingDirectory = nullableString(value.priorWorkingDirectory, "journal.priorWorkingDirectory");
  const priorGitSha = nullableString(value.priorGitSha, "journal.priorGitSha");
  if (!priorService && (priorLifecycle !== "unloaded" || priorStartupPolicy !== "absent")) {
    throw new Error("absent prior worker must be unloaded with absent startup policy");
  }
  if (priorService && priorStartupPolicy === "absent") {
    throw new Error("installed prior worker cannot have absent startup policy");
  }
  if (value.os === "linux" && priorService && priorLifecycle === "unloaded") {
    throw new Error("installed Linux prior worker cannot be unloaded");
  }
  if (value.os === "darwin" && priorStartupPolicy === "masked") {
    throw new Error("macOS prior worker cannot have a masked startup policy");
  }
  if (value.os === "darwin" && priorLifecycle === "stopped"
    && priorStartupPolicy === "enabled") {
    throw new Error("enabled KeepAlive worker cannot have a durable stopped lifecycle");
  }
  if (priorLifecycle === "running" && priorStartupPolicy === "masked") {
    throw new Error("masked prior worker cannot be running");
  }
  if (!priorService && (priorWorkingDirectory !== null || priorGitSha !== null)) {
    throw new Error("worker deploy journal has metadata for an absent prior service");
  }
  if ((priorService === null) !== (keeperUpdate === null)) {
    throw new Error("worker deploy journal keeper update must be null exactly when the prior service is absent");
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
  if (keeperUpdate && (!priorGitSha || !POSIX_FULL_GIT_SHA_RE.test(priorGitSha))) {
    throw new Error("worker deploy journal cannot prove the prior worker build identity");
  }
  if (value.phase === "prepared" && targetService) {
    throw new Error("prepared worker deploy journal cannot contain a target service definition");
  }
  if ((value.phase === "activated" || value.phase === "committing") && !targetService) {
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
    rolloutId,
    workerFingerprint,
    keeperUpdate,
    priorService,
    priorLifecycle,
    priorStartupPolicy,
    priorWorkingDirectory,
    priorGitSha,
    targetService,
  };
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
  definition: string, os: "linux" | "darwin", releaseDir: string, gitSha: string,
): boolean {
  const environment = parsePosixServiceEnvironment(definition, os);
  return serviceWorkingDirectory(definition, os) === releaseDir
    && (environment.GIT_SHA ?? environment.ROOST_GIT_SHA) === gitSha;
}
