// `roost push` — publish one clean commit, deploy every registered worker,
// update the coordinator's actual source checkout, and prove every process
// reports that commit before returning success.

import { randomUUID } from "node:crypto";
import { cpSync, existsSync, lstatSync, mkdirSync, readdirSync, readFileSync, realpathSync, rmSync } from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { coordServiceLabel, coordServicePath, roostServiceDir, workerServicePath } from "@roost/shared/paths";
import { durableRemove, durableWriteFile, flushDurablePath } from "@roost/shared/durability";
import { acquireMachineTransaction } from "./machine-transaction.ts";
import {
  parsePosixServiceEnvironment,
  parseSystemdServiceDirective,
} from "./deploy-plist-env.ts";
import { deploy } from "./deploy.ts";
import {
  DeployFailure,
  POSIX_WORKER_DEPLOY_JOURNAL_PATHS,
  failDeploy,
  resolveGitPublishTargetOrDie,
  resolveLocalGitShaOrDie,
  resolvePublishedGitShaOrDie,
  run,
} from "./deploy-exec.ts";
import {
  statusReport,
  workerInventory,
  type StatusReport,
  type WorkerStatus,
} from "./status.ts";
import { parseWindowsReleaseManifest } from "./windows/windows-update-journal.ts";
import { launchdBootstrapWithRetryCmd } from "./service-ctl.ts";
import { fetchAndVerifyReleaseAsset, WINDOWS_RELEASE_MANIFEST_ASSET } from "./update.ts";

const REPO_ROOT = resolve(import.meta.dir, "..", "..", "..");
const VERIFY_TIMEOUT_MS = 60_000;
const VERIFY_POLL_MS = 1_000;

function normalizedHost(value: string): string {
  return value.trim().toLowerCase().replace(/\.$/, "");
}

function hostLabel(value: string): string {
  return normalizedHost(value).split(".")[0] ?? "";
}

function assertSafeSshTarget(value: string): string {
  const target = value.trim();
  if (!/^[A-Za-z0-9][A-Za-z0-9._:-]*$/.test(target)) {
    throw new Error(`invalid SSH deployment target: ${JSON.stringify(value)}`);
  }
  return target;
}

export function resolvePushTargets(
  configured: string | undefined,
  workers: readonly WorkerStatus[],
): string[] {
  const requested = (configured ?? "").split(",").map((value) => value.trim()).filter(Boolean);
  const source = requested.length > 0
    ? requested
    : workers.map((worker) => worker.os === "win32"
      ? worker.fingerprint
      : (worker.reachableAddr || worker.label));
  const seen = new Set<string>();
  const targets: string[] = [];
  for (const value of source) {
    const resolved = resolveWorkerTarget(workers, value);
    const canonical = resolved.worker
      ? (resolved.worker.os === "win32"
        ? resolved.worker.fingerprint
        : (resolved.worker.reachableAddr || resolved.worker.label))
      : value;
    const target = assertSafeSshTarget(canonical);
    const key = normalizedHost(target);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    targets.push(target);
  }
  return targets;
}

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

function resolveWorkerTarget(
  workers: readonly WorkerStatus[],
  target: string,
): { worker: WorkerStatus | null; ambiguous: boolean } {
  const normalizedTarget = normalizedHost(target);
  const fingerprintMatches = workers.filter(
    (worker) => normalizedHost(worker.fingerprint) === normalizedTarget,
  );
  if (fingerprintMatches.length > 0) {
    return {
      worker: fingerprintMatches.length === 1 ? fingerprintMatches[0] : null,
      ambiguous: fingerprintMatches.length > 1,
    };
  }
  const exact = workers.filter((worker) =>
    [worker.label, worker.reachableAddr]
      .filter((value): value is string => Boolean(value))
      .some((value) => normalizedHost(value) === normalizedTarget)
  );
  if (exact.length > 0) {
    return { worker: exact.length === 1 ? exact[0] : null, ambiguous: exact.length > 1 };
  }
  const targetLabel = hostLabel(normalizedTarget);
  const aliases = workers.filter((worker) =>
    [worker.label, worker.reachableAddr]
      .filter((value): value is string => Boolean(value))
      .some((value) => hostLabel(normalizedHost(value)) === targetLabel)
  );
  return { worker: aliases.length === 1 ? aliases[0] : null, ambiguous: aliases.length > 1 };
}

