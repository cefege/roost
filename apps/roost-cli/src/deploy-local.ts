import { existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, rmSync, type Stats } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { durableRemove, durableWriteFile, flushDurablePath } from "@roost/shared/durability";
import { acquireMachineTransaction } from "./machine-transaction.ts";
import {
  coordServicePath,
  roostServiceDir,
  workerServicePath,
} from "@roost/shared/paths";
import {
  DeployFailure,
  failDeploy,
  finishWorkerDeploy,
  POSIX_WORKER_DEPLOY_JOURNAL_PATHS,
  run,
  workerServiceIsRunning,
  workerServiceMatchesRelease,
} from "./deploy-exec.ts";
import {
  _backfillEnvFromPlist,
  _resolveDeployEnvValue,
  parsePosixServiceEnvironment,
} from "./deploy-plist-env.ts";
import { linuxWorkerResourceEnvironment } from "./deploy-linux.ts";
import {
  launchdBootstrapWithRetryCmd,
  restartWorkerCmd,
  verifyWorkerCmd,
  WORKER_AGENT,
  WORKER_UNIT,
} from "./service-ctl.ts";


type CommandResult = { exit: number; stdout: string; stderr: string };
const LOCAL_WORKER_DEPLOY_JOURNAL_SCHEMA_VERSION = 1;
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
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value as Record<string, unknown>;
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

function decodeServiceSnapshot(snapshot: Readonly<LocalWorkerServiceSnapshot>): Buffer {
  return Buffer.from(snapshot.definitionBase64, "base64");
}

function serviceSnapshotMatches(
  actual: Readonly<LocalWorkerServiceSnapshot> | null,
  expected: Readonly<LocalWorkerServiceSnapshot> | null,
): boolean {
  return actual === null
    ? expected === null
    : expected !== null
      && actual.mode === expected.mode
      && actual.definitionBase64 === expected.definitionBase64;
}

function normalizedMetadataPath(value: string | null): string | null {
  return value && isAbsolute(value) && resolve(value) === value ? value : null;
}

function serviceGitSha(
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
  return isAbsolute(releaseRoot)
    && isAbsolute(stagedReleasePath)
    && resolve(releaseRoot) === releaseRoot
    && resolve(stagedReleasePath) === stagedReleasePath
    && dirname(stagedReleasePath) === releaseRoot;
}

