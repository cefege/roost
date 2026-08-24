// Schema, confinement, parsing, and phase bookkeeping for the coordinator
// self-update deploy journal (coordinator-deploy.json under transactions/).
// Also owns the installed-coordinator-definition parsers every journal check
// validates against (WorkingDirectory + environment identity). The runtime
// rollback/recovery drivers live in coordinator-deploy-recovery.ts; `roost
// push` (deployLocalCoordinator) consumes both. Built on the shared
// posix-deploy-journal core.
import { existsSync, lstatSync, readFileSync } from "node:fs";
import { coordServiceLabel } from "@roost/shared/paths";

import { dirname, isAbsolute, relative } from "node:path";
import { durableWriteFile } from "@roost/shared/durability";
import { DeployFailure } from "./deploy-exec.ts";
import {
  parsePosixServiceEnvironment,
  parseSystemdServiceDirective,
} from "./deploy-plist-env.ts";
import { launchdBootstrapWithRetryCmd } from "./service-ctl.ts";
import {
  POSIX_FULL_GIT_SHA_RE,
  POSIX_RELEASE_ID_SUFFIX_RE,
  isResolvedCanonicalAbsolutePath,
  posixDeployJournalDecision,
  posixJournalObjectValue,
  type PosixDeployJournalPhase,
} from "./posix-deploy-journal.ts";

export function coordinatorRepoFromService(
  definition: string,
  platform: NodeJS.Platform,
): string | null {
  if (platform === "linux") {
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
  if (platform === "darwin") {
    const value = /<key>WorkingDirectory<\/key>\s*<string>([^<]+)<\/string>/.exec(definition)?.[1];
    return value
      ? value
        .replaceAll("&lt;", "<")
        .replaceAll("&gt;", ">")
        .replaceAll("&quot;", "\"")
        .replaceAll("&apos;", "'")
        .replaceAll("&amp;", "&")
        .trim() || null
      : null;
  }
  return null;
}

export function coordinatorInstallEnvironment(
  definition: string,
  platform: "darwin" | "linux",
): Record<string, string> {
  const environment = parsePosixServiceEnvironment(definition, platform);
  if (platform === "linux") {
    for (const [directive, key] of [
      ["MemoryHigh", "ROOST_COORD_MEMORY_HIGH"],
      ["MemoryMax", "ROOST_COORD_MEMORY_MAX"],
      ["TasksMax", "ROOST_COORD_TASKS_MAX"],
    ] as const) {
      const value = parseSystemdServiceDirective(definition, directive);
      if (value) environment[key] = value;
    }
  }
  if (environment.ROOST_FRONTED === undefined) {
    const bind = environment.ROOST_COORDINATOR_BIND;
    if (environment.ROOST_TRUST_PROXY === "1" || bind?.startsWith("127.0.0.1:")) {
      environment.ROOST_FRONTED = "1";
    } else if (bind || environment.ROOST_TLS_CERT_PATH || environment.ROOST_TLS_KEY_PATH) {
      environment.ROOST_FRONTED = "0";
    }
  }
  return environment;
}

export const COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION = 1 as const;
const COORDINATOR_DEPLOY_JOURNAL_MAX_BYTES = 2 * 1024 * 1024;
const COORDINATOR_SERVICE_DEFINITION_MAX_BYTES = 1024 * 1024;

export type CoordinatorDeployPhase = PosixDeployJournalPhase;

export interface CoordinatorDeployJournalV1 {
  schemaVersion: typeof COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION;
  phase: CoordinatorDeployPhase;
  priorDefinitionBase64: string;
  priorDefinitionMode: number;
  priorSha: string;
  targetSha: string;
  servicePath: string;
  sourceReleasePath: string;
  stagingRepoPath: string;
  stagedReleasePath: string;
}

export interface CoordinatorDeployJournalContext {
  servicePath: string;
  releaseRoot: string;
  platform: "darwin" | "linux";
}

export type CoordinatorDeployRecoveryAction =
  | "clean-prepared"
  | "commit-target"
  | "rollback-prior";

function journalString(
  value: Partial<Record<keyof CoordinatorDeployJournalV1, unknown>>,
  key: keyof CoordinatorDeployJournalV1,
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

export function parseCoordinatorDeployJournal(
  serialized: string,
  context: CoordinatorDeployJournalContext,
): CoordinatorDeployJournalV1 {
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
  const fields = value as Partial<Record<keyof CoordinatorDeployJournalV1, unknown>>;
  if (fields.schemaVersion !== COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION) {
    throw new Error(`unsupported coordinator deploy journal schema ${String(fields.schemaVersion)}`);
  }
  const phase = fields.phase;
  if (phase !== "prepared" && phase !== "activating" && phase !== "activated") {
    throw new Error(`invalid coordinator deploy phase ${String(phase)}`);
  }

  const expectedServicePath = canonicalAbsolutePath(context.servicePath, "expected servicePath");
  const releaseRoot = canonicalAbsolutePath(context.releaseRoot, "releaseRoot");
  const servicePath = canonicalAbsolutePath(journalString(fields, "servicePath"), "servicePath");
  if (servicePath !== expectedServicePath) {
    throw new Error(`servicePath does not match the installed coordinator service`);
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

  return {
    schemaVersion: COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION,
    phase,
    priorDefinitionBase64,
    priorDefinitionMode: priorDefinitionMode as number,
    priorSha,
    targetSha,
    servicePath,
    sourceReleasePath,
    stagingRepoPath,
    stagedReleasePath,
  };
}

export function coordinatorDeployRecoveryAction(
  phase: CoordinatorDeployPhase,
  targetHealthy: boolean,
): CoordinatorDeployRecoveryAction {
  // Shared prepared⇒clean / health⇒commit|rollback decision, relabeled for
  // the coordinator's action vocabulary.
  const decision = posixDeployJournalDecision(phase, targetHealthy);
  return decision === "clean-prepared"
    ? "clean-prepared"
    : decision === "commit"
      ? "commit-target"
      : "rollback-prior";
}

export async function writeCoordinatorDeployJournal(
  journalPath: string,
  journal: CoordinatorDeployJournalV1,
): Promise<void> {
  await durableWriteFile(journalPath, `${JSON.stringify(journal)}\n`, { mode: 0o600 });
}

export async function writeCoordinatorDeployPhase(
  journalPath: string,
  journal: CoordinatorDeployJournalV1,
  phase: CoordinatorDeployPhase,
): Promise<CoordinatorDeployJournalV1> {
  const next = { ...journal, phase };
  await writeCoordinatorDeployJournal(journalPath, next);
  return next;
}

export function loadCoordinatorDeployJournal(
  journalPath: string,
  context: CoordinatorDeployJournalContext,
): CoordinatorDeployJournalV1 | null {
  if (!existsSync(journalPath)) return null;
  try {
    const metadata = lstatSync(journalPath);
    if (!metadata.isFile() || metadata.size > COORDINATOR_DEPLOY_JOURNAL_MAX_BYTES) {
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

export function coordinatorRestartCommand(
  servicePath: string,
  platform: NodeJS.Platform = process.platform,
  label: string = coordServiceLabel(),
): string {
  if (platform === "linux") {
    const unit = label.endsWith(".service") ? label : `${label}.service`;
    return `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"; ` +
      `systemctl --user daemon-reload && systemctl --user restart '${unit.replaceAll("'", `'\"'\"'`)}'`;
  }
  if (platform !== "darwin") throw new Error(`unsupported POSIX coordinator platform ${platform}`);
  return launchdBootstrapWithRetryCmd(label, servicePath, { role: "coordinator rollback" });
}