export async function preflightWindowsFleetRelease(
  expectedSha: string,
  fetchImpl: typeof fetch = fetch,
): Promise<{ manifestSha256: string }> {
  const release = await fetchAndVerifyReleaseAsset(WINDOWS_RELEASE_MANIFEST_ASSET, {
    fetchImpl,
    subject: "Windows fleet manifest",
    timeoutMs: 30_000,
    checksumTimeoutMs: 30_000,
    fail: (message) => new DeployFailure(8, message),
  });
  const manifest = parseWindowsReleaseManifest(release.bytes);
  if (manifest.build !== expectedSha) {
    throw new DeployFailure(
      8,
      `Windows release manifest reports ${manifest.build}, expected source commit ${expectedSha}`,
    );
  }
  return { manifestSha256: release.sha256 };
}

export function ambiguousPushTargets(
  targets: readonly string[],
  workers: readonly WorkerStatus[],
): string[] {
  return targets.filter((target) => resolveWorkerTarget(workers, target).ambiguous);
}


export function workerVersionProblems(
  targets: readonly string[],
  workers: readonly WorkerStatus[],
  expectedSha: string,
  deployedAfter: ReadonlyMap<string, number> = new Map(),
): string[] {
  const problems: string[] = [];
  for (const target of targets) {
    const resolved = resolveWorkerTarget(workers, target);
    const worker = resolved.worker;
    if (resolved.ambiguous) {
      problems.push(`${target}: ambiguous coordinator worker identity`);
    } else if (!worker) {
      problems.push(`${target}: missing from coordinator worker inventory`);
    } else if (worker.stale) {
      problems.push(`${worker.label}: stale`);
    } else if (worker.gitSha !== expectedSha) {
      problems.push(`${worker.label}: reports ${worker.gitSha ?? "no SHA"}, expected ${expectedSha}`);
    } else if (worker.keeperState === "unknown") {
      problems.push(`${worker.label}: keeper build proof is unavailable`);
    } else if (worker.keeperState === "stale") {
      problems.push(`${worker.label}: keeper reports stale build ${worker.keeperBuild ?? "unknown"}`);
    } else if (worker.lastSeenMs <= (deployedAfter.get(normalizedHost(target)) ?? 0)) {
      problems.push(`${worker.label}: awaiting a post-deploy heartbeat`);
    }
  }
  return problems;
}

async function runChecked(
  cmd: string[],
  label: string,
  cwd: string,
  env?: Record<string, string>,
): Promise<void> {
  const result = await run(cmd, { cwd, quiet: true, env });
  if (result.exit !== 0) {
    throw new DeployFailure(
      result.exit,
      `${label} failed (exit ${result.exit})\n${result.stdout}\n${result.stderr}`,
    );
  }
  const output = `${result.stdout}${result.stderr}`.trim();
  if (output) console.log(output);
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

function hasWebDist(path: string): boolean {
  return existsSync(join(path, "index.html"));
}

export function preserveWebDistForNoBuild(
  releaseDir: string,
  installedEnvironment: Readonly<Record<string, string>>,
  priorRepo: string,
): string {
  const destination = join(releaseDir, "apps", "web", "dist");
  if (hasWebDist(destination)) return destination;
  const configured = installedEnvironment.ROOST_WEB_DIST_PATH;
  for (const candidate of [
    configured ? (isAbsolute(configured) ? configured : resolve(priorRepo, configured)) : undefined,
    join(priorRepo, "apps", "web", "dist"),
  ]) {
    if (!candidate || resolve(candidate) === resolve(destination) || !hasWebDist(candidate)) continue;
    mkdirSync(dirname(destination), { recursive: true });
    cpSync(candidate, destination, { recursive: true, dereference: true });
    return destination;
  }
  throw new DeployFailure(
    5,
    "--no-web requested but neither the staged commit nor the installed coordinator has apps/web/dist",
  );
}

export function workerConvergenceThresholds(
  targets: readonly string[],
  activatedAt: number,
): Map<string, number> {
  return new Map(targets.map((target) => [normalizedHost(target), activatedAt]));
}

const COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION = 1 as const;
const COORDINATOR_DEPLOY_JOURNAL_MAX_BYTES = 2 * 1024 * 1024;
const COORDINATOR_SERVICE_DEFINITION_MAX_BYTES = 1024 * 1024;
const GIT_SHA_PATTERN = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/;
const RELEASE_ID_SUFFIX_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

export type CoordinatorDeployPhase = "prepared" | "activating" | "activated";

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
  if (!isAbsolute(value) || resolve(value) !== value) {
    throw new Error(`${field} must be a canonical absolute path`);
  }
  return value;
}