function assertNormalizedAbsolutePath(value: string, label: string): void {
  if (!isAbsolute(value) || resolve(value) !== value) {
    throw new Error(`${label} must be a normalized absolute path`);
  }
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

async function priorServiceIsProven(
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
  if (journal.phase === "prepared") {
    await deps.cleanupStage(journal);
    await deps.clearJournal();
    return "prepared-cleaned";
  }

  if (journal.targetService) {
    let targetIsProven = false;
    try {
      const activeService = await deps.readService(journal);
      if (serviceSnapshotMatches(activeService, journal.targetService)) {
        targetIsProven = await deps.probeLifecycle(journal) === "running";
      }
    } catch {
      // Any failure to prove the exact target falls through to rollback.
    }
    if (targetIsProven) {
      await deps.commitTarget(journal);
      await deps.clearJournal();
      return "target-committed";
    }
  }

  await deps.restorePrior(journal);
  if (!await priorServiceIsProven(journal, deps)) {
    throw new Error("prior worker service definition and lifecycle could not be proven");
  }
  await deps.cleanupStage(journal);
  await deps.clearJournal();
  return "prior-restored";
}

export interface LocalWorkerActivation {
  install: () => Promise<CommandResult>;
  restart: () => Promise<CommandResult>;
  verify: () => Promise<CommandResult>;
  rollback: () => Promise<string | null>;
  cleanupStage: () => Promise<void>;
}

async function failLocalActivation(
  deps: LocalWorkerActivation,
  exitCode: number,
  message: string,
): Promise<never> {
  let rollbackError: string | null;
  try {
    rollbackError = await deps.rollback();
  } catch (error) {
    rollbackError = `rollback failed: ${error instanceof Error ? error.message : String(error)}`;
  }
  if (!rollbackError) await deps.cleanupStage();
  throw new DeployFailure(
    exitCode,
    `${message}\n${rollbackError ?? "prior worker service restored"}`,
  );
}

export async function _activateLocalWorker(
  deps: LocalWorkerActivation,
): Promise<{ install: CommandResult; verify: CommandResult }> {
  let install: CommandResult;
  try {
    install = await deps.install();
  } catch (error) {
    return failLocalActivation(
      deps,
      5,
      `install.sh failed: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  if (install.exit !== 0) {
    return failLocalActivation(
      deps,
      5,
      `install.sh failed\n${install.stdout}\n${install.stderr}`,
    );
  }

  let restarted: CommandResult;
  try {
    restarted = await deps.restart();
  } catch (error) {
    return failLocalActivation(
      deps,
      4,
      `restart failed: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  if (restarted.exit !== 0) {
    return failLocalActivation(
      deps,
      4,
      `restart failed (exit ${restarted.exit})\n${restarted.stdout}\n${restarted.stderr}`,
    );
  }

  let verify: CommandResult;
  try {
    verify = await deps.verify();
  } catch (error) {
    return failLocalActivation(
      deps,
      8,
      `worker service verification failed: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  if (verify.exit !== 0 || !workerServiceIsRunning(verify.stdout, process.platform === "linux" ? "linux" : "darwin")
    || !workerServiceMatchesRelease(verify.stdout)) {
    return failLocalActivation(
      deps,
      verify.exit || 8,
      `worker service verification failed\n${verify.stdout}\n${verify.stderr}`,
    );
  }
  return { install, verify };
}

function unescapeXml(value: string): string {
  return value
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", "\"")
    .replaceAll("&apos;", "'")
    .replaceAll("&amp;", "&");
}

function serviceWorkingDirectory(
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

function restoreCommand(
  os: "linux" | "darwin",
  servicePath: string,
  shouldRun: boolean,
): string {
  if (os === "linux") {
    const runtime = `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}";`;
    return shouldRun
      ? `${runtime} systemctl --user daemon-reload && systemctl --user restart ${WORKER_UNIT}`
      : `${runtime} systemctl --user stop ${WORKER_UNIT} 2>/dev/null || true; ` +
        `systemctl --user daemon-reload`;
  }
  if (!shouldRun) {
    return `launchctl bootout gui/$(id -u)/${WORKER_AGENT} 2>/dev/null || true`;
  }
  return launchdBootstrapWithRetryCmd(WORKER_AGENT, servicePath, { role: "worker rollback" });
}

export function localWorkerDeployJournalPath(serviceDir: string = roostServiceDir()): string {
  return join(resolve(serviceDir), "transactions", LOCAL_WORKER_DEPLOY_JOURNAL_FILE);
}
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


function readLocalWorkerServiceSnapshot(servicePath: string): LocalWorkerServiceSnapshot | null {
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

async function checkpointLocalWorkerDeployJournal(
  journalPath: string,
  journal: Readonly<LocalWorkerDeployJournalV1>,
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

async function probeLocalWorkerLifecycle(
  os: "linux" | "darwin",
): Promise<LocalWorkerLifecycle> {
  const status = await run(["bash", "-lc", verifyWorkerCmd(os)], { quiet: true });
  if (status.exit === 0 && workerServiceIsRunning(status.stdout, os)) return "running";
  if (os === "darwin") return status.exit === 0 || status.exit === 1 ? "stopped" : "unknown";
  if (status.exit === 0) return "stopped";
  const active = await run(
    [
      "bash",
      "-lc",
      `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"; ` +
        `systemctl --user is-active ${WORKER_UNIT} 2>/dev/null`,
    ],
    { quiet: true },
  );
  return ["inactive", "failed", "unknown", "deactivating"].includes(active.stdout.trim())
    ? "stopped"
    : "unknown";
}

async function localWorkerStartupPolicyIsEnabled(os: "linux" | "darwin"): Promise<boolean> {
  if (os === "linux") {
    const result = await run([
      "bash",
      "-lc",
      `export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"; ` +
        `systemctl --user is-enabled ${WORKER_UNIT}`,
    ], { quiet: true });
    return result.exit === 0 && result.stdout.trim() === "enabled";
  }
  const result = await run([
    "bash",
    "-lc",
    `launchctl print-disabled gui/$(id -u)`,
  ], { quiet: true });
  return result.exit === 0
    && !new RegExp(`"${WORKER_AGENT.replaceAll(".", "[.]")}"\\s*=>\\s*true`).test(result.stdout);
}

async function cleanupLocalWorkerStage(
  journal: Readonly<LocalWorkerDeployJournalV1>,
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

async function removeManagedPriorRelease(
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
  const coordinatorPath = coordServicePath();
  let coordinatorWorkingDirectory: string | null = null;
  if (existsSync(coordinatorPath)) {
    try {
      coordinatorWorkingDirectory = serviceWorkingDirectory(
        readFileSync(coordinatorPath, "utf8"),
        process.platform as "linux" | "darwin",
      );
    } catch (error) {
      throw new Error(
        `cannot prove coordinator release use before worker cleanup: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
    if (!coordinatorWorkingDirectory) {
      throw new Error("cannot prove coordinator WorkingDirectory before worker cleanup");
    }
  }
  if (coordinatorWorkingDirectory === priorWorkingDirectory) return;
  const removed = await run(["git", "worktree", "remove", "--force", priorWorkingDirectory], {
    cwd: sourceRepo,
    quiet: true,
  });
  if (removed.exit !== 0) {
    throw new Error(
      `cannot retire prior worker release ${priorWorkingDirectory}: ${removed.stderr.trim() || `exit ${removed.exit}`}`,
    );
  }
  if (removed.exit === 0 && existsSync(releaseRoot)) await flushDurablePath(releaseRoot);
}

function createLocalWorkerDeployRecoveryDeps(
  servicePath: string,
  journalPath: string,
  confinement: Readonly<LocalWorkerDeployConfinement>,
): LocalWorkerDeployRecoveryDeps {
  return {
    readService: () => readLocalWorkerServiceSnapshot(servicePath),
    probeLifecycle: (journal) => probeLocalWorkerLifecycle(journal.os),
    restorePrior: async (journal) => {
      if (journal.priorService) {
        await durableWriteFile(
          servicePath,
          decodeServiceSnapshot(journal.priorService),
          { mode: journal.priorService.mode },
        );
      } else if (lstatIfPresent(servicePath)) {
        await durableRemove(servicePath);
      }
      const restored = await run(
        ["bash", "-lc", restoreCommand(journal.os, servicePath, journal.priorWasRunning)],
        { cwd: journal.sourceRoot, quiet: true },
      );
      if (restored.exit !== 0) {
        throw new Error(
          `rollback failed (exit ${restored.exit})\n${restored.stdout}\n${restored.stderr}`,
        );
      }
    },
    cleanupStage: (journal) => cleanupLocalWorkerStage(journal, confinement),
    commitTarget: (journal) => removeManagedPriorRelease(
      journal.sourceRoot,
      journal.releaseRoot,
      journal.priorWorkingDirectory,
    ),
    clearJournal: async () => {
      if (lstatIfPresent(journalPath)) await durableRemove(journalPath);
    },
  };
}

/** Localhost source deployment uses the same immutable stage, service
 * snapshot, activation proof, and rollback contract as remote POSIX deploys. */
export async function _deployLocal(
  host: string,
  options: { sourceRoot: string; gitSha: string },
): Promise<void> {
  const os: "linux" | "darwin" = process.platform === "linux" ? "linux" : "darwin";
  const sourceRoot = realpathSync(resolve(options.sourceRoot));
  const localGitSha = options.gitSha;
  if (!/^[0-9a-f]{40}(?:[0-9a-f]{24})?$/i.test(localGitSha)) {
    failDeploy(7, "a localhost deploy requires an exact clean source commit");
  }
  const service = os === "linux" ? WORKER_UNIT : WORKER_AGENT;
  const configuredServiceDir = resolve(roostServiceDir());
  mkdirSync(configuredServiceDir, { recursive: true, mode: 0o700 });
  const serviceDir = realpathSync(configuredServiceDir);
  const journalPath = localWorkerDeployJournalPath(serviceDir);
  const transactionDirectory = dirname(journalPath);
  mkdirSync(transactionDirectory, { recursive: true, mode: 0o700 });
  if (realpathSync(transactionDirectory) !== transactionDirectory) {
    throw new DeployFailure(5, "worker deploy transaction directory must not traverse a symbolic link");
  }
  const releaseRoot = join(serviceDir, "releases", "worker");
  const confinement: LocalWorkerDeployConfinement = {
    os,
    sourceRoot,
    releaseRoot,
  };
  const servicePath = workerServicePath();
  const recoveryDeps = createLocalWorkerDeployRecoveryDeps(
    servicePath,
    journalPath,
    confinement,
  );
  const transaction = await acquireMachineTransaction("deploy", journalPath);
  try {
    for (const relative of [
      POSIX_WORKER_DEPLOY_JOURNAL_PATHS.linux,
      POSIX_WORKER_DEPLOY_JOURNAL_PATHS.darwin,
      POSIX_WORKER_DEPLOY_JOURNAL_PATHS.coordinator,
    ]) {
      const foreignJournal = join(serviceDir, relative);
      if (lstatIfPresent(foreignJournal)) {
        throw new DeployFailure(
          5,
          `cannot mutate past unsettled foreign worker deploy journal: ${foreignJournal}`,
        );
      }
    }
    const existingJournal = readLocalWorkerDeployJournal(journalPath);
    if (existingJournal) {
      try {
        const decision = await _recoverLocalWorkerDeployJournal(
          existingJournal,
          confinement,
          recoveryDeps,
        );
        console.log(`>> recovered interrupted local worker deploy (${decision})`);
      } catch (error) {
        throw new DeployFailure(
          5,
          `cannot recover interrupted local worker deploy: ${
            error instanceof Error ? error.message : String(error)
          }`,
        );
      }
    }
    const bunBin = Bun.which("bun") ?? process.execPath;
    const releaseId = `${localGitSha}-${crypto.randomUUID()}`;
    const releaseDir = join(releaseRoot, releaseId);


    console.log(`>> local deploy on ${host}`);
    const { env: hostEnv, filled } = await _backfillEnvFromPlist("self");
    if (filled.length > 0) {
      console.log(`>> reused from existing service: ${filled.join(", ")}`);
    }
    const priorService = readLocalWorkerServiceSnapshot(servicePath);
    const priorText = priorService
      ? decodeServiceSnapshot(priorService).toString("utf8")
      : "";
    const priorWorkingDirectory = priorService
      ? normalizedMetadataPath(serviceWorkingDirectory(priorText, os))
      : null;
    const priorGitSha = priorService ? serviceGitSha(priorText, os) : null;
    const priorStatus = priorService
      ? await run(["bash", "-lc", verifyWorkerCmd(os)], { quiet: true })
      : { exit: 1, stdout: "", stderr: "" };
    if (priorService && os === "linux" && priorStatus.exit !== 0) {
      failDeploy(5, `cannot snapshot ${service} active state before activation`);
    }
    const priorWasRunning = priorService !== null
      && priorStatus.exit === 0
      && workerServiceIsRunning(priorStatus.stdout, os);
    if (priorService && (!priorWasRunning || !await localWorkerStartupPolicyIsEnabled(os))) {
      failDeploy(
        5,
        "the existing local worker must be running with its normal automatic startup policy before update",
      );
    }

    const installEnv: Record<string, string> = {
      ...hostEnv,
      ...(os === "linux" ? linuxWorkerResourceEnvironment(priorText) : {}),
    };
    for (const key of ["GIT_SHA", "ROOST_GIT_SHA", "ROOST_WORKDIR", "ROOST_EXEC_BIN", "ROOST_BOOTSTRAP_TOKEN"]) {
      delete installEnv[key];
    }
    for (const key of [
      "ROOST_COORDINATOR_URL",
      "ROOST_WORKER_LABEL",
      "ROOST_REACHABLE_ADDR",
    ]) {
      const value = _resolveDeployEnvValue(key, hostEnv);
      if (value === undefined) delete installEnv[key];
      else installEnv[key] = value;
    }
    if (process.env.ROOST_BOOTSTRAP_TOKEN) {
      installEnv.ROOST_BOOTSTRAP_TOKEN = process.env.ROOST_BOOTSTRAP_TOKEN;
    }
    if (!installEnv.ROOST_COORDINATOR_URL) {
      failDeploy(6, "ROOST_COORDINATOR_URL env var required (no prior service definition to reuse)");
    }
    installEnv.BUN_BIN = bunBin;
    installEnv.GIT_SHA = localGitSha;

    mkdirSync(releaseRoot, { recursive: true, mode: 0o700 });
    if (realpathSync(releaseRoot) !== releaseRoot) {
      failDeploy(5, "worker release root must not traverse a symbolic link");
    }
    let journal: LocalWorkerDeployJournalV1 = {
      schemaVersion: LOCAL_WORKER_DEPLOY_JOURNAL_SCHEMA_VERSION,
      phase: "prepared",
      os,
      sourceRoot,
      releaseRoot,
      stagedReleasePath: releaseDir,
      targetSha: localGitSha,
      priorService,
      priorWasRunning,
      priorWorkingDirectory,
      priorGitSha,
      targetService: null,
    };
    await checkpointLocalWorkerDeployJournal(journalPath, journal, confinement);
    const cleanupStage = async (): Promise<void> => {
      await recoveryDeps.cleanupStage(journal);
      await recoveryDeps.clearJournal();
    };

    console.log(`>> stage ${localGitSha.slice(0, 8)} in ${releaseDir}`);
    const stage = await run(
      ["git", "worktree", "add", "--quiet", "--force", "--detach", releaseDir, localGitSha],
      { cwd: sourceRoot, quiet: true },
    );
    if (stage.exit !== 0) {
      await cleanupStage();
      failDeploy(2, `local source snapshot failed\n${stage.stdout}\n${stage.stderr}`);
    }
    let expectedRelease: string;
    let dependencies: CommandResult;
    try {
      expectedRelease = realpathSync(releaseDir);
      if (expectedRelease !== releaseDir) {
        throw new Error("staged worker release resolved outside its journaled path");
      }
      console.log(">> frozen bun install locally");
      dependencies = await run([bunBin, "install", "--frozen-lockfile"], {
        cwd: expectedRelease,
        quiet: true,
      });
    } catch (error) {
      await cleanupStage();
      throw error;
    }
    if (dependencies.exit !== 0) {
      await cleanupStage();
      failDeploy(4, `bun install failed\n${dependencies.stdout}\n${dependencies.stderr}`);
    }

    journal = { ...journal, phase: "activating" };
    await checkpointLocalWorkerDeployJournal(journalPath, journal, confinement);
    const rollback = async (): Promise<string | null> => {
      try {
        await recoveryDeps.restorePrior(journal);
        if (await priorServiceIsProven(journal, recoveryDeps)) return null;
        return priorWasRunning
          ? "rollback service did not become healthy with its exact prior definition"
          : "rollback could not prove the prior stopped lifecycle and exact definition";
      } catch (error) {
        return `rollback failed: ${error instanceof Error ? error.message : String(error)}`;
      }
    };

    console.log(`>> activate staged ${service}`);
    const activated = await _activateLocalWorker({
      install: async () => {
        const result = await run(
          ["bash", join(expectedRelease, "apps", "worker", "scripts", "install.sh"), "install"],
          { cwd: expectedRelease, quiet: true, env: installEnv },
        );
        if (result.exit !== 0) return result;
        const targetService = readLocalWorkerServiceSnapshot(servicePath);
        if (!targetService) throw new Error("install.sh did not create a worker service definition");
        const targetDefinition = decodeServiceSnapshot(targetService).toString("utf8");
        if (!localWorkerReleaseMatches(targetDefinition, os, expectedRelease, localGitSha)) {
          throw new Error("install.sh created a worker service definition for the wrong release");
        }
        journal = { ...journal, targetService };
        await checkpointLocalWorkerDeployJournal(journalPath, journal, confinement);
        return result;
      },
      restart: () => run(["bash", "-lc", restartWorkerCmd(os)], {
        cwd: expectedRelease,
        quiet: true,
      }),
      verify: async () => {
        const result = await run(["bash", "-lc", verifyWorkerCmd(os)], { quiet: true });
        try {
          const activeService = readLocalWorkerServiceSnapshot(servicePath);
          if (
            journal.targetService
            && serviceSnapshotMatches(activeService, journal.targetService)
            && localWorkerReleaseMatches(
              decodeServiceSnapshot(journal.targetService).toString("utf8"),
              os,
              expectedRelease,
              localGitSha,
            )
          ) {
            result.stdout += `${result.stdout.endsWith("\n") ? "" : "\n"}RoostReleaseMatch=yes\n`;
          }
        } catch {
          // The missing, unreadable, or changed definition fails release verification.
        }
        return result;
      },
      rollback,
      cleanupStage,
    });
    const installOutput = `${activated.install.stdout}${activated.install.stderr}`.trim();
    if (installOutput) {
      console.log(installOutput.split("\n").map((line) => `   ${line}`).join("\n"));
    }
    journal = { ...journal, phase: "activated" };
    await checkpointLocalWorkerDeployJournal(journalPath, journal, confinement);
    await recoveryDeps.commitTarget(journal);
    await recoveryDeps.clearJournal();
    finishWorkerDeploy(
      activated.verify,
      `>> done — ${host} v2 worker deployed (local)`,
      os,
    );
  } finally {
    await transaction.release();
  }
}