export function coordinatorReleasePathIsConfined(
  releaseRoot: string,
  candidate: string,
): boolean {
  if (!isAbsolute(releaseRoot) || resolve(releaseRoot) !== releaseRoot
    || !isAbsolute(candidate) || resolve(candidate) !== candidate) {
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
    && RELEASE_ID_SUFFIX_PATTERN.test(releaseId.slice(targetSha.length + 1));
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
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("journal root must be an object");
  }
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
  if (!GIT_SHA_PATTERN.test(targetSha)) throw new Error("targetSha is not a full Git SHA");
  const priorSha = journalString(fields, "priorSha");
  if (!GIT_SHA_PATTERN.test(priorSha)) throw new Error("priorSha is not a full Git SHA");

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
  if (phase === "prepared") return "clean-prepared";
  return targetHealthy ? "commit-target" : "rollback-prior";
}

function coordinatorReportIsHealthy(
  report: StatusReport,
  expectedSha: string,
): boolean {
  return coordinatorReportIsOperational(report)
    && report.coord.gitSha === expectedSha;
}

function coordinatorReportIsOperational(report: StatusReport): boolean {
  return report.coord.reachable
    && report.coordAgentLoaded
    && report.tlsMode !== "missing";
}


async function writeCoordinatorDeployJournal(
  journalPath: string,
  journal: CoordinatorDeployJournalV1,
): Promise<void> {
  await durableWriteFile(journalPath, `${JSON.stringify(journal)}\n`, { mode: 0o600 });
}

async function writeCoordinatorDeployPhase(
  journalPath: string,
  journal: CoordinatorDeployJournalV1,
  phase: CoordinatorDeployPhase,
): Promise<CoordinatorDeployJournalV1> {
  const next = { ...journal, phase };
  await writeCoordinatorDeployJournal(journalPath, next);
  return next;
}

function loadCoordinatorDeployJournal(
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

async function removeStagedCoordinatorRelease(
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

async function flushCoordinatorReleaseTree(
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

async function coordinatorStartupPolicyIsEnabled(
  platform: "darwin" | "linux",
  label: string = coordServiceLabel(),
): Promise<boolean> {
  if (platform === "linux") {
    const unit = label.endsWith(".service") ? label : `${label}.service`;
    const result = await run(["bash", "-lc",
      `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"; ` +
        `systemctl --user is-enabled '${unit.replaceAll("'", `'\"'\"'`)}'`,
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

async function restorePriorCoordinator(
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

function currentServiceTargetsRelease(
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

async function commitCoordinatorDeploy(
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

async function recoverCoordinatorDeploy(
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

function resolveCoordinatorRepo(): string {
  if (process.platform !== "linux" && process.platform !== "darwin") {
    failDeploy(2, "roost push updates source-installed POSIX coordinators; use `roost update` on Windows");
  }
  const override = process.env.ROOST_COORD_REPO_DIR?.trim();
  let installed: string | null = null;
  const servicePath = coordServicePath();
  if (existsSync(servicePath)) {
    installed = coordinatorRepoFromService(readFileSync(servicePath, "utf8"), process.platform);
  }
  for (const candidate of [override, installed, REPO_ROOT]) {
    if (!candidate) continue;
    const absolute = resolve(candidate);
    if (existsSync(join(absolute, ".git"))
      && existsSync(join(absolute, "apps", "coord", "scripts", "install.sh"))) {
      return realpathSync(absolute);
    }
  }
  failDeploy(
    2,
    `cannot locate the coordinator source checkout from ${servicePath}; set ROOST_COORD_REPO_DIR`,
  );
}

export function foreignWorkerDeployJournalForCoordinator(serviceRoot: string): string | null {
  for (const relative of [
    POSIX_WORKER_DEPLOY_JOURNAL_PATHS.local,
    POSIX_WORKER_DEPLOY_JOURNAL_PATHS.linux,
    POSIX_WORKER_DEPLOY_JOURNAL_PATHS.darwin,
  ]) {
    const candidate = join(serviceRoot, relative);
    try {
      lstatSync(candidate);
      return candidate;
    } catch (error) {
      if (!(error instanceof Error
        && "code" in error
        && (error as NodeJS.ErrnoException).code === "ENOENT")) {
        throw error;
      }
    }
  }
  return null;
}

async function deployLocalCoordinator(expectedSha: string, buildWeb: boolean): Promise<void> {
  if (process.platform !== "linux" && process.platform !== "darwin") {
    failDeploy(2, "roost push updates source-installed POSIX coordinators; use `roost update` on Windows");
  }
  const platform = process.platform;
  const configuredServiceRoot = resolve(roostServiceDir());
  mkdirSync(configuredServiceRoot, { recursive: true, mode: 0o700 });
  const serviceRoot = realpathSync(configuredServiceRoot);
  const releaseRoot = join(serviceRoot, "releases", "coord");
  const journalPath = join(serviceRoot, "transactions", "coordinator-deploy.json");
  const servicePath = resolve(coordServicePath());
  mkdirSync(releaseRoot, { recursive: true, mode: 0o700 });
  mkdirSync(dirname(journalPath), { recursive: true, mode: 0o700 });
  if (realpathSync(releaseRoot) !== releaseRoot
    || realpathSync(dirname(journalPath)) !== dirname(journalPath)) {
    failDeploy(5, "coordinator deployment directories must not traverse symbolic links");
  }

  const foreignJournal = foreignWorkerDeployJournalForCoordinator(serviceRoot);
  if (foreignJournal !== null) {
    failDeploy(5, `cannot mutate past unsettled foreign worker deploy journal: ${foreignJournal}`);
  }
  const transaction = await acquireMachineTransaction("deploy", journalPath);
  try {
    const journalContext: CoordinatorDeployJournalContext = {
      servicePath,
      releaseRoot,
      platform,
    };
    await recoverCoordinatorDeploy(journalPath, journalContext);

    const coordinatorRepo = resolveCoordinatorRepo();
    const releaseId = `${expectedSha}-${crypto.randomUUID()}`;
    const releaseDir = join(releaseRoot, releaseId);
    await runChecked(["git", "fetch", "--quiet", "origin"], "coordinator git fetch", coordinatorRepo);

    if (!existsSync(servicePath)) {
      failDeploy(5, `coordinator service definition is missing: ${servicePath}`);
    }
    const priorDefinition = readFileSync(servicePath);
    const priorDefinitionMode = lstatSync(servicePath).mode & 0o777;
    const priorDefinitionText = priorDefinition.toString("utf8");
    const installedEnvironment = coordinatorInstallEnvironment(priorDefinitionText, platform);
    const priorSha = installedEnvironment.ROOST_GIT_SHA ?? installedEnvironment.GIT_SHA;
    if (!priorSha) {
      failDeploy(5, "coordinator service definition does not contain a prior Git SHA");
    }
    const priorReport = await statusReport();
    if (!coordinatorReportIsHealthy(priorReport, priorSha)
      || !await coordinatorStartupPolicyIsEnabled(platform)) {
      failDeploy(
        5,
        `coordinator must be running at its recorded prior build ${priorSha} with automatic startup before update`,
      );
    }
    const sourceReleasePath = coordinatorRepoFromService(priorDefinitionText, platform);
    if (!sourceReleasePath) {
      failDeploy(5, "coordinator service definition does not contain a WorkingDirectory");
    }
    const priorEnvironment: Record<string, string> = {
      ...installedEnvironment,
      GIT_SHA: expectedSha,
      ROOST_GIT_SHA: expectedSha,
      ROOST_WORKDIR: releaseDir,
      ROOST_EXEC_BIN: "",
      ROOST_REPO_ROOT: releaseDir,
      ROOST_SKIP_ENV_LOCAL: "1",
      ROOST_WEB_DIST_PATH: join(releaseDir, "apps", "web", "dist"),
      ...(platform === "linux"
        ? { ROOST_COORD_UNIT: servicePath }
        : { ROOST_COORD_PLIST: servicePath }),
    };
    let journal = parseCoordinatorDeployJournal(
      JSON.stringify({
        schemaVersion: COORDINATOR_DEPLOY_JOURNAL_SCHEMA_VERSION,
        phase: "prepared",
        priorDefinitionMode,
        priorDefinitionBase64: priorDefinition.toString("base64"),
        priorSha,
        targetSha: expectedSha,
        servicePath,
        sourceReleasePath,
        stagingRepoPath: coordinatorRepo,
        stagedReleasePath: releaseDir,
      }),
      journalContext,
    );
    await writeCoordinatorDeployJournal(journalPath, journal);

    console.log(`\n>> stage coordinator release ${releaseDir}`);
    try {
      await runChecked(
        ["git", "worktree", "add", "--quiet", "--force", "--detach", releaseDir, expectedSha],
        "coordinator worktree stage",
        coordinatorRepo,
      );
      await runChecked(["bun", "install", "--frozen-lockfile"], "coordinator frozen bun install", releaseDir);
      if (buildWeb) {
        console.log(`\n>> build coordinator SPA (${releaseDir}/apps/web → dist)`);
        await runChecked(["bun", "run", "build"], "coordinator SPA build", join(releaseDir, "apps", "web"));
      } else {
        preserveWebDistForNoBuild(releaseDir, installedEnvironment, sourceReleasePath);
      }
      await flushCoordinatorReleaseTree(releaseRoot, releaseDir, expectedSha);
    } catch (error) {
      try {
        await removeStagedCoordinatorRelease(
          releaseRoot,
          coordinatorRepo,
          releaseDir,
          expectedSha,
        );
        await durableRemove(journalPath);
      } catch (cleanupError) {
        throw new DeployFailure(
          error instanceof DeployFailure ? error.exitCode : 5,
          `${error instanceof Error ? error.message : String(error)}\n` +
            `coordinator prepared-stage cleanup failed: ${
              cleanupError instanceof Error ? cleanupError.message : String(cleanupError)
            }`,
        );
      }
      throw error;
    }

    console.log("\n>> activate and restart local coordinator");
    try {
      journal = await writeCoordinatorDeployPhase(journalPath, journal, "activating");
      await runChecked(
        ["bash", "apps/coord/scripts/install.sh", "install"],
        "coordinator install",
        releaseDir,
        priorEnvironment,
      );
    } catch (error) {
      const rollbackError = await restorePriorCoordinator(journalPath, releaseRoot, journal);
      throw new DeployFailure(
        5,
        `${error instanceof Error ? error.message : String(error)}\n` +
          (rollbackError ?? "prior coordinator service restored"),
      );
    }

    const deadline = Date.now() + VERIFY_TIMEOUT_MS;
    for (;;) {
      let report: StatusReport;
      try {
        report = await statusReport();
      } catch (error) {
        const rollbackError = await restorePriorCoordinator(journalPath, releaseRoot, journal);
        throw new DeployFailure(
          8,
          `coordinator verification failed: ${error instanceof Error ? error.message : String(error)}\n` +
            (rollbackError ?? "prior coordinator service restored"),
        );
      }
      if (currentServiceTargetsRelease(journal, platform)
        && coordinatorReportIsHealthy(report, expectedSha)) {
        await commitCoordinatorDeploy(journalPath, releaseRoot, journal);
        return;
      }
      if (Date.now() >= deadline) {
        const rollbackError = await restorePriorCoordinator(journalPath, releaseRoot, journal);
        failDeploy(
          8,
          `coordinator did not become healthy at ${expectedSha}; ` +
            `reported ${report.coord.gitSha ?? "no SHA"}, TLS=${report.tlsMode}\n` +
            (rollbackError ?? "prior coordinator service restored"),
        );
      }
      await Bun.sleep(VERIFY_POLL_MS);
    }
  } finally {
    await transaction.release();
  }
}

async function verifyWorkerVersions(
  targets: readonly string[],
  expectedSha: string,
  deployedAfter: ReadonlyMap<string, number>,
): Promise<void> {
  const deadline = Date.now() + VERIFY_TIMEOUT_MS;
  let problems: string[] = [];
  do {
    problems = workerVersionProblems(targets, workerInventory(), expectedSha, deployedAfter);
    if (problems.length === 0) return;
    await Bun.sleep(VERIFY_POLL_MS);
  } while (Date.now() < deadline);
  failDeploy(8, `workers did not converge on ${expectedSha}:\n${problems.join("\n")}`);
}

interface LocalWindowsUpdateFrame {
  sequence: number;
  phase: string;
  message: string;
  terminal: boolean;
  success: boolean;
  error: string;
}

export interface WindowsCoordinatorDeployOptions {
  platform?: NodeJS.Platform;
  timeoutMs?: number;
  now?: () => number;
  sleep?: (milliseconds: number) => Promise<void>;
  start?: (expectedSha: string) => Promise<{ jobId: string; frames: LocalWindowsUpdateFrame[] }>;
  status?: (jobId: string, afterSequence: number) => Promise<LocalWindowsUpdateFrame[]>;
  log?: (message: string) => void;
  prove?: (expectedSha: string) => Promise<boolean>;
  current?: (expectedSha: string) => Promise<boolean>;
}

/** Update a Windows coordinator through the same signed SCM broker as its
 * local worker. POSIX returns null so the caller uses source deployment. */
export async function tryCoordinatorWindowsDeploy(
  expectedSha: string,
  options: WindowsCoordinatorDeployOptions = {},
): Promise<boolean | null> {
  if ((options.platform ?? process.platform) !== "win32") return null;
  const now = options.now ?? Date.now;
  const sleep = options.sleep ?? Bun.sleep;
  const log = options.log ?? ((message: string) => console.log(message));
  const timeoutMs = options.timeoutMs ?? 5 * 60_000;
  const prove = options.prove ?? (async (sha: string) =>
    coordinatorReportIsHealthy(await statusReport(), sha));
  const current = options.current ?? prove;
  try {
    if (await current(expectedSha)) {
      log("Windows coordinator already reports the exact target build");
      return true;
    }
  } catch {
    // A stale or temporarily unavailable coordinator still needs the updater
    // transaction below. The post-update proof remains authoritative.
  }
  let defaultStatus: WindowsCoordinatorDeployOptions["status"];
  const started = options.start
    ? await options.start(expectedSha)
    : await (async () => {
      const release = await fetchAndVerifyReleaseAsset(WINDOWS_RELEASE_MANIFEST_ASSET, {
        subject: "Windows coordinator manifest",
        timeoutMs: 30_000,
        checksumTimeoutMs: 30_000,
        fail: (message) => new DeployFailure(8, message),
      });
      const manifest = parseWindowsReleaseManifest(release.bytes);
      if (manifest.build !== expectedSha) {
        throw new DeployFailure(
          8,
          `signed Windows release reports ${manifest.build}, expected source commit ${expectedSha}`,
        );
      }
      const { handleUpdateBrokerCommand } = await import("./windows/windows-update-control.ts");
      const jobId = randomUUID();
      const command = {
        requestId: randomUUID(),
        jobId,
        action: "START" as const,
        manifestUrl: release.url,
        signatureUrl: `${release.url}.p7s`,
        manifestSha256: release.sha256,
        publisherSha256: "",
      };
      const frames = await handleUpdateBrokerCommand(command);
      defaultStatus = async (statusJobId, afterSequence) =>
        await handleUpdateBrokerCommand({
          ...command,
          requestId: randomUUID(),
          jobId: statusJobId,
          action: "STATUS",
          afterSequence,
        });
      return { jobId, frames };
    })();
  const readStatus = options.status ?? defaultStatus;
  if (!readStatus) throw new Error("Windows coordinator update status reader is unavailable");

  let afterSequence = 0;
  let frames = started.frames;
  const deadline = now() + timeoutMs;
  for (;;) {
    for (const frame of frames) {
      if (frame.sequence <= afterSequence) continue;
      afterSequence = frame.sequence;
      log(`>> [${frame.phase}] ${frame.message}`);
      if (frame.terminal) {
        if (!frame.success) {
          throw new DeployFailure(8, frame.error || `Windows coordinator update failed in ${frame.phase}`);
        }
        if (!(await prove(expectedSha))) {
          throw new DeployFailure(
            8,
            `Windows coordinator did not report healthy build ${expectedSha} after durable update success`,
          );
        }
        return true;
      }
    }
    if (now() >= deadline) {
      throw new DeployFailure(
        8,
        `Windows coordinator update ${started.jobId} did not reach durable terminal success within ${timeoutMs}ms`,
      );
    }
    await sleep(250);
    frames = await readStatus(started.jobId, afterSequence);
  }
}

export async function deployCoordinatorForPlatform(
  expectedSha: string,
  buildWeb: boolean,
  deps: {
    windows?: (sha: string) => Promise<boolean | null>;
    posix?: (sha: string, build: boolean) => Promise<void>;
  } = {},
): Promise<"windows" | "posix"> {
  const windowsUpdated = await (deps.windows ?? tryCoordinatorWindowsDeploy)(expectedSha);
  if (windowsUpdated !== null) return "windows";
  await (deps.posix ?? deployLocalCoordinator)(expectedSha, buildWeb);
  return "posix";
}

export async function push(args: string[]): Promise<void> {
  const skipGit = args.includes("--no-git");
  const skipLocalCoord = args.includes("--no-coord");
  const targetsArg = args.find((arg) => arg.startsWith("--targets="));
  const inventory = workerInventory();
  const configured = targetsArg?.slice("--targets=".length) ?? process.env.ROOST_PUSH_TARGETS;
  const targets = resolvePushTargets(configured, inventory);
  const ambiguousTargets = ambiguousPushTargets(targets, inventory);
  if (ambiguousTargets.length > 0) {
    failDeploy(
      2,
      `ambiguous push targets: ${ambiguousTargets.join(", ")}; use exact full worker labels or addresses`,
    );
  }
  if (targets.length === 0) {
    failDeploy(
      2,
      "no push targets: register a worker, set ROOST_PUSH_TARGETS, or pass --targets=host1,host2",
    );
  }

  const expectedSha = resolveLocalGitShaOrDie(REPO_ROOT);
  if (!expectedSha || expectedSha.endsWith("-dirty")) {
    failDeploy(7, "roost push requires a clean commit");
  }

  const publishTarget = resolveGitPublishTargetOrDie(REPO_ROOT);
  if (!skipGit) {
    console.log(`>> git push ${publishTarget.remote} HEAD:${publishTarget.mergeRef}`);
    await runChecked(
      ["git", "push", "--", publishTarget.remote, `HEAD:${publishTarget.mergeRef}`],
      "git push",
      REPO_ROOT,
    );
  } else {
    console.log(">> skipping git transmission (--no-git); verifying remote identity");
  }
  resolvePublishedGitShaOrDie(REPO_ROOT, expectedSha);
  const targetsWindows = targets.some((target) =>
    resolveWorkerTarget(inventory, target).worker?.os === "win32"
  );
  const windowsRelease = targetsWindows
    ? await preflightWindowsFleetRelease(expectedSha)
    : null;
  const initialCoordinator = await statusReport();
  if (!coordinatorReportIsOperational(initialCoordinator)) {
    failDeploy(8, "coordinator must be running and healthy before fleet mutation begins");
  }

  if (skipLocalCoord) {
    if (!coordinatorReportIsHealthy(initialCoordinator, expectedSha)) {
      failDeploy(
        8,
        "--no-coord requires a live coordinator already reporting the exact target build",
      );
    }
    console.log("\n>> skipping local coordinator update (--no-coord)");
  } else {
    // Activate the coordinator first so every Windows START RPC understands
    // and enforces the preflight build/digest pins. Its own transaction proves
    // health or rolls back before any remote worker mutates.
    await deployCoordinatorForPlatform(expectedSha, !args.includes("--no-web"));
  }

  console.log(`>> fleet targets: ${targets.join(", ")}`);
  const deployedAfter = new Map<string, number>();
  const outcomes = await Promise.all(targets.map(async (host) => {
    console.log(`\n>> roost deploy ${host}`);
    deployedAfter.set(normalizedHost(host), Date.now());
    try {
      await deploy([
        host,
        `--source-root=${REPO_ROOT}`,
        `--expected-sha=${expectedSha}`,
        ...(windowsRelease
          ? [`--expected-manifest-sha256=${windowsRelease.manifestSha256}`]
          : []),
      ]);
      return { host, error: null };
    } catch (error) {
      return {
        host,
        error: error instanceof Error ? error.message : String(error),
      };
    }
  }));
  const deployed = outcomes.filter(({ error }) => error === null).map(({ host }) => host);
  const failures = outcomes
    .filter(({ error }) => error !== null)
    .map(({ host, error }) => `${host}: ${error}`);
  for (const { host, error } of outcomes) {
    if (error !== null) console.error(`deploy ${host} failed after independent fleet dispatch`);
  }



  try {
    await verifyWorkerVersions(deployed, expectedSha, deployedAfter);
  } catch (error) {
    failures.push(error instanceof Error ? error.message : String(error));
  }
  const finalCoordinator = await statusReport();
  if (!coordinatorReportIsHealthy(finalCoordinator, expectedSha)) {
    failures.push(`coordinator is not live on exact build ${expectedSha} after worker rollout`);
  }

  if (failures.length > 0) {
    failDeploy(1, `fleet push incomplete:\n${failures.join("\n")}`);
  }
  console.log(`\n>> push complete — coordinator and ${deployed.length} workers report ${expectedSha}`);
}
